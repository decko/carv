// Word-based stable anchors with duplicate-line disambiguation.
//
// - [`anchors`] — word dictionary + FNV-1a hash → deterministic word mapping
// - [`state`]   — per-file anchor cache with occurrence-index disambiguation

pub mod anchors;
pub mod state;
