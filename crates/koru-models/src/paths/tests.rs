// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures;

/// Every path lands under the root the caller chose — the invariant that keeps
/// an override from writing outside the directory it named.
#[test]
fn every_path_lands_under_the_root_it_was_given() {
    let paths = ModelPaths::at(PathBuf::from("/cache"));
    let descriptor = fixtures::descriptor();
    for path in [
        paths.id_dir(&descriptor.id),
        paths.version_dir(&descriptor.id, &descriptor.version),
        paths.model_dir(&descriptor),
        paths.staging_dir(),
        paths.parts_dir(),
        paths.archive_path(&descriptor.digest),
        paths.install_lock(&descriptor.digest),
        paths.leases_dir(&descriptor.digest),
    ] {
        assert!(path.starts_with(paths.root()), "{}", path.display());
    }
}

/// `<id>/<version>/<digest>` — the layout voice's cache already has on disk.
#[test]
fn a_model_directory_is_id_then_version_then_digest() {
    let descriptor = fixtures::descriptor();
    assert_eq!(
        ModelPaths::at("/cache").model_dir(&descriptor),
        Path::new("/cache")
            .join("parakeet-tdt-v3")
            .join("1.2.0")
            .join(fixtures::ABC_SHA256)
    );
}

#[test]
fn no_model_id_can_name_an_internal_directory() {
    for name in [STAGING, PARTS, LOCKS, LEASES] {
        assert!(ModelPaths::is_internal(name));
        assert!(ModelId::parse(name).is_err(), "{name}");
    }
    assert!(!ModelPaths::is_internal("parakeet-tdt-v3"));
}
