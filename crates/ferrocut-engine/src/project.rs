//! Project operations on a timeline *file*: journaled edits, dry runs, undo,
//! log and branches. Shared by the `ferrocut` CLI and `ferrocut-mcp`.
//!
//! # Journal
//!
//! Every applied edit script appends one JSON line to `X.journal.jsonl`
//! beside the timeline `X.json` (append-only; never rewritten). An entry
//! records the ops (with their parameters), the per-op change summaries and
//! the timeline's canonical hash before and after. Entries carry a sequence
//! number and **no timestamps**, so the same edits produce byte-identical
//! journals. Hashes are blake3 over the timeline's canonical JSON
//! ([`timeline_hash`]: serde form with object keys sorted recursively, so
//! formatting and key order in the file don't matter).
//!
//! Every state the journal mentions is stored as a content-addressed snapshot
//! in `<dir>/.ferrocut/snapshots/<hash>.json`, which is what makes undo,
//! checkout and merge possible without reverse ops.
//!
//! # Undo
//!
//! [`undo`] reverts the newest not-yet-undone edit (or merge) on the current
//! branch: the file must still hash to that entry's `after` (otherwise it was
//! changed outside ferrocut, and undo refuses unless forced), the `before`
//! snapshot is restored, and an `undo` entry is appended. Repeated undos walk
//! further back. (There is no redo yet: re-apply the ops from the log.)
//!
//! # Branches
//!
//! Branches are named timeline snapshots tracked through the journal: the
//! current branch is the last checkout's target (initially `main`) and each
//! branch's tip is the latest state any of its entries produced. [`branch`]
//! names the current state, [`checkout`] swaps the file to a branch's tip, and
//! [`merge`] replays the other branch's live ops since it forked onto the
//! current timeline, atomically (a rebase-style merge: a conflict is an op
//! that no longer applies, and nothing is written).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail, ensure};
use ferrocut_core::RationalTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::diff::RenderImpact;
use crate::edit::{Change, EditOp, MediaLengths, apply};
use crate::timeline::Timeline;

pub const MAIN: &str = "main";

/// `v` with object keys sorted recursively (independent of serde_json's
/// `preserve_order` feature).
pub fn canonical_value(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut kv: Vec<(String, Value)> = m.into_iter().collect();
            kv.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                kv.into_iter()
                    .map(|(k, v)| (k, canonical_value(v)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.into_iter().map(canonical_value).collect()),
        v => v,
    }
}

/// blake3 hex of the timeline's canonical JSON.
pub fn timeline_hash(tl: &Timeline) -> String {
    let v = canonical_value(serde_json::to_value(tl).expect("timeline serializes"));
    blake3::hash(&serde_json::to_vec(&v).expect("json"))
        .to_hex()
        .to_string()
}

/// Parse a timeline file as written (relative sources stay relative).
pub fn read_timeline(path: &Path) -> anyhow::Result<Timeline> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Timeline::from_json(&text).with_context(|| format!("parsing timeline {}", path.display()))
}

/// The timeline as `Timeline::load` would see it if stored in `dir`.
pub fn resolved(tl: &Timeline, dir: &Path) -> Timeline {
    let mut t = tl.clone();
    t.resolve_sources(dir);
    t
}

/// Pretty JSON + newline: the on-disk form ferrocut writes.
pub fn timeline_text(tl: &Timeline) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(tl)? + "\n")
}

pub fn dir_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// A [`Change`] in owned, deserializable form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub op: usize,
    pub kind: String,
    pub summary: String,
    /// `[start, end)` timeline span whose output may change.
    pub span: (RationalTime, RationalTime),
}

impl From<&Change> for ChangeRecord {
    fn from(c: &Change) -> Self {
        ChangeRecord {
            op: c.op,
            kind: c.kind.to_string(),
            summary: c.summary.clone(),
            span: c.span,
        }
    }
}

/// One journal line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// An applied edit script.
    Edit {
        seq: u64,
        branch: String,
        ops: Vec<EditOp>,
        changes: Vec<ChangeRecord>,
        before: String,
        after: String,
    },
    /// `undid`'s `before` state was restored.
    Undo {
        seq: u64,
        branch: String,
        undid: u64,
        before: String,
        after: String,
    },
    /// Branch `name` created at state `at` while on `from`.
    Branch {
        seq: u64,
        name: String,
        from: String,
        at: String,
    },
    /// Switched from branch `from` to `to`; the file went `before` -> `after`.
    Checkout {
        seq: u64,
        from: String,
        to: String,
        before: String,
        after: String,
    },
    /// `ops` (branch `from`'s live ops since its fork) replayed onto `branch`.
    Merge {
        seq: u64,
        branch: String,
        from: String,
        ops: Vec<EditOp>,
        changes: Vec<ChangeRecord>,
        before: String,
        after: String,
    },
}

impl Entry {
    pub fn seq(&self) -> u64 {
        match self {
            Entry::Edit { seq, .. }
            | Entry::Undo { seq, .. }
            | Entry::Branch { seq, .. }
            | Entry::Checkout { seq, .. }
            | Entry::Merge { seq, .. } => *seq,
        }
    }
    /// One human-readable line.
    pub fn describe(&self) -> String {
        let h = |s: &str| s[..12.min(s.len())].to_string();
        match self {
            Entry::Edit {
                branch,
                ops,
                before,
                after,
                ..
            } => format!(
                "edit   [{branch}] {} op(s): {}  {} -> {}",
                ops.len(),
                ops.iter().map(EditOp::kind).collect::<Vec<_>>().join(", "),
                h(before),
                h(after)
            ),
            Entry::Undo {
                branch,
                undid,
                before,
                after,
                ..
            } => format!("undo   [{branch}] #{undid}  {} -> {}", h(before), h(after)),
            Entry::Branch { name, from, at, .. } => {
                format!("branch {name} (from {from}) at {}", h(at))
            }
            Entry::Checkout {
                from,
                to,
                before,
                after,
                ..
            } => format!("checkout {from} -> {to}  {} -> {}", h(before), h(after)),
            Entry::Merge {
                branch,
                from,
                ops,
                before,
                after,
                ..
            } => format!(
                "merge  [{branch}] <- {from}: {} op(s)  {} -> {}",
                ops.len(),
                h(before),
                h(after)
            ),
        }
    }
}

/// Journal state derived from the entries.
#[derive(Clone, Debug, Default, Serialize)]
pub struct State {
    pub branch: String,
    /// Latest known state per branch.
    pub tips: BTreeMap<String, String>,
    /// Seqs of edit/merge entries that were undone.
    pub undone: BTreeSet<u64>,
    /// Seq of each branch's `branch` entry.
    pub forks: BTreeMap<String, u64>,
}

impl State {
    fn from_entries(entries: &[Entry]) -> State {
        let mut s = State {
            branch: MAIN.to_string(),
            ..State::default()
        };
        for e in entries {
            match e {
                Entry::Edit { branch, after, .. } | Entry::Merge { branch, after, .. } => {
                    s.tips.insert(branch.clone(), after.clone());
                }
                Entry::Undo {
                    branch,
                    undid,
                    after,
                    ..
                } => {
                    s.undone.insert(*undid);
                    s.tips.insert(branch.clone(), after.clone());
                }
                Entry::Branch {
                    seq,
                    name,
                    from,
                    at,
                } => {
                    s.tips.entry(from.clone()).or_insert_with(|| at.clone());
                    s.tips.insert(name.clone(), at.clone());
                    s.forks.insert(name.clone(), *seq);
                }
                Entry::Checkout {
                    from,
                    to,
                    before,
                    after,
                    ..
                } => {
                    s.tips.entry(from.clone()).or_insert_with(|| before.clone());
                    s.tips.insert(to.clone(), after.clone());
                    s.branch = to.clone();
                }
            }
        }
        s
    }
}

/// The journal and snapshot store of one timeline file.
pub struct Journal {
    pub timeline: PathBuf,
    pub path: PathBuf,
    pub snapshots: PathBuf,
}

impl Journal {
    pub fn for_timeline(timeline: &Path) -> Journal {
        Journal {
            timeline: timeline.to_path_buf(),
            path: timeline.with_extension("journal.jsonl"),
            snapshots: dir_of(timeline).join(".ferrocut").join("snapshots"),
        }
    }

    pub fn entries(&self) -> anyhow::Result<Vec<Entry>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        text.lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l)
                    .with_context(|| format!("{} line {}", self.path.display(), i + 1))
            })
            .collect()
    }

    pub fn state(&self) -> anyhow::Result<(Vec<Entry>, State)> {
        let e = self.entries()?;
        let s = State::from_entries(&e);
        Ok((e, s))
    }

    fn next_seq(entries: &[Entry]) -> u64 {
        entries.last().map_or(1, |e| e.seq() + 1)
    }

    fn append(&self, e: &Entry) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(e)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        f.write_all(line.as_bytes())
            .with_context(|| format!("appending to {}", self.path.display()))
    }

    /// Store `tl` content-addressed; returns its hash.
    pub fn save_snapshot(&self, tl: &Timeline) -> anyhow::Result<String> {
        let h = timeline_hash(tl);
        let p = self.snapshots.join(format!("{h}.json"));
        if !p.exists() {
            std::fs::create_dir_all(&self.snapshots)
                .with_context(|| format!("creating {}", self.snapshots.display()))?;
            let tmp = p.with_extension("json.tmp");
            std::fs::write(&tmp, timeline_text(tl)?)?;
            std::fs::rename(&tmp, &p)?;
        }
        Ok(h)
    }

    pub fn load_snapshot(&self, hash: &str) -> anyhow::Result<Timeline> {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "bad snapshot hash {hash:?}"
        );
        let p = self.snapshots.join(format!("{hash}.json"));
        let tl = read_timeline(&p).with_context(|| format!("snapshot {hash} is missing"))?;
        ensure!(
            timeline_hash(&tl) == hash,
            "snapshot {} is corrupt",
            p.display()
        );
        Ok(tl)
    }
}

fn write_timeline(path: &Path, tl: &Timeline) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.ferrocut-tmp");
    std::fs::write(&tmp, timeline_text(tl)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[derive(Clone, Debug)]
pub struct EditOptions {
    /// Write here instead of editing the timeline in place.
    pub output: Option<PathBuf>,
    /// Apply and report, write nothing (no file, no journal).
    pub dry_run: bool,
    /// Bound trims/slips by probed media lengths (else unbounded).
    pub probe: bool,
    /// Append to the output's journal (default true).
    pub journal: bool,
    /// Compute which output chunks would re-render (hashes source media).
    pub plan: bool,
    /// Structure-only dry run that opens no media (sandbox pre-checks):
    /// unknown lengths, placeholder durations for `add_clip`.
    pub sources_only: bool,
}

impl Default for EditOptions {
    fn default() -> Self {
        EditOptions {
            output: None,
            dry_run: false,
            probe: true,
            journal: true,
            plan: false,
            sources_only: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EditOutcome {
    pub output: PathBuf,
    pub dry_run: bool,
    pub written: bool,
    pub changes: Vec<ChangeRecord>,
    pub before: String,
    pub after: String,
    /// Journal entry seq, if one was appended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal_seq: Option<u64>,
    /// Relative sources were made absolute (output in another directory).
    pub sources_absolutized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderImpact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_error: Option<String>,
    #[serde(skip)]
    pub timeline: Option<Timeline>,
}

enum Kind {
    Edit,
    Merge(String),
}

/// Apply `ops` to the timeline file (atomically), optionally writing and
/// journaling the result. See [`EditOptions`].
pub fn edit_file(
    timeline: &Path,
    ops: &[EditOp],
    opts: &EditOptions,
) -> anyhow::Result<EditOutcome> {
    edit_impl(timeline, ops, opts, Kind::Edit)
}

fn edit_impl(
    timeline: &Path,
    ops: &[EditOp],
    opts: &EditOptions,
    kind: Kind,
) -> anyhow::Result<EditOutcome> {
    let mut before_tl = read_timeline(timeline)?;
    let base = dir_of(timeline);
    let mut media = if opts.sources_only {
        MediaLengths::placeholder()
    } else if opts.probe {
        MediaLengths::new(&base, |p| crate::media::media_duration(p).ok().flatten()).with_info(
            |p| {
                let i = crate::media::probe(p)?;
                Ok(crate::edit::MediaFacts {
                    duration: i.duration,
                    has_video: i.has_video,
                    has_audio: i.has_audio,
                })
            },
        )
    } else {
        MediaLengths::unbounded()
    };
    let (mut new, changes) = apply(&before_tl, ops, &mut media)?;
    let output = opts
        .output
        .clone()
        .unwrap_or_else(|| timeline.to_path_buf());
    let out_dir = dir_of(&output);
    let moved = !same_dir(&out_dir, &base);
    if moved {
        before_tl.absolutize_sources(&base);
        new.absolutize_sources(&base);
    }
    let (before, after) = (timeline_hash(&before_tl), timeline_hash(&new));
    let (mut render, mut render_error) = (None, None);
    if opts.plan {
        match crate::diff::render_impact(&resolved(&before_tl, &base), &resolved(&new, &base)) {
            Ok(r) => render = Some(r),
            Err(e) => render_error = Some(format!("{e:#}")),
        }
    }
    let mut out = EditOutcome {
        output: output.clone(),
        dry_run: opts.dry_run,
        written: false,
        changes: changes.iter().map(ChangeRecord::from).collect(),
        before,
        after,
        journal_seq: None,
        sources_absolutized: moved,
        render,
        render_error,
        timeline: None,
    };
    if !opts.dry_run {
        let entry_seq = if opts.journal {
            let j = Journal::for_timeline(&output);
            let (entries, state) = j.state()?;
            j.save_snapshot(&before_tl)?;
            j.save_snapshot(&new)?;
            let seq = Journal::next_seq(&entries);
            write_timeline(&output, &new)?;
            let (b, a) = (out.before.clone(), out.after.clone());
            j.append(&match kind {
                Kind::Edit => Entry::Edit {
                    seq,
                    branch: state.branch,
                    ops: ops.to_vec(),
                    changes: out.changes.clone(),
                    before: b,
                    after: a,
                },
                Kind::Merge(from) => Entry::Merge {
                    seq,
                    branch: state.branch,
                    from,
                    ops: ops.to_vec(),
                    changes: out.changes.clone(),
                    before: b,
                    after: a,
                },
            })?;
            Some(seq)
        } else {
            write_timeline(&output, &new)?;
            None
        };
        out.written = true;
        out.journal_seq = entry_seq;
    }
    out.timeline = Some(new);
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct UndoOutcome {
    pub undid: u64,
    pub seq: u64,
    pub branch: String,
    /// Hash of the state that was replaced.
    pub before: String,
    /// Hash of the restored state.
    pub after: String,
    pub forced: bool,
}

/// Undo the newest live edit/merge on the current branch (see module docs).
pub fn undo(timeline: &Path, force: bool) -> anyhow::Result<UndoOutcome> {
    let j = Journal::for_timeline(timeline);
    let (entries, state) = j.state()?;
    let current = timeline_hash(&read_timeline(timeline)?);
    let fork = state.forks.get(&state.branch).copied().unwrap_or(0);
    let target = entries.iter().rev().find_map(|e| match e {
        Entry::Edit {
            seq,
            branch,
            before,
            after,
            ..
        }
        | Entry::Merge {
            seq,
            branch,
            before,
            after,
            ..
        } if *branch == state.branch && *seq > fork && !state.undone.contains(seq) => {
            Some((*seq, before.clone(), after.clone()))
        }
        _ => None,
    });
    let Some((undid, before, after)) = target else {
        bail!("nothing to undo on branch {:?}", state.branch);
    };
    let forced = current != after;
    if forced && !force {
        bail!(
            "{} changed since journal entry #{undid} (hash {} != {}); undo would discard that change: re-run with force to undo anyway",
            timeline.display(),
            &current[..12],
            &after[..12]
        );
    }
    let restored = j.load_snapshot(&before)?;
    // Keep the replaced state recoverable.
    j.save_snapshot(&read_timeline(timeline)?)?;
    write_timeline(timeline, &restored)?;
    let seq = Journal::next_seq(&entries);
    j.append(&Entry::Undo {
        seq,
        branch: state.branch.clone(),
        undid,
        before: current.clone(),
        after: before.clone(),
    })?;
    Ok(UndoOutcome {
        undid,
        seq,
        branch: state.branch,
        before: current,
        after: before,
        forced,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    #[serde(flatten)]
    pub entry: Entry,
    /// Edit/merge entries only: undone later.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub undone: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Log {
    pub journal: PathBuf,
    pub branch: String,
    /// Canonical hash of the timeline file now.
    pub current: String,
    /// The file matches the current branch's journaled tip.
    pub clean: bool,
    pub tips: BTreeMap<String, String>,
    pub entries: Vec<LogEntry>,
}

pub fn log(timeline: &Path) -> anyhow::Result<Log> {
    let j = Journal::for_timeline(timeline);
    let (entries, state) = j.state()?;
    let current = timeline_hash(&read_timeline(timeline)?);
    let clean = state.tips.get(&state.branch).is_none_or(|t| *t == current);
    Ok(Log {
        journal: j.path.clone(),
        branch: state.branch.clone(),
        current,
        clean,
        tips: state.tips.clone(),
        entries: entries
            .into_iter()
            .map(|e| {
                let undone = state.undone.contains(&e.seq())
                    && matches!(e, Entry::Edit { .. } | Entry::Merge { .. });
                LogEntry { entry: e, undone }
            })
            .collect(),
    })
}

fn check_name(name: &str) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b)),
        "branch names are 1-64 chars of [A-Za-z0-9-_./], got {name:?}"
    );
    Ok(())
}

/// Create branch `name` at the timeline's current state (stays on the
/// current branch). Returns the entry.
pub fn branch(timeline: &Path, name: &str) -> anyhow::Result<Entry> {
    check_name(name)?;
    let j = Journal::for_timeline(timeline);
    let (entries, state) = j.state()?;
    ensure!(
        name != MAIN && !state.tips.contains_key(name) && !state.forks.contains_key(name),
        "branch {name:?} already exists"
    );
    let at = j.save_snapshot(&read_timeline(timeline)?)?;
    let e = Entry::Branch {
        seq: Journal::next_seq(&entries),
        name: name.to_string(),
        from: state.branch,
        at,
    };
    j.append(&e)?;
    Ok(e)
}

/// Switch the timeline file to branch `name`'s tip.
pub fn checkout(timeline: &Path, name: &str, force: bool) -> anyhow::Result<Entry> {
    let j = Journal::for_timeline(timeline);
    let (entries, state) = j.state()?;
    ensure!(name != state.branch, "already on branch {name:?}");
    let cur_tl = read_timeline(timeline)?;
    let current = timeline_hash(&cur_tl);
    let Some(tip) = state.tips.get(name).cloned() else {
        bail!(
            "no branch {name:?} (known: {:?})",
            state.tips.keys().collect::<Vec<_>>()
        );
    };
    if let Some(t) = state.tips.get(&state.branch)
        && *t != current
        && !force
    {
        bail!(
            "{} has changes not in the journal (hash {} != branch {:?} tip {}); checkout would discard them: re-run with force",
            timeline.display(),
            &current[..12],
            state.branch,
            &t[..12]
        );
    }
    j.save_snapshot(&cur_tl)?;
    let restored = j.load_snapshot(&tip)?;
    write_timeline(timeline, &restored)?;
    let e = Entry::Checkout {
        seq: Journal::next_seq(&entries),
        from: state.branch,
        to: name.to_string(),
        before: current,
        after: tip,
    };
    j.append(&e)?;
    Ok(e)
}

/// Replay branch `name`'s live ops since it forked onto the current branch.
pub fn merge(timeline: &Path, name: &str, opts: &EditOptions) -> anyhow::Result<EditOutcome> {
    let j = Journal::for_timeline(timeline);
    let (entries, state) = j.state()?;
    ensure!(
        name != state.branch,
        "cannot merge branch {name:?} into itself"
    );
    ensure!(state.tips.contains_key(name), "no branch {name:?}");
    let fork = state.forks.get(name).copied().unwrap_or(0);
    let ops: Vec<EditOp> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Edit {
                seq, branch, ops, ..
            }
            | Entry::Merge {
                seq, branch, ops, ..
            } if branch == name && *seq > fork && !state.undone.contains(seq) => Some(ops.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    ensure!(!ops.is_empty(), "branch {name:?} has no edits to merge");
    let opts = EditOptions {
        output: None,
        journal: true,
        ..opts.clone()
    };
    edit_impl(timeline, &ops, &opts, Kind::Merge(name.to_string())).with_context(|| {
        format!(
            "replaying {} op(s) from {name:?} (nothing written)",
            ops.len()
        )
    })
}
