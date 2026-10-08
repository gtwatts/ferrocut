//! Ferrocut delivery encoder: the engine's lossless FFV1 master → an
//! H.264 (Cisco OpenH264, downloaded at runtime) + AAC MP4 with faststart.
//!
//! ```no_run
//! use ferrocut_deliver::{DeliverOptions, deliver, default_output, openh264::Provider};
//! let master = std::path::Path::new("out.mkv");
//! let opts = DeliverOptions::new(Provider::from_env()?);
//! let report = deliver(master, &default_output(master), &opts)?;
//! println!("{} frames, {}", report.frames, report.openh264.notice);
//! # Ok::<(), ferrocut_types::error::NodeError>(())
//! ```
//!
//! See the README for the OpenH264 licensing model and the engine
//! integration proposal.

pub mod color;
pub mod deliver;
pub mod encoder;
pub mod h264;
pub mod media;
pub mod mux;
pub mod openh264;

pub use deliver::{
    ChunkPlan, DeliverOptions, DeliverReport, default_jobs, default_output, deliver,
    render_chunk_starts, report_path,
};
