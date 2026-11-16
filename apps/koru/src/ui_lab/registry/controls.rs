// SPDX-License-Identifier: MPL-2.0

//! **Controls**: the shared widgets of `koru-ui`, and the input support filed
//! beside the controls that use it. Each record was written from the module
//! it names; `registry/MANIFEST.md` lists what was read.

use super::*;

/// Every control record, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[BUTTON_ENTRY];
