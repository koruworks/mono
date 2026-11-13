// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::schema::genesis_root_id;
use koru_core::{DocumentId, WorkspaceDoc, WorkspaceId};

pub(in crate::bots) fn seed_hash(home: koru_ai_core::WorkspaceId, title: &str) -> Result<[u8; 32]> {
    if title.trim().is_empty() || title.len() > 256 || title.contains(['\r', '\n']) {
        return Err(MemoryCodecError::InvalidSeed);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"koru-bot-memory-seed/1\0");
    hash.update(home.as_uuid().as_bytes());
    hash.update(&(title.len() as u32).to_be_bytes());
    hash.update(title.as_bytes());
    Ok(*hash.finalize().as_bytes())
}

fn validate(identity: &BotMemoryIdentity) -> Result<()> {
    identity
        .validate()
        .map_err(|_| MemoryCodecError::InvalidSeed)?;
    if seed_hash(identity.workspace, &identity.seed_title)? != identity.seed_hash {
        return Err(MemoryCodecError::InvalidSeed);
    }
    Ok(())
}

pub(in crate::bots) fn seed_update(identity: &BotMemoryIdentity) -> Result<Vec<u8>> {
    validate(identity)?;
    let home = WorkspaceId::from_uuid(identity.workspace.as_uuid());
    let page = DocumentId::from_uuid(identity.candidate.as_uuid());
    let mut doc =
        WorkspaceDoc::page_genesis(home, page).map_err(|_| MemoryCodecError::InvalidSeed)?;
    doc.set_text(genesis_root_id(page), &identity.seed_title)
        .map_err(|_| MemoryCodecError::InvalidSeed)?;
    doc.validate_page_seed_v1(home, page, &identity.seed_title)
        .map_err(|_| MemoryCodecError::InvalidSeed)?;
    doc.export_all_updates()
        .map_err(|_| MemoryCodecError::InvalidSeed)
}

/// Return the original import's VV. Random title-edit peer bytes are not a seed
/// identity and must never be compared with a newly generated update's bytes.
pub(in crate::bots) fn validate_seed_update(
    identity: &BotMemoryIdentity,
    update: &[u8],
) -> Result<Vec<u8>> {
    Ok(validate_seed_document(identity, update)?.version().encode())
}

/// The adapter compares semantic VVs before producing one shared encoding for
/// the repository confirmation. Loro's map encoding order is not canonical.
pub(in crate::bots) fn validate_seed_document(
    identity: &BotMemoryIdentity,
    update: &[u8],
) -> Result<WorkspaceDoc> {
    validate(identity)?;
    let doc = bounded_import(update)?;
    let vv = doc.version().encode();
    doc.validate_page_seed_v1(
        WorkspaceId::from_uuid(identity.workspace.as_uuid()),
        DocumentId::from_uuid(identity.candidate.as_uuid()),
        &identity.seed_title,
    )
    .map_err(|_| MemoryCodecError::InvalidSeed)?;
    if vv.is_empty() || vv.len() > 64 * 1024 {
        return Err(MemoryCodecError::InvalidSeed);
    }
    Ok(doc)
}
