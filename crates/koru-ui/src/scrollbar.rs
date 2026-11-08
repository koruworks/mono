// SPDX-License-Identifier: MPL-2.0

//! One overlay scrollbar shared by document and navigation views.
//!
//! The track is a hit target, not paint: only the rounded thumb is drawn. The
//! gutter is wider than the thumb so it remains easy to grab without making the
//! interface wear a heavier bar.
//!
//! The bar itself is `koru_draw::scrollbar`, where the element layer's
//! scroll viewport draws it too (QUESTIONS M1): one geometry, one thumb.

pub use koru_draw::scrollbar::{ScrollbarGeom, draw, draw_thumb, draw_thumb_in};
