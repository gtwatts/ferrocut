//! Structural diff of two reports, grouped by chunk, so an agent can see
//! exactly what an edit changed (and that nothing else did).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::report::Report;

pub const DIFF_VERSION: &str = "ferrocut.perceive.diff/1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    /// JSON Pointer (RFC 6901) into the new report (into the chunk for chunk
    /// changes). Records in arrays (issues, events, cuts, spans, samples) are
    /// aligned by identity: an added record has `old: null`, a removed one
    /// `new: null` and its index in the old report.
    pub path: String,
    pub old: Value,
    pub new: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkDiff {
    pub index: usize,
    /// The engine re-rendered this chunk (its key changed).
    pub rerendered: bool,
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diff {
    pub schema_version: String,
    pub identical: bool,
    /// Chunks present in both reports whose analysis differs.
    pub changed_chunks: Vec<usize>,
    pub added_chunks: Vec<usize>,
    pub removed_chunks: Vec<usize>,
    /// Changes outside `chunks` (summary, shots, issues, audio, settings).
    pub timeline_changes: Vec<Change>,
    pub chunks: Vec<ChunkDiff>,
}

fn escape(k: &str) -> String {
    k.replace('~', "~0").replace('/', "~1")
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}/{}", escape(k));
                walk(
                    &p,
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.iter().chain(y).all(Value::is_object) => {
            // Records: align by identity so one inserted issue/event doesn't
            // shift every later element. Matched and added elements use the
            // new index, removed ones the old index.
            let (kx, ky): (Vec<String>, Vec<String>) = (
                x.iter().map(identity).collect(),
                y.iter().map(identity).collect(),
            );
            let pairs = lcs(&kx, &ky);
            let (mut i, mut j) = (0, 0);
            for (pi, pj) in pairs.into_iter().chain(std::iter::once((x.len(), y.len()))) {
                for (k, e) in x.iter().enumerate().take(pi).skip(i) {
                    out.push(Change {
                        path: format!("{path}/{k}"),
                        old: e.clone(),
                        new: Value::Null,
                    });
                }
                for (k, e) in y.iter().enumerate().take(pj).skip(j) {
                    out.push(Change {
                        path: format!("{path}/{k}"),
                        old: Value::Null,
                        new: e.clone(),
                    });
                }
                if pi < x.len() && pj < y.len() {
                    walk(&format!("{path}/{pj}"), &x[pi], &y[pj], out);
                }
                (i, j) = (pi + 1, pj + 1);
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            for i in 0..x.len().max(y.len()) {
                walk(
                    &format!("{path}/{i}"),
                    x.get(i).unwrap_or(&Value::Null),
                    y.get(i).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if a != b => out.push(Change {
            path: if path.is_empty() {
                "/".into()
            } else {
                path.into()
            },
            old: a.clone(),
            new: b.clone(),
        }),
        _ => {}
    }
}

/// Identity of a record in an array: what stays the same when its measured
/// values change (kind + position), so the diff reports a change in place.
fn identity(v: &Value) -> String {
    let get = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
    if v.get("index").is_some() {
        format!("index={}", get("index"))
    } else if v.get("kind").is_some() && v.get("frame").is_some() {
        format!("{}@{}..{}", get("kind"), get("frame"), get("end_frame"))
    } else if v.get("kind").is_some() {
        format!("{}:{}", get("kind"), get("message"))
    } else if v.get("frame").is_some() {
        format!("frame={}", get("frame"))
    } else if v.get("start").is_some() {
        format!("start={}", get("start"))
    } else {
        v.to_string()
    }
}

/// Longest common subsequence of equal keys: matched index pairs, ascending.
fn lcs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * (m + 1) + j] = if a[i] == b[j] {
                t[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                t[(i + 1) * (m + 1) + j].max(t[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if t[(i + 1) * (m + 1) + j] >= t[i * (m + 1) + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

pub fn diff_values(a: &Value, b: &Value) -> Vec<Change> {
    let mut v = Vec::new();
    walk("", a, b, &mut v);
    v
}

/// Diff two reports. Chunks are matched by index.
pub fn diff(a: &Report, b: &Report) -> Diff {
    let mut va = serde_json::to_value(a).expect("report serializes");
    let mut vb = serde_json::to_value(b).expect("report serializes");
    let take = |v: &mut Value| {
        v.as_object_mut()
            .and_then(|o| o.remove("chunks"))
            .unwrap_or(Value::Array(vec![]))
    };
    let (ca, cb) = (take(&mut va), take(&mut vb));
    let timeline_changes = diff_values(&va, &vb);
    let (ca, cb) = (
        ca.as_array().cloned().unwrap_or_default(),
        cb.as_array().cloned().unwrap_or_default(),
    );
    let mut chunks = Vec::new();
    for (i, (x, y)) in ca.iter().zip(&cb).enumerate() {
        let changes = diff_values(x, y);
        if !changes.is_empty() {
            chunks.push(ChunkDiff {
                index: i,
                rerendered: x.get("key") != y.get("key"),
                changes,
            });
        }
    }
    let changed_chunks: Vec<usize> = chunks.iter().map(|c| c.index).collect();
    let added_chunks: Vec<usize> = (ca.len()..cb.len()).collect();
    let removed_chunks: Vec<usize> = (cb.len()..ca.len()).collect();
    Diff {
        schema_version: DIFF_VERSION.into(),
        identical: timeline_changes.is_empty()
            && changed_chunks.is_empty()
            && added_chunks.is_empty()
            && removed_chunks.is_empty(),
        changed_chunks,
        added_chunks,
        removed_chunks,
        timeline_changes,
        chunks,
    }
}

impl Diff {
    /// Short human/agent-readable summary.
    pub fn summary(&self) -> String {
        if self.identical {
            return "identical\n".into();
        }
        let mut s = String::new();
        let list = |v: &[usize]| {
            v.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        s += &format!("changed chunks: [{}]\n", list(&self.changed_chunks));
        if !self.added_chunks.is_empty() || !self.removed_chunks.is_empty() {
            s += &format!(
                "added chunks: [{}], removed chunks: [{}]\n",
                list(&self.added_chunks),
                list(&self.removed_chunks)
            );
        }
        let show = |s: &mut String, prefix: &str, c: &Change| {
            *s += &format!("  {prefix}{}: {} -> {}\n", c.path, c.old, c.new);
        };
        for c in &self.chunks {
            s += &format!(
                "chunk {}{}: {} change(s)\n",
                c.index,
                if c.rerendered { " (re-rendered)" } else { "" },
                c.changes.len()
            );
            for ch in c.changes.iter().take(12) {
                show(&mut s, "", ch);
            }
            if c.changes.len() > 12 {
                s += &format!("  ... {} more\n", c.changes.len() - 12);
            }
        }
        if !self.timeline_changes.is_empty() {
            s += &format!("timeline: {} change(s)\n", self.timeline_changes.len());
            for ch in self.timeline_changes.iter().take(20) {
                show(&mut s, "", ch);
            }
            if self.timeline_changes.len() > 20 {
                s += &format!("  ... {} more\n", self.timeline_changes.len() - 20);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_align_by_identity() {
        let a = json!({"issues": [
            {"kind": "x", "frame": 1, "end_frame": 2, "message": "a"},
            {"kind": "y", "frame": 9, "end_frame": 10, "message": "b"}]});
        let b = json!({"issues": [
            {"kind": "x", "frame": 1, "end_frame": 2, "message": "a"},
            {"kind": "z", "frame": 5, "end_frame": 6, "message": "new"},
            {"kind": "y", "frame": 9, "end_frame": 10, "message": "b2"}]});
        let c = diff_values(&a, &b);
        assert_eq!(c.len(), 2, "{c:?}");
        assert_eq!(c[0].path, "/issues/1");
        assert_eq!(c[0].old, Value::Null);
        assert_eq!(
            c[1],
            Change {
                path: "/issues/2/message".into(),
                old: json!("b"),
                new: json!("b2")
            }
        );
    }

    #[test]
    fn pointer_paths() {
        let c = diff_values(
            &json!({"a": {"b/c": [1, 2]}, "x": 1}),
            &json!({"a": {"b/c": [1, 3, 4]}, "x": 1}),
        );
        assert_eq!(
            c,
            vec![
                Change {
                    path: "/a/b~1c/1".into(),
                    old: json!(2),
                    new: json!(3)
                },
                Change {
                    path: "/a/b~1c/2".into(),
                    old: Value::Null,
                    new: json!(4)
                },
            ]
        );
    }
}
