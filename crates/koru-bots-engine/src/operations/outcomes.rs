// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{Database, messages::receipts};
use koru_delegate_core::NativeResultKind;
use rusqlite::{Connection, params};

pub(super) fn validate(record: &Record, outcome: &Outcome) -> Result<(), Error> {
    match outcome {
        Outcome::Applied { evidence, result } => {
            if !evidence.valid()
                || evidence.operation_id != record.key.operation
                || evidence.workspace != record.store.workspace
                || evidence.document != record.store.document
                || !matches!(
                    (record.store.kind, evidence.kind),
                    (
                        NativeEffectKind::CreateDocument,
                        NativeResultKind::CreatedPage
                    ) | (NativeEffectKind::EditDocument, NativeResultKind::EditedPage)
                )
                || encode(result)?.len() > koru_bot_client_wire::MAX_FRAME_BYTES / 2
            {
                return Err(Error::InvalidRequest);
            }
            koru_store_core::native_effect::same_native_edit_revision(
                &evidence.version,
                &evidence.version,
            )
            .map_err(|_| Error::InvalidRequest)?;
        }
        Outcome::Refused { stable_code } => {
            if stable_code.is_empty()
                || stable_code.len() > 64
                || !stable_code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(Error::InvalidRequest);
            }
        }
    }
    Ok(())
}

pub(super) fn save(connection: &Connection, record: &Record) -> Result<(), Error> {
    let state = match record.state {
        State::Recorded => "recorded",
        State::Leased => "leased",
        State::Applied => "applied",
        State::Refused => "refused",
        State::Unresolved => "unresolved",
    };
    let changed = connection.execute(
        "UPDATE operations SET state=?1,outcome=?2,recovery=?3 WHERE scope=?4 AND bot=?5 AND thread=?6 AND id=?7",
        params![state,record.outcome.as_ref().map(encode).transpose()?,encode(&record.recovery)?,record.key.scope,record.key.bot.to_string(),record.key.thread.to_string(),record.key.operation.to_string()])?;
    if changed != 1 {
        return Err(Error::StorageUnavailable);
    }
    Ok(())
}

/// Returns false for a contradictory observation. It is retained on the
/// operation, outside the prunable receipt answer, without changing its result.
pub(super) fn observe(
    connection: &Connection,
    record: &mut Record,
    outcome: Outcome,
) -> Result<bool, Error> {
    validate(record, &outcome)?;
    if record.state == State::Leased {
        record.state = match outcome {
            Outcome::Applied { .. } => State::Applied,
            Outcome::Refused { .. } => State::Refused,
        };
        record.outcome = Some(outcome);
        save(connection, record)?;
        if record.state == State::Applied {
            outputs::attach_if_published(connection, record)?;
        }
        return Ok(true);
    }
    if record.outcome.as_ref() == Some(&outcome) {
        return Ok(true);
    }
    if !record.recovery.observations.contains(&outcome) {
        record.recovery.observations.push(outcome);
        save(connection, record)?;
    }
    Ok(false)
}

impl Database {
    /// Only the authenticated private boundary channel may enter this method.
    /// Reporting is deliberately independent of the provider's current grant:
    /// Stop revokes execution, not the ability to record an already made effect.
    pub fn report_operation(
        &mut self,
        boundary: Uuid,
        command: Uuid,
        key: &Key,
        lease: &Lease,
        outcome: Outcome,
        now: i64,
    ) -> Result<Record, Error> {
        if !v4(boundary) || !v4(command) {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        let mut record = ledger::load(&tx, key)?.ok_or(Error::InvalidRequest)?;
        if lease.boundary != boundary || record.lease.as_ref() != Some(lease) {
            return Err(Error::LeaseMismatch);
        }
        validate(&record, &outcome)?;
        let subject = format!("boundary:{boundary}");
        let digest = receipts::digest("report-operation", &(key, lease, &outcome))?;
        let accepted =
            if let Some(accepted) = receipts::find::<bool>(&tx, &subject, command, &digest)? {
                accepted
            } else {
                let accepted = observe(&tx, &mut record, outcome)?;
                receipts::save(&tx, &subject, command, &digest, &accepted, now)?;
                accepted
            };
        tx.commit()?;
        if accepted {
            Ok(record)
        } else {
            Err(Error::OutcomeConflict)
        }
    }

    /// C1: the same live boundary has atomically abandoned its dispatch path.
    /// The command must be the original Acquire, not a later Pending retry.
    /// A restarted boundary is never permitted to manufacture this proof.
    pub fn execute_not_received(
        &mut self,
        boundary: Uuid,
        command: Uuid,
        key: &Key,
        acquire_command: Uuid,
        now: i64,
    ) -> Result<Record, Error> {
        let record = ledger::load(&self.connection, key)?.ok_or(Error::InvalidRequest)?;
        let lease = record.lease.ok_or(Error::LeaseMismatch)?;
        if lease.boundary != boundary || lease.acquire_command != acquire_command {
            return Err(Error::LeaseMismatch);
        }
        self.report_operation(
            boundary,
            command,
            key,
            &lease,
            Outcome::Refused {
                stable_code: "execute_not_received".into(),
            },
            now,
        )
    }
}
