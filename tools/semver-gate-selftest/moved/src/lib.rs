//! `find` needs `serde` now as well as `regex`. A `--features regex` build
//! loses it, and no other view can see that: the default and explicit-only
//! views lack `find` on both sides, and the all-features view has it on
//! both.
pub fn kept() {}
pub fn removed() {}
pub fn to_svg() {}
#[cfg(all(feature = "regex", feature = "serde"))]
pub fn find() {}
