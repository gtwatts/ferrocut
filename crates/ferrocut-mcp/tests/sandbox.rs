//! Project-root restriction: relative/absolute escapes, symlinked files and
//! directories pointing out, dangling symlinks, `..` through missing dirs,
//! and every tool refusing outside paths (including media sources) before
//! reading, probing, writing or touching a GPU.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use ferrocut_mcp::{Ctx, Root, call};
use serde_json::{Value, json};

const TL: &str = r#"{
  "output": { "width": 64, "height": 32, "fps": "24", "gop": 12 },
  "tracks": [ { "name": "V1", "clips": [
    { "id": "a", "source": "a.mkv", "start": 0, "source_in": "0", "duration": "2" } ]}]
}"#;

struct Fixture {
    _dir: tempfile::TempDir,
    proj: PathBuf,
    outside: PathBuf,
}

/// base/proj (the root) and base/outside, with symlinks from proj:
/// link_dir -> ../outside, evil.json -> ../outside/secret.json,
/// dangling -> ../outside/missing.mkv, inner -> sub (stays inside).
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (proj, outside) = (base.join("proj"), base.join("outside"));
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(proj.join("tl.json"), TL).unwrap();
    std::fs::write(proj.join("sub/inner.json"), TL).unwrap();
    std::fs::write(outside.join("secret.json"), TL).unwrap();
    std::fs::write(
        proj.join("escape-src.json"),
        TL.replace("\"a.mkv\"", "\"../outside/a.mkv\""),
    )
    .unwrap();
    std::fs::write(
        proj.join("link-src.json"),
        TL.replace("\"a.mkv\"", "\"link_dir/a.mkv\""),
    )
    .unwrap();
    symlink("../outside", proj.join("link_dir")).unwrap();
    symlink("../outside/secret.json", proj.join("evil.json")).unwrap();
    symlink("../outside/missing.mkv", proj.join("dangling")).unwrap();
    symlink("sub", proj.join("inner")).unwrap();
    Fixture {
        _dir: dir,
        proj,
        outside,
    }
}

fn outside_err(r: anyhow::Result<PathBuf>) -> String {
    let e = format!("{:#}", r.expect_err("should be rejected"));
    assert!(
        e.contains("outside the project root")
            || e.contains("dangling")
            || e.contains("`..` after a missing directory"),
        "{e}"
    );
    e
}

#[test]
fn root_resolves_inside_and_rejects_escapes() {
    let f = fixture();
    let root = Root::new(&f.proj).unwrap();
    assert_eq!(root.dir(), f.proj);
    let ok = |p: &str| {
        root.check(Path::new(p))
            .unwrap_or_else(|e| panic!("{p}: {e:#}"))
    };

    assert_eq!(ok("tl.json"), f.proj.join("tl.json"));
    assert_eq!(ok("."), f.proj);
    assert_eq!(
        ok(f.proj.join("tl.json").to_str().unwrap()),
        f.proj.join("tl.json")
    );
    assert_eq!(ok("sub/../tl.json"), f.proj.join("tl.json"));
    assert_eq!(ok("new/deeper/out.mkv"), f.proj.join("new/deeper/out.mkv"));
    assert_eq!(ok("./sub/./new.mkv"), f.proj.join("sub/new.mkv"));
    // A symlink that stays inside is fine (resolved to its target).
    assert_eq!(ok("inner/inner.json"), f.proj.join("sub/inner.json"));
    assert_eq!(ok("inner/new.mkv"), f.proj.join("sub/new.mkv"));

    for p in [
        "..",
        "../outside/secret.json",
        "../outside/new.mkv",
        "sub/../../outside/secret.json",
        "link_dir/secret.json",
        "link_dir/new.mkv",
        "link_dir/new/deeper.mkv",
        "evil.json",
        "dangling",
        "new/../../outside/x.mkv",
        "/etc/passwd",
        "/nonexistent-root-dir/x.mkv",
    ] {
        outside_err(root.check(Path::new(p)));
    }
    outside_err(root.check(&f.outside.join("secret.json")));
    // The root itself through a symlink is canonicalized.
    let alias = f.proj.parent().unwrap().join("alias");
    symlink(&f.proj, &alias).unwrap();
    assert_eq!(Root::new(&alias).unwrap().dir(), f.proj);
    assert!(Root::new(&f.proj.join("tl.json")).is_err());
    assert!(Root::new(&f.proj.join("missing")).is_err());
}

fn run(cx: &Ctx, tool: &str, args: Value) -> Result<Value, String> {
    call(cx, tool, args)
        .expect("known tool")
        .map_err(|e| format!("{e:#}"))
}

fn rejected(cx: &Ctx, tool: &str, args: Value) -> String {
    let e = run(cx, tool, args.clone()).expect_err(&format!("{tool} {args} should fail"));
    assert!(
        e.contains("outside the project root") || e.contains("dangling"),
        "{tool} {args}: {e}"
    );
    e
}

#[test]
fn tools_refuse_paths_outside_the_root() {
    let f = fixture();
    let cx = Ctx::new(Root::new(&f.proj).unwrap());
    let before = std::fs::read(f.proj.join("tl.json")).unwrap();

    // Allowed: relative to the root, absolute inside it.
    let v = run(&cx, "timeline_get", json!({ "timeline": "tl.json" })).unwrap();
    assert_eq!(v["frame_count"], 48);
    run(
        &cx,
        "timeline_get",
        json!({ "timeline": f.proj.join("sub/inner.json") }),
    )
    .unwrap();

    // Timeline files outside (relative, absolute, via symlinks).
    rejected(
        &cx,
        "timeline_get",
        json!({ "timeline": "../outside/secret.json" }),
    );
    rejected(
        &cx,
        "timeline_get",
        json!({ "timeline": f.outside.join("secret.json") }),
    );
    rejected(&cx, "timeline_get", json!({ "timeline": "evil.json" }));
    rejected(
        &cx,
        "timeline_get",
        json!({ "timeline": "link_dir/secret.json" }),
    );
    rejected(&cx, "log", json!({ "timeline": "evil.json" }));
    rejected(&cx, "undo", json!({ "timeline": "evil.json" }));
    rejected(
        &cx,
        "branch",
        json!({ "timeline": "evil.json", "action": "create", "name": "x" }),
    );
    rejected(&cx, "report_read", json!({ "report": "evil.json" }));
    rejected(&cx, "markers_list", json!({ "timeline": "evil.json" }));
    rejected(&cx, "media_status", json!({ "timeline": "evil.json" }));
    rejected(&cx, "proxy_generate", json!({ "timeline": "evil.json" }));
    rejected(
        &cx,
        "proxy_generate",
        json!({ "media": ["../outside/a.mkv"] }),
    );
    rejected(
        &cx,
        "proxy_generate",
        json!({ "media": ["link_dir/a.mkv"] }),
    );
    rejected(
        &cx,
        "diff",
        json!({ "a": "tl.json", "b": "evil.json", "render": false }),
    );

    // Media sources outside: plan/render/diff/quality_check refuse before
    // hashing or decoding anything.
    for tl in ["escape-src.json", "link-src.json"] {
        let e = rejected(&cx, "plan", json!({ "timeline": tl }));
        assert!(e.contains("clip source"), "{e}");
        rejected(&cx, "media_status", json!({ "timeline": tl }));
        rejected(&cx, "proxy_generate", json!({ "timeline": tl }));
        rejected(
            &cx,
            "render",
            json!({ "timeline": tl, "output": "out.mkv" }),
        );
        rejected(&cx, "diff", json!({ "a": "tl.json", "b": tl }));
        rejected(
            &cx,
            "quality_check",
            json!({ "render": "out.mkv", "timeline": tl }),
        );
    }

    // Render outputs, caches and reports outside.
    rejected(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "../out.mkv" }),
    );
    rejected(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "link_dir/out.mkv" }),
    );
    rejected(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "dangling" }),
    );
    rejected(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "out.mkv", "cache_dir": "../cache" }),
    );
    rejected(
        &cx,
        "render",
        json!({ "timeline": "tl.json", "output": "out.mkv", "report": "link_dir/r.json" }),
    );

    // Relink: outside targets and search directories.
    for op in [
        json!({ "op": "relink", "clip": "a", "to": "../outside/a.mkv" }),
        json!({ "op": "relink", "from": "a.mkv", "to": "link_dir/a.mkv" }),
        json!({ "op": "relink", "search": "../outside" }),
        json!({ "op": "relink", "search": "link_dir" }),
    ] {
        rejected(
            &cx,
            "edit_apply",
            json!({ "timeline": "tl.json", "ops": [op] }),
        );
    }

    // Edits: outside output, and an inserted clip whose source is outside.
    let slip = json!([{ "op": "slip", "clip": "a", "delta": "1/2" }]);
    rejected(
        &cx,
        "edit_apply",
        json!({ "timeline": "tl.json", "ops": slip, "output": "link_dir/edited.json" }),
    );
    rejected(
        &cx,
        "edit_apply",
        json!({ "timeline": "tl.json", "ops": slip, "output": "../edited.json" }),
    );
    for src in [
        "../outside/b.mkv",
        "link_dir/b.mkv",
        "dangling",
        "/etc/hostname",
    ] {
        let insert = json!([{ "op": "ripple_insert", "track": "V1", "at": "2",
            "clip": { "id": "x", "source": src, "start": 0, "source_in": "0", "duration": "1" } }]);
        let e = rejected(
            &cx,
            "edit_apply",
            json!({ "timeline": "tl.json", "ops": insert }),
        );
        assert!(e.contains("clip source"), "{e}");
        // add_clip probes its file: the root check must come first.
        let add = json!([{ "op": "add_clip", "track": "V1", "source": src }]);
        let e = rejected(
            &cx,
            "edit_apply",
            json!({ "timeline": "tl.json", "ops": add }),
        );
        assert!(e.contains("clip source"), "add_clip {src}: {e}");
        let e = rejected(&cx, "media_probe", json!({ "path": src }));
        assert!(
            e.contains("root") || e.contains("outside") || e.contains("dangling"),
            "media_probe {src}: {e}"
        );
    }
    assert_eq!(std::fs::read(f.proj.join("tl.json")).unwrap(), before);
    assert!(
        !f.proj.join("tl.journal.jsonl").exists(),
        "nothing journaled"
    );
    assert!(!f.outside.join("edited.json").exists());
    assert!(!f.proj.join("../edited.json").exists());

    // The same edit inside the root works (sources inside).
    let v = run(
        &cx,
        "edit_apply",
        json!({ "timeline": "tl.json", "ops": slip, "probe": false }),
    )
    .unwrap();
    assert_eq!(v["written"], true);
    let insert = json!([{ "op": "ripple_insert", "track": "V1", "at": "2",
        "clip": { "id": "x", "source": "sub/b.mkv", "start": 0, "source_in": "0", "duration": "1" } }]);
    run(
        &cx,
        "edit_apply",
        json!({ "timeline": "tl.json", "ops": insert, "probe": false }),
    )
    .unwrap();
}
