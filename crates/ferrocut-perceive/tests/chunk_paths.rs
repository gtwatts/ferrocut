//! Chunk-directory resolution from a hand-written render report.
//! No rendering: temp directories and JSON only.

use std::fs;
use std::path::{Path, PathBuf};

use ferrocut_perceive::input::{ChunkDirSource, RenderReport, resolve_chunk_dir};

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
    assert_eq!(res.tried, vec![abs]);
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
    assert_eq!(res.tried.len(), 2);
    assert!(
        res.tried
            .iter()
            .all(|p| ends_with_rel(p, "sub/.ferrocut-cache/chunks/t"))
    );
    assert!(res.tried.iter().any(|p| p.starts_with(&a)));
    assert!(res.tried.iter().any(|p| p.starts_with(&b)));
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
    assert_eq!(res.tried, vec![chunk]);
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
