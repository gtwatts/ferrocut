//! Lossless concatenation of chunk files: packets are copied, never re-encoded.

use std::path::Path;

use anyhow::{Context as _, anyhow, ensure};
use ferrocut_core::{FrameRate, RationalTime};
use ffmpeg_next::{codec, encoder, format, media};

use super::encode::{pts_to_frame, set_bitexact};
use super::{init, to_core};

/// Concatenate chunks (each starting at frame `start_frames[i]` on the output
/// timeline) into `out`. Verifies all chunks share identical codec extradata.
pub fn concat(
    chunks: &[&Path],
    start_frames: &[i64],
    fps: FrameRate,
    out: &Path,
) -> anyhow::Result<u64> {
    init();
    ensure!(
        !chunks.is_empty() && chunks.len() == start_frames.len(),
        "bad concat input"
    );
    let mut octx = format::output_as(out, "matroska")
        .with_context(|| format!("creating {}", out.display()))?;
    set_bitexact(&mut octx);
    let first = format::input(chunks[0])?;
    let ist = first
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("chunk has no video"))?;
    let ref_extra = extradata(&ist.parameters());
    {
        let mut ost = octx.add_stream(encoder::find(codec::Id::None))?;
        ost.set_parameters(ist.parameters());
        ost.set_time_base(ist.time_base());
        // SAFETY: clearing the codec tag on our own stream parameters before write_header.
        unsafe {
            (*ost.parameters().as_mut_ptr()).codec_tag = 0;
        }
    }
    drop(first);
    octx.write_header()?;
    let ost_tb = octx.stream(0).expect("stream").time_base();
    let mut packets = 0u64;
    for (path, &start) in chunks.iter().zip(start_frames) {
        let mut ictx =
            format::input(path).with_context(|| format!("opening chunk {}", path.display()))?;
        let (idx, ist_tb) = {
            let ist = ictx
                .streams()
                .best(media::Type::Video)
                .ok_or_else(|| anyhow!("chunk has no video"))?;
            ensure!(
                extradata(&ist.parameters()) == ref_extra,
                "{}: codec config differs",
                path.display()
            );
            (ist.index(), ist.time_base())
        };
        for (s, mut pkt) in ictx.packets() {
            if s.index() != idx {
                continue;
            }
            let local = pts_to_frame(
                pkt.pts().ok_or_else(|| anyhow!("packet without pts"))?,
                ist_tb,
                fps,
            );
            let g = start + local;
            let pts = RationalTime::from_frames(g, fps).to_pts(to_core(ost_tb));
            let next = RationalTime::from_frames(g + 1, fps).to_pts(to_core(ost_tb));
            pkt.set_pts(Some(pts));
            pkt.set_dts(Some(pts)); // intra-only: dts == pts
            pkt.set_duration(next - pts);
            pkt.set_position(-1);
            pkt.set_stream(0);
            pkt.write_interleaved(&mut octx)?;
            packets += 1;
        }
    }
    octx.write_trailer()?;
    Ok(packets)
}

fn extradata(p: &codec::Parameters) -> Vec<u8> {
    // SAFETY: reading extradata/extradata_size from valid codec parameters.
    unsafe {
        let raw = p.as_ptr();
        if (*raw).extradata.is_null() || (*raw).extradata_size <= 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts((*raw).extradata, (*raw).extradata_size as usize).to_vec()
        }
    }
}
