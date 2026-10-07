//! The out-of-process CEF host and its line protocol (see host/src/main.cpp).
//! Chromium, its renderer and GPU/utility processes live in that process tree;
//! a crash, hang or OOM kill there surfaces here as an [`HtmlError`].

use std::path::PathBuf;
use std::time::Duration;

use ferrocut_ipc::{Host, HostSpec, IpcError};

use crate::policy::ResolvedPolicy;

pub const PROTOCOL_VERSION: &str = "1";
/// Version of the injected time shim, from host/src/shim.h. Part of node hashes.
pub const SHIM_VERSION: &str = env!("FERROCUT_HTML_SHIM_VERSION");
/// Pinned CEF build (scripts/fetch-cef.sh). Part of node hashes.
pub const CEF_VERSION: &str = env!("FERROCUT_HTML_CEF_VERSION");

/// Where the host binary lives and how long we give it.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub host_exe: PathBuf,
    /// Max time for HELLO and OPEN (page load).
    pub open_timeout: Duration,
    /// Max time for one ADVANCE / STEP / CAPTURE before the host is killed.
    pub step_timeout: Duration,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig { host_exe: bundled_host_exe(), open_timeout: Duration::from_secs(60), step_timeout: Duration::from_secs(30) }
    }
}

/// The host built by build.rs (override at run time with FERROCUT_HTML_HOST).
pub fn bundled_host_exe() -> PathBuf {
    std::env::var_os("FERROCUT_HTML_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("FERROCUT_HTML_HOST_EXE")))
}

/// Errors from the host process (see [`ferrocut_ipc::IpcError`]). Page
/// failures (load error, script exception in a step, paint never settled) are
/// `ERR` replies and read "page: ...".
pub type HtmlError = IpcError;

/// Name used in error messages ("html host died during STEP: ...").
pub const HOST_NAME: &str = "html host";

/// Chromium log lines that are noise for us (kept out of error messages).
const BENIGN_STDERR: &[&str] = &["page_load_metrics_update_dispatcher"];

impl HostConfig {
    /// How [`ferrocut_ipc`] starts this host. Each process gets a private
    /// Chromium profile dir (`FERROCUT_HTML_PROFILE_DIR`), removed after it exits
    /// even if it was killed.
    pub fn host_spec(&self) -> HostSpec {
        let mut spec = HostSpec::new(HOST_NAME, "ferrocut-html", &self.host_exe);
        spec.remote_label = "page";
        spec.private_dir_env = Some("FERROCUT_HTML_PROFILE_DIR");
        spec.stderr_ignore = BENIGN_STDERR;
        spec.quit_timeout = Duration::from_secs(2);
        spec
    }
}

/// Start one `ferrocut-html-host` process with `policy` and check that it is
/// the build this crate expects (protocol, CEF and shim versions all feed node
/// hashes).
pub fn spawn_host(cfg: &HostConfig, policy: &ResolvedPolicy) -> Result<Host, HtmlError> {
    let mut spec = cfg.host_spec();
    spec.env.extend(policy.host_env());
    let mut h = Host::spawn(&spec)?;
    h.handshake(&["ferrocut-html-host", PROTOCOL_VERSION, CEF_VERSION, SHIM_VERSION], cfg.open_timeout)?;
    Ok(h)
}
