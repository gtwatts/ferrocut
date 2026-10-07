//! Network policy: by default a page can't reach the network or files outside
//! its directory, and blocked requests fail the same way every run.
//!
//! The page fires one probe per request type at a local TCP listener (so a
//! leak would be seen as a connection, not inferred from timing) and paints
//! one square per probe: green = the expected outcome (blocked, or loaded for
//! the allowed control), red = the opposite. `gate.js` re-inserts itself
//! until every probe has settled; dynamically inserted scripts delay the
//! `load` event, and OPEN returns only after `load`, so frame 0 already
//! shows every outcome. Nothing here waits on a clock.
#![cfg(not(ferrocut_html_no_host))]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ferrocut_core::{AccessPattern, ErrorKind, Rational, RenderNode};
use ferrocut_html::{HtmlNode, HtmlParams, HtmlSession, HtmlSource, NetworkPolicy, OutputEncoding};

const W: u32 = 320;
const H: u32 = 320;
const COLS: usize = 4;
const CELL: usize = 80;

/// Counts TCP connections; answers each with a tiny CORS-open HTTP 200 so an
/// allowed fetch completes.
struct Listener {
    port: u16,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        l.set_nonblocking(true).unwrap();
        let (accepted, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (a, s) = (accepted.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((mut c, _)) => {
                        a.fetch_add(1, Ordering::SeqCst);
                        let _ = c.set_nonblocking(false);
                        let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        let _ = c.read(&mut buf);
                        let _ = c.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\n\
                              Content-Length: 2\r\nConnection: close\r\n\r\nok",
                        );
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
                }
            }
        });
        Listener { port, accepted, stop, thread: Some(thread) }
    }
    fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Site {
    dir: PathBuf,
    page: PathBuf,
    secret: PathBuf,
}

impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const GATE_JS: &str = "(function () {
  window.__gateN = (window.__gateN || 0) + 1;
  if (window.__pending > 0 && window.__gateN < 20000) {
    var s = document.createElement('script');
    s.src = 'gate.js?' + window.__gateN;
    document.head.appendChild(s);
  }
})();
";

const SVG: &str =
    "<svg xmlns='http://www.w3.org/2000/svg' width='4' height='4'><rect width='4' height='4' fill='#fff'/></svg>";

/// `probes`: (name, JS run with `ok()`/`bad()` in scope).
fn site(name: &str, probes: &[(&str, String)]) -> Site {
    let dir = std::env::temp_dir().join(format!("ferrocut-html-net-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("page")).unwrap();
    std::fs::create_dir_all(dir.join("outside")).unwrap();
    let dir = dir.canonicalize().unwrap();
    std::fs::write(dir.join("outside/secret.svg"), SVG).unwrap();
    std::fs::write(dir.join("page/ok.svg"), SVG).unwrap();
    std::fs::write(dir.join("page/gate.js"), GATE_JS).unwrap();
    let mut cells = String::new();
    let mut js = String::new();
    for (i, (pname, body)) in probes.iter().enumerate() {
        let (x, y) = ((i % COLS) * CELL, (i / COLS) * CELL);
        cells.push_str(&format!(
            "<div id='p{i}' title='{pname}' style='position:absolute;left:{x}px;top:{y}px;width:{CELL}px;height:{CELL}px;background:#808080'></div>\n"
        ));
        js.push_str(&format!(
            "(function () {{ var ok = function () {{ settle({i}, true); }}, bad = function () {{ settle({i}, false); }};\n  try {{ {body} }} catch (e) {{ ok(); }}\n}})();\n"
        ));
    }
    let html = format!(
        "<!doctype html><html><head><meta charset='utf-8'><style>html,body{{margin:0;background:#000}}</style></head><body>
{cells}<script>
window.__pending = {n};
var done = {{}};
function settle(i, good) {{
  if (done[i]) return;
  done[i] = true;
  document.getElementById('p' + i).style.background = good ? '#00ff00' : '#ff0000';
  window.__pending--;
}}
{js}</script>
<script src='gate.js'></script>
</body></html>",
        n = probes.len()
    );
    let page = dir.join("page/index.html");
    std::fs::write(&page, html).unwrap();
    Site { secret: dir.join("outside/secret.svg"), page, dir }
}

fn params() -> HtmlParams {
    HtmlParams { width: W, height: H, fps: Rational::new(30, 1), encoding: OutputEncoding::SrgbEncoded }
}

fn frame(s: &mut HtmlSession, k: i64) -> Vec<u8> {
    let mut out = Vec::new();
    s.render_frame(k, &|| false, &mut out).unwrap_or_else(|e| panic!("frame {k}: {e}"));
    out
}

/// BGRA at the centre of probe `i`'s square.
fn cell(px: &[u8], i: usize) -> [u8; 4] {
    let (x, y) = ((i % COLS) * CELL + CELL / 2, (i / COLS) * CELL + CELL / 2);
    let o = (y * W as usize + x) * 4;
    px[o..o + 4].try_into().unwrap()
}

fn verdict(px: &[u8], i: usize) -> &'static str {
    match cell(px, i) {
        [b, g, r, _] if g > 200 && r < 60 && b < 60 => "green",
        [b, g, r, _] if r > 200 && g < 60 && b < 60 => "RED (opposite of expected)",
        _ => "GREY (never settled)",
    }
}

fn file_url(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn probes(port: u16, secret: &Path) -> Vec<(&'static str, String)> {
    let r = format!("http://127.0.0.1:{port}");
    let secret = file_url(secret);
    vec![
        ("fetch", format!("fetch('{r}/fetch').then(bad, ok);")),
        (
            "xhr",
            format!(
                "var x = new XMLHttpRequest(); x.onloadend = function () {{ x.status === 0 ? ok() : bad(); }}; x.open('GET', '{r}/xhr'); x.send();"
            ),
        ),
        ("img", format!("var i = new Image(); i.onload = bad; i.onerror = ok; i.src = '{r}/img.png';")),
        (
            "css",
            format!(
                "var l = document.createElement('link'); l.rel = 'stylesheet'; l.onload = bad; l.onerror = ok; l.href = '{r}/style.css'; document.head.appendChild(l);"
            ),
        ),
        (
            "script",
            format!(
                "var s = document.createElement('script'); s.onload = bad; s.onerror = ok; s.src = '{r}/script.js'; document.head.appendChild(s);"
            ),
        ),
        (
            "websocket",
            format!(
                "var w = new WebSocket('ws://127.0.0.1:{port}/ws'); w.onopen = function () {{ w.close(); bad(); }}; w.onerror = ok; w.onclose = ok;"
            ),
        ),
        (
            "websocket-localhost",
            format!(
                "var w = new WebSocket('ws://localhost:{port}/ws'); w.onopen = function () {{ w.close(); bad(); }}; w.onerror = ok; w.onclose = ok;"
            ),
        ),
        ("fetch-localhost", format!("fetch('http://localhost:{port}/fetch').then(bad, ok);")),
        ("font", format!("new FontFace('Probe', 'url({r}/font.woff2)').load().then(bad, ok);")),
        (
            "eventsource",
            format!(
                "var e = new EventSource('{r}/events'); e.onopen = function () {{ e.close(); bad(); }}; e.onerror = function () {{ e.close(); ok(); }};"
            ),
        ),
        ("dns-name", "fetch('http://example.com/fetch').then(bad, ok);".into()),
        (
            "file-relative-outside",
            "var i = new Image(); i.onload = bad; i.onerror = ok; i.src = '../outside/secret.svg?rel';".into(),
        ),
        (
            "file-absolute-outside",
            format!("var i = new Image(); i.onload = bad; i.onerror = ok; i.src = '{secret}?abs';"),
        ),
        ("allowed-local", "var i = new Image(); i.onload = ok; i.onerror = bad; i.src = 'ok.svg';".into()),
        (
            "iframe",
            format!(
                "var f = document.createElement('iframe'); f.src = '{r}/frame.html'; document.body.appendChild(f); ok();"
            ),
        ),
        ("beacon", format!("navigator.sendBeacon('{r}/beacon', 'x'); ok();")),
    ]
}

#[test]
fn remote_and_outside_requests_are_blocked_deterministically() {
    let net = Listener::start();
    let probes = probes(net.port, &PathBuf::from("/placeholder"));
    // Rebuild with the real secret path (needs the site dir first).
    let s = site("block", &[]);
    let probes: Vec<_> =
        probes.into_iter().map(|(n, b)| (n, b.replace("file:///placeholder", &file_url(&s.secret)))).collect();
    drop(s);
    let s = site("block", &probes);
    let node = HtmlNode::new(HtmlSource::File(s.page.clone()), params()).unwrap();

    let mut runs = Vec::new();
    for run in 0..2 {
        let mut sess = node.new_session();
        let f0 = frame(&mut sess, 0);
        let f10 = frame(&mut sess, 10);
        let blocked: BTreeSet<String> = sess.blocked_requests().unwrap().into_iter().collect();
        let verdicts: Vec<_> = probes.iter().enumerate().map(|(i, (n, _))| (*n, verdict(&f0, i))).collect();
        eprintln!("run {run}: verdicts {verdicts:?}");
        eprintln!("run {run}: blocked {blocked:#?}");
        eprintln!("run {run}: listener connections so far: {}", net.connections());
        let bad: Vec<_> = verdicts.iter().filter(|(_, v)| *v != "green").collect();
        assert!(bad.is_empty(), "run {run}: probes with unexpected outcomes: {bad:?}");
        runs.push((f0, f10, blocked));
    }
    assert_eq!(net.connections(), 0, "a blocked request reached the network");

    let r = format!("http://127.0.0.1:{}", net.port);
    let secret = file_url(&s.secret);
    let want: BTreeSet<String> = [
        format!("{r}/fetch"),
        format!("{r}/xhr"),
        format!("{r}/img.png"),
        format!("{r}/style.css"),
        format!("{r}/script.js"),
        format!("http://localhost:{}/fetch", net.port),
        format!("{r}/font.woff2"),
        format!("{r}/events"),
        "http://example.com/fetch".into(),
        format!("{secret}?rel"),
        format!("{secret}?abs"),
        format!("{r}/frame.html"),
        format!("{r}/beacon"),
    ]
    .into_iter()
    .collect();
    let (a, b) = (&runs[0], &runs[1]);
    let missing: Vec<_> = want.difference(&a.2).collect();
    let extra: Vec<_> = a.2.difference(&want).collect();
    assert!(missing.is_empty() && extra.is_empty(), "blocked list: missing {missing:?}, unexpected {extra:?}");
    assert_eq!(a.2, b.2, "blocked list differs between hosts");
    assert!(a.0 == b.0 && a.1 == b.1, "frames differ between hosts");
}

#[test]
fn allow_remote_reaches_the_listener() {
    // Control for the test above: the same listener does see a fetch when the
    // policy allows it, so "0 connections" there is meaningful. Files outside
    // the page directory stay blocked.
    let net = Listener::start();
    let s0 = site("allow", &[]);
    let secret = file_url(&s0.secret);
    drop(s0);
    let probes = vec![
        (
            "fetch",
            format!(
                "fetch('http://127.0.0.1:{}/fetch').then(function (r) {{ return r.text(); }}).then(function (t) {{ t === 'ok' ? ok() : bad(); }}, bad);",
                net.port
            ),
        ),
        ("file-absolute-outside", format!("var i = new Image(); i.onload = bad; i.onerror = ok; i.src = '{secret}';")),
        // The listener answers 200, not 101, so the handshake fails after connecting.
        (
            "websocket",
            format!("var w = new WebSocket('ws://127.0.0.1:{}/ws'); w.onopen = bad; w.onerror = ok;", net.port),
        ),
    ];
    let s = site("allow", &probes);
    let policy = NetworkPolicy { allow_remote: true, extra_roots: vec![] };
    let node = HtmlNode::with_policy(HtmlSource::File(s.page.clone()), params(), policy).unwrap();
    assert_ne!(
        node.content_hash(),
        HtmlNode::new(HtmlSource::File(s.page.clone()), params()).unwrap().content_hash(),
        "policy is part of the hash"
    );
    let mut sess = node.new_session();
    let f0 = frame(&mut sess, 0);
    let verdicts: Vec<_> = probes.iter().enumerate().map(|(i, (n, _))| (*n, verdict(&f0, i))).collect();
    assert!(verdicts.iter().all(|(_, v)| *v == "green"), "{verdicts:?}");
    eprintln!("allow-remote: verdicts {verdicts:?}, connections {}", net.connections());
    assert!(net.connections() >= 2, "allowed fetch/websocket never reached the listener");
    assert_eq!(sess.blocked_requests().unwrap(), vec![secret]);
}

#[test]
fn extra_roots_allow_files_and_feed_the_hash() {
    let s0 = site("roots", &[]);
    let probes = vec![(
        "outside-allowed",
        format!("var i = new Image(); i.onload = ok; i.onerror = bad; i.src = '{}';", file_url(&s0.secret)),
    )];
    drop(s0);
    let s = site("roots", &probes);
    let policy = NetworkPolicy { allow_remote: false, extra_roots: vec![s.dir.join("outside")] };
    let node = HtmlNode::with_policy(HtmlSource::File(s.page.clone()), params(), policy.clone()).unwrap();
    let mut sess = node.new_session();
    let f0 = frame(&mut sess, 0);
    assert_eq!(verdict(&f0, 0), "green");
    assert!(sess.blocked_requests().unwrap().is_empty());

    // Editing a sub-resource (in the page dir or an extra root) changes the hash.
    let h0 = node.content_hash();
    std::fs::write(&s.secret, SVG.replace("#fff", "#000")).unwrap();
    let h1 = HtmlNode::with_policy(HtmlSource::File(s.page.clone()), params(), policy.clone()).unwrap().content_hash();
    assert_ne!(h0, h1);
    std::fs::write(s.dir.join("page/ok.svg"), SVG.replace("#fff", "#000")).unwrap();
    let h2 = HtmlNode::with_policy(HtmlSource::File(s.page.clone()), params(), policy).unwrap().content_hash();
    assert_ne!(h1, h2);
}

#[test]
fn bad_policies_fail_permanently_up_front() {
    let s = site("bad", &[]);
    let err = HtmlNode::new(HtmlSource::Url("https://example.com/".into()), params())
        .err()
        .expect("remote page without allow_remote");
    assert_eq!(err.kind, ErrorKind::Permanent, "{err}");
    let policy = NetworkPolicy { allow_remote: false, extra_roots: vec![s.dir.join("missing")] };
    let err = HtmlNode::with_policy(HtmlSource::File(s.page.clone()), params(), policy).err().expect("missing root");
    assert_eq!(err.kind, ErrorKind::Permanent, "{err}");
    assert!(
        HtmlNode::with_policy(
            HtmlSource::Url("https://example.com/".into()),
            params(),
            NetworkPolicy { allow_remote: true, extra_roots: vec![] }
        )
        .is_ok()
    );
}

#[test]
fn node_hash_survives_moving_the_page_folder() {
    let probes =
        vec![("allowed-local", "var i = new Image(); i.onload = ok; i.onerror = bad; i.src = 'ok.svg';".to_string())];
    let a = site("move-a", &probes);
    let b = site("move-b", &probes);
    let na = HtmlNode::new(HtmlSource::File(a.page.clone()), params()).unwrap();
    let nb = HtmlNode::new(HtmlSource::File(b.page.clone()), params()).unwrap();
    assert_eq!(na.content_hash(), nb.content_hash(), "same page in another folder must keep its cache key");
    let url = HtmlNode::new(HtmlSource::Url(format!("file://{}", b.page.display())), params()).unwrap();
    assert_eq!(na.content_hash(), url.content_hash(), "file:// URL and File source are the same page");
    assert_eq!(na.access_pattern(), AccessPattern::Sequential);
}
