// SPDX-License-Identifier: MPL-2.0
//! Startup consent delivery is an owner fact, never an OS preference write.
use super::*;

const CANDIDATE: Uuid = Uuid::from_u128(1);
const CLAIMED: Uuid = Uuid::from_u128(2);

fn subject(owner: &str) -> String {
    format!("routine-startup:{owner}")
}
fn digest() -> Result<[u8; 32], Error> {
    messages::receipts::digest("routine-startup", &1u8)
}

pub(super) fn created(c: &Connection, owner: &str, routine: Uuid, now: i64) -> Result<(), Error> {
    messages::receipts::save(c, &subject(owner), CANDIDATE, &digest()?, &routine, now)
}
pub(super) fn offered(c: &Connection, owner: &str, now: i64) -> Result<(), Error> {
    if messages::receipts::find::<bool>(c, &subject(owner), CLAIMED, &digest()?)?.is_none() {
        messages::receipts::save(c, &subject(owner), CLAIMED, &digest()?, &true, now)?;
    }
    Ok(())
}

/// A new request can obtain the first offer once. Its exact retry recovers the
/// original answer. Recording delivery before showing the modal means a crash
/// after delivery can lose the offer; Settings remains the explicit recovery.
pub fn claim(
    db: &mut Database,
    who: &Person,
    request: Uuid,
    bot: Uuid,
    now: i64,
) -> Result<Receipt<bool>, Error> {
    if request.is_nil() {
        return Err(Error::InvalidRequest);
    }
    let command_digest = messages::receipts::digest("claim-routine-startup", &bot)?;
    let tx = db.connection.transaction()?;
    authorize(&tx, who, bot)?;
    let caller = who.receipt_subject();
    if let Some(answer) = messages::receipts::find(&tx, &caller, request, &command_digest)? {
        return Ok(Receipt {
            answer,
            replayed: true,
        });
    }
    let subject = subject(&who.owner);
    let candidate = messages::receipts::find::<Uuid>(&tx, &subject, CANDIDATE, &digest()?)?;
    let claimed = messages::receipts::find::<bool>(&tx, &subject, CLAIMED, &digest()?)?.is_some();
    let answer = if let Some(routine) = candidate.filter(|_| !claimed) {
        tx.query_row("SELECT EXISTS(SELECT 1 FROM routines r JOIN bots b ON b.id=r.bot WHERE r.id=?1 AND b.owner=?2 AND r.enabled=1 AND r.first_enabled_at_ms IS NOT NULL)", params![routine.to_string(),who.owner], |r| r.get::<_,bool>(0))?
    } else {
        false
    };
    if answer {
        offered(&tx, &who.owner, now)?;
    }
    messages::receipts::save(&tx, &caller, request, &command_digest, &answer, now)?;
    tx.commit()?;
    Ok(Receipt {
        answer,
        replayed: false,
    })
}
