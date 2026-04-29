// Word-based stable anchors with duplicate-line disambiguation.
//
// - [`anchors`] — word dictionary + FNV-1a hash → deterministic word mapping
// - `state` (issue #11) — per-file anchor state with collision disambiguation

pub mod anchors;
