//! `Frame::to_gpu` uploads through the texture pool: they count against the
//! memory budget, are reused, and never overwrite a texture that unsubmitted
//! batched work still reads (`queue.write_texture` runs at the start of the
//! next submit, ahead of that work). Skips without a GPU adapter.

use ferrocut_core::{
    AdapterPreference, ColorSpace, CpuFrame, ErrorKind, Frame, FrameStorage, GpuContext, GpuFault,
    WORKING_FORMAT, WorkerState, with_alloc_scope,
};
use half::f16;

const N: u32 = 16;

fn gpu_or_skip() -> Option<GpuContext> {
    match GpuContext::new(AdapterPreference::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipping: no GPU adapter ({e})");
            None
        }
    }
}

fn solid(rgba: [f32; 4]) -> Frame {
    let px: Vec<f16> = (0..N * N).flat_map(|_| rgba.map(f16::from_f32)).collect();
    Frame::from_cpu(&CpuFrame::new(N, N, ColorSpace::acescg(), px))
}

fn first_pixel(gpu: &GpuContext, f: &Frame) -> [f32; 4] {
    let c = f.to_cpu(gpu).unwrap();
    let FrameStorage::Cpu(img) = &c.storage else {
        unreachable!()
    };
    [0, 1, 2, 3].map(|i| img.pixels[i].to_f32())
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

#[test]
fn uploads_are_pooled_reused_and_budgeted() {
    let Some(gpu) = gpu_or_skip() else { return };
    let up = solid(RED).to_gpu(&gpu);
    assert!(up.gpu().unwrap().is_pooled());
    let one = gpu.pool_live_bytes();
    assert_eq!(
        one,
        (N * N * 8) as u64,
        "an upload counts as live pool bytes"
    );
    assert_eq!(first_pixel(&gpu, &up), RED);
    drop(up);

    // No batched work pending: the idle texture is reused for the next upload.
    let before = gpu.pool_stats();
    let up = solid(GREEN).to_gpu(&gpu);
    let after = gpu.pool_stats();
    assert_eq!(after.allocated, before.allocated);
    assert_eq!(after.reused, before.reused + 1);
    assert_eq!(first_pixel(&gpu, &up), GREEN);

    // Over budget: the upload is a Retryable out-of-memory error.
    gpu.set_memory_budget(Some(one));
    let e = with_alloc_scope(&gpu, || solid(BLUE).to_gpu(&gpu)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Retryable, "{e}");
    assert_eq!(e.gpu_fault, Some(GpuFault::OutOfMemory), "{e}");
    drop(up);
}

#[test]
fn upload_never_reuses_a_texture_unsubmitted_work_reads() {
    let Some(gpu) = gpu_or_skip() else { return };
    let mut worker = WorkerState::default();
    let dst = gpu.pooled_texture(
        N,
        N,
        WORKING_FORMAT,
        ferrocut_core::frame::WORKING_USAGE,
        "dst",
    );

    // Batched (unsubmitted) copy that reads upload A, then A is dropped.
    let a = solid(RED).to_gpu(&gpu);
    worker.encoder(&gpu).copy_texture_to_texture(
        a.gpu().unwrap().texture.as_image_copy(),
        dst.texture.as_image_copy(),
        a.gpu().unwrap().texture.size(),
    );
    drop(a);

    // Reusing A here would let B's write land before the copy runs.
    let before = gpu.pool_stats();
    let b = solid(GREEN).to_gpu(&gpu);
    let after = gpu.pool_stats();
    assert_eq!(
        after.allocated,
        before.allocated + 1,
        "A must not be reused yet"
    );
    assert_eq!(after.reused, before.reused);

    worker.flush(&gpu);
    let dst_frame = solid(BLUE).with_storage(FrameStorage::Gpu(dst));
    assert_eq!(
        first_pixel(&gpu, &dst_frame),
        RED,
        "the copy saw A's pixels"
    );
    drop(b);

    // Once the batch is submitted, idle uploads are reused again.
    let before = gpu.pool_stats();
    let c = solid(BLUE).to_gpu(&gpu);
    let after = gpu.pool_stats();
    assert_eq!(after.allocated, before.allocated);
    assert_eq!(after.reused, before.reused + 1);
    assert_eq!(first_pixel(&gpu, &c), BLUE);
}
