// SPDX-License-Identifier: MPL-2.0

//! The file form of the inventory, under `disk-tests` because it opens a
//! real `catalog.db`: a fresh 13 written by the ordinary open is read back
//! read-only, a missing file is a refusal, and the read changes nothing —
//! the stamp and the row counts before and after are equal.
//!
//! `cargo test -p koru-store-sqlite --features disk-tests --lib inspection`

use super::*;
use crate::SqliteCatalog;
use koru_store_core::ports::Catalog;

#[test]
fn a_real_catalog_is_inspected_read_only_and_a_missing_file_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.db");
    assert_eq!(
        inspect_file(&path).unwrap_err().code(),
        Code::StorageFailed,
        "no file, no report"
    );
    let catalog = SqliteCatalog::open(&path).unwrap();
    let scope = catalog.local_user_scope().unwrap();
    drop(catalog);
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();

    let report = inspect_file(&path).unwrap();
    assert_eq!(report.schema_version, TO);
    assert_eq!(report.scope, scope.to_storage());
    assert!(report.profile_units.is_empty());
    assert!(report.workspace_units.is_empty());
    assert_eq!(report.foreign_scope_rows, 0);
    assert!(!report.account_bound);
    // Reading twice is the same answer, and the file was not written to.
    assert_eq!(inspect_file(&path).unwrap(), report);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before,
        "the inventory is a read"
    );
    // The ordinary open still works afterwards: nothing was locked or
    // left behind by the read-only connection.
    assert_eq!(
        SqliteCatalog::open(&path)
            .unwrap()
            .schema_version()
            .unwrap(),
        TO
    );
}
