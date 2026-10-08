//! Cisco's OpenH264 binary: pinned runtime download, verification, user
//! cache, user control (enable / disable / re-enable), and the licence notice
//! Cisco's binary licence requires.
//!
//! Ferrocut never bundles or builds OpenH264. Cisco covers the AVC/H.264
//! patent royalties for *its* binary only when (see
//! [`BINARY_LICENSE`], conditions 1-4):
//! 1. the binary is downloaded separately to the end user's device;
//! 2. the user can enable, disable and re-enable it
//!    ([`Provider::enable`] / [`Provider::disable`], `ferrocut-deliver openh264 ...`);
//! 3. where the user controls it, the software shows [`NOTICE`];
//! 4. the licence text is reproduced where licensing information is shown
//!    (`ferrocut-deliver openh264 license`, the crate README).
//!
//! Integrity: Cisco publishes only an MD5 of each decompressed library
//! (`<lib>.signed.md5.txt`). The [`PINS`] carry that MD5 *and* SHA-256 of
//! both the `.bz2` archive and the library, computed from files verified
//! against Cisco's MD5. A download must match all of them, and Cisco's live
//! `.signed.md5.txt` must still equal the pinned MD5. The cached library is
//! re-hashed (SHA-256) every time it is loaded.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferrocut_types::error::NodeError;
use md5::Md5;
use sha2::{Digest, Sha256};

/// The pinned OpenH264 release.
pub const VERSION: &str = "2.6.0";
/// ABI the C shim is compiled against (vendored v2.6.0 headers).
pub const ABI: (u32, u32) = (2, 6);
/// Text Cisco's licence requires wherever users control the binary.
pub const NOTICE: &str = "OpenH264 Video Codec provided by Cisco Systems, Inc.";
/// Cisco's binary licence (BSD + AVC/H.264 patent portfolio notice and
/// conditions), reproduced verbatim as condition 4 requires.
pub const BINARY_LICENSE: &str = include_str!("../OPENH264_BINARY_LICENSE.txt");
pub const LICENSE_URL: &str = "http://www.openh264.org/BINARY_LICENSE.txt";
/// Cisco's download host. Plain HTTP is what Cisco serves (Firefox's GMP
/// download uses it too); integrity comes from the pinned hashes.
pub const DEFAULT_BASE_URL: &str = "http://ciscobinary.openh264.org";

/// Offline override: path to an already-downloaded Cisco library.
pub const ENV_LIB: &str = "FERROCUT_OPENH264_LIB";
/// `1`: accept a [`ENV_LIB`] library whose hash is not pinned (your own
/// build or a distro package). You then need your own H.264 patent licence.
pub const ENV_UNVERIFIED: &str = "FERROCUT_OPENH264_UNVERIFIED";
/// Override the cache directory (default: the user cache dir).
pub const ENV_CACHE: &str = "FERROCUT_OPENH264_CACHE";
/// Override the download base URL (a mirror of Cisco's files; tests).
pub const ENV_BASE_URL: &str = "FERROCUT_OPENH264_URL";

/// One platform's pinned Cisco binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    pub platform: &'static str,
    /// File on Cisco's host (`<lib>.bz2`).
    pub archive: &'static str,
    pub archive_sha256: &'static str,
    pub archive_size: u64,
    /// Decompressed library file name (also the cached file name).
    pub lib: &'static str,
    pub lib_sha256: &'static str,
    /// Cisco's published MD5 (`<lib>.signed.md5.txt`).
    pub lib_md5: &'static str,
    pub lib_size: u64,
}

/// v2.6.0 pins, verified 2026-10-07 against Cisco's `.signed.md5.txt` files.
pub const PINS: &[Pin] = &[
    Pin {
        platform: "linux-x86_64",
        archive: "libopenh264-2.6.0-linux64.8.so.bz2",
        archive_sha256: "27ab53323c110b76214c1c72222f459d17febbcd1e252136cadc292b0308d75b",
        archive_size: 634_264,
        lib: "libopenh264-2.6.0-linux64.8.so",
        lib_sha256: "2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04",
        lib_md5: "1859c0aaf825429cbf36f1f496c5e08c",
        lib_size: 1_731_128,
    },
    Pin {
        platform: "linux-aarch64",
        archive: "libopenh264-2.6.0-linux-arm64.8.so.bz2",
        archive_sha256: "a78aea7970150f46bcd3bb7994c9e6dd90bd7a9ea785920f5a73f6964e3fcda7",
        archive_size: 619_371,
        lib: "libopenh264-2.6.0-linux-arm64.8.so",
        lib_sha256: "12e7b33623667cdab0e575170c147b1b36eadb77d0d2aa7ceb5afd3e58902140",
        lib_md5: "9394e36085a540e34fc6e5d16929f151",
        lib_size: 1_492_120,
    },
    Pin {
        platform: "macos-aarch64",
        archive: "libopenh264-2.6.0-mac-arm64.dylib.bz2",
        archive_sha256: "6db362ee5abdab572311aeadb96d3f44b0617d9a4a4b9f4db4cb5ac4d968da71",
        archive_size: 482_124,
        lib: "libopenh264-2.6.0-mac-arm64.dylib",
        lib_sha256: "052e98bfcf7a9167d22f3bbb3f5988ef79065591f36af8b52924b22b13624551",
        lib_md5: "dc5ee5d08b6a7290ff254ded561e14df",
        lib_size: 1_207_136,
    },
    Pin {
        platform: "macos-x86_64",
        archive: "libopenh264-2.6.0-mac-x64.dylib.bz2",
        archive_sha256: "38b2ed6d1d45b6a3e408c734173f2d67ab44a10d0e154ff3489b89877cd60e7e",
        archive_size: 529_614,
        lib: "libopenh264-2.6.0-mac-x64.dylib",
        lib_sha256: "e3dc8bc01fe69363f61fd3c02fd27798537a585eadd38cd808f303d1ee505a19",
        lib_md5: "ea0ca009c3cb5f5ab8906de3993c1484",
        lib_size: 1_381_104,
    },
    Pin {
        platform: "windows-x86_64",
        archive: "openh264-2.6.0-win64.dll.bz2",
        archive_sha256: "dab5f2a872777f9a58b69bfa9fbcf20d9f82f2d6ec91383fd70bff49bd34ac9f",
        archive_size: 452_053,
        lib: "openh264-2.6.0-win64.dll",
        lib_sha256: "2076cb5675ec6c1a4c70e7a2a322552f547b6eeed649d6dfcd9e02a543b24691",
        lib_md5: "ce1282f5845f56761954282e4992730d",
        lib_size: 978_520,
    },
    Pin {
        platform: "windows-aarch64",
        archive: "openh264-2.6.0-win-arm64.dll.bz2",
        archive_sha256: "6b57d3ecd06bd80b5f3ee1abbb31d48798ca6d89ef1bc98cf2e6d1ebddf5cf45",
        archive_size: 378_234,
        lib: "openh264-2.6.0-win-arm64.dll",
        lib_sha256: "fb75103938f4f47d119b983e06334df41a803bc72fb5c46e3623f6fea5782732",
        lib_md5: "9f4d683a9af470059f31b0b7a13e7c1a",
        lib_size: 810_584,
    },
];

/// `<os>-<arch>` of this build.
pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The pin for this platform, if Cisco ships a binary for it.
pub fn host_pin() -> Option<&'static Pin> {
    let p = host_platform();
    PINS.iter().find(|pin| pin.platform == p)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub fn md5_hex(data: &[u8]) -> String {
    hex(&Md5::digest(data))
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// Default cache root: `$XDG_CACHE_HOME/ferrocut/openh264` (or
/// `~/.cache/...`), `~/Library/Caches/ferrocut/openh264` on macOS,
/// `%LOCALAPPDATA%\ferrocut\cache\openh264` on Windows.
pub fn default_cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(ENV_CACHE).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(target_os = "windows") {
        env("LOCALAPPDATA").map(|d| d.join("ferrocut").join("cache"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches/ferrocut"))
    } else {
        env("XDG_CACHE_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".cache")))
            .map(|d| d.join("ferrocut"))
    };
    base.map(|b| b.join("openh264"))
}

/// The user's choice about the Cisco binary (condition 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserChoice {
    /// Never asked: nothing is downloaded until the user enables it.
    Unset,
    Enabled,
    Disabled,
}

/// Where the loaded library came from.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// [`ENV_LIB`] / [`Provider::override_lib`].
    Override,
    /// Previously downloaded and cached.
    Cache,
    /// Downloaded by this call.
    Download,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Status {
    pub version: &'static str,
    pub platform: String,
    pub choice: UserChoice,
    pub cache_dir: PathBuf,
    /// The cached library, if present (`verified` says whether it hashes right).
    pub cached: Option<PathBuf>,
    pub verified: bool,
    pub override_lib: Option<PathBuf>,
    pub notice: &'static str,
    pub license_url: &'static str,
}

/// Finds, downloads and verifies Cisco's library.
#[derive(Clone, Debug)]
pub struct Provider {
    pub cache_dir: PathBuf,
    pub base_url: String,
    /// The caller has the user's go-ahead to download now (an explicit
    /// "enable" / `--download-openh264`). Records [`UserChoice::Enabled`].
    pub allow_download: bool,
    pub override_lib: Option<PathBuf>,
    pub allow_unverified: bool,
    /// Called with [`NOTICE`] text whenever a download starts (show it).
    pub on_notice: Option<fn(&str)>,
}

impl Provider {
    /// From the environment ([`ENV_LIB`], [`ENV_UNVERIFIED`], [`ENV_CACHE`],
    /// [`ENV_BASE_URL`]); `allow_download` is false.
    pub fn from_env() -> Result<Self, NodeError> {
        let cache_dir = default_cache_dir().ok_or_else(|| {
            NodeError::permanent(format!(
                "no user cache directory for OpenH264 (set {ENV_CACHE} or HOME)"
            ))
        })?;
        Ok(Self {
            cache_dir,
            base_url: std::env::var(ENV_BASE_URL)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            allow_download: false,
            override_lib: std::env::var_os(ENV_LIB)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            allow_unverified: std::env::var(ENV_UNVERIFIED).is_ok_and(|v| v == "1"),
            on_notice: None,
        })
    }

    /// No environment: a given cache dir, Cisco's host, no override.
    pub fn with_cache_dir(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            base_url: DEFAULT_BASE_URL.into(),
            allow_download: false,
            override_lib: None,
            allow_unverified: false,
            on_notice: None,
        }
    }

    fn choice_path(&self) -> PathBuf {
        self.cache_dir.join("choice")
    }

    fn version_dir(&self) -> PathBuf {
        self.cache_dir.join(VERSION)
    }

    pub fn choice(&self) -> UserChoice {
        match std::fs::read_to_string(self.choice_path()) {
            Ok(s) if s.trim() == "enabled" => UserChoice::Enabled,
            Ok(s) if s.trim() == "disabled" => UserChoice::Disabled,
            _ => UserChoice::Unset,
        }
    }

    fn set_choice(&self, c: UserChoice) -> Result<(), NodeError> {
        let text = match c {
            UserChoice::Enabled => "enabled\n",
            UserChoice::Disabled => "disabled\n",
            UserChoice::Unset => {
                return remove_if_exists(&self.choice_path());
            }
        };
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| io_err(&self.cache_dir, e))?;
        write_atomic(&self.choice_path(), text.as_bytes())
    }

    /// Path of the cached library for this platform (may not exist).
    pub fn cached_lib(&self) -> Option<PathBuf> {
        host_pin().map(|p| self.version_dir().join(p.lib))
    }

    pub fn status(&self) -> Status {
        let cached = self.cached_lib().filter(|p| p.is_file());
        let verified = match (&cached, host_pin()) {
            (Some(p), Some(pin)) => verify_lib_file(p, pin).is_ok(),
            _ => false,
        };
        Status {
            version: VERSION,
            platform: host_platform(),
            choice: self.choice(),
            cache_dir: self.cache_dir.clone(),
            cached,
            verified,
            override_lib: self.override_lib.clone(),
            notice: NOTICE,
            license_url: LICENSE_URL,
        }
    }

    /// The user enables the codec: record it and make sure the verified
    /// binary is cached (downloading it if needed). Returns the library path.
    pub fn enable(&self) -> Result<PathBuf, NodeError> {
        self.set_choice(UserChoice::Enabled)?;
        let p = Provider {
            allow_download: true,
            override_lib: None,
            ..self.clone()
        };
        p.locate().map(|(path, _)| path)
    }

    /// The user disables the codec: nothing loads it until re-enabled.
    /// `remove` also deletes the cached binary.
    pub fn disable(&self, remove: bool) -> Result<(), NodeError> {
        self.set_choice(UserChoice::Disabled)?;
        if remove {
            let dir = self.version_dir();
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
            }
        }
        Ok(())
    }

    /// Resolve a verified library path. Order: override; refusal if the user
    /// disabled the codec; the verified cache; a download if allowed.
    pub fn locate(&self) -> Result<(PathBuf, Source), NodeError> {
        if let Some(lib) = &self.override_lib {
            return self
                .check_override(lib)
                .map(|()| (lib.clone(), Source::Override));
        }
        let pin = host_pin().ok_or_else(|| {
            NodeError::permanent(format!(
                "Cisco ships no OpenH264 {VERSION} binary for {}; set {ENV_LIB} to a library \
                 (with {ENV_UNVERIFIED}=1 if it is not Cisco's)",
                host_platform()
            ))
        })?;
        let choice = self.choice();
        if choice == UserChoice::Disabled && !self.allow_download {
            return Err(NodeError::permanent(format!(
                "OpenH264 is disabled by the user; re-enable it with \
                 `ferrocut-deliver openh264 enable` ({NOTICE})"
            )));
        }
        let path = self.version_dir().join(pin.lib);
        if path.is_file() {
            match verify_lib_file(&path, pin) {
                Ok(()) => return Ok((path, Source::Cache)),
                Err(e) if !self.allow_download && choice != UserChoice::Enabled => {
                    return Err(NodeError::permanent(format!(
                        "cached OpenH264 at {} failed verification ({}); run \
                         `ferrocut-deliver openh264 enable` to download it again",
                        path.display(),
                        e.message
                    )));
                }
                Err(_) => {} // enabled: re-download over it
            }
        } else if !self.allow_download && choice != UserChoice::Enabled {
            return Err(NodeError::permanent(format!(
                "Cisco's OpenH264 {VERSION} binary is not installed. H.264 export downloads it \
                 from Cisco on request: run `ferrocut-deliver openh264 enable` (or pass \
                 --download-openh264), or set {ENV_LIB} to a downloaded copy. {NOTICE}"
            )));
        }
        if self.allow_download && choice != UserChoice::Enabled {
            self.set_choice(UserChoice::Enabled)?;
        }
        self.download(pin, &path)?;
        Ok((path, Source::Download))
    }

    fn check_override(&self, lib: &Path) -> Result<(), NodeError> {
        let data = std::fs::read(lib).map_err(|e| {
            NodeError::permanent(format!(
                "{ENV_LIB}={}: cannot read the OpenH264 library: {e}",
                lib.display()
            ))
        })?;
        let sha = sha256_hex(&data);
        if PINS.iter().any(|p| p.lib_sha256 == sha) || self.allow_unverified {
            return Ok(());
        }
        Err(NodeError::permanent(format!(
            "{ENV_LIB}={} is not Cisco's pinned OpenH264 {VERSION} binary (sha256 {sha}). \
             Use the file Cisco ships for {} (its decompressed {} has sha256 {}), or set \
             {ENV_UNVERIFIED}=1 to load it anyway (then Cisco's patent licence does not cover it)",
            lib.display(),
            host_platform(),
            host_pin().map_or("?", |p| p.lib),
            host_pin().map_or("?", |p| p.lib_sha256),
        )))
    }

    /// Download `pin` from [`Self::base_url`], verify, cache at `dest`.
    fn download(&self, pin: &Pin, dest: &Path) -> Result<(), NodeError> {
        if let Some(cb) = self.on_notice {
            cb(&format!(
                "Downloading Cisco's OpenH264 {VERSION} binary ({}) from {}.\n{NOTICE}\n\
                 Licence: {LICENSE_URL} (`ferrocut-deliver openh264 license`).",
                pin.archive, self.base_url
            ));
        }
        let base = self.base_url.trim_end_matches('/');
        let archive = http_get(&format!("{base}/{}", pin.archive), 8 << 20)?;
        verify_archive(pin, &archive)?;
        let published = http_get(&format!("{base}/{}.signed.md5.txt", pin.lib), 4096)?;
        let lib = verify_download(pin, &archive, &published)?;
        let dir = dest.parent().expect("versioned dir");
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        write_atomic(dest, &lib)
    }

    /// [`locate`](Self::locate) and load.
    pub fn load(&self) -> Result<Arc<OpenH264>, NodeError> {
        let (path, source) = self.locate()?;
        OpenH264::load(&path, source).map(Arc::new)
    }
}

/// Check a download: archive size + SHA-256, Cisco's published MD5 equals the
/// pin, and the decompressed library's size, SHA-256 and MD5. Returns the
/// library bytes. Every mismatch is Permanent: retrying gets the same bytes.
pub fn verify_download(
    pin: &Pin,
    archive: &[u8],
    published_md5: &[u8],
) -> Result<Vec<u8>, NodeError> {
    verify_archive(pin, archive)?;
    let text = String::from_utf8_lossy(published_md5);
    let cisco = text
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| w.len() == 32)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if cisco != pin.lib_md5 {
        return Err(mismatch(pin, "Cisco published MD5", pin.lib_md5, &cisco));
    }
    let mut lib = Vec::with_capacity(pin.lib_size as usize);
    bzip2::read::BzDecoder::new(archive)
        .take(pin.lib_size + 1)
        .read_to_end(&mut lib)
        .map_err(|e| {
            NodeError::permanent(format!("OpenH264 archive {}: bad bzip2: {e}", pin.archive))
        })?;
    verify_lib_bytes(&lib, pin)?;
    Ok(lib)
}

fn mismatch(pin: &Pin, what: &str, want: &str, got: &str) -> NodeError {
    NodeError::permanent(format!(
        "OpenH264 download {}: {what} mismatch (want {want}, got {got}); refusing to install it",
        pin.archive
    ))
}

/// Size + SHA-256 of the downloaded `.bz2` against the pin.
pub fn verify_archive(pin: &Pin, archive: &[u8]) -> Result<(), NodeError> {
    let sha = sha256_hex(archive);
    if archive.len() as u64 != pin.archive_size || sha != pin.archive_sha256 {
        return Err(mismatch(pin, "archive sha256", pin.archive_sha256, &sha));
    }
    Ok(())
}

fn verify_lib_bytes(lib: &[u8], pin: &Pin) -> Result<(), NodeError> {
    let sha = sha256_hex(lib);
    let md5 = md5_hex(lib);
    if lib.len() as u64 != pin.lib_size || sha != pin.lib_sha256 || md5 != pin.lib_md5 {
        return Err(NodeError::permanent(format!(
            "{}: hash mismatch (sha256 {sha}, md5 {md5}; pinned sha256 {}, Cisco md5 {})",
            pin.lib, pin.lib_sha256, pin.lib_md5
        )));
    }
    Ok(())
}

fn verify_lib_file(path: &Path, pin: &Pin) -> Result<(), NodeError> {
    let data = std::fs::read(path).map_err(|e| io_err(path, e))?;
    verify_lib_bytes(&data, pin)
}

fn io_err(path: &Path, e: std::io::Error) -> NodeError {
    NodeError::retryable(format!("{}: {e}", path.display()))
}

fn remove_if_exists(p: &Path) -> Result<(), NodeError> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(p, e)),
    }
}

/// Write via a temp file + rename so readers never see a partial file.
fn write_atomic(dest: &Path, data: &[u8]) -> Result<(), NodeError> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dest.with_extension(format!("tmp{}-{seq}", std::process::id()));
    let mut f = std::fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
    f.write_all(data)
        .and_then(|()| f.sync_all())
        .map_err(|e| io_err(&tmp, e))?;
    drop(f);
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err(dest, e)
    })
}

/// Minimal HTTP/1.1 GET (Cisco serves plain HTTP; no TLS stack needed).
/// Honours `http_proxy`/`HTTP_PROXY`, follows up to 5 `http://` redirects,
/// accepts Content-Length, chunked or close-delimited bodies up to `max`
/// bytes. Network failures are Retryable; a 4xx is Permanent.
pub fn http_get(url: &str, max: usize) -> Result<Vec<u8>, NodeError> {
    let mut url = url.to_string();
    for _ in 0..6 {
        match http_get_once(&url, max)? {
            HttpResult::Body(b) => return Ok(b),
            HttpResult::Redirect(loc) => {
                if !loc.starts_with("http://") {
                    return Err(NodeError::permanent(format!(
                        "{url}: redirected to {loc}; only http:// is supported \
                         (download it manually and set {ENV_LIB})"
                    )));
                }
                url = loc;
            }
        }
    }
    Err(NodeError::permanent(format!("{url}: too many redirects")))
}

enum HttpResult {
    Body(Vec<u8>),
    Redirect(String),
}

fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, 80),
    };
    (!host.is_empty()).then(|| (host.to_string(), port, path.to_string()))
}

fn http_get_once(url: &str, max: usize) -> Result<HttpResult, NodeError> {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    let (host, port, path) = parse_http_url(url)
        .ok_or_else(|| NodeError::permanent(format!("unsupported URL {url} (http:// only)")))?;
    let proxy = ["http_proxy", "HTTP_PROXY"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .filter(|_| !no_proxy_matches(&host));
    let (conn_host, conn_port, target) = match proxy.as_deref().and_then(parse_http_url) {
        Some((ph, pp, _)) => (ph, pp, url.to_string()),
        None => (host.clone(), port, path),
    };
    let net = |e: std::io::Error| NodeError::retryable(format!("downloading {url}: {e}"));
    let addr = (conn_host.as_str(), conn_port)
        .to_socket_addrs()
        .map_err(net)?
        .next()
        .ok_or_else(|| {
            NodeError::retryable(format!("downloading {url}: {conn_host} did not resolve"))
        })?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(15)).map_err(net)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(net)?;
    s.set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(net)?;
    let host_hdr = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: {host_hdr}\r\nUser-Agent: ferrocut-deliver/{}\r\n\
         Accept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    )
    .map_err(net)?;
    let mut raw = Vec::new();
    s.take((max + 64 * 1024) as u64)
        .read_to_end(&mut raw)
        .map_err(net)?;
    let hdr_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| NodeError::retryable(format!("downloading {url}: truncated response")))?;
    let head = String::from_utf8_lossy(&raw[..hdr_end]).to_string();
    let body = &raw[hdr_end + 4..];
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| NodeError::retryable(format!("downloading {url}: bad status line")))?;
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    match status {
        200 => {}
        301 | 302 | 303 | 307 | 308 => {
            let loc = header("location")
                .ok_or_else(|| NodeError::retryable(format!("{url}: redirect without Location")))?;
            return Ok(HttpResult::Redirect(loc));
        }
        400..=499 => {
            return Err(NodeError::permanent(format!(
                "downloading {url}: HTTP {status}"
            )));
        }
        _ => {
            return Err(NodeError::retryable(format!(
                "downloading {url}: HTTP {status}"
            )));
        }
    }
    let body = if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        dechunk(body)
            .ok_or_else(|| NodeError::retryable(format!("downloading {url}: bad chunked body")))?
    } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        if body.len() < len {
            return Err(NodeError::retryable(format!(
                "downloading {url}: connection closed after {} of {len} bytes",
                body.len()
            )));
        }
        body[..len].to_vec()
    } else {
        body.to_vec()
    };
    if body.len() > max {
        return Err(NodeError::permanent(format!(
            "downloading {url}: response larger than {max} bytes"
        )));
    }
    Ok(HttpResult::Body(body))
}

fn no_proxy_matches(host: &str) -> bool {
    // Loopback never goes through a proxy (mirrors and tests on localhost).
    if host == "localhost" || host == "::1" || host == "[::1]" || host.starts_with("127.") {
        return true;
    }
    let list = std::env::var("no_proxy")
        .or_else(|_| std::env::var("NO_PROXY"))
        .unwrap_or_default();
    list.split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .any(|d| {
            d == "*"
                || host == d.trim_start_matches('.')
                || host.ends_with(&format!(".{}", d.trim_start_matches('.')))
        })
}

fn dechunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n")?;
        let size_str = std::str::from_utf8(&b[..eol]).ok()?;
        let size = usize::from_str_radix(size_str.split(';').next()?.trim(), 16).ok()?;
        b = &b[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(b.get(..size)?);
        b = b.get(size + 2..)?;
    }
}

type CreateFn = unsafe extern "C" fn(*mut *mut std::ffi::c_void) -> std::ffi::c_int;
type DestroyFn = unsafe extern "C" fn(*mut std::ffi::c_void);
type VersionFn = unsafe extern "C" fn(*mut [u32; 4]);

/// A loaded, version-checked Cisco OpenH264 library.
pub struct OpenH264 {
    pub(crate) create: CreateFn,
    pub(crate) destroy: DestroyFn,
    pub version: [u32; 4],
    pub path: PathBuf,
    pub source: Source,
    // Keeps the symbols above valid; dropped last.
    _lib: libloading::Library,
}

impl std::fmt::Debug for OpenH264 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264")
            .field("version", &self.version)
            .field("path", &self.path)
            .field("source", &self.source)
            .finish()
    }
}

impl OpenH264 {
    /// dlopen `path` (already verified by [`Provider::locate`]) and check the
    /// ABI version matches the headers the shim was compiled against.
    pub fn load(path: &Path, source: Source) -> Result<Self, NodeError> {
        let perm =
            |what: String| NodeError::permanent(format!("OpenH264 {}: {what}", path.display()));
        // SAFETY: loading Cisco's library runs its initialisers; the file is
        // the pinned, hash-verified Cisco binary (or the user's explicit override).
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| perm(format!("cannot load: {e}")))?;
        // SAFETY: the symbol types match codec_api.h for the v2.x ABI.
        let (create, destroy, version) = unsafe {
            let c: libloading::Symbol<CreateFn> = lib
                .get(b"WelsCreateSVCEncoder\0")
                .map_err(|e| perm(e.to_string()))?;
            let d: libloading::Symbol<DestroyFn> = lib
                .get(b"WelsDestroySVCEncoder\0")
                .map_err(|e| perm(e.to_string()))?;
            let v: libloading::Symbol<VersionFn> = lib
                .get(b"WelsGetCodecVersionEx\0")
                .map_err(|e| perm(e.to_string()))?;
            let mut ver = [0u32; 4];
            v(&mut ver);
            (*c, *d, ver)
        };
        if (version[0], version[1]) != ABI {
            return Err(perm(format!(
                "version {}.{}.{} does not match the {}.{} ABI this build uses",
                version[0], version[1], version[2], ABI.0, ABI.1
            )));
        }
        Ok(Self {
            create,
            destroy,
            version,
            path: path.to_path_buf(),
            source,
            _lib: lib,
        })
    }

    pub fn version_string(&self) -> String {
        format!(
            "{}.{}.{}",
            self.version[0], self.version[1], self.version[2]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_well_formed_and_cover_the_host() {
        for p in PINS {
            assert_eq!(p.archive, format!("{}.bz2", p.lib));
            assert!(p.lib.contains(VERSION));
            for h in [p.archive_sha256, p.lib_sha256] {
                assert!(h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()));
            }
            assert_eq!(p.lib_md5.len(), 32);
        }
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(host_pin().unwrap().platform, "linux-x86_64");
        }
    }

    #[test]
    fn license_text_carries_the_required_notice() {
        assert!(BINARY_LICENSE.contains(NOTICE));
        assert!(BINARY_LICENSE.contains("AVC/H.264 Patent Portfolio License Conditions"));
    }

    #[test]
    fn http_helpers() {
        assert_eq!(
            parse_http_url("http://ciscobinary.openh264.org/a.bz2"),
            Some(("ciscobinary.openh264.org".into(), 80, "/a.bz2".into()))
        );
        assert_eq!(
            parse_http_url("http://127.0.0.1:8080"),
            Some(("127.0.0.1".into(), 8080, "/".into()))
        );
        assert_eq!(parse_http_url("https://x/y"), None);
        assert_eq!(
            dechunk(b"3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n").unwrap(),
            b"abcde"
        );
    }

    #[test]
    fn verify_download_rejects_tampering() {
        let pin = &PINS[0];
        let e = verify_download(pin, b"not a bz2", b"").unwrap_err();
        assert_eq!(e.kind, ferrocut_types::error::ErrorKind::Permanent);
        assert!(e.message.contains("archive sha256 mismatch"), "{e}");
    }
}
