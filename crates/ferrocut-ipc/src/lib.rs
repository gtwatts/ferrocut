//! Plumbing shared by Ferrocut's out-of-process plugin hosts
//! (`ferrocut-ofx`, `ferrocut-html`).
//!
//! Plugins that might crash, hang, leak or need a different runtime (OpenFX
//! binaries, Chromium) run in a child process. This crate owns everything about
//! that child that is not plugin-specific:
//!
//! - [`Host`]: spawn a host binary, talk to it with a line protocol, enforce
//!   per-request timeouts (SIGKILL on expiry), report crashes with the signal
//!   name and the last stderr lines, own a private temp dir, shut down politely.
//! - [`HostSlot`]: one lazily started host per (worker, node): restart after a
//!   crash/hang, recycle when it grows past a memory budget, count spawns.
//! - [`ShmBuffer`]: a `/dev/shm` file mapped into both processes for frames
//!   (zero-copy); unlinked on drop.
//! - [`IpcError`]: what went wrong, with [`IpcError::kind`] /
//!   [`IpcError::to_node_error`] mapping it onto the engine's retry policy.
//!
//! # Wire protocol
//!
//! One request line on the host's stdin gets exactly one reply line on its
//! stdout. Fields are tab-separated; the first request field is the verb.
//!
//! | reply              | meaning                                         | result |
//! |--------------------|-------------------------------------------------|--------|
//! | `OK\t<fields…>`    | success                                         | `Ok(fields)` |
//! | `ERR\t<message>`   | request failed in the plugin; host still usable | [`IpcError::Remote`] |
//! | `FATAL\t<message>` | host is going down (e.g. its renderer died)     | [`IpcError::HostDied`] |
//! | anything else      | confused host                                   | [`IpcError::Protocol`] |
//!
//! Hosts must exit on `QUIT` or on EOF on stdin, and must answer a `HELLO`
//! with an identification line (see [`Host::handshake`]). Everything else is
//! up to the plugin crate.
//!
//! # Error policy
//!
//! A dead, hung or confused host is *lost*: the request may well succeed in a
//! fresh process, so it maps to [`ErrorKind::Retryable`] and [`HostSlot`]
//! discards the process. `ERR` replies, missing binaries and I/O errors repeat
//! deterministically and map to [`ErrorKind::Permanent`].
//!
//! Linux only (uses `/proc`, `/dev/shm` and Unix signals).

mod error;
mod host;
mod shm;
mod slot;

pub use error::IpcError;
pub use ferrocut_types::ErrorKind;
pub use host::{Host, HostSpec};
pub use shm::ShmBuffer;
pub use slot::HostSlot;
