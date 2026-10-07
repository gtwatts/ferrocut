//! Color-space tags and the alpha convention.

use std::sync::Arc;

/// OCIO color space name a frame is encoded in, e.g. `ACEScg` (the working space).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColorSpace(Arc<str>);

impl ColorSpace {
    pub const ACESCG: &'static str = "ACEScg";
    pub fn new(name: &str) -> Self {
        ColorSpace(Arc::from(name))
    }
    pub fn acescg() -> Self {
        Self::new(Self::ACESCG)
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// Frames are always premultiplied. Nodes that need straight alpha (OCIO
/// transforms, some OFX plugins) unpremultiply internally and re-premultiply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum AlphaMode {
    #[default]
    Premultiplied,
}
