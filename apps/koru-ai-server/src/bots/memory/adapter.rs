// SPDX-License-Identifier: MPL-2.0

//! Store authority stays on its existing owner; decoding and projection stay
//! on the calling background lane. No imported body reaches the AI writer.
use super::{MemoryCodecError, SNAPSHOT_LIMIT, history, index, seed};
use crate::bots::BotAuthorityAdapter;
use koru_ai_core::bots::*;
use koru_ai_core::{Cancellation, ProfileId};
use koru_delegate_core::DocumentVersionId;
use koru_store_core::ports::{CreationOrigin, CreationOriginLookup, HistoryReason};
use koru_store_server::WorkerStore;

mod projection;
pub(in crate::bots) mod write;

type Result<T> = std::result::Result<T, MemoryAuthorityError>;

fn cancelled(cancellation: &Cancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(BotError::Unavailable.into())
    } else {
        Ok(())
    }
}

fn codec(error: MemoryCodecError) -> MemoryAuthorityError {
    match error {
        MemoryCodecError::UnsupportedDetail => MemoryAuthorityError::DetailUnsupported,
        MemoryCodecError::DetailTooLarge => MemoryAuthorityError::DetailTooLarge,
        MemoryCodecError::InvalidDetailPosition => BotError::Invalid.into(),
        MemoryCodecError::TooLarge(_) => BotError::ContextTooLarge.into(),
        _ => MemoryAuthorityError::HistoryUnavailable,
    }
}

fn store_error(error: koru_store_core::errors::Error) -> MemoryAuthorityError {
    use koru_store_core::errors::Code;
    match error.code() {
        Code::PayloadTooLarge => BotError::ContextTooLarge.into(),
        Code::CorruptState => MemoryAuthorityError::HistoryUnavailable,
        _ => BotError::Unavailable.into(),
    }
}

fn home_authority(
    store: &mut WorkerStore,
    owner: BotOwner,
    reader: ProfileId,
    home: koru_ai_core::WorkspaceId,
    cancellation: &Cancellation,
) -> Result<()> {
    cancelled(cancellation)?;
    if store.local_user_scope().map_err(store_error)? != owner.user_scope
        || store.current_placement_profile(home).map_err(store_error)? != Some(reader)
    {
        return Err(BotError::Forbidden.into());
    }
    store.workspace(home).map_err(store_error)?;
    store.profile(reader).map_err(store_error)?;
    Ok(())
}

fn live_authority(
    store: &mut WorkerStore,
    owner: BotOwner,
    reader: ProfileId,
    page: &BotPage,
    cancellation: &Cancellation,
) -> Result<()> {
    home_authority(store, owner, reader, page.workspace, cancellation)?;
    match store
        .document_entry_live(page.workspace, page.document)
        .map_err(store_error)?
    {
        Some(entry) if entry.kind == koru_core::explore::KIND_DOCUMENT => Ok(()),
        _ => Err(BotError::Forbidden.into()),
    }
}

fn device(adapter: &BotAuthorityAdapter, owner: BotOwner) -> Result<()> {
    if owner.device != adapter.device {
        return Err(BotError::Forbidden.into());
    }
    Ok(())
}

fn binding_authority(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    binding: &BotMemoryBinding,
) -> Result<()> {
    device(adapter, owner)?;
    if binding.owner != owner {
        return Err(BotError::Forbidden.into());
    }
    Ok(())
}

fn recheck(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    page: &BotPage,
    cancellation: &Cancellation,
) -> Result<()> {
    cancelled(cancellation)?;
    let page = page.clone();
    let cancel = cancellation.clone();
    adapter
        .client
        .with_store(move |store| live_authority(store, owner, reader, &page, &cancel))
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)
}

fn origin_matches(identity: &BotMemoryIdentity, origin: &CreationOrigin) -> bool {
    origin.document == identity.candidate
        && origin.scope == BOT_MEMORY_SCOPE
        && origin.request_key == identity.store_request.to_storage()
        && origin.request_hash
            == blake3::Hash::from_bytes(identity.seed_hash)
                .to_hex()
                .as_str()
}

pub(in crate::bots) fn prepare(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotMemoryIdentity,
    cancellation: &Cancellation,
) -> Result<PreparedMemoryPage> {
    device(adapter, owner)?;
    cancelled(cancellation)?;
    if identity.owner != owner {
        return Err(BotError::Forbidden.into());
    }
    let update = seed::seed_update(identity).map_err(|_| BotError::Invalid)?;
    let identity_for_store = identity.clone();
    let cancel = cancellation.clone();
    let origin = adapter
        .client
        .with_store_announcing(move |store, announcements| -> Result<CreationOrigin> {
            let identity = identity_for_store;
            home_authority(store, owner, reader, identity.workspace, &cancel)?;
            let lookup = |store: &mut WorkerStore| {
                store
                    .bounded_creation_origin(identity.workspace, identity.candidate)
                    .map_err(store_error)
            };
            match lookup(store)? {
                CreationOriginLookup::Known(origin) => Ok(origin),
                CreationOriginLookup::Unknown => Err(BotError::Unavailable.into()),
                CreationOriginLookup::Missing => {
                    cancelled(&cancel)?;
                    let created = store.create_seeded_document_with_origin(
                        identity.workspace,
                        &identity.seed_title,
                        koru_core::explore::KIND_DOCUMENT,
                        None,
                        None,
                        BOT_MEMORY_SCOPE,
                        &identity.store_request.to_storage(),
                        blake3::Hash::from_bytes(identity.seed_hash)
                            .to_hex()
                            .as_str(),
                        identity.candidate,
                        update,
                        None,
                        koru_store_core::model::Timestamp::now(),
                    );
                    match created {
                        Ok(proven) => {
                            if let Some(delta) = proven.creation.manifest_delta {
                                announcements.manifest(identity.workspace, delta);
                            }
                            Ok(proven.origin)
                        }
                        Err(error) => match lookup(store)? {
                            CreationOriginLookup::Known(origin) => Ok(origin),
                            CreationOriginLookup::Unknown => Err(BotError::Unavailable.into()),
                            CreationOriginLookup::Missing => {
                                Err(crate::bots::role_refusal(error.code()).into())
                            }
                        },
                    }
                }
            }
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    if !origin_matches(identity, &origin) {
        return Ok(PreparedMemoryPage::Disagreement(
            BotMemoryRefusal::OriginMismatch,
        ));
    }
    let original = match seed::validate_seed_document(identity, &origin.initial_update) {
        Ok(doc) => doc.version(),
        Err(_) => {
            return Ok(PreparedMemoryPage::Disagreement(
                BotMemoryRefusal::OriginMismatch,
            ));
        }
    };
    cancelled(cancellation)?;
    let identity = identity.clone();
    let cancel = cancellation.clone();
    let result = adapter
        .client
        .with_store(move |store| -> Result<PreparedMemoryPage> {
            home_authority(store, owner, reader, identity.workspace, &cancel)?;
            let placement = store
                .document_entry_live(identity.workspace, identity.candidate)
                .map_err(store_error)?;
            match placement {
                Some(entry) if entry.kind == koru_core::explore::KIND_DOCUMENT => {}
                Some(_) => {
                    return Ok(PreparedMemoryPage::Disagreement(
                        BotMemoryRefusal::OriginMismatch,
                    ));
                }
                None => {
                    let refusal = if store
                        .document_entry(identity.workspace, identity.candidate)
                        .map_err(store_error)?
                        .is_some()
                    {
                        BotMemoryRefusal::DocumentTrashed
                    } else {
                        // A retained origin never authorizes manifest resurrection.
                        BotMemoryRefusal::DocumentMoved
                    };
                    return Ok(PreparedMemoryPage::Disagreement(refusal));
                }
            }
            let live = store
                .version(identity.workspace, identity.candidate)
                .map_err(store_error)?;
            if live != original {
                return Ok(PreparedMemoryPage::Disagreement(
                    BotMemoryRefusal::DocumentChanged,
                ));
            }
            let revision = original.encode();
            Ok(PreparedMemoryPage::Confirmed(BotMemoryConfirmation {
                identity,
                original_edit_revision: revision.clone(),
                live_edit_revision: revision,
            }))
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    Ok(result)
}

fn exact(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    page: &BotPage,
    version: DocumentVersionId,
    cancellation: &Cancellation,
) -> Result<history::VerifiedMemory> {
    let evidence = exact_evidence(adapter, owner, reader, page, version, cancellation)?;
    let memory = history::validate(page.clone(), version, evidence).map_err(codec)?;
    recheck(adapter, owner, reader, page, cancellation)?;
    Ok(memory)
}

fn exact_evidence(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    page: &BotPage,
    version: DocumentVersionId,
    cancellation: &Cancellation,
) -> Result<koru_store_core::ports::BoundedDocumentVersion> {
    cancelled(cancellation)?;
    let owned_page = page.clone();
    let cancel = cancellation.clone();
    let evidence = adapter
        .client
        .with_store(move |store| {
            live_authority(store, owner, reader, &owned_page, &cancel)?;
            store
                .bounded_document_version(owned_page.workspace, version)
                .map_err(store_error)?
                .ok_or(MemoryAuthorityError::HistoryUnavailable)
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    recheck(adapter, owner, reader, page, cancellation)?;
    Ok(evidence)
}

fn current(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    page: &BotPage,
    cancellation: &Cancellation,
) -> Result<history::VerifiedMemory> {
    cancelled(cancellation)?;
    let owned_page = page.clone();
    let cancel = cancellation.clone();
    let fork = adapter
        .client
        .with_store(move |store| {
            live_authority(store, owner, reader, &owned_page, &cancel)?;
            store
                .fork_document(owned_page.workspace, owned_page.document)
                .map_err(store_error)
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    // The snapshot cap is post-export. It is not a bound on Loro's heap or on
    // opening a cold document; history reads have separate SQL allocation caps.
    let fork = koru_core::WorkspaceDoc::adopt(fork)
        .map_err(|_| MemoryAuthorityError::CaptureUnavailable)?;
    let snapshot = fork
        .export_snapshot()
        .map_err(|_| MemoryAuthorityError::CaptureUnavailable)?;
    let vv = fork.doc().oplog_vv();
    let frontiers = fork.doc().oplog_frontiers();
    let version_vector = vv.encode();
    let encoded_frontiers = frontiers.encode();
    if snapshot.len() > SNAPSHOT_LIMIT
        || version_vector.len() > 65536
        || encoded_frontiers.len() > 65536
    {
        return Err(BotError::ContextTooLarge.into());
    }
    cancelled(cancellation)?;
    let owned_page = page.clone();
    let cancel = cancellation.clone();
    let evidence = adapter
        .client
        .with_store(move |store| {
            live_authority(store, owner, reader, &owned_page, &cancel)?;
            let retained = store
                .record_history_state(
                    owned_page.workspace,
                    owned_page.document,
                    &snapshot,
                    &version_vector,
                    &encoded_frontiers,
                    HistoryReason::Manual,
                    koru_store_core::model::Timestamp::now(),
                )
                .map_err(store_error)?;
            // Deduplication alone does not validate retained bytes.
            store
                .bounded_document_version(owned_page.workspace, retained.version)
                .map_err(store_error)?
                .ok_or(MemoryAuthorityError::HistoryUnavailable)
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancellation)?;
    let version = DocumentVersionId::from_uuid(evidence.id.as_uuid());
    let memory = history::validate(page.clone(), version, evidence).map_err(codec)?;
    if memory.doc.doc().oplog_vv() != vv || memory.doc.doc().oplog_frontiers() != frontiers {
        return Err(MemoryAuthorityError::HistoryUnavailable);
    }
    recheck(adapter, owner, reader, page, cancellation)?;
    Ok(memory)
}

pub(in crate::bots) fn capture(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    binding: &BotMemoryBinding,
    cancellation: &Cancellation,
) -> Result<PreparedMemoryCapture> {
    binding_authority(adapter, owner, binding)?;
    let memory = current(adapter, owner, reader, &binding.page, cancellation)?;
    let index = index::encode_index(&memory.source).map_err(codec)?;
    let result = PreparedMemoryCapture {
        owner,
        bot: binding.bot,
        creation_request: binding.creation_request,
        page: binding.page.clone(),
        version: memory.source.version,
        nodes: index
            .nodes
            .iter()
            .map(|node| BotMemoryNodeId::from_uuid(node.as_uuid()))
            .collect(),
        omitted_count: index.omitted_count,
        invalid_count: index.invalid_count,
        index_digest: index.digest,
    };
    result.validate_for_binding(binding)?;
    recheck(adapter, owner, reader, &binding.page, cancellation)?;
    Ok(result)
}

pub(in crate::bots) fn replay(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    home: koru_ai_core::WorkspaceId,
    capture: &BotLibraryCapture,
    binding: Option<&BotMemoryBinding>,
    cancellation: &Cancellation,
) -> Result<VerifiedMemoryIndex> {
    device(adapter, owner)?;
    cancelled(cancellation)?;
    if capture.owner != owner {
        return Err(BotError::Forbidden.into());
    }
    capture.validate(home)?;
    let bytes = match &capture.memory {
        BotCapturedMemory::Absent {} => {
            // Captured absence stays absent even when a later binding exists.
            // Canonical capture/Run authority is checked by the worker.
            if capture.index_digest != *blake3::hash(&[]).as_bytes() {
                return Err(MemoryAuthorityError::CaptureUnavailable);
            }
            Vec::new()
        }
        BotCapturedMemory::Captured {
            page,
            version,
            creation_request,
            nodes,
            omitted_count,
            invalid_count,
        } => {
            let binding = binding.ok_or(MemoryAuthorityError::CaptureUnavailable)?;
            binding_authority(adapter, owner, binding)?;
            if binding.bot != capture.bot
                || binding.page != *page
                || binding.creation_request != *creation_request
            {
                return Err(MemoryAuthorityError::CaptureUnavailable);
            }
            let memory = exact(adapter, owner, reader, page, *version, cancellation)?;
            let index = index::encode_index(&memory.source).map_err(codec)?;
            projection::verify_index(
                &index,
                nodes,
                *omitted_count,
                *invalid_count,
                capture.index_digest,
            )?;
            recheck(adapter, owner, reader, page, cancellation)?;
            index.bytes
        }
    };
    cancelled(cancellation)?;
    Ok(VerifiedMemoryIndex {
        capture: capture.clone(),
        bytes,
    })
}

pub(in crate::bots) fn read(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    binding: &BotMemoryBinding,
    snapshot: MemorySnapshot,
    position: MemoryReadPosition,
    cancellation: &Cancellation,
) -> Result<MemoryPageResult> {
    binding_authority(adapter, owner, binding)?;
    if snapshot == MemorySnapshot::Current
        && !matches!(
            position,
            MemoryReadPosition::List { after: 0, .. }
                | MemoryReadPosition::Detail {
                    node_ordinal: 0,
                    start_utf8: 0,
                    ..
                }
        )
    {
        return Err(BotError::Invalid.into());
    }
    let memory = match snapshot {
        MemorySnapshot::Current => current(adapter, owner, reader, &binding.page, cancellation)?,
        MemorySnapshot::Exact(version) => {
            exact(adapter, owner, reader, &binding.page, version, cancellation)?
        }
    };
    cancelled(cancellation)?;
    let result = projection::page(&memory, position)?;
    recheck(adapter, owner, reader, &binding.page, cancellation)?;
    Ok(result)
}

#[cfg(test)]
mod tests;
