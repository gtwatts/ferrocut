//! ferrocut-ofx: OpenFX image effect plugins as Ferrocut render nodes.
//!
//! Plugins never load into the Ferrocut process. Each render worker gets its
//! own `ferrocut-ofx-host` child process (C++, built on the OpenFX
//! HostSupport library) that hosts one plugin instance. Frames travel through
//! shared memory (RGBA f32 files in /dev/shm, zero-copy on both sides); control
//! is a tab-separated line protocol over the child's stdin/stdout. If a plugin
//! segfaults, aborts, hangs or leaks, only its host dies (or is killed /
//! recycled) and the node returns a [`ferrocut_core::NodeError`].

//!
//! Without the fetched OpenFX sources (see README) build.rs sets
//! `cfg(ferrocut_ofx_no_host)` and only the shared-memory helper is built.

#[cfg(not(ferrocut_ofx_no_host))]
pub mod host;
#[cfg(not(ferrocut_ofx_no_host))]
pub mod node;
pub mod shm;

#[cfg(not(ferrocut_ofx_no_host))]
pub use host::{HostConfig, HostProcess, OfxError, RenderReply, bundled_plugin_dir, OPAQUE, PREMULTIPLIED, UNPREMULTIPLIED};
#[cfg(not(ferrocut_ofx_no_host))]
pub use node::{OfxNode, OfxPluginSpec, OfxSession};
pub use shm::ShmFrame;
