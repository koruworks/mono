// SPDX-License-Identifier: MPL-2.0
//! Exact retained Schedule creation on the existing private Store owner.
use super::*;
use koru_ai_core::Cancellation;
use koru_delegate_document::{SCHEDULE_KIND, ScheduleDocument};
use koru_store_core::ports::{CreationOrigin, CreationOriginLookup};
use koru_store_server::WorkerStore;

fn cancelled(cancel: &Cancellation) -> Result<(), BotError> {
    if cancel.is_cancelled() {
        Err(BotError::Unavailable)
    } else {
        Ok(())
    }
}

pub(super) fn verify_definition(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotRoutineCreationIdentity,
    definition: &RoutineScheduleDefinition,
    epoch: Option<koru_delegate_core::HandoffId>,
    cancel: &Cancellation,
) -> Result<(), BotError> {
    identity.validate_digests()?;
    let route = identity.route();
    if !route.valid()
        || route.owner.user_scope != owner.user_scope
        || route.owner.device != owner.device
        || adapter.device != owner.device
        || definition.template.requested_by
            != (koru_delegate_core::ActorRef::Person { profile: reader })
        || definition.routine.as_ref().is_none_or(|a| {
            a.owner != route.owner
                || a.bot != route.bot
                || a.home != route.workspace
                || a.creation_request != route.creation_request
        })
    {
        return Err(BotError::Forbidden);
    }
    let owned = route.clone();
    let creation = identity.clone();
    let cancellation = cancel.clone();
    let (fork, origin) = adapter
        .client
        .with_store(move |store| {
            authority(store, owner, reader, owned.workspace, &cancellation)?;
            let document = koru_ai_core::DocumentId::from_uuid(owned.schedule.as_uuid());
            let origin = match store
                .bounded_creation_origin(owned.workspace, document)
                .map_err(|_| BotError::Unavailable)?
            {
                CreationOriginLookup::Known(origin) => origin,
                _ => return Err(BotError::Unavailable),
            };
            if origin.document != document
                || origin.scope != BOT_ROUTINE_SCOPE
                || origin.request_key != creation.store_request.to_string()
                || origin.request_hash != String::from(creation.seed_hash)
            {
                return Err(BotError::Conflict);
            }
            let entry = store
                .document_entry_live(owned.workspace, document)
                .map_err(|_| BotError::Unavailable)?
                .ok_or(BotError::NotFound)?;
            if entry.kind != SCHEDULE_KIND {
                return Err(BotError::Forbidden);
            }
            // A matching header elsewhere never aliases this physical candidate.
            let fork = store
                .fork_document(owned.workspace, document)
                .map_err(|_| BotError::Unavailable)?;
            Ok((fork, origin))
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancel)?;
    // Imports and validation run off the Store and AI writers.
    ScheduleDocument::verify_routine_seed(
        &origin.initial_update,
        &identity.seed.header,
        &identity.seed.name,
    )
    .map_err(|_| BotError::Conflict)?;
    let document = ScheduleDocument::adopt(fork).map_err(|_| BotError::Invalid)?;
    // Export is outside the Store closure. The import bound is encoded bytes,
    // not a claim about heap usage or cold document opening.
    let snapshot = document
        .export_snapshot()
        .map_err(|_| BotError::Unavailable)?;
    ScheduleDocument::verify_routine_occurrence_snapshot(
        &snapshot,
        route.schedule,
        definition,
        owner.device,
        epoch,
    )
    .map_err(|_| BotError::Conflict)?;
    let owned = route.clone();
    let cancellation = cancel.clone();
    adapter
        .client
        .with_store(move |store| authority(store, owner, reader, owned.workspace, &cancellation))
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancel)
}
fn authority(
    store: &mut WorkerStore,
    owner: BotOwner,
    reader: ProfileId,
    home: WorkspaceId,
    cancel: &Cancellation,
) -> Result<(), BotError> {
    cancelled(cancel)?;
    if store
        .local_user_scope()
        .map_err(|_| BotError::Unavailable)?
        != owner.user_scope
        || store
            .current_placement_profile(home)
            .map_err(|_| BotError::Unavailable)?
            != Some(reader)
    {
        return Err(BotError::Forbidden);
    }
    store.workspace(home).map_err(|_| BotError::Forbidden)?;
    store.profile(reader).map_err(|_| BotError::Forbidden)?;
    Ok(())
}

pub(super) fn prepare(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotRoutineCreationIdentity,
    cancel: &Cancellation,
) -> Result<PreparedRoutineDocument, BotError> {
    identity.validate_digests()?;
    if identity.owner.user_scope != owner.user_scope
        || identity.owner.device != owner.device
        || adapter.device != owner.device
    {
        return Err(BotError::Forbidden);
    }
    cancelled(cancel)?;
    let update = ScheduleDocument::routine_seed_update(&identity.seed.header, &identity.seed.name)
        .map_err(|_| BotError::Invalid)?;
    let owned = identity.clone();
    let cancellation = cancel.clone();
    let origin = adapter
        .client
        .with_store_announcing(
            move |store, announcements| -> Result<CreationOrigin, BotError> {
                authority(store, owner, reader, owned.home, &cancellation)?;
                let candidate = koru_ai_core::DocumentId::from_uuid(owned.schedule.as_uuid());
                let lookup = |store: &mut WorkerStore| {
                    store
                        .bounded_creation_origin(owned.home, candidate)
                        .map_err(|_| BotError::Unavailable)
                };
                match lookup(store)? {
                    CreationOriginLookup::Known(origin) => Ok(origin),
                    CreationOriginLookup::Unknown => Err(BotError::Unavailable),
                    CreationOriginLookup::Missing => {
                        cancelled(&cancellation)?;
                        let created = store.create_seeded_document_with_origin(
                            owned.home,
                            owned.seed.name.as_str(),
                            SCHEDULE_KIND,
                            None,
                            None,
                            BOT_ROUTINE_SCOPE,
                            &owned.store_request.to_string(),
                            &String::from(owned.seed_hash),
                            candidate,
                            update,
                            None,
                            koru_store_core::model::Timestamp::now(),
                        );
                        match created {
                            Ok(proven) => {
                                if let Some(delta) = proven.creation.manifest_delta {
                                    announcements.manifest(owned.home, delta);
                                }
                                Ok(proven.origin)
                            }
                            Err(_) => match lookup(store)? {
                                CreationOriginLookup::Known(origin) => Ok(origin),
                                _ => Err(BotError::Unavailable),
                            },
                        }
                    }
                }
            },
        )
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancel)?;
    if origin.document.as_uuid() != identity.schedule.as_uuid()
        || origin.scope != BOT_ROUTINE_SCOPE
        || origin.request_key != identity.store_request.to_string()
        || origin.request_hash != String::from(identity.seed_hash)
    {
        return Ok(PreparedRoutineDocument::Disagreement(
            BotRoutineCreationRefusal::OriginMismatch,
        ));
    }
    let original = match ScheduleDocument::verify_routine_seed(
        &origin.initial_update,
        &identity.seed.header,
        &identity.seed.name,
    ) {
        Ok(doc) => doc.version(),
        Err(_) => {
            return Ok(PreparedRoutineDocument::Disagreement(
                BotRoutineCreationRefusal::OriginMismatch,
            ));
        }
    };
    let owned = identity.clone();
    let cancellation = cancel.clone();
    let result = adapter
        .client
        .with_store(move |store| -> Result<PreparedRoutineDocument, BotError> {
            authority(store, owner, reader, owned.home, &cancellation)?;
            let candidate = koru_ai_core::DocumentId::from_uuid(owned.schedule.as_uuid());
            match store
                .document_entry_live(owned.home, candidate)
                .map_err(|_| BotError::Unavailable)?
            {
                Some(entry) if entry.kind == SCHEDULE_KIND => {
                    if entry.title != owned.seed.name.as_str() {
                        return Ok(PreparedRoutineDocument::Disagreement(
                            BotRoutineCreationRefusal::DocumentChanged,
                        ));
                    }
                }
                Some(_) => {
                    return Ok(PreparedRoutineDocument::Disagreement(
                        BotRoutineCreationRefusal::OriginMismatch,
                    ));
                }
                None => {
                    return Ok(PreparedRoutineDocument::Disagreement(
                        BotRoutineCreationRefusal::DocumentChanged,
                    ));
                }
            }
            let live = store
                .version(owned.home, candidate)
                .map_err(|_| BotError::Unavailable)?;
            // Encoded VV maps have no canonical order: compare semantic vectors.
            if live != original {
                return Ok(PreparedRoutineDocument::Disagreement(
                    BotRoutineCreationRefusal::DocumentChanged,
                ));
            }
            Ok(PreparedRoutineDocument::Confirmed(
                BotRoutineStoreConfirmation {
                    identity: owned,
                    origin_edit_revision: original.encode(),
                },
            ))
        })
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancel)?;
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests;
