//! Host / HostSlot / ShmBuffer against a tiny `/bin/sh` host speaking the
//! line protocol (no plugin runtime needed).

use std::time::{Duration, Instant};

use ferrocut_ipc::{ErrorKind, Host, HostSlot, HostSpec, IpcError, ShmBuffer};

const FAKE_HOST: &str = r#"
while IFS= read -r line; do
  verb=${line%%	*}
  case "$verb" in
    HELLO) printf 'OK\tfake-host\t1\textra\n' ;;
    ECHO) printf 'OK\t%s\n' "${line#*	}" ;;
    FAIL) printf 'ERR\tno such\tthing\n' ;;
    FATAL) printf 'FATAL\trenderer gone\n'; exit 3 ;;
    SEGV) echo "noise: benign" >&2; echo "about to crash" >&2; kill -SEGV $$ ;;
    HANG) exec sleep 30 ;;
    GARBAGE) echo what ;;
    DIR) printf 'OK\t%s\n' "$FAKE_PRIVATE_DIR" ;;
    EXIT) exit 3 ;;
    QUIT) printf 'OK\n'; exit 0 ;;
  esac
done
"#;

const T: Duration = Duration::from_secs(5);

fn spec() -> HostSpec {
    let mut s = HostSpec::new("fake host", "ferrocut-ipc-test", "/bin/sh");
    s.args = vec!["-c".into(), FAKE_HOST.into()];
    s.remote_label = "fake plugin error";
    s.stderr_ignore = &["noise:"];
    s.private_dir_env = Some("FAKE_PRIVATE_DIR");
    s
}

fn started() -> Host {
    let mut h = Host::spawn(&spec()).unwrap();
    h.handshake(&["fake-host", "1"], T).unwrap();
    h
}

#[test]
fn ok_and_err_replies() {
    let mut h = started();
    assert_eq!(h.request("ECHO\ta\tb", T).unwrap(), ["a", "b"]);
    let e = h.request("FAIL", T).unwrap_err();
    assert_eq!(e.to_string(), "fake plugin error: no such thing");
    assert!(!e.host_lost());
    assert_eq!(e.kind(), ErrorKind::Permanent);
    // ERR leaves the host usable.
    assert!(h.is_alive());
    assert_eq!(h.request("ECHO\tc", T).unwrap(), ["c"]);
}

#[test]
fn handshake_mismatch_is_a_protocol_error_and_kills_the_host() {
    let mut h = Host::spawn(&spec()).unwrap();
    let e = h.handshake(&["fake-host", "2"], T).unwrap_err();
    assert!(matches!(e, IpcError::Protocol { .. }), "{e}");
    assert!(e.to_string().contains("want fake-host 2"), "{e}");
    assert!(!h.is_alive());
}

#[test]
fn crash_reports_signal_and_filtered_stderr_then_fails_fast() {
    let mut h = started();
    let e = h.request("SEGV", T).unwrap_err();
    let msg = e.to_string();
    assert!(msg.starts_with("fake host died during SEGV: killed by SIGSEGV (11)"), "{msg}");
    assert!(msg.contains("about to crash") && !msg.contains("noise"), "{msg}");
    assert_eq!(e.kind(), ErrorKind::Retryable);
    let n = e.to_node_error("node x");
    assert_eq!(n.kind, ErrorKind::Retryable);
    assert!(n.message.starts_with("node x: fake host died"), "{}", n.message);
    // Later requests fail immediately with the recorded status.
    let t0 = Instant::now();
    let e2 = h.request("ECHO\tx", T).unwrap_err();
    assert!(e2.host_lost() && e2.to_string().contains("SIGSEGV"), "{e2}");
    assert!(t0.elapsed() < Duration::from_millis(500));
}

#[test]
fn exit_status_is_reported() {
    let mut h = started();
    let e = h.request("EXIT", T).unwrap_err();
    assert!(e.to_string().contains("exited with exit status: 3"), "{e}");
}

#[test]
fn hang_times_out_and_kills() {
    let mut h = started();
    let t0 = Instant::now();
    let e = h.request("HANG", Duration::from_millis(300)).unwrap_err();
    assert!(matches!(e, IpcError::Timeout { .. }), "{e}");
    assert!(e.to_string().contains("timed out after 300ms during HANG; host was killed"), "{e}");
    assert_eq!(e.kind(), ErrorKind::Retryable);
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(!h.is_alive());
}

#[test]
fn fatal_and_garbage_lose_the_host() {
    let mut h = started();
    let e = h.request("FATAL", T).unwrap_err();
    assert!(e.to_string().contains("died during FATAL: renderer gone"), "{e}");
    assert!(!h.is_alive());
    let mut h = started();
    let e = h.request("GARBAGE", T).unwrap_err();
    assert!(matches!(e, IpcError::Protocol { .. }) && e.host_lost(), "{e}");
}

#[test]
fn private_dir_is_passed_and_removed_even_after_kill() {
    for kill in [false, true] {
        let mut h = started();
        let dir = h.private_dir().unwrap().to_path_buf();
        assert_eq!(h.request("DIR", T).unwrap(), [dir.to_str().unwrap()]);
        std::fs::write(dir.join("scratch"), b"x").unwrap();
        if kill {
            h.kill();
        }
        drop(h);
        assert!(!dir.exists(), "{} left behind (kill={kill})", dir.display());
    }
}

#[test]
fn drop_quits_politely() {
    let mut s = spec();
    s.quit_timeout = Duration::from_secs(10);
    let mut h = Host::spawn(&s).unwrap();
    h.handshake(&["fake-host"], T).unwrap();
    let t0 = Instant::now();
    drop(h);
    assert!(t0.elapsed() < Duration::from_secs(2), "host should exit on QUIT, not after the timeout");
}

#[test]
fn missing_binary_is_permanent_and_leaves_nothing() {
    let mut s = spec();
    s.exe = "/nonexistent/ferrocut-host".into();
    let before = temp_entries();
    let Err(e) = Host::spawn(&s) else { panic!("spawned a missing binary") };
    assert!(matches!(e, IpcError::Spawn { .. }), "{e}");
    assert!(e.to_string().starts_with("failed to start fake host /nonexistent/ferrocut-host"), "{e}");
    assert_eq!(e.kind(), ErrorKind::Permanent);
    assert!(temp_entries().is_subset(&before), "private dir leaked");
}

fn temp_entries() -> std::collections::HashSet<String> {
    let me = format!("ferrocut-ipc-test-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&me))
        .collect()
}

#[test]
fn slot_spawns_lazily_restarts_after_loss_and_recycles() {
    let mut slot: HostSlot = HostSlot::new();
    let spawn = || {
        let mut h = Host::spawn(&spec())?;
        h.handshake(&["fake-host", "1"], T)?;
        Ok(h)
    };
    let pid = slot.get(spawn, |_| Ok(())).unwrap().pid();
    assert_eq!(slot.get(spawn, |_| Ok(())).unwrap().pid(), pid, "reused");
    assert_eq!(slot.spawns(), 1);

    // A crash surfaces once, then the slot starts a new host.
    let r = slot.get(spawn, |_| Ok(())).unwrap().request("SEGV", T);
    assert!(slot.check(r).is_err());
    assert!(slot.current().is_none(), "lost host discarded");
    assert_ne!(slot.get(spawn, |_| Ok(())).unwrap().pid(), pid);
    assert_eq!(slot.spawns(), 2);

    // External kill (OOM killer): noticed on the next get.
    slot.kill();
    slot.get(spawn, |_| Ok(())).unwrap();
    assert_eq!(slot.spawns(), 3);

    // Plugin errors don't discard.
    let r = slot.get(spawn, |_| Ok(())).unwrap().request("FAIL", T);
    assert!(slot.check(r).is_err());
    assert!(slot.is_running());

    // Failed init: counted, host dropped, error returned.
    let e = slot.get(|| { slot_free_spawn() }, |h| h.request("FAIL", T).map(|_| ()));
    assert!(e.is_ok(), "running host is reused, init not rerun");
    slot.discard();
    let e = slot.get(spawn, |h| h.request("FAIL", T).map(|_| ())).err().unwrap();
    assert!(e.to_string().contains("no such thing"));
    assert_eq!(slot.spawns(), 4);
    assert!(slot.current().is_none());

    // Memory budget.
    slot.get(spawn, |_| Ok(())).unwrap();
    assert!(!slot.recycle_if_rss_above(None));
    assert!(!slot.recycle_if_rss_above(Some(u64::MAX)));
    assert!(slot.recycle_if_rss_above(Some(1)));
    assert!(slot.current().is_none());
}

fn slot_free_spawn() -> Result<Host, IpcError> {
    panic!("must not spawn while a host is running")
}

#[test]
fn shm_buffer_is_shared_zeroed_and_unlinked() {
    let mut b = ShmBuffer::new("ferrocut-ipc-test", 64).unwrap();
    let path = b.path().to_path_buf();
    assert!(path.file_name().unwrap().to_str().unwrap().starts_with(&format!("ferrocut-ipc-test-{}-", std::process::id())));
    assert_eq!(b.len(), 64);
    assert!(b.bytes().iter().all(|&v| v == 0));
    b.f32s_mut()[1] = 1.5;
    b.bytes_mut()[63] = 7;
    // Another mapper (the host) sees the writes through the file.
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(f32::from_ne_bytes(on_disk[4..8].try_into().unwrap()), 1.5);
    assert_eq!(on_disk[63], 7);
    assert_eq!(b.f32s().len(), 16);
    let b2 = ShmBuffer::new("ferrocut-ipc-test", 64).unwrap();
    assert_ne!(b2.path(), path);
    drop(b);
    assert!(!path.exists());
}
