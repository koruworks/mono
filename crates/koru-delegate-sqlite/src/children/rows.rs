// SPDX-License-Identifier: MPL-2.0

//! The sole decoder of `bot_child_requests`. A row is ids, a digest, closed
//! tokens, numbers and times; the retained binding body is never decoded on
//! a read path.
use super::*;
use koru_delegate_core::children::{
    BOT_CHILD_WAIT_MS, ChildClock, ChildRequestReceipt, ChildRequestState, ChildSettledBy,
};
use koru_delegate_core::decisions::DecisionDigest;
use koru_delegate_core::{BotAdmissionState, BotRunKey, ChildRequestId, DelegateBootId, ProfileId};
use rusqlite::{Connection, Row, params};

const TABLE: &str = "bot_child_requests";
const COLUMNS: &str = "q.child_request_id,q.parent_run_id,q.wait_id,q.ask_key,q.ordinal,q.user_scope,\
q.device_id,q.parent_bot_id,q.recipient_bot_id,q.human_profile_id,q.child_run_id,q.binding_digest,\
q.state,q.settled_by,q.wait_elapsed_ms,q.clock,q.clock_boot_id,q.clock_since_ms,q.revision,\
q.created_at_ms,(SELECT a.state FROM bot_admissions a WHERE a.run_id=q.child_run_id)";

/// One retained request and the scalars its clock is computed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestRow {
    /// `elapsed_ms` is as of the `now` the row was read at.
    pub receipt: ChildRequestReceipt,
    pub ask_key: uuid::Uuid,
    pub human: ProfileId,
    pub stored_elapsed_ms: u64,
    pub boot: Option<DelegateBootId>,
    pub since_ms: Option<i64>,
    pub created_at_ms: i64,
}

fn text(row: &Row<'_>, index: usize, column: &'static str) -> Result<String, BotLedgerError> {
    row.get::<_, String>(index)
        .map_err(|_| corrupt(TABLE, column))
}
fn id<T: std::str::FromStr>(
    row: &Row<'_>,
    index: usize,
    column: &'static str,
) -> Result<T, BotLedgerError> {
    text(row, index, column)?
        .parse()
        .map_err(|_| corrupt(TABLE, column))
}
fn token<T: serde::de::DeserializeOwned>(
    value: String,
    column: &'static str,
) -> Result<T, BotLedgerError> {
    serde_json::from_value(serde_json::Value::String(value)).map_err(|_| corrupt(TABLE, column))
}

/// Counted time as of `now`: the stored amount plus the open interval, never
/// above the budget and never below the stored amount. A report only.
pub(crate) fn elapsed_at(stored: u64, since: Option<i64>, now: i64) -> u64 {
    let open = since.map_or(0, |since| {
        u64::try_from(now.saturating_sub(since)).unwrap_or(0)
    });
    stored.saturating_add(open).min(BOT_CHILD_WAIT_MS)
}

fn decode(row: &Row<'_>, now: i64) -> Result<RequestRow, BotLedgerError> {
    let owner: BotOwner = serde_json::from_value(serde_json::json!({
        "user_scope": text(row, 5, "user_scope")?,
        "device": text(row, 6, "device_id")?,
    }))
    .map_err(|_| corrupt(TABLE, "owner"))?;
    let state: ChildRequestState = token(text(row, 12, "state")?, "state")?;
    let settled_by: Option<ChildSettledBy> = row
        .get::<_, Option<String>>(13)
        .map_err(|_| corrupt(TABLE, "settled_by"))?
        .map(|value| token(value, "settled_by"))
        .transpose()?;
    let stored: u64 = row.get(14).map_err(|_| corrupt(TABLE, "wait_elapsed_ms"))?;
    let clock: ChildClock = token(text(row, 15, "clock")?, "clock")?;
    let boot: Option<DelegateBootId> = row
        .get::<_, Option<String>>(16)
        .map_err(|_| corrupt(TABLE, "clock_boot_id"))?
        .map(|value| value.parse().map_err(|_| corrupt(TABLE, "clock_boot_id")))
        .transpose()?;
    let since: Option<i64> = row.get(17).map_err(|_| corrupt(TABLE, "clock_since_ms"))?;
    let revision = DecisionRevision::new(row.get(18).map_err(|_| corrupt(TABLE, "revision"))?)
        .map_err(|_| corrupt(TABLE, "revision"))?;
    let digest: [u8; 32] = row
        .get::<_, Vec<u8>>(11)
        .map_err(|_| corrupt(TABLE, "binding_digest"))?
        .try_into()
        .map_err(|_| corrupt(TABLE, "binding_digest"))?;
    // A child admission is retained for as long as its request is.
    let child: BotAdmissionState = token(
        row.get::<_, Option<String>>(20)
            .map_err(|_| corrupt(TABLE, "child_run_id"))?
            .ok_or_else(|| corrupt(TABLE, "child_run_id"))?,
        "child_run_id",
    )?;
    let ordinal: u8 = row.get(4).map_err(|_| corrupt(TABLE, "ordinal"))?;
    let receipt = ChildRequestReceipt {
        child_request: id::<ChildRequestId>(row, 0, "child_request_id")?,
        parent: BotRunKey {
            owner,
            bot: id(row, 7, "parent_bot_id")?,
            run: id(row, 1, "parent_run_id")?,
        },
        wait: id(row, 2, "wait_id")?,
        ordinal,
        recipient: id(row, 8, "recipient_bot_id")?,
        child_run: id(row, 10, "child_run_id")?,
        state,
        settled_by,
        child,
        clock,
        elapsed_ms: u32::try_from(elapsed_at(stored, since, now))
            .map_err(|_| corrupt(TABLE, "wait_elapsed_ms"))?,
        budget_ms: BOT_CHILD_WAIT_MS as u32,
        revision,
        binding_digest: DecisionDigest::from_bytes(digest),
    };
    use koru_delegate_core::decisions::DecisionValidate;
    receipt
        .validate()
        .map_err(|_| corrupt(TABLE, "correlation"))?;
    if (clock == ChildClock::Running) != (boot.is_some() && since.is_some()) {
        return Err(corrupt(TABLE, "clock"));
    }
    Ok(RequestRow {
        receipt,
        ask_key: id(row, 3, "ask_key")?,
        human: ProfileId::from_uuid(id(row, 9, "human_profile_id")?),
        stored_elapsed_ms: stored,
        boot,
        since_ms: since,
        created_at_ms: row.get(19).map_err(|_| corrupt(TABLE, "created_at_ms"))?,
    })
}

fn collect(
    conn: &Connection,
    predicate: &str,
    values: &[&dyn rusqlite::ToSql],
    now: i64,
) -> Result<Vec<RequestRow>, BotLedgerError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM {TABLE} q WHERE {predicate}"
        ))
        .map_err(sql)?;
    let mut rows = statement.query(values).map_err(sql)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(sql)? {
        out.push(decode(row, now)?);
    }
    Ok(out)
}

/// Every request of one wait, in ordinal order. At most three.
pub(crate) fn by_wait(
    conn: &Connection,
    wait: BotWaitId,
    now: i64,
) -> Result<Vec<RequestRow>, BotLedgerError> {
    collect(
        conn,
        "q.wait_id=?1 ORDER BY q.ordinal LIMIT 4",
        &[&wait.to_storage()],
        now,
    )
}

/// Every request a parent Run ever made, in creation order. At most three;
/// a fourth row is a broken ledger and is reported as one.
pub(crate) fn by_parent(
    conn: &Connection,
    run: RunId,
    now: i64,
) -> Result<Vec<RequestRow>, BotLedgerError> {
    let rows = collect(
        conn,
        "q.parent_run_id=?1 ORDER BY q.created_at_ms,q.wait_id,q.ordinal LIMIT 4",
        &[&run.to_storage()],
        now,
    )?;
    if rows.len() > koru_delegate_core::children::CHILD_REQUEST_LIMIT {
        return Err(corrupt(TABLE, "limit"));
    }
    Ok(rows)
}

/// The requests of one ask of one parent Run, in ordinal order.
pub(crate) fn by_ask(
    conn: &Connection,
    run: RunId,
    ask_key: uuid::Uuid,
    now: i64,
) -> Result<Vec<RequestRow>, BotLedgerError> {
    collect(
        conn,
        "q.parent_run_id=?1 AND q.ask_key=?2 ORDER BY q.ordinal LIMIT 4",
        &[&run.to_storage(), &ask_key.hyphenated().to_string()],
        now,
    )
}

pub(crate) fn by_id(
    conn: &Connection,
    request: ChildRequestId,
    now: i64,
) -> Result<Option<RequestRow>, BotLedgerError> {
    Ok(collect(conn, "q.child_request_id=?1", &[&request.to_storage()], now)?.pop())
}

pub(crate) fn by_child(
    conn: &Connection,
    child_run: RunId,
    now: i64,
) -> Result<Option<RequestRow>, BotLedgerError> {
    Ok(collect(conn, "q.child_run_id=?1", &[&child_run.to_storage()], now)?.pop())
}

/// How many requests of this wait are still open. At most three.
pub(crate) fn open_count(conn: &Connection, wait: BotWaitId) -> Result<u8, BotLedgerError> {
    conn.query_row(
        "SELECT count(*) FROM bot_child_requests INDEXED BY bot_child_requests_wait
         WHERE wait_id=?1 AND state='open'",
        params![wait.to_storage()],
        |r| r.get::<_, u8>(0),
    )
    .map_err(sql)
}
