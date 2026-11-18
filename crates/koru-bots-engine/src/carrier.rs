// SPDX-License-Identifier: MPL-2.0

//! The registered worker's private control channel. Identity comes from the
//! one-use registration, never from these deserialized command bodies.

use crate::{
    Database, Error,
    authority::{self, Binding, Descriptor, Issued, Mint, Secret, Worker},
    capacity::{Admission, Reservation},
    messages::{Delivery, Resume, Turn},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub enum Command {
    Poll,
    Context {
        reservation: Reservation,
    },
    Configuration {
        reservation: Reservation,
    },
    Publish {
        reservation: Reservation,
        request: crate::messages::Publish,
    },
    Event {
        reservation: Reservation,
        turn: Turn,
        event: koru_session_protocol::AgentEvent,
    },
    SetupFailed {
        reservation: Reservation,
        code: crate::configuration::SetupFailure,
    },
    Head {
        reservation: Reservation,
    },
    Started {
        reservation: Reservation,
    },
    Mint {
        reservation: Reservation,
        request: Mint,
    },
    Claim {
        reservation: Reservation,
        session: Uuid,
        resume: Resume,
        budget: usize,
    },
    Catalog {
        reservation: Reservation,
        session: Uuid,
        secret: Secret,
    },
    Bind {
        reservation: Reservation,
        session: Uuid,
        secret: Secret,
        command: Uuid,
        descriptor: Descriptor,
    },
    OwnTool {
        reservation: Reservation,
        session: Uuid,
        secret: Secret,
        binding: Uuid,
        door_call: Uuid,
        tool: String,
        arguments: Vec<u8>,
    },
    Accepted {
        reservation: Reservation,
        turn: Turn,
        delivery: Uuid,
    },
    OfferSteer {
        reservation: Reservation,
        turn: Turn,
        budget: usize,
    },
    RejectSteer {
        reservation: Reservation,
        turn: Turn,
        delivery: Uuid,
    },
    Unknown {
        reservation: Reservation,
        turn: Turn,
        delivery: Uuid,
    },
    Retired {
        reservation: Reservation,
        turn: Option<Turn>,
    },
}

#[derive(Serialize, Deserialize)]
pub enum Reply {
    Work {
        starts: Vec<Reservation>,
        stops: Vec<Reservation>,
    },
    Issued(Issued),
    Delivery(Option<Delivery>),
    CatalogGrant(serde_json::Value),
    Bound(Binding),
    Tool(serde_json::Value),
    LoadContext(crate::workspace::Sources),
    Context(crate::workspace::Context),
    Configuration(crate::configuration::Configuration),
    Published(crate::messages::Published),
    Observed(crate::observations::Observation),
    ContextPending,
    Head(Option<Uuid>),
    Done,
    Refused(Error),
}

pub struct Request {
    pub worker: Worker,
    pub command: Command,
    pub reply: mpsc::SyncSender<Reply>,
}

pub(crate) fn handle(
    db: &mut Database,
    admission: &mut Admission,
    who: Worker,
    command: Command,
    now: i64,
) -> Reply {
    execute(db, admission, who, command, now).unwrap_or_else(Reply::Refused)
}

fn execute(
    db: &mut Database,
    admission: &mut Admission,
    who: Worker,
    command: Command,
    now: i64,
) -> Result<Reply, Error> {
    authority::worker(&db.connection, who)?;
    if matches!(command, Command::Poll) {
        return Ok(Reply::Work {
            starts: admission.assigned(who.bot, who.incarnation),
            stops: admission.retiring(who.bot, who.incarnation),
        });
    }
    let reservation = match &command {
        Command::Poll => unreachable!(),
        Command::Started { reservation }
        | Command::Context { reservation }
        | Command::Configuration { reservation }
        | Command::Publish { reservation, .. }
        | Command::Event { reservation, .. }
        | Command::SetupFailed { reservation, .. }
        | Command::Head { reservation }
        | Command::Mint { reservation, .. }
        | Command::Claim { reservation, .. }
        | Command::Catalog { reservation, .. }
        | Command::Bind { reservation, .. }
        | Command::OwnTool { reservation, .. }
        | Command::Accepted { reservation, .. }
        | Command::OfferSteer { reservation, .. }
        | Command::RejectSteer { reservation, .. }
        | Command::Unknown { reservation, .. }
        | Command::Retired { reservation, .. } => *reservation,
    };
    if reservation.key.bot != who.bot {
        return Err(Error::Unauthenticated);
    }
    if !matches!(command, Command::Retired { .. }) && !admission.owns(reservation, who.incarnation)
    {
        return Err(Error::StaleTurn);
    }
    match command {
        Command::Poll => unreachable!(),
        Command::Started { .. } => admission.started(reservation, who.incarnation)?,
        Command::Context { .. } => {
            return db
                .context_sources(who, reservation.key.thread)
                .map(Reply::LoadContext);
        }
        Command::Configuration { .. } => {
            return db
                .configuration(who, reservation.key.thread)
                .map(Reply::Configuration);
        }
        Command::Publish { request, .. } => {
            check_turn(db, who, reservation, request.turn)?;
            return db
                .publish_reply(&request, now)
                .map(|receipt| Reply::Published(receipt.answer));
        }
        Command::Event { turn, event, .. } => {
            check_turn(db, who, reservation, turn)?;
            let live: bool = db.connection.query_row(
                "SELECT state='running' AND stopped=0 FROM turns WHERE id=?1",
                [turn.id.to_string()],
                |r| r.get(0),
            )?;
            if !live {
                return Err(Error::StaleTurn);
            }
            let value = serde_json::to_value(&event).map_err(|_| Error::InvalidRequest)?;
            if value
                .get("turn_id")
                .is_some_and(|id| id.as_str() != Some(&turn.id.to_string()))
            {
                return Err(Error::StaleTurn);
            }
            return Ok(Reply::Observed(crate::observations::Observation {
                thread: reservation.key.thread,
                bot: who.bot,
                turn,
                event,
            }));
        }
        Command::SetupFailed { code, .. } => {
            let session = admission.provider(reservation, who.incarnation)?;
            db.setup_failure(
                who,
                reservation.assignment,
                reservation.key.thread,
                session,
                code,
                now,
            )?;
        }
        Command::Head { .. } => {
            return db
                .session_head(who, reservation.key.thread)
                .map(Reply::Head);
        }
        Command::Catalog {
            session, secret, ..
        } => {
            check_session(db, admission, who, reservation, session, &secret)?;
            return db
                .session_catalog_grant(who, &secret)
                .map(Reply::CatalogGrant);
        }
        Command::Bind {
            session,
            secret,
            command,
            descriptor,
            ..
        } => {
            check_session(db, admission, who, reservation, session, &secret)?;
            return db
                .bind_descriptor(who, &secret, &descriptor, command, now)
                .map(Reply::Bound);
        }
        Command::OwnTool {
            session,
            secret,
            binding,
            door_call,
            tool,
            arguments,
            ..
        } => {
            check_session(db, admission, who, reservation, session, &secret)?;
            let call = authority::Call::parse(door_call, tool, &arguments)?;
            return crate::own_tools::execute(db, &secret, binding, &call).map(Reply::Tool);
        }
        Command::Mint { request, .. } => {
            if request.thread != reservation.key.thread {
                return Err(Error::NotAllowed);
            }
            let issued = db.mint_session(who, request, now)?;
            if !issued.replayed {
                admission.bind_provider(reservation, who.incarnation, issued.session)?;
            }
            return Ok(Reply::Issued(issued));
        }
        Command::Claim {
            session,
            resume,
            budget,
            ..
        } => {
            if admission.provider(reservation, who.incarnation)? != Some(session) {
                return Err(Error::StaleTurn);
            }
            return Ok(Reply::Delivery(
                db.claim_session_messages(who, session, resume, budget, now)?,
            ));
        }
        Command::Accepted { turn, delivery, .. } => {
            check_turn(db, who, reservation, turn)?;
            db.delivery_accepted(turn, delivery)?;
        }
        Command::OfferSteer { turn, budget, .. } => {
            check_turn(db, who, reservation, turn)?;
            return db.offer_steer(turn, budget, now).map(Reply::Delivery);
        }
        Command::RejectSteer { turn, delivery, .. } => {
            check_turn(db, who, reservation, turn)?;
            db.reject_steer(turn, delivery)?;
        }
        Command::Unknown { turn, delivery, .. } => {
            check_turn(db, who, reservation, turn)?;
            db.mark_delivery_unknown(turn, delivery)?;
        }
        Command::Retired { turn, .. } => {
            // Validate the reservation before any durable mutation. Retirement
            // is allowed after an engine-requested stop, unlike new work.
            let known = admission
                .assigned(who.bot, who.incarnation)
                .contains(&reservation)
                || admission.owns(reservation, who.incarnation)
                || admission
                    .retiring(who.bot, who.incarnation)
                    .contains(&reservation);
            if !known {
                return Err(Error::StaleTurn);
            }
            if let Some(turn) = turn {
                check_turn(db, who, reservation, turn)?;
            }
            // A Claim reply may have been lost before the driver learned its
            // turn ID. Find the engine's own turn for this provider anyway;
            // worker uncertainty must not strand a running turn forever.
            if let Some(session) = admission.provider(reservation, who.incarnation)? {
                let row: Option<(String,u64)> = db.connection.query_row(
                    "SELECT id,generation FROM turns WHERE bot=?1 AND thread=?2 AND worker_incarnation=?3 AND session=?4 ORDER BY generation DESC LIMIT 1",
                    rusqlite::params![who.bot.to_string(),reservation.key.thread.to_string(),who.incarnation.to_string(),session.to_string()], |r|Ok((r.get(0)?,r.get(1)?)),
                ).optional()?;
                if let Some((id, generation)) = row {
                    db.interrupt_turn(Turn {
                        id: Uuid::parse_str(&id).map_err(|_| Error::StorageUnavailable)?,
                        generation,
                        worker: who.incarnation,
                        session,
                    })?;
                }
            }
            // Failed launch can have minted a credential without opening a
            // turn. Confirmed retirement must revoke that authority too.
            db.connection.execute("UPDATE credentials SET revoked=1 WHERE bot=?1 AND thread=?2 AND worker_incarnation=?3",rusqlite::params![who.bot.to_string(),reservation.key.thread.to_string(),who.incarnation.to_string()])?;
            admission.retired(reservation, who.incarnation)?;
        }
    }
    Ok(Reply::Done)
}

fn check_session(
    db: &Database,
    admission: &Admission,
    who: Worker,
    reservation: Reservation,
    session: Uuid,
    secret: &Secret,
) -> Result<(), Error> {
    if admission.provider(reservation, who.incarnation)? != Some(session) {
        return Err(Error::StaleTurn);
    }
    db.check_session(who, reservation.key.thread, session, secret)
}

fn check_turn(
    db: &Database,
    who: Worker,
    reservation: Reservation,
    turn: Turn,
) -> Result<(), Error> {
    if turn.worker != who.incarnation {
        return Err(Error::StaleTurn);
    }
    let exact: bool = db.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE id=?1 AND bot=?2 AND thread=?3 AND worker_incarnation=?4 AND generation=?5 AND session=?6)",
        rusqlite::params![turn.id.to_string(),who.bot.to_string(),reservation.key.thread.to_string(),who.incarnation.to_string(),turn.generation,turn.session.to_string()],|r|r.get(0),
    )?;
    if exact { Ok(()) } else { Err(Error::StaleTurn) }
}

#[cfg(test)]
mod tests;
