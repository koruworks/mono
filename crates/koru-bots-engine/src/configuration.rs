// SPDX-License-Identifier: MPL-2.0
//! Provider choices retained with the Bot. A missing/invalid choice is setup
//! work; no provider or model is guessed when starting a worker assignment.
use crate::{
    Database, Error,
    authority::{self, Worker},
};
use koru_session_protocol::{LaunchSelection, ProviderKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Configuration {
    pub provider: ProviderKind,
    #[serde(default)]
    pub selection: LaunchSelection,
    /// A selected folder's verified canonical display binding. None uses the
    /// Bot's managed workspace. It freezes when the first input is accepted.
    #[serde(default)]
    pub working_folder: Option<String>,
}

impl Configuration {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.working_folder.as_ref().is_some_and(|path| {
            path.len() > 4096 || path.contains('\0') || !std::path::Path::new(path).is_absolute()
        }) {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupFailure {
    Configuration,
    Context,
    Provider,
    Input,
    Retirement,
}
impl Database {
    pub fn configuration(&self, who: Worker, thread: uuid::Uuid) -> Result<Configuration, Error> {
        authority::membership(&self.connection, who, thread)?;
        let bytes: Vec<u8> = self.connection.query_row(
            "SELECT settings FROM bots WHERE id=?1",
            [who.bot.to_string()],
            |r| r.get(0),
        )?;
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidRequest)
    }
    pub(crate) fn setup_failure(
        &mut self,
        who: Worker,
        assignment: uuid::Uuid,
        thread: uuid::Uuid,
        session: Option<uuid::Uuid>,
        code: SetupFailure,
        now: i64,
    ) -> Result<(), Error> {
        use rusqlite::{OptionalExtension, params};
        let tx = self.connection.transaction()?;
        authority::membership(&tx, who, thread)?;
        let subject = format!("setup:{}:{}", who.bot, who.incarnation);
        let digest = crate::messages::receipts::digest("setup-failed", &(thread, session, code))?;
        if crate::messages::receipts::find::<()>(&tx, &subject, assignment, &digest)?.is_some() {
            return Ok(());
        }
        let existing:Option<(String,String)>=tx.query_row("SELECT id,state FROM turns WHERE bot=?1 AND thread=?2 AND worker_incarnation=?3 AND session=?4 ORDER BY generation DESC LIMIT 1",
            params![who.bot.to_string(),thread.to_string(),who.incarnation.to_string(),session.map(|id|id.to_string())],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if existing.as_ref().is_some_and(|(_, state)| {
            matches!(state.as_str(), "answered" | "stopped" | "needs_you")
        }) {
            return Ok(());
        }
        let turn = if let Some((turn, _)) = existing {
            tx.execute("UPDATE turns SET state='needs_you',details=CAST(json_set(CAST(details AS TEXT),'$.setup_failure',?2) AS BLOB) WHERE id=?1",params![turn,serde_json::to_value(code).map_err(|_|Error::InvalidRequest)?.as_str()])?;
            turn
        } else {
            let turn = uuid::Uuid::new_v4().to_string();
            let details = serde_json::to_vec(&serde_json::json!({"setup_failure":code}))
                .map_err(|_| Error::InvalidRequest)?;
            tx.execute("INSERT INTO turns(id,thread,bot,session,grant,state,generation,worker_incarnation,details,created_at_ms) SELECT ?1,?2,id,?3,access,'needs_you',(SELECT COALESCE(MAX(generation),0)+1 FROM turns WHERE bot=?4 AND thread=?2),?5,?6,?7 FROM bots WHERE id=?4",params![turn,thread.to_string(),session.map(|id|id.to_string()),who.bot.to_string(),who.incarnation.to_string(),details,now])?;
            turn
        };
        let content = crate::messages::Content {
            body: String::new(),
            attachments: vec![],
            references: vec![
                serde_json::json!({"notice":"session_setup_failed","turn":turn,"reason":code}),
            ],
            in_reply_to: None,
            deadline_ms: None,
            reply_to: crate::messages::ReplyTo::Thread,
        };
        tx.execute("INSERT INTO messages(id,thread,sequence,sender,kind,payload,state,accepted_at_ms) VALUES (?1,?2,?3,?4,'setup_note',?5,'published',?6)",params![uuid::Uuid::new_v4().to_string(),thread.to_string(),crate::messages::next_sequence(&tx,thread)?,who.bot.to_string(),serde_json::to_vec(&content).map_err(|_|Error::InvalidRequest)?,now])?;
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE bot=?1 AND thread=?2 AND worker_incarnation=?3",
            params![
                who.bot.to_string(),
                thread.to_string(),
                who.incarnation.to_string()
            ],
        )?;
        crate::messages::receipts::save(&tx, &subject, assignment, &digest, &(), now)?;
        tx.commit()?;
        Ok(())
    }
}
