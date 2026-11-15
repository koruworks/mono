// SPDX-License-Identifier: MPL-2.0
//! Live visibility precedes origin/content; retained identity is never authority.
use super::*;

pub(in crate::bots) fn read(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotRoutineCreationIdentity,
    cancel: &Cancellation,
) -> Result<RoutineOverviewObservation, BotError> {
    read_detail(adapter, owner, reader, identity, cancel).map(|observation| match observation {
        RoutineDetailObservation::Current {
            name,
            definition_revision,
            state,
            ..
        } => RoutineOverviewObservation::Current {
            name,
            definition_revision,
            state,
        },
        RoutineDetailObservation::NotLive => RoutineOverviewObservation::NotLive,
        RoutineDetailObservation::Unavailable => RoutineOverviewObservation::Unavailable,
    })
}

pub(in crate::bots) fn read_detail(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    identity: &BotRoutineCreationIdentity,
    cancel: &Cancellation,
) -> Result<RoutineDetailObservation, BotError> {
    identity.validate_digests()?;
    if identity.owner.user_scope != owner.user_scope
        || identity.owner.device != owner.device
        || adapter.device != owner.device
        || identity.seed.header.definition.template.requested_by
            != (koru_delegate_core::ActorRef::Person { profile: reader })
    {
        return Err(BotError::Forbidden);
    }
    cancelled(cancel)?;
    let owned = identity.clone();
    let cancellation = cancel.clone();
    let read = adapter
        .client
        .with_store(move |store| {
            authority(store, owner, reader, owned.home, &cancellation)?;
            let candidate = koru_ai_core::DocumentId::from_uuid(owned.schedule.as_uuid());
            let entry = store
                .document_entry_live(owned.home, candidate)
                .map_err(|_| BotError::Unavailable)?;
            let Some(entry) = entry else {
                return Ok(None);
            };
            if entry.kind != SCHEDULE_KIND {
                return Err(BotError::Invalid);
            }
            let origin = match store
                .bounded_creation_origin(owned.home, candidate)
                .map_err(|_| BotError::Unavailable)?
            {
                CreationOriginLookup::Known(origin) => origin,
                _ => return Err(BotError::Unavailable),
            };
            if origin.document != candidate
                || origin.scope != BOT_ROUTINE_SCOPE
                || origin.request_key != owned.store_request.to_string()
                || origin.request_hash != String::from(owned.seed_hash)
            {
                return Err(BotError::Invalid);
            }
            let fork = store
                .fork_document(owned.home, candidate)
                .map_err(|_| BotError::Unavailable)?;
            Ok(Some((fork, origin)))
        })
        .map_err(|_| BotError::Unavailable);
    let result = match read {
        Ok(Ok(None)) => Ok(RoutineDetailObservation::NotLive),
        Ok(Ok(Some((fork, origin)))) => {
            let projected = (|| {
                cancelled(cancel)?;
                let original = ScheduleDocument::verify_routine_seed(
                    &origin.initial_update,
                    &identity.seed.header,
                    &identity.seed.name,
                )
                .map_err(|_| BotError::Invalid)?;
                let doc = ScheduleDocument::adopt(fork).map_err(|_| BotError::Invalid)?;
                if !doc.version().includes_vv(&original.version()) {
                    return Err(BotError::Invalid);
                }
                let projected = doc.routine_detail().map_err(|_| BotError::Invalid)?;
                let current = projected.overview;
                let seed = &identity.seed.header;
                if current.schedule != identity.schedule
                    || current.created_at_ms != seed.created_at_ms
                    || current.initial_owner != seed.initial_owner
                    || Some(&current.association) != seed.definition.routine.as_ref()
                    || current.requested_by != seed.definition.template.requested_by
                {
                    return Err(BotError::Invalid);
                }
                Ok(RoutineDetailObservation::Current {
                    name: current.name,
                    definition_revision: current.definition_revision,
                    state: current.state,
                    effective_at_ms: projected.effective_at_ms,
                    recurrence: projected.recurrence,
                })
            })();
            Ok(projected.unwrap_or(RoutineDetailObservation::Unavailable))
        }
        Ok(Err(BotError::Forbidden)) => Err(BotError::Forbidden),
        _ => Ok(RoutineDetailObservation::Unavailable),
    };
    finish(adapter, owner, reader, identity.home, cancel, result)
}

fn finish<T>(
    adapter: &BotAuthorityAdapter,
    owner: BotOwner,
    reader: ProfileId,
    home: WorkspaceId,
    cancel: &Cancellation,
    result: Result<T, BotError>,
) -> Result<T, BotError> {
    // Recheck even after an item refusal: late owner/Profile drift must not be
    // mistaken for a trusted per-item observation, including NotLive.
    let cancellation = cancel.clone();
    adapter
        .client
        .with_store(move |store| authority(store, owner, reader, home, &cancellation))
        .map_err(|_| BotError::Unavailable)??;
    cancelled(cancel)?;
    result
}

#[cfg(test)]
mod tests;
