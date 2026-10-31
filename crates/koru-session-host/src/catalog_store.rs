// SPDX-License-Identifier: MPL-2.0

//! The model catalog's file, `<root>/agent/sessions/provider-models.json`.
//!
//! The runtime's `ProviderCatalogCache` decides what the file means; this
//! is only where its bytes live. The file is replaced whole through the same
//! [`write_atomic`](crate::projects::write_atomic) the project registry
//! uses, and never quarantined: a file that cannot be read is an empty
//! cache, rebuilt at the next write.

use koru_session_paths::AtomicPath;
use koru_session_runtime::providers::{CatalogStore, MAX_CATALOG_FILE_BYTES};

pub struct FileCatalogStore {
    path: AtomicPath,
}

impl FileCatalogStore {
    pub fn new(path: AtomicPath) -> Self {
        Self { path }
    }
}

impl CatalogStore for FileCatalogStore {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        let destination = self.path.destination();
        match std::fs::metadata(destination) {
            Ok(metadata) if metadata.len() > MAX_CATALOG_FILE_BYTES as u64 => {
                return Err("oversized".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.kind().to_string()),
        }
        std::fs::read(destination)
            .map(Some)
            .map_err(|error| error.kind().to_string())
    }

    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        crate::projects::write_atomic(&self.path, bytes).map_err(|error| error.kind().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_and_an_absent_or_oversized_one_is_no_cache() {
        let directory = tempfile::tempdir().unwrap();
        let paths =
            koru_session_paths::SessionPaths::from_resolved_directory(directory.path().into());
        let store = FileCatalogStore::new(paths.provider_models_atomic_path());
        assert_eq!(store.load(), Ok(None));
        store.save(b"{\"format\":1,\"providers\":{}}").unwrap();
        assert_eq!(
            store.load().unwrap().as_deref(),
            Some(&b"{\"format\":1,\"providers\":{}}"[..])
        );
        assert!(!paths.provider_models_atomic_path().staging().exists());
        std::fs::write(
            paths.provider_models_file(),
            vec![b' '; MAX_CATALOG_FILE_BYTES + 1],
        )
        .unwrap();
        assert_eq!(store.load(), Err("oversized".into()));
    }
}
