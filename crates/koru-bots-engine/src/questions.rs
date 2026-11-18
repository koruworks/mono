// SPDX-License-Identifier: MPL-2.0
//! Questions belong to durable turns. Secret answer text is held only in the
//! actor's bounded transient map; neither receipts nor journal entries carry it.
use crate::{
    Database, Error,
    messages::{self, Person, Receipt, Turn},
};
use koru_session_protocol as protocol;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

const LIMIT: usize = 32;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_TRANSIENT: usize = 256;

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Prompt {
    Approval {
        summary: String,
        choices: Vec<protocol::ApprovalChoice>,
    },
    Questions {
        questions: Vec<protocol::AgentQuestion>,
        blocking: bool,
        delivery: protocol::QuestionsDelivery,
    },
}
impl Prompt {
    fn blocking(&self) -> bool {
        match self {
            Self::Approval { .. } => true,
            Self::Questions { blocking, .. } => *blocking,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Answer {
    Approval {
        decision: protocol::ApprovalDecision,
        choice: Option<protocol::ApprovalChoiceId>,
    },
    Questions {
        answers: Option<Vec<protocol::QuestionAnswer>>,
    },
}
#[derive(Serialize, Deserialize)]
struct Pending {
    answer: Answer,
    secret: bool,
    message: Uuid,
}
#[derive(Serialize, Deserialize)]
pub struct Question {
    pub id: Uuid,
    pub turn: Uuid,
    pub prompt: Prompt,
    pub answer: Option<Answer>,
    pub state: String,
    pub delivery_state: String,
    pub provider_available: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub question: Uuid,
    pub command: Uuid,
    pub turn: Turn,
    pub answer: Answer,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Accepted {
    pub question: Uuid,
    pub continued: bool,
}
#[derive(Default)]
pub struct Transient {
    answers: BTreeMap<Uuid, Answer>,
}
impl Transient {
    pub(crate) fn prune(&mut self, db: &Database) {
        self.answers.retain(|command,_| db.connection.query_row("SELECT EXISTS(SELECT 1 FROM questions q JOIN turns t ON t.id=q.turn WHERE q.command=?1 AND q.delivery_state='pending' AND q.state='answered' AND t.stopped=0)",[command.to_string()],|r|r.get::<_,bool>(0)).unwrap_or(true));
    }
}

fn bytes(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(value: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(value).map_err(|_| Error::StorageUnavailable)
}
fn id(value: String) -> Result<Uuid, Error> {
    Uuid::parse_str(&value).map_err(|_| Error::StorageUnavailable)
}

fn prepared(prompt: &Prompt, answer: &Answer) -> Result<(Answer, Pending), Error> {
    let raw = match (prompt, answer) {
        (Prompt::Approval { choices, .. }, Answer::Approval { decision, choice }) => {
            // Empty choices are the older one-time boolean protocol. A choice
            // cannot be invented, nor can a remembered permission be inferred.
            if choices.is_empty() {
                if choice.is_some() {
                    return Err(Error::InvalidRequest);
                }
                answer.clone()
            } else {
                let choice = protocol::resolve_approval_choice(choices, *decision, *choice)
                    .map_err(|_| Error::InvalidRequest)?;
                Answer::Approval {
                    decision: choice.intent.decision(),
                    choice: Some(choice.id),
                }
            }
        }
        (Prompt::Questions { questions, .. }, Answer::Questions { answers }) => Answer::Questions {
            answers: answers
                .clone()
                .map(|answers| {
                    protocol::validate_answers(questions, answers)
                        .map_err(|_| Error::InvalidRequest)
                })
                .transpose()?,
        },
        _ => return Err(Error::InvalidRequest),
    };
    let stored = match (&raw, prompt) {
        (
            Answer::Questions {
                answers: Some(answers),
            },
            Prompt::Questions { questions, .. },
        ) => Answer::Questions {
            answers: Some(protocol::withhold_secrets(questions, answers)),
        },
        _ => raw.clone(),
    };
    let secret = bytes(&raw)? != bytes(&stored)?;
    Ok((
        raw,
        Pending {
            answer: stored,
            secret,
            message: Uuid::nil(),
        },
    ))
}
fn refresh(c: &Connection, turn: Uuid) -> Result<(), Error> {
    c.execute("UPDATE turns SET state=CASE WHEN EXISTS(SELECT 1 FROM questions WHERE turn=?1 AND state='open' AND (json_extract(CAST(prompt AS TEXT),'$.kind')='approval' OR json_extract(CAST(prompt AS TEXT),'$.blocking')=1)) THEN 'needs_you' ELSE 'running' END WHERE id=?1 AND state IN ('running','needs_you') AND stopped=0",[turn.to_string()])?;
    Ok(())
}

impl Database {
    pub(crate) fn pending_question_answers(
        &self,
        turn: Turn,
        transient: &Transient,
    ) -> Result<Vec<Delivery>, Error> {
        let row = messages::turns::turn_row(&self.connection, turn)?;
        if !matches!(row.2.as_str(), "running" | "needs_you") {
            return Err(Error::StaleTurn);
        }
        let rows=self.connection.prepare("SELECT id,command,answer FROM questions WHERE turn=?1 AND state='answered' AND delivery_state='pending' ORDER BY id LIMIT 32")?.query_map([turn.id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Vec<u8>>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut result = vec![];
        let mut size = 0;
        for (question, command, answer) in rows {
            let pending: Pending = decode(&answer)?;
            let command = id(command)?;
            let answer = if pending.secret {
                transient
                    .answers
                    .get(&command)
                    .cloned()
                    .ok_or(Error::EngineUnavailable)?
            } else {
                pending.answer
            };
            let delivery = Delivery {
                question: id(question)?,
                command,
                turn,
                answer,
            };
            size += bytes(&delivery)?.len();
            if size > koru_bot_client_wire::MAX_FRAME_BYTES / 2 {
                break;
            }
            result.push(delivery);
        }
        Ok(result)
    }
    pub(crate) fn question_answer_sent(
        &mut self,
        turn: Turn,
        question: Uuid,
        command: Uuid,
    ) -> Result<(), Error> {
        let row = messages::turns::turn_row(&self.connection, turn)?;
        if !matches!(row.2.as_str(), "running" | "needs_you") {
            return Err(Error::StaleTurn);
        }
        let changed=self.connection.execute("UPDATE questions SET delivery_state='sent' WHERE id=?1 AND turn=?2 AND command=?3 AND state='answered'",params![question.to_string(),turn.id.to_string(),command.to_string()])?;
        if changed == 0 {
            return Err(Error::StaleTurn);
        }
        Ok(())
    }
    /// Called by the authenticated carrier, before acknowledging the event.
    /// The id and prompt are immutable, so a lost event ack can be retried.
    pub(crate) fn question_event(
        &mut self,
        turn: Turn,
        event: &protocol::AgentEvent,
        now: i64,
    ) -> Result<(), Error> {
        use protocol::AgentEvent as E;
        let tx = self.connection.transaction()?;
        let row = messages::turns::turn_row(&tx, turn)?;
        if !matches!(row.2.as_str(), "running" | "needs_you") {
            return Err(Error::StaleTurn);
        }
        let required = match event {
            E::ApprovalRequired {
                approval_id,
                summary,
                choices,
                ..
            } => Some((
                approval_id.as_uuid(),
                Prompt::Approval {
                    summary: summary.clone(),
                    choices: choices.clone(),
                },
            )),
            E::QuestionsRequired {
                request_id,
                questions,
                blocking,
                delivery,
                ..
            } => {
                let questions =
                    protocol::bound_questions(questions.clone()).ok_or(Error::InvalidRequest)?;
                Some((
                    request_id.as_uuid(),
                    Prompt::Questions {
                        questions,
                        blocking: *blocking,
                        delivery: *delivery,
                    },
                ))
            }
            _ => None,
        };
        if let Some((key, prompt)) = required {
            let encoded = bytes(&prompt)?;
            if encoded.len() > MAX_PROMPT_BYTES {
                return Err(Error::InvalidRequest);
            }
            let old: Option<(String, Vec<u8>)> = tx
                .query_row(
                    "SELECT turn,prompt FROM questions WHERE id=?1",
                    [key.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((old_turn, old_prompt)) = old {
                if old_turn != turn.id.to_string() || old_prompt != encoded {
                    return Err(Error::Conflict);
                }
            } else {
                let count: u64 = tx.query_row(
                    "SELECT count(*) FROM questions WHERE turn=?1",
                    [turn.id.to_string()],
                    |r| r.get(0),
                )?;
                let pending: u64 = tx.query_row(
                    "SELECT count(*) FROM questions WHERE state='open' OR delivery_state='pending'",
                    [],
                    |r| r.get(0),
                )?;
                if count >= LIMIT as u64 || pending >= MAX_TRANSIENT as u64 {
                    return Err(Error::InvalidRequest);
                }
                tx.execute(
                    "INSERT INTO questions(id,turn,prompt,state) VALUES (?1,?2,?3,'open')",
                    params![key.to_string(), turn.id.to_string(), encoded],
                )?;
                history_note(
                    &tx,
                    id(row.0.clone())?,
                    &row.1,
                    serde_json::json!({"notice":"question","question":key,"prompt":prompt}),
                    now,
                )?;
            }
            if prompt.blocking() {
                refresh(&tx, turn.id)?;
            }
        } else {
            let resolved = match event {
                E::ApprovalResolved { approval_id, .. } => Some((approval_id.as_uuid(), false)),
                E::ApprovalExpired { approval_id, .. } => Some((approval_id.as_uuid(), true)),
                E::QuestionsResolved {
                    request_id,
                    outcome,
                    ..
                } => Some((
                    request_id.as_uuid(),
                    matches!(outcome, protocol::QuestionsOutcome::Expired),
                )),
                _ => None,
            };
            if let Some((key, expired)) = resolved {
                // Provider outcome events already withhold secrets. Still
                // sanitize against the stored prompt before retaining them.
                let prompt: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT prompt FROM questions WHERE id=?1 AND turn=?2",
                        params![key.to_string(), turn.id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(prompt) = prompt {
                    let mut sanitized = event.clone();
                    if let (
                        Prompt::Questions { questions, .. },
                        E::QuestionsResolved {
                            outcome: protocol::QuestionsOutcome::Answered { answers },
                            ..
                        },
                    ) = (decode::<Prompt>(&prompt)?, &mut sanitized)
                    {
                        *answers = protocol::withhold_secrets(&questions, answers);
                    }
                    tx.execute("UPDATE questions SET state=?3,delivery_state='sent',resolution=?4 WHERE id=?1 AND turn=?2 AND state<>'cancelled'",params![key.to_string(),turn.id.to_string(),if expired {"cancelled"}else{"answered"},bytes(&sanitized)?])?;
                    refresh(&tx, turn.id)?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn read_questions(&self, who: &Person, thread: Uuid) -> Result<Vec<Question>, Error> {
        messages::authorize_thread(&self.connection, who, thread)?;
        let rows=self.connection.prepare("SELECT q.id,q.turn,q.prompt,q.answer,q.state,q.delivery_state FROM questions q JOIN turns t ON t.id=q.turn WHERE t.thread=?1 AND (q.state='open' OR q.delivery_state='pending') ORDER BY t.created_at_ms,q.id LIMIT 32")?.query_map([thread.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Vec<u8>>(2)?,r.get::<_,Option<Vec<u8>>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
        rows.into_iter()
            .map(|r| {
                Ok(Question {
                    id: id(r.0)?,
                    turn: id(r.1.clone())?,
                    prompt: decode(&r.2)?,
                    answer: r
                        .3
                        .map(|a| decode::<Pending>(&a).map(|p| p.answer))
                        .transpose()?,
                    state: r.4,
                    delivery_state: r.5,
                provider_available:self.connection.query_row("SELECT EXISTS(SELECT 1 FROM turns t JOIN credentials c ON c.session=t.session JOIN workers w ON w.incarnation=t.worker_incarnation WHERE t.id=?1 AND c.revoked=0 AND t.stopped=0)",[r.1],|row|row.get(0))?,
                })
            })
            .collect()
    }

    pub fn answer_question(
        &mut self,
        who: &Person,
        command: Uuid,
        question: Uuid,
        answer: &Answer,
        transient: &mut Transient,
        now: i64,
    ) -> Result<Receipt<Accepted>, Error> {
        if command.is_nil() || question.is_nil() || bytes(answer)?.len() > MAX_PROMPT_BYTES {
            return Err(Error::InvalidRequest);
        }
        let digest = messages::receipts::digest("question-answer", &(question, answer))?;
        let subject = who.receipt_subject();
        let tx = self.connection.transaction()?;
        let row:Option<(String,String,String,Vec<u8>,String,String,u64,String)>=tx.query_row("SELECT t.id,t.thread,t.bot,q.prompt,q.state,t.state,t.generation,t.worker_incarnation FROM questions q JOIN turns t ON t.id=q.turn WHERE q.id=?1",[question.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
        let row = row.ok_or(Error::NotAllowed)?;
        let turn = id(row.0)?;
        let thread = id(row.1)?;
        let bot = id(row.2)?;
        messages::authorize_thread(&tx, who, thread)?;
        let (raw, mut pending) = prepared(&decode(&row.3)?, answer)?;
        if pending.secret
            && !transient.answers.contains_key(&command)
            && transient.answers.len() >= MAX_TRANSIENT
        {
            return Err(Error::EngineUnavailable);
        }
        if let Some(accepted) = messages::receipts::find(&tx, &subject, command, &digest)? {
            // A retry can replace the transient secret after a lost response,
            // but can never restart a stopped/cancelled provider request.
            if pending.secret {
                transient.answers.insert(command, raw);
            }
            return Ok(Receipt {
                answer: accepted,
                replayed: true,
            });
        }
        messages::authorize_bot(&tx, who, bot)?;
        let newer: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE thread=?1 AND bot=?2 AND generation>?3)",
            params![thread.to_string(), bot.to_string(), row.6],
            |r| r.get(0),
        )?;
        let next_message = matches!(
            decode::<Prompt>(&row.3)?,
            Prompt::Questions {
                delivery: protocol::QuestionsDelivery::NextMessage,
                ..
            }
        );
        if newer
            || row.4 != "open"
            || !(matches!(row.5.as_str(), "running" | "needs_you" | "interrupted")
                || (next_message && row.5 == "answered"))
        {
            return Err(Error::StaleTurn);
        }
        let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM workers w JOIN credentials c ON c.worker_incarnation=w.incarnation JOIN turns t ON t.session=c.session WHERE t.id=?1 AND c.revoked=0 AND w.incarnation=?2)",params![turn.to_string(),row.7],|r|r.get(0))?;
        let continued = !live || row.5 == "answered";
        // There is no durable secret fallback. A disconnected secret form
        // remains unanswered; ordinary answers can continue from history.
        if continued && pending.secret {
            return Err(Error::EngineUnavailable);
        }
        pending.message = continue_answer(
            &tx,
            who,
            thread,
            bot,
            question,
            &pending.answer,
            now,
            continued,
        )?;
        tx.execute("UPDATE questions SET answer=?2,command=?3,state='answered',delivery_state=?4 WHERE id=?1",params![question.to_string(),bytes(&pending)?,command.to_string(),if continued {"sent"}else{"pending"}])?;
        if continued {
            tx.execute(
                "UPDATE turns SET state='interrupted' WHERE id=?1 AND state='needs_you' AND NOT EXISTS(SELECT 1 FROM questions WHERE turn=?1 AND state='open' AND (json_extract(CAST(prompt AS TEXT),'$.kind')='approval' OR json_extract(CAST(prompt AS TEXT),'$.blocking')=1))",
                [turn.to_string()],
            )?;
        } else {
            refresh(&tx, turn)?;
        }
        let accepted = Accepted {
            question,
            continued,
        };
        messages::receipts::save(&tx, &subject, command, &digest, &accepted, now)?;
        tx.commit()?;
        if pending.secret {
            transient.answers.insert(command, raw);
        }
        Ok(Receipt {
            answer: accepted,
            replayed: false,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn continue_answer(
    c: &Connection,
    who: &Person,
    thread: Uuid,
    bot: Uuid,
    question: Uuid,
    answer: &Answer,
    now: i64,
    enqueue: bool,
) -> Result<Uuid, Error> {
    let message = Uuid::new_v4();
    let content = messages::Content {
        body: String::new(),
        attachments: vec![],
        references: vec![serde_json::json!({"question":question,"answer":answer})],
        in_reply_to: None,
        deadline_ms: None,
        reply_to: messages::ReplyTo::Thread,
    };
    c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'words',?5,'accepted',?6)",params![message.to_string(),thread.to_string(),messages::next_sequence(c,thread)?,who.sender,bytes(&content)?,now])?;
    if enqueue {
        enqueue_answer(c, thread, bot, message, now)?;
    }
    Ok(message)
}
fn enqueue_answer(
    c: &Connection,
    thread: Uuid,
    bot: Uuid,
    message: Uuid,
    now: i64,
) -> Result<(), Error> {
    let obligation = Uuid::new_v4();
    c.execute("INSERT INTO obligations(id,thread,message,bot,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'owed',?5)",params![obligation.to_string(),thread.to_string(),message.to_string(),bot.to_string(),now])?;
    c.execute(
        "INSERT INTO inbox(id,thread,message,bot,accepted_at_ms) VALUES (?1,?2,?3,?4,?5)",
        params![
            obligation.to_string(),
            thread.to_string(),
            message.to_string(),
            bot.to_string(),
            now
        ],
    )?;
    Ok(())
}

fn history_note(
    c: &Connection,
    thread: Uuid,
    bot: &str,
    reference: serde_json::Value,
    now: i64,
) -> Result<Uuid, Error> {
    let content = messages::Content {
        body: String::new(),
        attachments: vec![],
        references: vec![reference],
        in_reply_to: None,
        deadline_ms: None,
        reply_to: messages::ReplyTo::Thread,
    };
    let message = Uuid::new_v4();
    c.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'setup_note',?5,'published',?6)",params![message.to_string(),thread.to_string(),messages::next_sequence(c,thread)?,bot,bytes(&content)?,now])?;
    Ok(message)
}

/// Only after credential revocation/confirmed process loss. Accepted ordinary
/// answers continue as engine work. A lost secret is explicitly withheld and
/// the replacement is told to ask again; no secret is reconstructed from disk.
pub(crate) fn recover(c: &Connection, worker: Option<Uuid>) -> Result<(), Error> {
    let rows=c.prepare("SELECT q.id,t.id,t.thread,t.bot,q.answer FROM questions q JOIN turns t ON t.id=q.turn WHERE q.state='answered' AND q.delivery_state='pending' AND t.stopped=0 AND (?1 IS NULL OR t.worker_incarnation=?1) AND NOT EXISTS(SELECT 1 FROM credentials cr WHERE cr.session=t.session AND cr.revoked=0) LIMIT 256")?.query_map([worker.map(|id|id.to_string())],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Vec<u8>>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (question, turn, thread, bot, answer) in rows {
        let pending: Pending = decode(&answer)?;
        // Recovery retains the answer's original acceptance order, rather
        // than making it jump ahead of older queued work with a zero clock.
        let now: i64 = c.query_row(
            "SELECT accepted_at_ms FROM messages WHERE id=?1",
            [pending.message.to_string()],
            |r| r.get(0),
        )?;
        let thread = id(thread)?;
        let bot_id = id(bot.clone())?;
        if pending.secret {
            history_note(
                c,
                thread,
                &bot,
                serde_json::json!({"notice":"secret_answer_not_delivered","question":question,"instruction":"Ask the person again. The secret answer was not retained."}),
                now,
            )?;
        }
        enqueue_answer(c, thread, bot_id, pending.message, now)?;
        c.execute(
            "UPDATE questions SET delivery_state='sent',state=?2 WHERE id=?1",
            params![
                question,
                if pending.secret {
                    "cancelled"
                } else {
                    "answered"
                }
            ],
        )?;
        c.execute(
            "UPDATE turns SET state='interrupted' WHERE id=?1 AND state IN ('running','needs_you') AND NOT EXISTS(SELECT 1 FROM questions WHERE turn=?1 AND state='open' AND (json_extract(CAST(prompt AS TEXT),'$.kind')='approval' OR json_extract(CAST(prompt AS TEXT),'$.blocking')=1))",
            [turn],
        )?;
    }
    Ok(())
}

pub(crate) fn reask(
    c: &Connection,
    who: &Person,
    thread: Uuid,
    question: Uuid,
    now: i64,
) -> Result<(), Error> {
    messages::authorize_thread(c, who, thread)?;
    let row:Option<(String,String)>=c.query_row("SELECT t.id,t.bot FROM questions q JOIN turns t ON t.id=q.turn WHERE q.id=?1 AND t.thread=?2 AND q.state='open' AND (t.state IN ('needs_you','interrupted') OR (t.state='answered' AND json_extract(CAST(q.prompt AS TEXT),'$.delivery')='next_message')) AND t.stopped=0 AND NOT EXISTS(SELECT 1 FROM credentials cr WHERE cr.session=t.session AND cr.revoked=0) AND NOT EXISTS(SELECT 1 FROM turns newer WHERE newer.thread=t.thread AND newer.bot=t.bot AND newer.generation>t.generation)",params![question.to_string(),thread.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (turn, bot) = row.ok_or(Error::StaleTurn)?;
    messages::authorize_bot(c, who, id(bot.clone())?)?;
    c.execute(
        "UPDATE questions SET state='cancelled',delivery_state='sent' WHERE id=?1",
        [question.to_string()],
    )?;
    let message = history_note(
        c,
        thread,
        &bot,
        serde_json::json!({"notice":"question_reask","question":question,"instruction":"Ask the person again in this session. The prior request's provider ended."}),
        now,
    )?;
    // A next-message question can outlive its completed turn. Reasking it
    // needs new owed work; changing an answered turn alone cannot wake it.
    enqueue_answer(c, thread, id(bot)?, message, now)?;
    c.execute("UPDATE turns SET state='interrupted' WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM questions WHERE turn=?1 AND state='open' AND (json_extract(CAST(prompt AS TEXT),'$.kind')='approval' OR json_extract(CAST(prompt AS TEXT),'$.blocking')=1))",[turn])?;
    Ok(())
}
