//! HTML/CSS/JS layers for Ferrocut, rendered by Chromium (CEF, BSD-3-Clause)
//! offscreen in an isolated host process. Page clocks (CSS animations, rAF,
//! `performance.now`, `Date`, timers, media) follow the graph's `RationalTime`
//! only; see README for the determinism model.
//!
//! Without the CEF distribution (scripts/fetch-cef.sh) the crate builds as a
//! stub exposing only [`color`].

pub mod color;

#[cfg(not(ferrocut_html_no_host))]
mod adapter;
#[cfg(not(ferrocut_html_no_host))]
pub mod host;
#[cfg(not(ferrocut_html_no_host))]
pub mod session;

pub use color::OutputEncoding;

#[cfg(not(ferrocut_html_no_host))]
pub use adapter::{HtmlNode, HtmlParams, HtmlSource};
#[cfg(not(ferrocut_html_no_host))]
pub use host::{HostConfig, HtmlError};
#[cfg(not(ferrocut_html_no_host))]
pub use session::{HtmlSession, SessionParams};
