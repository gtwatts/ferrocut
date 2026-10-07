//! Content hashes used as cache keys.

use std::fmt;

use crate::time::RationalTime;

/// Hash of a node's own parameters (not its inputs).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeHash(pub [u8; 32]);

/// Cache key for one rendered frame: Merkle hash of
/// `(node hash at t, t, frame keys of every input pulled at t)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameKey(pub [u8; 32]);

impl NodeHash {
    /// Hash a node kind plus a list of parameter byte strings.
    pub fn of(kind: &str, params: &[&[u8]]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"ferrocut.node.v1\0");
        h.update(kind.as_bytes());
        for p in params {
            h.update(&(p.len() as u64).to_le_bytes());
            h.update(p);
        }
        NodeHash(*h.finalize().as_bytes())
    }
}

impl FrameKey {
    pub fn compute(node: NodeHash, t: RationalTime, inputs: &[FrameKey]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"ferrocut.frame.v1\0");
        h.update(&node.0);
        h.update(&t.hash_bytes());
        h.update(&(inputs.len() as u64).to_le_bytes());
        for k in inputs {
            h.update(&k.0);
        }
        FrameKey(*h.finalize().as_bytes())
    }
}

fn short(b: &[u8; 32], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for x in &b[..6] {
        write!(f, "{x:02x}")?;
    }
    Ok(())
}
impl fmt::Display for NodeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        short(&self.0, f)
    }
}
impl fmt::Debug for NodeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        short(&self.0, f)
    }
}
impl fmt::Display for FrameKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        short(&self.0, f)
    }
}
impl fmt::Debug for FrameKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        short(&self.0, f)
    }
}
