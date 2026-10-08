//! Bounded file orchestration around the pure FilmCraft adapter. Paths pass
//! through the caller's guard before reading/probing/writing. New documents
//! never overwrite existing work; ordinary failures roll back files created
//! by this call. A multi-file import is not a crash-atomic filesystem transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};

use crate::Timeline;
use crate::interchange::{self as ic, Format, SourceMetadata};

pub type PathGuard<'a> = dyn FnMut(&Path) -> anyhow::Result<PathBuf> + 'a;

/// CLI guard: absolute paths without imposing an MCP project root.
pub fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn local_media_path(path: &Path) -> anyhow::Result<()> {
    // Upstream joining may turn https:// into https:/; inspect components
    // before resolving or probing rather than treating a network URL as a file.
    for component in path.components() {
        if let Component::Normal(c) = component {
            let s = c.to_string_lossy();
            if let Some((scheme, _)) = s.split_once(':') {
                ensure!(
                    ![
                        "http", "https", "ftp", "ftps", "s3", "gs", "data", "rtsp", "rtmp", "ssh",
                        "smb"
                    ]
                    .contains(&scheme.to_ascii_lowercase().as_str()),
                    "non-file media URI scheme {scheme:?} is unsupported; relink to a local project file"
                );
            }
        }
    }
    Ok(())
}

fn read(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= ic::MAX_DOCUMENT_BYTES as u64,
        "document exceeds {} byte limit",
        ic::MAX_DOCUMENT_BYTES
    );
    let mut bytes = Vec::new();
    file.take(ic::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= ic::MAX_DOCUMENT_BYTES,
        "document grew beyond byte limit"
    );
    Ok(bytes)
}

pub fn format(name: Option<&str>, path: &Path, bytes: Option<&[u8]>) -> anyhow::Result<Format> {
    let ext = path.extension().and_then(|e| e.to_str());
    let format = match name {
        Some("otio") => Some(Format::Otio),
        Some("fcp7" | "fcp7_xml" | "xml") => Some(Format::Fcp7Xml),
        Some(s) => bail!("unsupported connected format {s:?}; choose otio or fcp7"),
        None => bytes
            .and_then(|b| ic::detect(b, ext))
            .or_else(|| ext.and_then(Format::from_extension)),
    }
    .context("cannot detect interchange format; choose otio or fcp7")?;
    ensure!(
        matches!(format, Format::Otio | Format::Fcp7Xml),
        "detected {} is retained upstream but not connected; choose otio or fcp7",
        format.name()
    );
    Ok(format)
}

fn check_own(tl: &mut Timeline, guard: &mut PathGuard<'_>) -> anyhow::Result<()> {
    for p in tl.sources_mut() {
        local_media_path(p)?;
        *p = guard(p)?;
    }
    for p in tl.assets_mut() {
        *p = guard(p)?;
    }
    Ok(())
}

fn load(path: &Path, guard: &mut PathGuard<'_>) -> anyhow::Result<Timeline> {
    let path = guard(path)?;
    let mut tl: Timeline = serde_json::from_slice(&read(&path)?)?;
    tl.resolve_sources(path.parent().context("timeline has no parent directory")?);
    check_own(&mut tl, guard)?;
    tl.validate()?;
    Ok(tl)
}

fn collect(
    tl: &Timeline,
    guard: &mut PathGuard<'_>,
    nested: &mut BTreeMap<PathBuf, Timeline>,
    media: &mut BTreeMap<PathBuf, SourceMetadata>,
    issues: &mut Vec<Value>,
    stack: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> anyhow::Result<()> {
    ensure!(depth < 32, "interchange nested depth exceeds 32");
    let mut copy = tl.clone();
    let paths: BTreeSet<PathBuf> = copy.sources_mut().map(|p| p.clone()).collect();
    for path in paths {
        let path = guard(&path)?;
        if crate::comp::is_comp(&path) {
            ensure!(
                !stack.contains(&path),
                "nested timeline cycle at {}",
                path.display()
            );
            if nested.contains_key(&path) {
                continue;
            }
            ensure!(
                nested.len() < 256,
                "interchange nested timeline count exceeds 256"
            );
            let inner = load(&path, guard)?;
            stack.insert(path.clone());
            collect(&inner, guard, nested, media, issues, stack, depth + 1)?;
            stack.remove(&path);
            nested.insert(path, inner);
        } else if !media.contains_key(&path) {
            ensure!(
                media.len() + issues.len() < 1000,
                "interchange unique source count exceeds 1000"
            );
            match crate::media::probe(&path) {
                Ok(i) if i.duration.is_some() => {
                    media.insert(path,SourceMetadata {duration:i.duration.context("duration missing")?,width:i.width,height:i.height,fps:i.fps,sample_rate:i.sample_rate,channels:i.channels});
                }
                Ok(_) => issues.push(json!({"path":path,"error":"source has unknown duration; equivalence cannot be guaranteed"})),
                Err(e) => issues.push(json!({"path":path,"error":format!("source probe failed: {e:#}")})),
            }
        }
    }
    Ok(())
}

fn write_new(files: &[(PathBuf, Vec<u8>)]) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for (path, _) in files {
        ensure!(
            seen.insert(path),
            "duplicate output path {}",
            path.display()
        );
        ensure!(
            std::fs::symlink_metadata(path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "refusing to overwrite {}",
            path.display()
        );
        ensure!(
            path.parent().is_some_and(Path::is_dir),
            "output directory must exist for {}",
            path.display()
        );
    }
    let mut created = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        for (path, bytes) in files {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?;
            created.push(path);
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut rollback = Vec::new();
        for path in created.into_iter().rev() {
            if let Err(e) = std::fs::remove_file(path) {
                rollback.push(format!("{}: {e}", path.display()));
            }
        }
        if !rollback.is_empty() {
            bail!("{error:#}; rollback incomplete: {}", rollback.join("; "));
        }
        return Err(error);
    }
    Ok(())
}

pub fn export_file(
    timeline: &Path,
    output: &Path,
    selected_format: Option<&str>,
    allow_loss: bool,
    dry_run: bool,
    guard: &mut PathGuard<'_>,
) -> anyhow::Result<Value> {
    let timeline = guard(timeline)?;
    let output = guard(output)?;
    let tl = load(&timeline, guard)?;
    let format = format(selected_format, &output, None)?;
    let mut options = ic::ExportOptions {
        base_dir: timeline.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    let mut nested = BTreeMap::new();
    let mut issues = Vec::new();
    collect(
        &tl,
        guard,
        &mut nested,
        &mut options.media,
        &mut issues,
        &mut BTreeSet::from([timeline.clone()]),
        0,
    )?;
    let mut result =
        ic::export_timeline_with_resolver(&tl, format, &options, |p| Ok(nested.get(p).cloned()))?;
    for issue in &issues {
        result.report.entries.push(ic::ReportEntry {
            severity: ic::Severity::Loss,
            feature: "media_probe".into(),
            location: issue["path"].to_string(),
            message: issue["error"].as_str().unwrap_or("unknown metadata").into(),
        });
    }
    if !dry_run {
        if !allow_loss {
            result.report.ensure_lossless()?;
        }
        write_new(&[(output.clone(), result.bytes.clone())])?;
    }
    Ok(
        json!({"output":output,"format":format.name(),"dry_run":dry_run,"written":!dry_run,
        "bytes":result.bytes.len(),"report":result.report,"has_losses":result.report.has_losses(),"source_probe_issues":issues}),
    )
}

pub fn import_file(
    input: &Path,
    output: &Path,
    selected_format: Option<&str>,
    sequence: usize,
    allow_loss: bool,
    dry_run: bool,
    guard: &mut PathGuard<'_>,
) -> anyhow::Result<Value> {
    let input = guard(input)?;
    let output = guard(output)?;
    ensure!(
        output
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json")),
        "native output must have .json extension"
    );
    let bytes = read(&input)?;
    let format = format(selected_format, &input, Some(&bytes))?;
    let base = input.parent().context("input has no parent directory")?;
    let out_dir = output.parent().context("output has no parent directory")?;
    let mut result = ic::import_document(
        &bytes,
        format,
        &ic::ImportOptions {
            base_dir: Some(base.to_path_buf()),
            sequence,
        },
    )?;
    let names: BTreeSet<PathBuf> = result.nested.iter().map(|n| n.filename.clone()).collect();
    for name in &names {
        ensure!(
            matches!(name.components().next(), Some(Component::Normal(_)))
                && name.components().count() == 1,
            "unsafe generated nested filename"
        );
    }
    for tl in std::iter::once(&mut result.timeline)
        .chain(result.nested.iter_mut().map(|n| &mut n.timeline))
    {
        for path in tl.sources_mut() {
            local_media_path(path)?;
            if names.contains(path) {
                guard(&out_dir.join(&*path))?;
            } else {
                if path.is_relative() {
                    *path = base.join(&*path);
                }
                *path = guard(path)?;
            }
        }
        for asset in tl.assets_mut() {
            if asset.is_relative() {
                *asset = base.join(&*asset);
            }
            *asset = guard(asset)?;
        }
        tl.validate()?;
    }
    // Imported document metadata can overstate source handles. Check the
    // actual guarded files rather than trusting normalized foreign durations.
    let mut requirements = BTreeMap::new();
    for tl in std::iter::once(&result.timeline).chain(result.nested.iter().map(|n| &n.timeline)) {
        for (path, end) in ic::imported_source_requirements(tl, &names)? {
            requirements
                .entry(path)
                .and_modify(|old: &mut ferrocut_core::RationalTime| *old = (*old).max(end))
                .or_insert(end);
        }
    }
    ensure!(
        requirements.len() <= 1000,
        "interchange unique source count exceeds 1000"
    );
    let mut issues = Vec::new();
    for (path, end) in requirements {
        let path = guard(&path)?;
        let issue = match crate::media::probe(&path) {
            Ok(info) => match info.duration {
                Some(duration) if end > duration => Some((
                    "media_source_range",
                    format!("consumed source end {end} exceeds probed duration {duration}"),
                )),
                Some(_) => None,
                None => Some((
                    "media_probe",
                    "source has unknown duration; equivalence cannot be guaranteed".into(),
                )),
            },
            Err(error) => Some(("media_probe", format!("source probe failed: {error:#}"))),
        };
        if let Some((feature, message)) = issue {
            issues.push(json!({"path":path,"feature":feature,"error":message}));
            result.report.entries.push(ic::ReportEntry {
                severity: ic::Severity::Loss,
                feature: feature.into(),
                location: path.display().to_string(),
                message,
            });
        }
    }
    let mut files: Vec<_> = result
        .nested
        .iter()
        .map(|n| {
            Ok((
                guard(&out_dir.join(&n.filename))?,
                crate::project::timeline_text(&n.timeline)?.into_bytes(),
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    // Publish the parent last, after complete nested files are present.
    files.push((
        output.clone(),
        crate::project::timeline_text(&result.timeline)?.into_bytes(),
    ));
    if !dry_run {
        if !allow_loss {
            result.report.ensure_lossless()?;
        }
        write_new(&files)?;
    }
    Ok(
        json!({"input":input,"output":output,"format":format.name(),"dry_run":dry_run,"written":!dry_run,
        "report":result.report,"has_losses":result.report.has_losses(),"timeline":result.timeline,
        "nested_files":files.iter().take(files.len()-1).map(|(p,_)|p).collect::<Vec<_>>(),"source_probe_issues":issues}),
    )
}
