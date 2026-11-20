// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_time_zone::SystemZone;

fn text(name: &str, objective: &str) -> Result<(), Error> {
    koru_ai_core::bots::RoutineScheduleName::new(name).map_err(|_| Error::InvalidRequest)?;
    koru_delegate_core::Objective::new(objective).map_err(|_| Error::InvalidRequest)?;
    Ok(())
}
fn next(rule: &RecurrenceRule, now: i64) -> Result<Option<i64>, Error> {
    let zone = if rule.zone_name().is_some() {
        SystemZone::utc()
    } else {
        SystemZone::detect()
    };
    rule.next_after(now.saturating_sub(1), &zone)
        .map(|at| at.map(|at| at.instant_ms()))
        .map_err(|_| Error::InvalidRequest)
}
fn active(c: &Connection, bot: Uuid, require_unpaused: bool) -> Result<(), Error> {
    let yes:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM bots b LEFT JOIN team t ON t.owner=b.owner WHERE b.id=?1 AND b.lifecycle='active' AND (?2=0 OR (b.paused=0 AND COALESCE(t.paused,0)=0)))",params![bot.to_string(),require_unpaused],|r|r.get(0))?;
    if yes { Ok(()) } else { Err(Error::NotAllowed) }
}
fn revision(c: &Connection, bot: Uuid, key: Uuid, expected: u32) -> Result<Routine, Error> {
    let row = read(c, bot, key)?;
    if row.revision != expected {
        return Err(Error::RevisionConflict);
    }
    let live: bool = c.query_row(
        "SELECT lifecycle='active' FROM threads WHERE id=?1",
        [row.thread.to_string()],
        |r| r.get(0),
    )?;
    if !live {
        return Err(Error::NotAllowed);
    }
    Ok(row)
}
fn edit(
    c: &Connection,
    bot: Uuid,
    key: Uuid,
    expected: u32,
    name: &str,
    objective: Option<&str>,
    recurrence: &RecurrenceRule,
    now: i64,
) -> Result<Uuid, Error> {
    active(c, bot, false)?;
    let row = revision(c, bot, key, expected)?;
    let objective = objective.unwrap_or(&row.objective);
    text(name, objective)?;
    let deadline = next(recurrence, now)?;
    schedule::cancel_queued(c, key, "definition_changed", now)?;
    c.execute("UPDATE routines SET revision=revision+1,name=?2,objective=?3,recurrence=?4,effective_at_ms=?5,next_due_ms=?6,updated_at_ms=?5 WHERE id=?1",params![key.to_string(),name,objective,encode(recurrence)?,now,if row.enabled {deadline}else{None}])?;
    c.execute(
        "UPDATE threads SET title=?2,revision=revision+1,updated_at_ms=?3 WHERE id=?1",
        params![row.thread.to_string(), name, now],
    )?;
    Ok(key)
}

/// The same durable command receipt discipline as all other person actions.
/// A retry finds its original answer before checking a revision it changed.
pub fn change(
    db: &mut Database,
    who: &Person,
    command: Uuid,
    action: &Change,
    now: i64,
) -> Result<Receipt<Changed>, Error> {
    if command.is_nil() || encode(action)?.len() > crate::person::MAX_COMMAND_BYTES {
        return Err(Error::InvalidRequest);
    }
    let digest = messages::receipts::digest("routine-change", action)?;
    let subject = who.receipt_subject();
    let tx = db.connection.transaction()?;
    authorize(&tx, who, action.bot())?;
    if let Some(answer) = messages::receipts::find(&tx, &subject, command, &digest)? {
        return Ok(Receipt {
            answer,
            replayed: true,
        });
    }
    let answer = apply(&tx, &who.owner, &who.sender, command, action, now)?;
    messages::receipts::save(&tx, &subject, command, &digest, &answer, now)?;
    tx.commit()?;
    Ok(Receipt {
        answer,
        replayed: false,
    })
}

/// Called only after the entry point establishes person or bound Bot authority.
/// Owner and sender supply participant metadata; they do not grant authority.
pub(super) fn apply(
    tx: &Connection,
    owner: &str,
    sender: &str,
    command: Uuid,
    action: &Change,
    now: i64,
) -> Result<Changed, Error> {
    let mut fired = None;
    let mut first_enabled = false;
    let key = match action {
        Change::Create {
            bot,
            expected_bot_revision,
            name,
            objective,
            recurrence,
            documents,
        } => {
            active(&tx, *bot, false)?;
            text(name, objective)?;
            let deadline = next(recurrence, now)?;
            if documents.len() > 16
                || documents.iter().any(Uuid::is_nil)
                || documents
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != documents.len()
            {
                return Err(Error::InvalidRequest);
            }
            let (rev, policy_rev, policy): (u64, u64, Vec<u8>) = tx.query_row(
                "SELECT revision,policy_revision,access FROM bots WHERE id=?1",
                [bot.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if rev != *expected_bot_revision {
                return Err(Error::RevisionConflict);
            }
            let current: CapturedBotPolicy = decode(&policy)?;
            // A Routine begins with default access, never copied broad access.
            let access = CapturedBotPolicy {
                workspace: current.workspace,
                destination: current.destination,
                access: BotAccess::Default,
                selected_pages: documents
                    .iter()
                    .copied()
                    .map(koru_delegate_core::DocumentId::from_uuid)
                    .collect(),
                connections: Default::default(),
            };
            let key = Uuid::new_v4();
            let thread = Uuid::new_v4();
            let first = !tx.query_row("SELECT EXISTS(SELECT 1 FROM routines r JOIN bots b ON b.id=r.bot WHERE b.owner=?1 AND r.first_enabled_at_ms IS NOT NULL)", [owner], |r| r.get::<_,bool>(0))?;
            tx.execute("INSERT INTO threads(id,bot,kind,title,lifecycle,created_at_ms,updated_at_ms) VALUES (?1,?2,'routine',?3,'active',?4,?4)",params![thread.to_string(),bot.to_string(),name,now])?;
            tx.execute("INSERT INTO participants(thread,participant,kind) VALUES (?1,?2,'bot'),(?1,?3,'person')",params![thread.to_string(),bot.to_string(),sender])?;
            crate::configuration::seed(&tx, *bot, thread)?;
            tx.execute("INSERT INTO routines(id,bot,thread,created_by,revision,grant_revision,source_policy_revision,name,objective,recurrence,access,initial_access,enabled,effective_at_ms,created_at_ms,updated_at_ms,next_due_ms,first_enabled_at_ms) VALUES (?1,?2,?3,?4,1,1,?5,?6,?7,?8,?9,?9,1,?10,?10,?10,?11,?10)",params![key.to_string(),bot.to_string(),thread.to_string(),command.to_string(),policy_rev,name,objective,encode(recurrence)?,encode(&access)?,now,deadline])?;
            if first {
                startup::created(tx, owner, key, now)?;
            }
            key
        }
        Change::Edit {
            bot,
            routine,
            revision: rev,
            name,
            objective,
            recurrence,
        } => edit(
            tx,
            *bot,
            *routine,
            *rev,
            name,
            Some(objective),
            recurrence,
            now,
        )?,
        Change::Timing {
            bot,
            routine,
            revision: rev,
            name,
            recurrence,
        } => edit(tx, *bot, *routine, *rev, name, None, recurrence, now)?,
        Change::Enable {
            bot,
            routine,
            revision: rev,
            enabled,
        } => {
            active(&tx, *bot, *enabled)?;
            let row = revision(&tx, *bot, *routine, *rev)?;
            if *enabled {
                first_enabled=!tx.query_row("SELECT EXISTS(SELECT 1 FROM routines r JOIN bots b ON b.id=r.bot WHERE b.owner=?1 AND r.first_enabled_at_ms IS NOT NULL)",[owner],|r|r.get::<_,bool>(0))?;
                if first_enabled {
                    startup::offered(tx, owner, now)?;
                }
            }
            if row.enabled != *enabled {
                if !enabled {
                    schedule::cancel_queued(&tx, *routine, "paused", now)?;
                }
                let deadline = if *enabled {
                    next(&row.recurrence, now)?
                } else {
                    None
                };
                tx.execute("UPDATE routines SET revision=revision+1,enabled=?2,effective_at_ms=?3,next_due_ms=?4,updated_at_ms=?3,first_enabled_at_ms=CASE WHEN ?2=1 THEN COALESCE(first_enabled_at_ms,?3) ELSE first_enabled_at_ms END WHERE id=?1",params![routine.to_string(),enabled,now,deadline])?;
            }
            *routine
        }
        Change::Access {
            bot,
            routine,
            revision: rev,
            access,
        } => {
            active(&tx, *bot, false)?;
            let row = read(&tx, *bot, *routine)?;
            if row.grant_revision != *rev {
                return Err(Error::RevisionConflict);
            }
            if !access.valid()
                || access.workspace != row.access.workspace
                || access.destination != row.access.destination
                || !access.connections.is_empty()
                || access.selected_pages.iter().any(|id| id.as_uuid().is_nil())
            {
                return Err(Error::InvalidRequest);
            }
            // Running turns keep their captured restriction. Every subsequent
            // effect also intersects this fresh grant, so narrowing is immediate.
            tx.execute("UPDATE routines SET access=?2,grant_revision=grant_revision+1,updated_at_ms=?3 WHERE id=?1",params![routine.to_string(),encode(access)?,now])?;
            *routine
        }
        Change::RunNow {
            bot,
            routine,
            revision: rev,
        } => {
            active(&tx, *bot, true)?;
            let row = revision(&tx, *bot, *routine, *rev)?;
            let at = schedule::enqueue(&tx, &row, now, true, now)?;
            fired = Some(occurrence(&tx, at)?);
            *routine
        }
    };
    Ok(Changed {
        routine: read(&tx, action.bot(), key)?,
        occurrence: fired,
        first_enabled,
    })
}
