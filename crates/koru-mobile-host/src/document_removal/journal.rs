// SPDX-License-Identifier: MPL-2.0
//! Machine-local retained source. Never an old-id Store or network update.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retained {
    format: u8,
    workspace: String,
    document: String,
    pub snapshot: Vec<u8>,
    pub fence: Vec<u8>,
    pub quiesced: bool,
    pub applied: Vec<String>,
    #[serde(default)]
    pub decision: Option<Decision>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum Decision {
    Delete,
    Keep {
        candidate: String,
        title: String,
        kind: String,
        key: String,
    },
}

impl Decision {
    pub fn candidate(&self) -> Result<Option<DocumentId>, String> {
        match self {
            Self::Delete => Ok(None),
            Self::Keep { candidate, .. } => {
                DocumentId::parse(candidate).map(Some).ok_or_else(invalid)
            }
        }
    }
}

fn path(root: &Path, document: DocumentId) -> PathBuf {
    root.join("mobile-purge-copies")
        .join(format!("{}.json", document.to_storage()))
}

pub(super) fn load<C: Catalog, B: Backend>(
    root: &Path,
    store: &mut Store<C, B>,
    workspace: WorkspaceId,
    document: DocumentId,
    at: Timestamp,
) -> Result<Option<Retained>, String> {
    let retained = match std::fs::read(path(root, document)) {
        Ok(bytes) => {
            let retained: Retained = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if retained.format != 1
                || retained.workspace != workspace.to_storage()
                || retained.document != document.to_storage()
            {
                return Err(invalid());
            }
            if let Some(decision) = &retained.decision {
                if !retained.quiesced || decision.candidate()? == Some(document) {
                    return Err(invalid());
                }
            }
            let fence = loro::VersionVector::decode(&retained.fence).map_err(|_| invalid())?;
            if !model(&retained.snapshot)?.version().includes_vv(&fence) {
                return Err(invalid());
            }
            Some(retained)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(invalid()),
    };
    let snapshot = match store.freeze_document_purge(workspace, document, at) {
        Ok(snapshot) => snapshot,
        Err(error) if error.code() == koru_store_core::errors::Code::DocumentNotFound => {
            // A recorded decision may have removed the body before a crash
            // left its location row behind. Only Store can prove the frozen
            // marker plus absent manifest/body and finish that release.
            let retained = retained.ok_or_else(invalid)?;
            let decision = retained.decision.as_ref().ok_or_else(invalid)?;
            let fence = loro::VersionVector::decode(&retained.fence).map_err(|_| invalid())?;
            db(store.release_document_purge(
                workspace,
                document,
                &fence,
                decision.candidate()?,
                at,
            ))?;
            remove(root, document);
            return Ok(None);
        }
        Err(error) => return Err(error.code().as_str().to_owned()),
    };
    let fence = model(&snapshot)?.version().encode();
    match retained {
        Some(retained) => {
            if retained.fence != fence {
                return Err(invalid());
            }
            Ok(Some(retained))
        }
        None => {
            let retained = Retained {
                format: 1,
                workspace: workspace.to_storage(),
                document: document.to_storage(),
                snapshot,
                fence,
                quiesced: false,
                applied: Vec::new(),
                decision: None,
            };
            save(root, document, &retained)?;
            Ok(Some(retained))
        }
    }
}

pub(super) fn save(root: &Path, document: DocumentId, retained: &Retained) -> Result<(), String> {
    use std::io::Write;
    let destination = path(root, document);
    let directory = destination.parent().ok_or_else(invalid)?;
    std::fs::create_dir_all(directory).map_err(|_| invalid())?;
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let bytes = serde_json::to_vec(retained).map_err(|_| invalid())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| invalid())?;
        file.write_all(&bytes).map_err(|_| invalid())?;
        file.sync_all().map_err(|_| invalid())?;
        drop(file);
        std::fs::rename(&temporary, &destination).map_err(|_| invalid())?;
        #[cfg(unix)]
        {
            std::fs::File::open(directory)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| invalid())?;
            // The retained directory itself may have been created this turn.
            std::fs::File::open(root)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| invalid())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

pub(super) fn remove(root: &Path, document: DocumentId) {
    let _ = std::fs::remove_file(path(root, document));
}
