// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{
    Database,
    authority::{Descriptor, Resolution, Secret, calls::resolve_on},
    messages::receipts,
};
use koru_delegate_core::{BotAccess, CapturedBotPolicy};
use koru_store_core::UserScopeId;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn load(connection: &Connection, key: &Key) -> Result<Option<Record>, Error> {
    let row: Option<(Vec<u8>, Option<Vec<u8>>, String, Option<Vec<u8>>, Option<Vec<u8>>)> = connection.query_row(
        "SELECT intent,lease,state,outcome,recovery FROM operations WHERE scope=?1 AND bot=?2 AND thread=?3 AND id=?4",
        params![key.scope,key.bot.to_string(),key.thread.to_string(),key.operation.to_string()],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).optional()?;
    row.map(|(intent, lease, state, outcome, recovery)| {
        let immutable: Immutable = decode(&intent)?;
        Ok(Record {
            key: key.clone(),
            prepared: immutable.prepared,
            store: immutable.store,
            output: immutable.output,
            recorded_call: immutable.recorded_call,
            state: State::parse(&state)?,
            lease: lease.as_deref().map(decode).transpose()?,
            outcome: outcome.as_deref().map(decode).transpose()?,
            recovery: recovery
                .as_deref()
                .map(decode)
                .transpose()?
                .unwrap_or_default(),
        })
    })
    .transpose()
}
fn matches_key(identity: &Resolution, key: &Key) -> Result<(), Error> {
    if identity.scope != key.scope
        || identity.bot != key.bot
        || identity.thread != key.thread
        || !v4(key.operation)
    {
        return Err(Error::NotAllowed);
    }
    Ok(())
}
fn authorize(
    connection: &Connection,
    identity: &Resolution,
    prepared: &Prepared,
) -> Result<(), Error> {
    let created = match prepared.document {
        None => false,
        Some(document) => connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE scope=?1 AND bot=?2 AND state='applied' AND json_extract(CAST(intent AS TEXT),'$.store.kind')='create_document' AND json_extract(CAST(intent AS TEXT),'$.store.workspace')=?3 AND json_extract(CAST(intent AS TEXT),'$.store.document')=?4)",
            params![identity.scope,identity.bot.to_string(),prepared.workspace.to_storage(),document.to_storage()], |row| row.get::<_,bool>(0),
        )?,
    };
    for grant in [&identity.captured_grant, &identity.current_grant] {
        let grant: CapturedBotPolicy =
            serde_json::from_value(grant.clone()).map_err(|_| Error::NotAllowed)?;
        if !grant.valid()
            || grant.workspace != prepared.workspace
            || !(prepared.kind == NativeEffectKind::CreateDocument
                || grant.access == BotAccess::Workspace
                || prepared
                    .document
                    .is_some_and(|document| grant.selected_pages.contains(&document))
                || created)
        {
            return Err(Error::NotAllowed);
        }
    }
    Ok(())
}
fn tool_matches(call: &Descriptor, prepared: &Prepared) -> Result<(), Error> {
    if matches!(
        (call.tool(), prepared.kind),
        ("create_document", NativeEffectKind::CreateDocument)
            | ("edit_document", NativeEffectKind::EditDocument)
    ) {
        Ok(())
    } else {
        Err(Error::InvalidRequest)
    }
}

impl Database {
    /// Gateway-only adapter: callers must authenticate the boundary before
    /// entering. Each call also resolves its live session/binding here.
    #[allow(clippy::too_many_arguments)]
    pub fn record_operation(
        &mut self,
        boundary: Uuid,
        command: Uuid,
        secret: &Secret,
        binding: Uuid,
        call: &Descriptor,
        operation: Uuid,
        prepared: Prepared,
        now: i64,
    ) -> Result<Record, Error> {
        if !v4(boundary) || !v4(command) || !v4(operation) {
            return Err(Error::InvalidRequest);
        }
        prepared.validate()?;
        tool_matches(call, &prepared)?;
        let tx = self.connection.transaction()?;
        let identity = resolve_on(&tx, secret, binding, call)?;
        authorize(&tx, &identity, &prepared)?;
        let key = Key {
            scope: identity.scope.clone(),
            bot: identity.bot,
            thread: identity.thread,
            operation,
        };
        let subject = format!("boundary:{boundary}");
        let digest = receipts::digest("record-operation", &(&key, binding, call, &prepared))?;
        let replayed = receipts::find::<Key>(&tx, &subject, command, &digest)?;
        let record = match load(&tx, &key)? {
            Some(record) => {
                if !record.prepared.same(&prepared) {
                    return Err(Error::OperationConflict);
                }
                record
            }
            None => {
                if replayed.is_some() {
                    return Err(Error::StorageUnavailable);
                }
                let owner = Uuid::parse_str(&key.scope).map_err(|_| Error::NotAllowed)?;
                let store = NativeEffectBinding {
                    user_scope: UserScopeId::from_uuid(owner),
                    scope: format!("bots:{}:{}", key.bot, key.thread),
                    request_key: Uuid::new_v4(),
                    workspace: prepared.workspace,
                    document: prepared.document.unwrap_or_else(DocumentId::new),
                    kind: prepared.kind,
                    request_fingerprint: prepared.fingerprint,
                };
                if !store.valid() {
                    return Err(Error::InvalidRequest);
                }
                let immutable = Immutable {
                    prepared: prepared.clone(),
                    store: store.clone(),
                    output: Uuid::new_v4(),
                    recorded_call: identity.binding,
                };
                tx.execute("INSERT INTO operations(scope,bot,thread,id,argument_digest,recorded_call,intent,state) VALUES (?1,?2,?3,?4,?5,?6,?7,'recorded')",
                    params![key.scope,key.bot.to_string(),key.thread.to_string(),key.operation.to_string(),prepared.fingerprint.as_slice(),binding.to_string(),encode(&immutable)?])?;
                Record {
                    key: key.clone(),
                    prepared,
                    store,
                    output: immutable.output,
                    recorded_call: identity.binding,
                    state: State::Recorded,
                    lease: None,
                    outcome: None,
                    recovery: Recovery::default(),
                }
            }
        };
        if let Some(saved) = replayed {
            if saved != key {
                return Err(Error::Conflict);
            }
        } else {
            receipts::save(&tx, &subject, command, &digest, &key, now)?;
        }
        tx.commit()?;
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn acquire_operation(
        &mut self,
        boundary: Uuid,
        command: Uuid,
        secret: &Secret,
        binding: Uuid,
        call: &Descriptor,
        key: &Key,
        fingerprint: [u8; 32],
        now: i64,
    ) -> Result<Admission, Error> {
        if !v4(boundary) || !v4(command) {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        let identity = resolve_on(&tx, secret, binding, call)?;
        matches_key(&identity, key)?;
        let mut record = load(&tx, key)?.ok_or(Error::InvalidRequest)?;
        tool_matches(call, &record.prepared)?;
        authorize(&tx, &identity, &record.prepared)?;
        if record.prepared.fingerprint != fingerprint {
            return Err(Error::OperationConflict);
        }
        let subject = format!("boundary:{boundary}");
        let digest = receipts::digest("acquire-operation", &(key, binding, call, fingerprint))?;
        let replayed = receipts::find::<bool>(&tx, &subject, command, &digest)?.is_some();
        if (record.state == State::Recorded) != record.lease.is_none() {
            return Err(Error::StorageUnavailable);
        }
        let execute = !replayed && record.state == State::Recorded && record.lease.is_none();
        if execute {
            let lease = Lease {
                id: Uuid::new_v4(),
                boundary,
                acquire_command: command,
                binding: identity.binding,
            };
            let changed = tx.execute("UPDATE operations SET state='leased',lease=?1 WHERE scope=?2 AND bot=?3 AND thread=?4 AND id=?5 AND state='recorded' AND lease IS NULL",
                params![encode(&lease)?,key.scope,key.bot.to_string(),key.thread.to_string(),key.operation.to_string()])?;
            if changed != 1 {
                return Err(Error::StorageUnavailable);
            }
            record.state = State::Leased;
            record.lease = Some(lease);
        }
        // Store only consumption, never an Execute response or capability.
        if !replayed {
            receipts::save(&tx, &subject, command, &digest, &true, now)?;
        }
        tx.commit()?;
        Ok(if execute {
            Admission::Execute(record)
        } else if matches!(
            record.state,
            State::Applied | State::Refused | State::Unresolved
        ) {
            Admission::Terminal(record)
        } else {
            Admission::Pending(record)
        })
    }

    pub fn get_operation(
        &mut self,
        secret: &Secret,
        binding: Uuid,
        call: &Descriptor,
        key: &Key,
    ) -> Result<Record, Error> {
        let tx = self.connection.transaction()?;
        let identity = resolve_on(&tx, secret, binding, call)?;
        matches_key(&identity, key)?;
        let record = load(&tx, key)?.ok_or(Error::InvalidRequest)?;
        authorize(&tx, &identity, &record.prepared)?;
        tx.commit()?;
        Ok(record)
    }
}
