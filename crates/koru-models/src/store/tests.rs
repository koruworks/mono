// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures;

struct Held(InstalledModel);

impl ModelLease for Held {
    fn installed(&self) -> &InstalledModel {
        &self.0
    }
}

fn installed() -> InstalledModel {
    let descriptor = fixtures::descriptor();
    let root = PathBuf::from("/cache").join(descriptor.path_components().join("/"));
    InstalledModel { descriptor, root }
}

#[test]
fn a_file_inside_a_model_may_not_leave_it() {
    let model = installed();
    assert!(model.file("encoder.onnx").unwrap().starts_with(&model.root));
    for bad in ["", "..", "../x", "./a", "/etc/passwd", "C:\\x", "a\\b"] {
        assert_eq!(
            model.file(bad).unwrap_err().code,
            ModelErrorCode::PathEscape,
            "{bad:?}"
        );
    }
}

/// A store holding one model and nothing else — enough to reach the engine
/// check without a disk.
struct OneModel;

impl ModelStore for OneModel {
    fn root(&self) -> &Path {
        Path::new("/cache")
    }

    fn installed(&self) -> Result<Vec<InstalledModel>, ModelError> {
        Ok(vec![installed()])
    }

    fn find(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Option<InstalledModel>, ModelError> {
        Ok(Some(installed()).filter(|m| m.id() == id && m.version() == version))
    }

    fn install(
        &self,
        _entry: &CatalogEntry,
        _keys: &TrustedKeys,
        _consent: &koru_voice_consent::Granted,
        _progress: &mut dyn FnMut(InstallProgress),
        _cancel: &AtomicBool,
    ) -> Result<InstalledModel, ModelError> {
        Err(ModelErrorCode::Offline.with("installs nothing"))
    }

    fn lease(
        &self,
        id: &ModelId,
        version: &ModelVersion,
    ) -> Result<Box<dyn ModelLease>, ModelError> {
        let model = self
            .find(id, version)?
            .ok_or_else(|| ModelErrorCode::NotInstalled.with(format!("{id}/{version}")))?;
        Ok(Box::new(Held(model)))
    }

    fn remove(&self, _id: &ModelId, _version: &ModelVersion) -> Result<(), ModelError> {
        Err(ModelErrorCode::Leased.with("removes nothing"))
    }

    fn prune(&self, _id: &ModelId, _keep: usize) -> Result<Vec<ModelVersion>, ModelError> {
        Ok(Vec::new())
    }
}

/// `engine` is signed so that verified bytes cannot reach the wrong native
/// parser; this is the check that makes the signature mean something.
#[test]
fn loading_refuses_a_model_built_for_another_engine() {
    let id = ModelId::parse("parakeet-tdt-v3").unwrap();
    let version = ModelVersion::parse("1.2.0").unwrap();
    LoadedModel::open(&OneModel, &id, &version, &Token::parse("sherpa").unwrap()).unwrap();
    let error = LoadedModel::open(&OneModel, &id, &version, &Token::parse("onnx-ocr").unwrap())
        .unwrap_err();
    assert_eq!(error.code, ModelErrorCode::EngineMismatch);
    assert!(anything_installed(&OneModel));
}

/// The load handle prints ids, never a path under somebody's home directory.
#[test]
fn the_load_handle_debugs_as_ids() {
    let loaded = LoadedModel::from_lease(Box::new(Held(installed())));
    let printed = format!("{loaded:?}");
    assert!(printed.contains("parakeet-tdt-v3"), "{printed}");
    assert!(!printed.contains("/cache"), "{printed}");
    assert_eq!(loaded.descriptor().engine.as_str(), "sherpa");
}
