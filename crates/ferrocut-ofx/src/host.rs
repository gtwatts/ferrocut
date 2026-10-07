//! The out-of-process OpenFX host and its control channel.

use std::path::PathBuf;
use std::time::Duration;

use ferrocut_core::{FrameRate, RationalTime};
use ferrocut_ipc::{Host, HostSpec, IpcError};

use crate::shm::ShmFrame;

pub const PROTOCOL_VERSION: &str = "1";

/// Where the host binary lives and how long we give it.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub host_exe: PathBuf,
    /// Directories scanned for `*.ofx.bundle` (in addition to /usr/OFX/Plugins).
    pub plugin_paths: Vec<PathBuf>,
    /// Max time for HELLO/LIST/LOAD/PARAM.
    pub control_timeout: Duration,
    /// Max time for one RENDER before the host is killed.
    pub render_timeout: Duration,
    /// Restart the host when its resident memory exceeds this (leaky plugins).
    pub max_rss_bytes: Option<u64>,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig {
            host_exe: std::env::var_os("FERROCUT_OFX_HOST")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("FERROCUT_OFX_HOST_EXE"))),
            plugin_paths: Vec::new(),
            control_timeout: Duration::from_secs(10),
            render_timeout: Duration::from_secs(60),
            max_rss_bytes: None,
        }
    }
}

/// Directory with the plugin bundles built alongside the host (SDK Invert +
/// Ferrocut test plugins).
pub fn bundled_plugin_dir() -> PathBuf {
    PathBuf::from(env!("FERROCUT_OFX_BUNDLED_PLUGINS"))
}

/// Errors from the host process (see [`ferrocut_ipc::IpcError`]); `ERR`
/// replies read "OFX plugin error: ...".
pub type OfxError = IpcError;

impl HostConfig {
    /// How [`ferrocut_ipc`] starts this host.
    pub fn host_spec(&self) -> HostSpec {
        let mut spec = HostSpec::new("OFX host", "ferrocut-ofx", &self.host_exe);
        spec.remote_label = "OFX plugin error";
        for p in &self.plugin_paths {
            spec.args.push("--plugin-path".into());
            spec.args.push(p.into());
        }
        spec.quit_timeout = Duration::from_millis(500);
        spec
    }
}

/// Result of one RENDER.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderReply {
    /// Premultiplication of the output buffer as declared by the plugin's
    /// output clip (`OfxImageAlphaPremultiplied`, `OfxImageAlphaUnPremultiplied`, `OfxImageOpaque`).
    pub output_premult: String,
    /// `rendered` or `identity` (plugin said pass-through; host copied source).
    pub how: String,
}

pub const PREMULTIPLIED: &str = "OfxImageAlphaPremultiplied";
pub const UNPREMULTIPLIED: &str = "OfxImageAlphaUnPremultiplied";
pub const OPAQUE: &str = "OfxImageOpaque";

/// One `ferrocut-ofx-host` child process hosting (at most) one plugin instance:
/// a [`ferrocut_ipc::Host`] plus the OFX verbs.
pub struct HostProcess {
    host: Host,
    cfg: HostConfig,
}

impl AsRef<Host> for HostProcess {
    fn as_ref(&self) -> &Host {
        &self.host
    }
}

impl AsMut<Host> for HostProcess {
    fn as_mut(&mut self) -> &mut Host {
        &mut self.host
    }
}

impl HostProcess {
    pub fn spawn(cfg: &HostConfig) -> Result<HostProcess, OfxError> {
        let mut host = Host::spawn(&cfg.host_spec())?;
        host.handshake(&["ferrocut-ofx-host", PROTOCOL_VERSION], cfg.control_timeout)?;
        Ok(HostProcess { host, cfg: cfg.clone() })
    }

    pub fn pid(&self) -> u32 {
        self.host.pid()
    }

    /// Resident set size of the host, from /proc.
    pub fn rss_bytes(&self) -> Option<u64> {
        self.host.rss_bytes()
    }

    pub fn is_alive(&mut self) -> bool {
        self.host.is_alive()
    }

    /// SIGKILL the host (used on timeouts; tests use it to simulate the OOM killer).
    pub fn kill(&mut self) {
        self.host.kill()
    }

    /// Send one request line, wait for its reply; returns the fields after `OK`.
    pub fn request(&mut self, line: &str, timeout: Duration) -> Result<Vec<String>, OfxError> {
        self.host.request(line, timeout)
    }

    /// `id:major.minor` for every plugin the host found.
    pub fn list(&mut self) -> Result<Vec<String>, OfxError> {
        let f = self.request("LIST", self.cfg.control_timeout)?;
        Ok(f.first().map(|s| s.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect()).unwrap_or_default())
    }

    /// Instantiate `plugin_id` in `context` ("filter" or "general"). Returns the plugin label.
    pub fn load(&mut self, plugin_id: &str, context: &str) -> Result<String, OfxError> {
        let f = self.request(&format!("LOAD\t{plugin_id}\t{context}"), self.cfg.control_timeout)?;
        Ok(f.first().cloned().unwrap_or_default())
    }

    pub fn set_param(&mut self, name: &str, values: &[String]) -> Result<(), OfxError> {
        let mut line = format!("PARAM\t{name}");
        for v in values {
            line.push('\t');
            line.push_str(v);
        }
        self.request(&line, self.cfg.control_timeout).map(|_| ())
    }

    /// Render `src` into `dst` (same size) at time `t` (seconds) for a clip at `rate` fps.
    pub fn render(
        &mut self,
        t: RationalTime,
        rate: FrameRate,
        src: &ShmFrame,
        dst: &ShmFrame,
        src_premult: &str,
    ) -> Result<RenderReply, OfxError> {
        let (w, h) = src.dims();
        if dst.dims() != (w, h) {
            return Err(self.host.protocol_error("src/dst size mismatch"));
        }
        let s = t.seconds();
        let line = format!(
            "RENDER\t{}\t{}\t{}\t{}\t{w}\t{h}\t{}\t{}\t{src_premult}",
            s.num(),
            s.den(),
            rate.num(),
            rate.den(),
            src.path().display(),
            dst.path().display()
        );
        let f = self.request(&line, self.cfg.render_timeout)?;
        Ok(RenderReply {
            output_premult: f.first().cloned().unwrap_or_default(),
            how: f.get(1).cloned().unwrap_or_default(),
        })
    }
}
