// SPDX-License-Identifier: MPL-2.0

//! A stream observation is staged before any ordinary event cache is changed.
//! Source publication owns its two frontiers; a physical pump epoch is only a
//! current-producer fence and never permission to reset either frontier.

mod snapshot;
