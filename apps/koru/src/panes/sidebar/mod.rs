// SPDX-License-Identifier: MPL-2.0

//! The sidebar's composition: lane · apps row · nav · floor
//! (`docs/features/workspace-foundation/TECHNICAL.final.md` §5, §6).
//!
//! The explore view still owns the column — its rows, its popover, its
//! accessibility tree — and reads these modules for the bands it composes
//! around the nav. Each module is geometry in one function, read by drawing
//! and hit-testing alike, with no `TextEngine` on the hit path: label widths
//! are measured at draw and cached, the way the tab strip's are.

pub mod apps_row;
pub mod floor;
