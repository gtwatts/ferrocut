//! `with_alloc_scope`: failed GPU allocations become Retryable out-of-memory
//! `NodeError`s (never invalid objects that break a later frame), and the pool
//! never hands out a texture from before an OOM. Skips without a GPU adapter.

use ferrocut_core::{
    AdapterPreference, ErrorKind, GpuContext, GpuFault, WORKING_FORMAT, with_alloc_scope,
};

fn gpu_or_skip() -> Option<GpuContext> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            None
        }
    }
}

const USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::COPY_DST);

#[test]
fn budget_overrun_is_retryable_oom_and_poisons_the_pool() {
    let Some(gpu) = gpu_or_skip() else { return };
    let tex = |gpu: &GpuContext| gpu.pooled_texture(64, 64, WORKING_FORMAT, USAGE, "t");

    // Warm the pool with one idle texture.
    drop(with_alloc_scope(&gpu, || tex(&gpu)).unwrap());
    let one = gpu.pool_live_bytes();
    assert!(one > 0);

    // Room for one texture only: the second allocation is an OOM.
    gpu.set_memory_budget(Some(one));
    let held = with_alloc_scope(&gpu, || tex(&gpu)).unwrap(); // reused, no new bytes
    let e = with_alloc_scope(&gpu, || tex(&gpu)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Retryable, "{e}");
    assert_eq!(e.gpu_fault, Some(GpuFault::OutOfMemory), "{e}");
    assert!(e.is_gpu_out_of_memory());
    assert_eq!(gpu.out_of_memory_events(), 1);

    // Textures leased before the OOM are not re-pooled: the next one is fresh.
    let before = gpu.pool_stats();
    drop(held);
    assert_eq!(gpu.pool_live_bytes(), 0, "pre-OOM textures were freed");
    gpu.set_memory_budget(None);
    drop(with_alloc_scope(&gpu, || tex(&gpu)).unwrap());
    let after = gpu.pool_stats();
    assert_eq!(after.allocated, before.allocated + 1);
    assert_eq!(after.reused, before.reused);
    assert_eq!(gpu.pool_live_bytes(), one);
}

#[test]
fn invalid_object_fallout_counts_as_oom_only_after_an_oom() {
    let Some(gpu) = gpu_or_skip() else { return };
    let bad = with_alloc_scope(&gpu, || {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bad"),
            size: wgpu::Extent3d {
                width: 0,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: USAGE,
            view_formats: &[],
        })
    });
    // A plain validation error is a bug: Permanent, not retried.
    let e = bad.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent, "{e}");

    // Reproduce "X is invalid": use an object whose creation failed.
    let invalid = {
        let scope = gpu.error_scope();
        let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bad"),
            size: wgpu::Extent3d {
                width: 0,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WORKING_FORMAT,
            usage: USAGE,
            view_formats: &[],
        });
        drop(scope.finish());
        t
    };
    let view = || invalid.create_view(&wgpu::TextureViewDescriptor::default());
    let e = with_alloc_scope(&gpu, view).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Permanent, "no OOM yet: {e}");

    // After an OOM, the same error is fallout of a failed allocation.
    gpu.note_out_of_memory();
    let e = with_alloc_scope(&gpu, view).unwrap_err();
    assert_eq!(e.gpu_fault, Some(GpuFault::OutOfMemory), "{e}");
    assert!(e.is_retryable());
}
