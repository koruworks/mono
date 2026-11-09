// SPDX-License-Identifier: MPL-2.0

//! `koru-store-server root-catalog`: the read-only inventory of one Store's
//! root catalog — units, overlays, personal rows, due rows, receipts and
//! join state, as ids and counts
//! (`docs/features/root-catalog/VERIFICATION.md`). It reads `catalog.db`
//! directly like `projection` and `search` do, opens it read-only and
//! never migrates it, so it is safe to run against a copied catalog before
//! the first 13 build has touched it: a 12 is reported as not yet migrated.

use std::path::Path;

use koru_store_core::errors::Result;

pub fn report_to_console(root: &Path) -> Result<()> {
    let path = root.join(crate::daemon::CATALOG_FILE);
    let report = koru_store_sqlite::catalog::inspect_root_catalog_file(&path)?;
    println!("{}", path.display());
    println!("{}", report.to_json()?);
    Ok(())
}
