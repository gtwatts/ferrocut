//! Chunk-directory resolution from a hand-written render report.
//! No rendering: temp directories and JSON only.

use std::fs;
use std::path::{Path, PathBuf};

use ferrocut_perceive::input::{
    ChunkDirSource, RenderReport, Timeline, check_paths, chunk_file_name, resolve_chunk_dir,
};
use ferrocut_perceive::{Options, Request, analyze};

fn report(chunk_dir: &str, output: &str) -> RenderReport {
    let chunk = chunk_dir.replace('\\', "\\\\").replace('"', "\\\"");
    let output = output.replace('\\', "\\\\").replace('"', "\\\"");
    RenderReport::from_json(&format!(
        r#"{{"total_frames":1,"chunk_frames":1,"chunks":[],"chunk_dir":"{chunk}","output":"{output}"}}"#
    ))
    .unwrap()
}

fn canon_temp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn ends_with_rel(path: &Path, rel: &str) -> bool {
    path.ends_with(Path::new(rel))
}

/// A. Relative output recorded from cwd A. Check runs with cwd B.
/// Report sits at the engine default name under the output's parent, so the
/// render cwd is recovered and the chunk dir is A's.
#[test]
fn relative_output_resolves_from_report_location() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let chunk = a.join("sub/.ferrocut-cache/chunks/t");
    fs::create_dir_all(&chunk).unwrap();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("sub/master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ReportLocation);
    assert_eq!(res.dir.as_deref(), Some(chunk.as_path()));
    assert!(res.tried.iter().any(|p| p == &chunk));
    assert!(
        res.tried
            .iter()
            .any(|p| p.starts_with(&b) && ends_with_rel(p, "sub/.ferrocut-cache/chunks/t"))
    );
}

/// B. An absolute chunk_dir is recorded as-is, even when the directory is missing.
#[test]
fn absolute_chunk_dir_is_recorded() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let abs = a.join("missing/chunks/t");
    let rr = report(abs.to_str().unwrap(), "master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Recorded);
    assert_eq!(res.dir.as_deref(), Some(abs.as_path()));
    // Nothing exists: the flat dir of the engine cache it sits in, then the
    // engine's default cache next to the report (chunks/<tag>, then chunks),
    // were also tried before keeping the recorded path.
    assert_eq!(
        res.tried,
        vec![
            abs,
            a.join("missing/chunks"),
            a.join(".ferrocut-cache/chunks/t"),
            a.join(".ferrocut-cache/chunks")
        ]
    );
    assert!(!res.dir.unwrap().exists());
}

/// C. Explicit relative cache next to a master in the report directory.
#[test]
fn explicit_relative_cache_uses_report_location() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let chunk = a.join(".c/chunks/t");
    fs::create_dir_all(&chunk).unwrap();
    let rr = report(".c/chunks/t", "master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ReportLocation);
    assert_eq!(res.dir.as_deref(), Some(chunk.as_path()));
}

/// D. Neither candidate exists.
#[test]
fn missing_candidates_are_unresolved() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("sub/master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Unresolved);
    assert!(res.dir.is_none());
    // The two rule-2 candidates, then the default cache next to the report.
    // Its tagged dir (<report dir>/.ferrocut-cache/chunks/t) is the same path
    // as the report-location candidate, so it is listed once.
    assert_eq!(res.tried.len(), 3);
    assert!(
        res.tried[..2]
            .iter()
            .all(|p| ends_with_rel(p, "sub/.ferrocut-cache/chunks/t"))
    );
    assert!(res.tried[..2].iter().any(|p| p.starts_with(&a)));
    assert!(res.tried[..2].iter().any(|p| p.starts_with(&b)));
    assert!(res.tried[..2].contains(&a.join("sub/.ferrocut-cache/chunks/t")));
    assert_eq!(res.tried[2..], [a.join("sub/.ferrocut-cache/chunks")]);
}

/// E. A copied report whose name is not `<output stem>.report.json`.
/// The chunk dir exists relative to the process cwd, so that candidate wins.
#[test]
fn non_default_report_name_uses_process_cwd() {
    let (_keep, cwd) = canon_temp();
    let (_other, elsewhere) = canon_temp();
    let chunk = cwd.join(".ferrocut-cache/chunks/t");
    fs::create_dir_all(&chunk).unwrap();
    let rr = report(".ferrocut-cache/chunks/t", "master.mkv");
    let res = resolve_chunk_dir(&rr, &elsewhere.join("copied-report.json"), &cwd, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ProcessCwd);
    assert_eq!(res.dir.as_deref(), Some(chunk.as_path()));
    assert_eq!(res.tried[0], chunk);
    assert_eq!(res.search[0], (ChunkDirSource::ProcessCwd, chunk));
}

/// F. Report-location and process-cwd candidates both exist and differ.
#[test]
fn both_candidates_existing_and_different_are_ambiguous() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    fs::create_dir_all(a.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    fs::create_dir_all(b.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("sub/master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Ambiguous);
    assert!(res.dir.is_none());
    assert_eq!(res.tried.len(), 2);
    assert!(res.tried.iter().any(|p| p.starts_with(&a)));
    assert!(res.tried.iter().any(|p| p.starts_with(&b)));
}

/// G'. A recorded absolute directory that is gone is rescued by `--cache-dir`.
#[test]
fn missing_recorded_dir_uses_cache_dir_override() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let abs = a.join("relocated/chunks/tag");
    let cache = b.join("explicit-cache");
    let rescued = cache.join("chunks/tag");
    fs::create_dir_all(&rescued).unwrap();
    let rr = report(abs.to_str().unwrap(), "master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("master.report.json"), &b, Some(&cache));
    assert_eq!(res.resolved_by, ChunkDirSource::CacheDirOverride);
    assert_eq!(res.dir.as_deref(), Some(rescued.as_path()));
}

/// K. A symlink alias of the same directory is one directory, not Ambiguous.
#[test]
fn symlink_alias_of_the_same_dir_is_not_ambiguous() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let real = a.join("sub/.ferrocut-cache/chunks/t");
    fs::create_dir_all(&real).unwrap();
    let alias_parent = b.join("sub/.ferrocut-cache/chunks");
    fs::create_dir_all(&alias_parent).unwrap();
    std::os::unix::fs::symlink(&real, alias_parent.join("t")).unwrap();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let res = resolve_chunk_dir(&rr, &a.join("sub/master.report.json"), &b, None);
    assert_ne!(res.resolved_by, ChunkDirSource::Ambiguous);
    let dir = res.dir.expect("one directory");
    assert_eq!(
        fs::canonicalize(&dir).unwrap(),
        fs::canonicalize(&real).unwrap()
    );
}

/// L. `master.report.json` in some other directory is only an inference.
/// (i) inferred dir missing, process cwd exists → ProcessCwd.
#[test]
fn inferred_report_location_missing_uses_process_cwd() {
    let (_keep_cwd, cwd) = canon_temp();
    let (_keep_else, elsewhere) = canon_temp();
    let real = cwd.join("sub/.ferrocut-cache/chunks/t");
    fs::create_dir_all(&real).unwrap();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let report = elsewhere.join("sub/master.report.json");
    let res = resolve_chunk_dir(&rr, &report, &cwd, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ProcessCwd);
    assert_eq!(res.dir.as_deref(), Some(real.as_path()));
}

/// L. (ii) inferred dir and process cwd both exist and differ → Ambiguous.
#[test]
fn inferred_report_location_and_process_cwd_are_ambiguous() {
    let (_keep_cwd, cwd) = canon_temp();
    let (_keep_else, elsewhere) = canon_temp();
    fs::create_dir_all(cwd.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    fs::create_dir_all(elsewhere.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    let rr = report("sub/.ferrocut-cache/chunks/t", "sub/master.mkv");
    let report = elsewhere.join("sub/master.report.json");
    let res = resolve_chunk_dir(&rr, &report, &cwd, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Ambiguous);
    assert!(res.dir.is_none());
}

/// M. A tree moved together with its report (recorded absolute path gone): the
/// engine's default cache next to the report is used, without any flag.
#[test]
fn moved_tree_uses_the_default_cache_next_to_the_report() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let old = a.join("old-place/.ferrocut-cache/chunks/tag");
    let moved = b.join("new-place/.ferrocut-cache/chunks/tag");
    fs::create_dir_all(&moved).unwrap();
    let rr = report(old.to_str().unwrap(), "/old-place/master.mkv");
    let res = resolve_chunk_dir(&rr, &b.join("new-place/master.report.json"), &a, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ReportDefaultCache);
    assert_eq!(res.dir.as_deref(), Some(moved.as_path()));
    // An explicit --cache-dir is preferred over the default location.
    let explicit = a.join("explicit/chunks/tag");
    fs::create_dir_all(&explicit).unwrap();
    let res = resolve_chunk_dir(
        &rr,
        &b.join("new-place/master.report.json"),
        &a,
        Some(&a.join("explicit")),
    );
    assert_eq!(res.resolved_by, ChunkDirSource::CacheDirOverride);
    assert_eq!(res.dir.as_deref(), Some(explicit.as_path()));
}

/// N. A report from an engine before per-adapter chunk dirs (no chunk_dir) keeps
/// the old flat lookup in the default cache; with nothing there it stays Absent.
#[test]
fn report_without_chunk_dir_keeps_the_flat_default_cache() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let rr = RenderReport::from_json(r#"{"total_frames":1,"chunk_frames":1,"chunks":[]}"#).unwrap();
    let res = resolve_chunk_dir(&rr, &a.join("master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Absent);
    assert!(res.dir.is_none());
    assert_eq!(res.tried, vec![a.join(".ferrocut-cache/chunks")]);
    let flat = a.join(".ferrocut-cache/chunks");
    fs::create_dir_all(&flat).unwrap();
    let res = resolve_chunk_dir(&rr, &a.join("master.report.json"), &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ReportDefaultCache);
    assert_eq!(res.dir.as_deref(), Some(flat.as_path()));
}

/// A report listing chunks with these keys.
fn report_with_chunks(chunk_dir: &str, output: &str, keys: &[&str]) -> RenderReport {
    let chunks: Vec<String> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!(r#"{{"index":{i},"start_frame":{i},"frames":1,"key":"{k}"}}"#))
        .collect();
    let chunk = chunk_dir.replace('\\', "\\\\").replace('"', "\\\"");
    let output = output.replace('\\', "\\\\").replace('"', "\\\"");
    RenderReport::from_json(&format!(
        r#"{{"total_frames":{n},"chunk_frames":1,"chunks":[{c}],"chunk_dir":"{chunk}","output":"{output}"}}"#,
        n = keys.len(),
        c = chunks.join(",")
    ))
    .unwrap()
}

const KEY_A: &str = "aaaaaaaaaaaaaaaa";
const KEY_B: &str = "bbbbbbbbbbbbbbbb";

/// P4. Lookup is per chunk file, in order. The recorded directory still exists
/// but has evicted one key; the explicit --cache-dir (tagged, then flat) holds
/// it. Each chunk comes from the first directory holding its file, as the
/// pre-resolution checker did. Harmless key-named files; nothing is decoded.
#[test]
fn partial_recorded_cache_falls_through_per_chunk_file() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let recorded = a.join("render/.ferrocut-cache/chunks/tag");
    fs::create_dir_all(&recorded).unwrap();
    fs::write(recorded.join(format!("{KEY_A}.mkv")), b"a").unwrap();
    let cache = b.join("explicit");
    let tagged = cache.join("chunks/tag");
    fs::create_dir_all(&tagged).unwrap();
    fs::write(tagged.join(format!("{KEY_B}.mkv")), b"b").unwrap();
    let rr = report_with_chunks(recorded.to_str().unwrap(), "master.mkv", &[KEY_A, KEY_B]);
    let res = resolve_chunk_dir(&rr, &a.join("render/master.report.json"), &b, Some(&cache));
    assert_eq!(res.resolved_by, ChunkDirSource::Recorded);
    assert_eq!(
        res.locate(KEY_A).unwrap(),
        Some(recorded.join(format!("{KEY_A}.mkv")))
    );
    assert_eq!(
        res.locate(KEY_B).unwrap(),
        Some(tagged.join(format!("{KEY_B}.mkv")))
    );
    // The flat explicit cache is the next file-level fallback.
    fs::remove_file(tagged.join(format!("{KEY_B}.mkv"))).unwrap();
    fs::write(cache.join(format!("chunks/{KEY_B}.mkv")), b"b").unwrap();
    assert_eq!(
        res.locate(KEY_B).unwrap(),
        Some(cache.join(format!("chunks/{KEY_B}.mkv")))
    );
    // check_paths reports exactly the files that will be opened.
    fs::write(
        a.join("render/master.report.json"),
        format!(
            r#"{{"total_frames":2,"chunk_frames":1,"chunks":[{{"index":0,"start_frame":0,"frames":1,"key":"{KEY_A}"}},{{"index":1,"start_frame":1,"frames":1,"key":"{KEY_B}"}}],"chunk_dir":"{}","output":"master.mkv"}}"#,
            recorded.display()
        ),
    )
    .unwrap();
    let (_, paths) = check_paths(&a.join("render/master.mkv"), None, Some(&cache), &b).unwrap();
    assert_eq!(
        paths.chunk_files,
        vec![
            recorded.join(format!("{KEY_A}.mkv")),
            cache.join(format!("chunks/{KEY_B}.mkv"))
        ]
    );
    assert_eq!(paths.cache_dir, cache);
}

/// P4. The advice for an ambiguous legacy path works: with --cache-dir only
/// the explicit cache is searched (no guess between the two candidates).
#[test]
fn ambiguous_relative_dir_is_recovered_by_cache_dir() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    fs::create_dir_all(a.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    fs::create_dir_all(b.join("sub/.ferrocut-cache/chunks/t")).unwrap();
    let rr = report_with_chunks("sub/.ferrocut-cache/chunks/t", "sub/master.mkv", &[KEY_A]);
    let report = a.join("sub/master.report.json");
    let res = resolve_chunk_dir(&rr, &report, &b, None);
    assert_eq!(res.resolved_by, ChunkDirSource::Ambiguous);
    assert!(res.search.is_empty());
    assert!(res.error(&report).contains("--cache-dir"));

    let chosen = a.join("sub/.ferrocut-cache");
    fs::write(chosen.join(format!("chunks/t/{KEY_A}.mkv")), b"a").unwrap();
    let res = resolve_chunk_dir(&rr, &report, &b, Some(&chosen));
    assert_eq!(res.resolved_by, ChunkDirSource::CacheDirOverride);
    assert!(
        res.search
            .iter()
            .all(|(s, _)| *s == ChunkDirSource::CacheDirOverride)
    );
    assert_eq!(
        res.locate(KEY_A).unwrap(),
        Some(chosen.join(format!("chunks/t/{KEY_A}.mkv")))
    );
}

/// P5. `alias/../cache/chunks/tag` where `alias` is a symlink to
/// `targets/nested`: the filesystem resolves it under `targets/`, not by
/// erasing `alias/..` as text. The recorded relative path is found as is.
#[cfg(unix)]
#[test]
fn symlink_then_parent_follows_the_filesystem() {
    let (_keep_cwd, cwd) = canon_temp();
    let (_keep_r, elsewhere) = canon_temp();
    fs::create_dir_all(cwd.join("targets/nested")).unwrap();
    let real = cwd.join("targets/cache/chunks/tag");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(cwd.join("targets/nested"), cwd.join("alias")).unwrap();
    // The text-folded spelling does not exist.
    assert!(!cwd.join("cache/chunks/tag").exists());
    let rr = report("alias/../cache/chunks/tag", "master.mkv");
    let res = resolve_chunk_dir(&rr, &elsewhere.join("copied.json"), &cwd, None);
    assert_eq!(res.resolved_by, ChunkDirSource::ProcessCwd);
    let dir = res.dir.expect("found through the symlink");
    assert_eq!(fs::canonicalize(&dir).unwrap(), real);
}

/// P2. Chunk keys are file names, never paths.
#[test]
fn chunk_keys_must_be_plain_hex() {
    assert_eq!(chunk_file_name(KEY_A).unwrap(), format!("{KEY_A}.mkv"));
    for bad in [
        "",
        "../outside",
        "/abs/x",
        "ab/cd",
        "abc.mkv",
        &"a".repeat(129),
    ] {
        assert!(chunk_file_name(bad).is_err(), "{bad:?}");
    }
    let (_keep, a) = canon_temp();
    fs::create_dir_all(a.join(".ferrocut-cache/chunks/t")).unwrap();
    fs::write(
        a.join("master.report.json"),
        r#"{"total_frames":1,"chunk_frames":1,"chunks":[{"index":0,"start_frame":0,"frames":1,"key":"../../outside"}],"chunk_dir":".ferrocut-cache/chunks/t","output":"master.mkv"}"#,
    )
    .unwrap();
    let err = check_paths(&a.join("master.mkv"), None, None, &a).unwrap_err();
    assert!(
        format!("{err:#}").contains("not a hex chunk key"),
        "{err:#}"
    );
}

/// P2. The audio master: the file pointed at, unless the report is given
/// explicitly (or is the render argument), when the report's `output` is used.
#[test]
fn audio_master_follows_the_checker_rule() {
    let (_keep, a) = canon_temp();
    let (_keep_o, other) = canon_temp();
    let json = format!(
        r#"{{"total_frames":0,"chunk_frames":1,"chunks":[],"chunk_dir":"{}","output":"{}","audio":{{"sample_rate":48000,"channels":2,"samples":0,"blake3":"00"}}}}"#,
        a.join(".ferrocut-cache/chunks/t").display(),
        other.join("elsewhere.mkv").display()
    );
    fs::write(a.join("master.report.json"), &json).unwrap();
    let (rr, paths) = check_paths(&a.join("master.mkv"), None, None, &a).unwrap();
    assert!(rr.audio.is_some(), "fixture must declare audio");
    assert_eq!(paths.audio, Some(a.join("master.mkv")));
    let (_, paths) = check_paths(&a.join("master.report.json"), None, None, &a).unwrap();
    assert_eq!(paths.audio, Some(other.join("elsewhere.mkv")));
    assert_eq!(paths.report, a.join("master.report.json"));
}

/// P3. A report with chunks and an unusable directory fails before any cache
/// is read or created, so a warm analysis cache cannot hide it: the same error
/// from check_paths and from analyze, and the cache dir is never created.
#[test]
fn unusable_resolution_fails_before_the_analysis_cache() {
    let (_keep_a, a) = canon_temp();
    let (_keep_b, b) = canon_temp();
    let rr_json = format!(
        r#"{{"total_frames":1,"chunk_frames":1,"chunks":[{{"index":0,"start_frame":0,"frames":1,"key":"{KEY_A}"}}],"chunk_dir":"gone/chunks/t","output":"master.mkv"}}"#
    );
    fs::write(a.join("master.report.json"), &rr_json).unwrap();
    let err = check_paths(&a.join("master.mkv"), None, None, &b).unwrap_err();
    assert!(format!("{err:#}").contains("unresolved"), "{err:#}");

    let mut rr = RenderReport::from_json(&rr_json).unwrap();
    rr.chunk_resolution = Some(resolve_chunk_dir(
        &rr,
        &a.join("master.report.json"),
        &b,
        None,
    ));
    let tl = Timeline::from_json(
        r#"{"output":{"width":64,"height":32,"fps":"24","gop":12},"tracks":[]}"#,
    )
    .unwrap();
    let cache = a.join("cache");
    let Err(err) = analyze(Request {
        timeline: &tl,
        render: &rr,
        cache_dir: &cache,
        out_dir: &a.join("out"),
        audio: None,
        options: Options::default(),
        gpu: None,
    }) else {
        panic!("an unusable resolution must be refused");
    };
    assert!(format!("{err:#}").contains("unresolved"), "{err:#}");
    assert!(!cache.exists(), "cache touched before the resolution check");
}
