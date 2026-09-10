//! `to_svg` moved behind a feature that is off by default. `--all-features`
//! never loses the item and cannot see it; the default view can.
pub fn kept() {}
pub fn removed() {}
#[cfg(feature = "render")]
pub fn to_svg() {}
