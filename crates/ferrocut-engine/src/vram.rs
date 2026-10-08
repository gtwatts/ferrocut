//! Free VRAM (NVIDIA, via NVML loaded at runtime) and the default render job
//! count derived from it.
//!
//! wgpu exposes no memory budget, so on NVIDIA adapters we ask NVML
//! (`libnvidia-ml.so.1`, shipped with the driver) through `dlopen`: no build or
//! link dependency, and nothing happens on machines without it. The adapter is
//! matched by PCI device id (falling back to the only NVIDIA GPU). Other
//! adapters (lavapipe: system RAM) keep the core-count default.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

const NVIDIA: u32 = 0x10de;

/// Bytes per output pixel one chunk worker keeps on the GPU, roughly (measured:
/// 12 workers of demo-av.json at 1080p fit in ~1.8 GB, ~150 MB each): working
/// frames (RGBA16F) for sources, layers and intermediates, the 3-deep RGBA8
/// output ring with its readback buffers, and upload staging. Deliberately
/// generous; the scheduler backs off adaptively if it is still too optimistic.
pub const BYTES_PER_PIXEL_PER_JOB: u64 = 80;
/// VRAM left for the driver, other contexts and allocation slack.
pub const RESERVE_BYTES: u64 = 512 << 20;

#[repr(C)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

#[repr(C)]
struct NvmlPciInfo {
    bus_id_legacy: [c_char; 16],
    domain: c_uint,
    bus: c_uint,
    device: c_uint,
    pci_device_id: c_uint,
    pci_sub_system_id: c_uint,
    bus_id: [c_char; 32],
}

type Device = *mut c_void;

struct Nvml {
    lib: *mut c_void,
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            if let Some(f) = self.sym::<unsafe extern "C" fn() -> c_int>(c"nvmlShutdown") {
                f();
            }
            libc::dlclose(self.lib);
        }
    }
}

impl Nvml {
    fn open() -> Option<Nvml> {
        let lib = unsafe { libc::dlopen(c"libnvidia-ml.so.1".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return None;
        }
        let n = Nvml { lib };
        let init = unsafe { n.sym::<unsafe extern "C" fn() -> c_int>(c"nvmlInit_v2")? };
        (unsafe { init() } == 0).then_some(n)
    }

    /// # Safety
    /// `T` must be the function-pointer type of the symbol.
    unsafe fn sym<T: Copy>(&self, name: &CStr) -> Option<T> {
        let p = unsafe { libc::dlsym(self.lib, name.as_ptr()) };
        (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
    }

    /// (free, total) bytes of the GPU with PCI device id `device`, or of the
    /// only GPU if none matches.
    fn memory(&self, device: u32) -> Option<(u64, u64)> {
        unsafe {
            let count_f =
                self.sym::<unsafe extern "C" fn(*mut c_uint) -> c_int>(c"nvmlDeviceGetCount_v2")?;
            let handle_f = self.sym::<unsafe extern "C" fn(c_uint, *mut Device) -> c_int>(
                c"nvmlDeviceGetHandleByIndex_v2",
            )?;
            let pci_f = self.sym::<unsafe extern "C" fn(Device, *mut NvmlPciInfo) -> c_int>(
                c"nvmlDeviceGetPciInfo_v3",
            )?;
            let mem_f = self.sym::<unsafe extern "C" fn(Device, *mut NvmlMemory) -> c_int>(
                c"nvmlDeviceGetMemoryInfo",
            )?;
            let mut count = 0;
            if count_f(&mut count) != 0 || count == 0 {
                return None;
            }
            let mut pick = None;
            for i in 0..count {
                let mut h: Device = std::ptr::null_mut();
                if handle_f(i, &mut h) != 0 {
                    continue;
                }
                let mut pci: NvmlPciInfo = std::mem::zeroed();
                if pci_f(h, &mut pci) == 0 && pci.pci_device_id >> 16 == device {
                    pick = Some(h);
                    break;
                }
                if count == 1 {
                    pick = Some(h);
                }
            }
            let mut m = NvmlMemory {
                total: 0,
                free: 0,
                used: 0,
            };
            (mem_f(pick?, &mut m) == 0).then_some((m.free, m.total))
        }
    }
}

/// (free, total) VRAM bytes of `info`'s GPU, if it is NVIDIA and NVML is available.
pub fn memory(info: &wgpu::AdapterInfo) -> Option<(u64, u64)> {
    if info.vendor != NVIDIA || info.device_type == wgpu::DeviceType::Cpu {
        return None;
    }
    Nvml::open()?.memory(info.device)
}

/// "; NVML: X MiB of Y MiB free" for error messages (empty if unknown).
pub fn free_note(info: &wgpu::AdapterInfo) -> String {
    match memory(info) {
        Some((free, total)) => format!("; NVML: {} MiB of {} MiB free", free >> 20, total >> 20),
        None => String::new(),
    }
}

/// Core-count default: min(cores, 12).
pub fn cpu_default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(12)
}

/// Pure sizing rule: jobs that fit in `free` bytes at `width`x`height`, within
/// 1..=`cap`.
pub fn jobs_for(free: u64, width: u32, height: u32, cap: usize) -> usize {
    let per_job = (width as u64 * height as u64 * BYTES_PER_PIXEL_PER_JOB).max(1);
    let fit = free.saturating_sub(RESERVE_BYTES) / per_job;
    (fit as usize).clamp(1, cap.max(1))
}

/// Default `-j`: min(cores, 12), lowered to what fits in free VRAM on NVIDIA.
/// Returns the job count and a one-line reason for logs.
pub fn default_jobs(info: &wgpu::AdapterInfo, width: u32, height: u32) -> (usize, String) {
    let cap = cpu_default_jobs();
    match memory(info) {
        Some((free, total)) => {
            let j = jobs_for(free, width, height, cap);
            (
                j,
                format!(
                    "{j} jobs: {} of {} MiB VRAM free, ~{} MiB per job at {width}x{height} (cap {cap})",
                    free >> 20,
                    total >> 20,
                    (width as u64 * height as u64 * BYTES_PER_PIXEL_PER_JOB) >> 20
                ),
            )
        }
        None => (cap, format!("{cap} jobs: min(cores, 12)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_rule() {
        let mib = 1u64 << 20;
        // 1080p: ~158 MiB per job.
        assert_eq!(jobs_for(1851 * mib, 1920, 1080, 12), 8);
        assert_eq!(jobs_for(24 * 1024 * mib, 1920, 1080, 12), 12);
        assert_eq!(jobs_for(300 * mib, 1920, 1080, 12), 1, "never below 1");
        assert_eq!(jobs_for(0, 1920, 1080, 12), 1);
        assert_eq!(jobs_for(4096 * mib, 64, 32, 3), 3, "capped");
    }
}
