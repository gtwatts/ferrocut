//! Nested compositions (After Effects precomps / Premiere nested sequences).
//!
//! A clip whose `source` is a timeline file (`*.json`) shows that timeline:
//! its picture is the inner timeline's composite and its linked audio the
//! inner mix (bus gains, ducking and master gain applied; loudness
//! normalization only happens on the outermost timeline). The clip's
//! `source_in`, `speed` / `time_remap`, opacity, transform and blend mode
//! work as for media; source time is inner timeline time.
//!
//! The inner timeline is compiled into the same render graph, so frame keys
//! compose: an outer frame's key includes the inner frame's key, and editing
//! the inner timeline re-renders only the outer chunks whose inner frames
//! changed. Comps can nest to any depth; a cycle is an error.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use ferrocut_core::RationalTime;

use crate::timeline::Timeline;

/// Is `path` a nested timeline (by extension)?
pub fn is_comp(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The stack of comps being expanded (outermost first), for cycle detection.
#[derive(Clone, Debug, Default)]
pub struct CompStack(Vec<PathBuf>);

impl CompStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the comp at `path` (sources resolved, validated), refusing a
    /// cycle. Returns its canonical path for [`Self::push`].
    pub fn load(&self, path: &Path) -> anyhow::Result<(PathBuf, Timeline)> {
        let key = canonical(path);
        if let Some(i) = self.0.iter().position(|p| *p == key) {
            let chain: Vec<String> = self.0[i..]
                .iter()
                .chain(std::iter::once(&key))
                .map(|p| p.display().to_string())
                .collect();
            bail!("nested composition cycle: {}", chain.join(" -> "));
        }
        ensure!(
            self.0.len() < 64,
            "nested compositions deeper than 64 levels at {}",
            path.display()
        );
        let tl = Timeline::load(path)
            .with_context(|| format!("nested composition {}", path.display()))?;
        Ok((key, tl))
    }

    pub fn push(&mut self, key: PathBuf) {
        self.0.push(key);
    }

    pub fn pop(&mut self) {
        self.0.pop();
    }
}

/// Media length of a clip source: a comp's duration, else the media file's.
pub fn source_duration(path: &Path) -> anyhow::Result<Option<RationalTime>> {
    if is_comp(path) {
        let tl = Timeline::load(path)?;
        Ok(Some(tl.duration()))
    } else {
        crate::media::media_duration(path)
    }
}

/// Stream facts of a clip source (comps: video always; audio if anything
/// inside could carry audio, probing nested sources).
pub fn source_facts(path: &Path) -> anyhow::Result<crate::edit::MediaFacts> {
    if is_comp(path) {
        let tl = Timeline::load(path)?;
        let mut stack = CompStack::new();
        stack.push(canonical(path));
        Ok(crate::edit::MediaFacts {
            duration: Some(tl.duration()),
            has_video: true,
            has_audio: has_audio(&tl, &mut stack)?,
            size: Some((tl.output.width, tl.output.height)),
        })
    } else {
        let i = crate::media::probe(path)?;
        Ok(crate::edit::MediaFacts {
            duration: i.duration,
            has_video: i.has_video,
            has_audio: i.has_audio,
            size: i.width.zip(i.height),
        })
    }
}

/// Does the timeline have any audio (audio-track clips, or unmuted video
/// clips whose source, or nested comp, has audio)? Probes sources.
pub fn has_audio(tl: &Timeline, stack: &mut CompStack) -> anyhow::Result<bool> {
    if tl.audio_tracks.iter().any(|t| !t.clips.is_empty()) {
        return Ok(true);
    }
    let mut seen = std::collections::HashSet::new();
    for c in tl.tracks.iter().flat_map(|t| &t.clips) {
        if c.audio.mute || c.is_generator() || !seen.insert(c.source.clone()) {
            continue;
        }
        let yes = if is_comp(&c.source) {
            let (key, inner) = stack.load(&c.source)?;
            stack.push(key);
            let r = has_audio(&inner, stack);
            stack.pop();
            r?
        } else {
            crate::media::probe(&c.source)?.has_audio
        };
        if yes {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Visit every comp reachable from `tl` (each once, depth first) with its
/// loaded timeline. Errors on cycles and unreadable comps; comp files that
/// don't exist are skipped.
pub fn visit(
    tl: &Timeline,
    f: &mut dyn FnMut(&Path, &Timeline) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    fn go(
        tl: &Timeline,
        stack: &mut CompStack,
        seen: &mut std::collections::HashSet<PathBuf>,
        f: &mut dyn FnMut(&Path, &Timeline) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let comps = tl
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| &c.source))
            .chain(
                tl.audio_tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| &c.source)),
            )
            .filter(|s| is_comp(s));
        for s in comps {
            if !s.exists() {
                // Not created yet (a dry-run `nest`); compiling reports it.
                continue;
            }
            let (key, inner) = stack.load(s)?;
            if !seen.insert(key.clone()) {
                continue;
            }
            f(s, &inner)?;
            stack.push(key);
            let r = go(&inner, stack, seen, f);
            stack.pop();
            r?;
        }
        Ok(())
    }
    go(tl, &mut CompStack::new(), &mut Default::default(), f)
}
