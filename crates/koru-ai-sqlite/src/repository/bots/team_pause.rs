// SPDX-License-Identifier: MPL-2.0

//! The team pause (M8, P8.1; ai.db 27): one row per person and computer.
//!
//! This row is the durable truth. The AI side refuses new work from the
//! commit of a row that says paused ([`team_paused_on`], read by the two
//! admission reads). The delegate holds a copy in memory for one run; the
//! worker sends [`BotTeamPauseRow::barrier`] and records the echo with
//! [`Repository::acknowledge_team_pause`]. No Bot row is read or written
//! here, and no Bot's revision moves.
//!
//! There is no receipts table. A repeat of the request that produced the
//! row's revision is answered from the row; the same id with other content
//! is `request_conflict`; anything else that names another revision is
//! `team_revision_conflict`, and the caller reads again.

use super::*;
use koru_delegate_core::BotTeamBarrier;
use rusqlite::TransactionBehavior;

/// The team pause row of one owner, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotTeamPauseRow {
    pub owner: BotOwner,
    pub paused: bool,
    /// The epoch sent to the delegate. 1 at the first change; never
    /// decreases.
    pub revision: u64,
    /// The request that produced this revision.
    pub request: BotRequestId,
    /// What that request asked for.
    pub fingerprint: String,
    pub stage: TeamPauseStage,
    pub changed_at_ms: i64,
    pub acknowledged_at_ms: Option<i64>,
}

impl BotTeamPauseRow {
    /// What a window is shown.
    pub fn view(&self) -> TeamPauseView {
        TeamPauseView {
            owner_device: self.owner.device,
            paused: self.paused,
            revision: self.revision,
            stage: self.stage,
            pending_since_ms: (self.stage == TeamPauseStage::Pending).then_some(self.changed_at_ms),
        }
    }

    /// What the delegate is sent, and what its echo must equal.
    pub fn barrier(&self) -> BotTeamBarrier {
        BotTeamBarrier {
            owner: delegate_owner(self.owner),
            epoch: self.revision,
            paused: self.paused,
            request: Some(self.request),
        }
    }
}

/// What a `PauseTeam` or `ResumeTeam` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamPauseChange {
    /// The row was written at the next revision, `pending`. The worker now
    /// sends its barrier.
    Changed(BotTeamPauseRow),
    /// The same request again: answered from the row, nothing written.
    Replayed(BotTeamPauseRow),
    /// The team was already in the asked state: nothing written. `None`
    /// when no row exists (a resume of a team that was never paused).
    Unchanged(Option<BotTeamPauseRow>),
}

impl TeamPauseChange {
    /// The team's state after the command, for its answer.
    pub fn view(&self, owner: BotOwner) -> TeamPauseView {
        match self {
            Self::Changed(row) | Self::Replayed(row) | Self::Unchanged(Some(row)) => row.view(),
            Self::Unchanged(None) => TeamPauseView::never_paused(owner.device),
        }
    }

    /// The row to send to the delegate, when the command wrote one.
    pub fn changed(&self) -> Option<&BotTeamPauseRow> {
        match self {
            Self::Changed(row) => Some(row),
            _ => None,
        }
    }
}

fn delegate_owner(owner: BotOwner) -> koru_delegate_core::BotOwner {
    koru_delegate_core::BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

/// The owner's row, bounded field by field; a malformed stored value is an
/// error, never read as "not paused".
fn row_on(conn: &Connection, owner: BotOwner) -> Result<Option<BotTeamPauseRow>, BotError> {
    type Raw = (
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
        bool,
    );
    let raw: Option<Raw> = conn
        .query_row(
            "SELECT
               CASE WHEN typeof(paused)='integer' AND paused IN (0,1) THEN paused END,
               CASE WHEN typeof(revision)='integer' AND revision>=1 THEN revision END,
               CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END,
               CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64 THEN fingerprint END,
               CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
               CASE WHEN typeof(changed_at_ms)='integer' THEN changed_at_ms END,
               CASE WHEN typeof(acknowledged_at_ms)='integer' THEN acknowledged_at_ms END,
               acknowledged_at_ms IS NULL
             FROM bot_team_pause WHERE user_scope_id=?1 AND owner_device_id=?2",
            params![owner.user_scope.to_storage(), owner.device.to_storage()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(bad)?;
    let Some((paused, revision, request, fingerprint, stage, changed, acknowledged, no_ack)) = raw
    else {
        return Ok(None);
    };
    let stage = stage
        .as_deref()
        .and_then(TeamPauseStage::parse)
        .ok_or(BotError::Storage)?;
    if (stage == TeamPauseStage::Acknowledged) != acknowledged.is_some()
        || (acknowledged.is_none() && !no_ack)
    {
        return Err(BotError::Storage);
    }
    Ok(Some(BotTeamPauseRow {
        owner,
        paused: paused.ok_or(BotError::Storage)? == 1,
        revision: u64::try_from(revision.ok_or(BotError::Storage)?).map_err(bad)?,
        request: request
            .ok_or(BotError::Storage)?
            .parse()
            .map_err(|_| BotError::Storage)?,
        fingerprint: fingerprint.ok_or(BotError::Storage)?,
        stage,
        changed_at_ms: changed.ok_or(BotError::Storage)?,
        acknowledged_at_ms: acknowledged,
    }))
}

/// Whether this owner's team is paused, read inside the caller's
/// transaction. No row is "never paused". The stage does not matter: the AI
/// side refuses from the commit of the row, acknowledged or not.
pub(super) fn team_paused_on(conn: &Connection, owner: BotOwner) -> Result<bool, BotError> {
    Ok(row_on(conn, owner)?.is_some_and(|row| row.paused))
}

fn write_on(conn: &Connection, row: &BotTeamPauseRow) -> Result<(), BotError> {
    conn.execute(
        "INSERT INTO bot_team_pause(user_scope_id,owner_device_id,paused,revision,request_id,fingerprint,stage,changed_at_ms,acknowledged_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,NULL)
         ON CONFLICT(user_scope_id,owner_device_id) DO UPDATE SET
           paused=excluded.paused,revision=excluded.revision,request_id=excluded.request_id,
           fingerprint=excluded.fingerprint,stage='pending',changed_at_ms=excluded.changed_at_ms,
           acknowledged_at_ms=NULL
         WHERE excluded.revision>bot_team_pause.revision",
        params![
            row.owner.user_scope.to_storage(),
            row.owner.device.to_storage(),
            row.paused,
            i64::try_from(row.revision).map_err(bad)?,
            row.request.to_storage(),
            row.fingerprint,
            row.changed_at_ms,
        ],
    )
    .map_err(bad)
    .and_then(|written| {
        if written == 1 {
            // The team's record changed. No Bot's marker moves: a Bot's
            // record holds no team state.
            super::client_feed::touch_team(conn, row.owner.device).map_err(bad)
        } else {
            Err(BotError::Storage)
        }
    })
}

fn owner_valid(owner: BotOwner) -> Result<(), BotError> {
    if owner.device.as_uuid().is_nil() {
        return Err(BotError::Invalid);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn change_on(
    conn: &Connection,
    owner: BotOwner,
    request: BotRequestId,
    fingerprint_value: &str,
    expected_revision: u64,
    paused: bool,
    now: i64,
) -> Result<TeamPauseChange, BotError> {
    owner_valid(owner)?;
    super::admission::fingerprint(fingerprint_value)?;
    let current = row_on(conn, owner)?;
    // 1. A repeat of the request that produced this revision.
    if let Some(row) = &current {
        if row.request == request {
            return if row.fingerprint == fingerprint_value && row.paused == paused {
                Ok(TeamPauseChange::Replayed(row.clone()))
            } else {
                Err(BotError::Conflict)
            };
        }
    }
    // 2. Another revision than the caller read: it reads again. This is
    //    also the answer to a retry of an older request.
    let revision = current.as_ref().map_or(0, |row| row.revision);
    if expected_revision != revision {
        return Err(BotError::TeamRevisionConflict);
    }
    // 3. Already in the asked state: nothing to write.
    if current.as_ref().is_some_and(|row| row.paused) == paused {
        return Ok(TeamPauseChange::Unchanged(current));
    }
    if revision >= i64::MAX as u64 - 1 {
        return Err(BotError::Invalid);
    }
    let row = BotTeamPauseRow {
        owner,
        paused,
        revision: revision + 1,
        request,
        fingerprint: fingerprint_value.to_owned(),
        stage: TeamPauseStage::Pending,
        changed_at_ms: now,
        acknowledged_at_ms: None,
    };
    write_on(conn, &row)?;
    row_on(conn, owner)?
        .filter(|written| *written == row)
        .map(TeamPauseChange::Changed)
        .ok_or(BotError::Storage)
}

impl Repository {
    /// The owner's team pause row; `None` for a team never paused.
    pub fn team_pause(&self, owner: BotOwner) -> Result<Option<BotTeamPauseRow>, BotError> {
        row_on(&self.connection, owner)
    }

    /// What `read_team_pause` answers.
    pub fn team_pause_view(&self, owner: BotOwner) -> Result<TeamPauseView, BotError> {
        Ok(self.team_pause(owner)?.map_or_else(
            || TeamPauseView::never_paused(owner.device),
            |row| row.view(),
        ))
    }

    /// What the delegate is sent for this owner: the row's barrier, or
    /// "never paused" when there is no row.
    pub fn team_pause_barrier(&self, owner: BotOwner) -> Result<BotTeamBarrier, BotError> {
        Ok(self.team_pause(owner)?.map_or_else(
            || BotTeamBarrier::never_paused(delegate_owner(owner)),
            |row| row.barrier(),
        ))
    }

    /// `PauseTeam` (`paused`) or `ResumeTeam`, in one transaction: a repeat
    /// of the row's own request is answered from the row; a wrong expected
    /// revision is `TeamRevisionConflict`; a team already in the asked state
    /// is answered and nothing is written; otherwise the row is written at
    /// the next revision, `pending`. `fingerprint_value` is 64 hex digits of
    /// what the request asked for.
    pub fn change_team_pause(
        &mut self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint_value: &str,
        expected_revision: u64,
        paused: bool,
        now: i64,
    ) -> Result<TeamPauseChange, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let change = change_on(
            &tx,
            owner,
            request,
            fingerprint_value,
            expected_revision,
            paused,
            now,
        )?;
        tx.commit().map_err(bad)?;
        Ok(change)
    }

    /// Record that a delegate run echoed `barrier`. Only an echo equal to
    /// the row (same revision, state and request) acknowledges it; an echo
    /// of anything else changes nothing. Answers the row as it is now, so
    /// the caller reads its stage; `None` when there is no row (the echo of
    /// "never paused" needs no record).
    pub fn acknowledge_team_pause(
        &mut self,
        owner: BotOwner,
        barrier: &BotTeamBarrier,
        now: i64,
    ) -> Result<Option<BotTeamPauseRow>, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let Some(row) = row_on(&tx, owner)? else {
            return Ok(None);
        };
        if row.barrier() != *barrier || row.stage == TeamPauseStage::Acknowledged {
            return Ok(Some(row));
        }
        let updated = tx
            .execute(
                "UPDATE bot_team_pause SET stage='acknowledged',acknowledged_at_ms=?3
                  WHERE user_scope_id=?1 AND owner_device_id=?2 AND revision=?4 AND stage='pending'",
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    now,
                    i64::try_from(row.revision).map_err(bad)?
                ],
            )
            .map_err(bad)?;
        if updated != 1 {
            return Err(BotError::Storage);
        }
        super::client_feed::touch_team(&tx, owner.device).map_err(bad)?;
        let row = row_on(&tx, owner)?.ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(Some(row))
    }

    /// The delegate answered `bot_team_stale` with the epoch it holds: this
    /// ai.db was restored to an older revision while the delegate kept
    /// running. Write the row at that epoch plus one, keeping its own
    /// `paused` (not paused when there is no row), `pending`, under
    /// `request`, so the next send is accepted and the team cannot stay held
    /// behind a row that says it is not paused. A row already past
    /// `held_epoch` is answered unchanged.
    pub fn advance_team_pause_past(
        &mut self,
        owner: BotOwner,
        held_epoch: u64,
        request: BotRequestId,
        fingerprint_value: &str,
        now: i64,
    ) -> Result<BotTeamPauseRow, BotError> {
        owner_valid(owner)?;
        super::admission::fingerprint(fingerprint_value)?;
        if held_epoch >= i64::MAX as u64 - 1 {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let current = row_on(&tx, owner)?;
        if let Some(row) = &current {
            if row.revision > held_epoch {
                return Ok(row.clone());
            }
        }
        let row = BotTeamPauseRow {
            owner,
            paused: current.is_some_and(|row| row.paused),
            revision: held_epoch + 1,
            request,
            fingerprint: fingerprint_value.to_owned(),
            stage: TeamPauseStage::Pending,
            changed_at_ms: now,
            acknowledged_at_ms: None,
        };
        write_on(&tx, &row)?;
        let written = row_on(&tx, owner)?
            .filter(|written| *written == row)
            .ok_or(BotError::Storage)?;
        tx.commit().map_err(bad)?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
