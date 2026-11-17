// SPDX-License-Identifier: MPL-2.0

//! **Test only.** The proof that no write forgets its marker.
//!
//! [`Repository::in_memory`], the one in-memory `ai.db` every test of this
//! crate and of the AI worker opens, installs this on its connection:
//!
//! - temporary triggers on every table a record is built from
//!   ([`FEED_RECORD_TABLES`]). Each says which subject a changed row belongs
//!   to, and only for a change a record would show: a text delta into a
//!   pending reply names nobody;
//! - a temporary trigger on `bot_client_feed` that says which subject's
//!   marker moved;
//! - a commit hook. **At every commit** it compares the two: a transaction
//!   that changed a subject and did not move its marker is rolled back, and
//!   the write that caused it is named on stderr and kept for the test to
//!   read. The order inside the transaction does not matter.
//!
//! A database a test opens **from a file** ([`Repository::open`] in a test
//! build) gets the same triggers and hook, reporting only: the unmarked
//! write is named on stderr (`feed audit:`) and kept, and the commit goes
//! through. Suites that run a real worker over a temporary root were written
//! before the audit and are not all run with every change; their output can
//! be searched for that line.
//!
//! Nothing here is in a production build, and nothing is written to the
//! database: the triggers and the two SQL functions they call live on the
//! connection.
//!
//! **Fixture writes are exempt by name.** A test that sets a row up with
//! plain SQL is not a write path and has no marker to move. A commit is
//! exempt when the connection was last reached from a test's own source file
//! (a path with a `tests` folder in it, or a `tests.rs` file), which is where
//! such SQL is written, or through [`AuditedConnection::fixture`], which the
//! `*_for_fixture` functions use. Everything else is a write path and is
//! held to the rule.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

use super::FEED_RECORD_TABLES;
use crate::db::OpenError;

#[derive(Default)]
struct State {
    /// Subjects a record of which changed in the open transaction, with the
    /// table whose write said so.
    changed: BTreeMap<(String, String), String>,
    /// Subjects whose marker moved in the open transaction.
    touched: BTreeSet<(String, String)>,
    /// The connection was last reached from a fixture.
    exempt: bool,
    /// Commits the hook looked at.
    commits: u64,
    /// Commits it let through because a fixture made them.
    exempted: u64,
    /// What each refused commit left unmarked.
    refusals: Vec<String>,
}

/// The audit's memory, shared by the connection's hook, its two SQL
/// functions and the test that reads it.
#[derive(Clone, Default)]
pub(crate) struct FeedAudit(Arc<Mutex<State>>);

impl FeedAudit {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Commits the hook has looked at since the database was opened.
    pub(crate) fn commits(&self) -> u64 {
        self.state().commits
    }

    /// Commits let through because a fixture made them.
    pub(crate) fn exempted(&self) -> u64 {
        self.state().exempted
    }

    /// One line per refused commit: `kind subject (table)` for each subject
    /// it left unmarked. Identities only.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.state().refusals.clone()
    }

    fn reached_from(&self, file: &str) {
        self.state().exempt = is_fixture_source(file);
    }
}

/// A test's own source file: a `tests` folder on its path, or a `tests.rs`.
fn is_fixture_source(file: &str) -> bool {
    file.split(['/', '\\'])
        .any(|part| part == "tests" || part == "tests.rs" || part.ends_with("_tests.rs"))
}

/// A connection that remembers where it was last reached from. In a
/// production build the repository holds a plain [`Connection`].
pub(crate) struct AuditedConnection {
    inner: Connection,
    audit: Option<FeedAudit>,
}

impl From<Connection> for AuditedConnection {
    /// A connection with no audit: a database opened from a file, or one
    /// migrated to an older version.
    fn from(inner: Connection) -> Self {
        Self { inner, audit: None }
    }
}

impl AuditedConnection {
    /// Install the audit, enforcing: a commit that leaves a subject unmarked
    /// is rolled back. Refused when a table in [`FEED_RECORD_TABLES`] has no
    /// audit trigger.
    pub(crate) fn audited(inner: Connection) -> Result<Self, OpenError> {
        Self::with_audit(inner, true)
    }

    /// Install the audit, reporting only: a commit that leaves a subject
    /// unmarked is named on stderr and kept for the test to read, and goes
    /// through. For a database a test opens from a file, where the same
    /// write paths run under suites that were written before the audit.
    pub(crate) fn watched(inner: Connection) -> Result<Self, OpenError> {
        Self::with_audit(inner, false)
    }

    fn with_audit(mut inner: Connection, enforce: bool) -> Result<Self, OpenError> {
        let audit = install(&mut inner, FEED_RECORD_TABLES, enforce).map_err(|detail| {
            OpenError::Storage {
                database: crate::DATABASE_FILE,
                operation: "install the feed audit on",
                detail,
            }
        })?;
        Ok(Self {
            inner,
            audit: Some(audit),
        })
    }

    pub(crate) fn audit(&self) -> Option<&FeedAudit> {
        self.audit.as_ref()
    }

    /// The connection, for a named fixture: what it commits is exempt.
    pub(crate) fn fixture(&self) -> &Connection {
        if let Some(audit) = &self.audit {
            audit.state().exempt = true;
        }
        &self.inner
    }

    #[track_caller]
    fn reached(&self) {
        if let Some(audit) = &self.audit {
            audit.reached_from(Location::caller().file());
        }
    }
}

impl Deref for AuditedConnection {
    type Target = Connection;

    #[track_caller]
    fn deref(&self) -> &Connection {
        self.reached();
        &self.inner
    }
}

impl DerefMut for AuditedConnection {
    #[track_caller]
    fn deref_mut(&mut self) -> &mut Connection {
        self.reached();
        &mut self.inner
    }
}

/// A write of this file's own, which is not a test's source file: what a
/// write path that forgot its marker looks like to the audit.
#[cfg(test)]
pub(crate) fn unmarked_write(repository: &crate::Repository, sql: &str) -> rusqlite::Result<()> {
    repository.connection.execute_batch(sql)
}

/// Install the functions, the triggers and the hook on `connection`, and
/// check that every table of `declared` is watched. With `enforce` a commit
/// that leaves a subject unmarked is rolled back; without, it is reported.
pub(crate) fn install(
    connection: &mut Connection,
    declared: &[&str],
    enforce: bool,
) -> Result<FeedAudit, String> {
    let audit = FeedAudit::default();
    let text = |error: rusqlite::Error| error.to_string();

    let changed = audit.clone();
    connection
        .create_scalar_function(
            "feed_audit_changed",
            3,
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let kind: String = context.get(0)?;
                // A fixture may hold anything in an identity column; only
                // text names a subject.
                let subject = match context.get_raw(1) {
                    rusqlite::types::ValueRef::Text(text) => {
                        Some(String::from_utf8_lossy(text).into_owned())
                    }
                    _ => None,
                };
                let source: String = context.get(2)?;
                if let Some(subject) = subject {
                    changed
                        .state()
                        .changed
                        .entry((kind, subject))
                        .or_insert(source);
                }
                Ok(0i64)
            },
        )
        .map_err(text)?;
    let touched = audit.clone();
    connection
        .create_scalar_function(
            "feed_audit_touched",
            2,
            FunctionFlags::SQLITE_UTF8,
            move |context| {
                let kind: String = context.get(0)?;
                if let rusqlite::types::ValueRef::Text(text) = context.get_raw(1) {
                    let subject = String::from_utf8_lossy(text).into_owned();
                    touched.state().touched.insert((kind, subject));
                }
                Ok(0i64)
            },
        )
        .map_err(text)?;

    connection.execute_batch(TRIGGERS).map_err(text)?;
    for table in declared {
        let watched: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_temp_master
                  WHERE type = 'trigger' AND tbl_name = ?1 AND name LIKE 'feed_audit_%')",
                [table],
                |row| row.get(0),
            )
            .map_err(text)?;
        if !watched {
            return Err(format!(
                "a feed record reads {table}, and no audit trigger watches it"
            ));
        }
    }

    let committing = audit.clone();
    connection
        .commit_hook(Some(move || {
            let mut state = committing.state();
            state.commits += 1;
            let changed = std::mem::take(&mut state.changed);
            let touched = std::mem::take(&mut state.touched);
            let unmarked: Vec<String> = changed
                .into_iter()
                .filter(|(subject, _)| !touched.contains(subject))
                .map(|((kind, subject), source)| format!("{kind} {subject} ({source})"))
                .collect();
            if unmarked.is_empty() {
                return false;
            }
            if state.exempt {
                state.exempted += 1;
                return false;
            }
            let refusal = unmarked.join(", ");
            eprintln!(
                "feed audit: a commit changed what a linked desktop is shown and moved no marker for: {refusal}"
            );
            state.refusals.push(refusal);
            // Enforcing, roll the transaction back: the write path is wrong,
            // and a test that went on would be testing a state no build can
            // reach.
            enforce
        }))
        .map_err(text)?;
    let rolling_back = audit.clone();
    connection
        .rollback_hook(Some(move || {
            let mut state = rolling_back.state();
            state.changed.clear();
            state.touched.clear();
        }))
        .map_err(text)?;
    Ok(audit)
}

/// The triggers. Each condition is what a record builder reads, no more: a
/// write that changes nothing a record shows names no subject.
///
/// `feed_audit_changed(kind, subject, table)` ignores a NULL subject, so a
/// row of an ordinary chat names nobody.
const TRIGGERS: &str = "
CREATE TEMP TRIGGER feed_audit_marker AFTER INSERT ON bot_client_feed BEGIN
  SELECT feed_audit_touched(NEW.kind, NEW.subject_id);
END;

-- A Bot: every column its record shows.
CREATE TEMP TRIGGER feed_audit_bots_insert AFTER INSERT ON bots BEGIN
  SELECT feed_audit_changed('bot', NEW.bot_id, 'bots');
END;
CREATE TEMP TRIGGER feed_audit_bots_update AFTER UPDATE ON bots
WHEN OLD.user_scope_id IS NOT NEW.user_scope_id OR OLD.home_workspace_id IS NOT NEW.home_workspace_id
  OR OLD.owner_device_id IS NOT NEW.owner_device_id OR OLD.name IS NOT NEW.name
  OR OLD.role_workspace_id IS NOT NEW.role_workspace_id OR OLD.role_document_id IS NOT NEW.role_document_id
  OR OLD.working_folder IS NOT NEW.working_folder OR OLD.provider IS NOT NEW.provider
  OR OLD.model IS NOT NEW.model OR OLD.reasoning IS NOT NEW.reasoning OR OLD.mode IS NOT NEW.mode
  OR OLD.fast IS NOT NEW.fast OR OLD.revision IS NOT NEW.revision
  OR OLD.policy_revision IS NOT NEW.policy_revision OR OLD.access IS NOT NEW.access
  OR OLD.lifecycle IS NOT NEW.lifecycle OR OLD.paused IS NOT NEW.paused
  OR OLD.folder_lock_request_id IS NOT NEW.folder_lock_request_id
  OR OLD.folder_frozen_at_ms IS NOT NEW.folder_frozen_at_ms
  OR OLD.created_at_ms IS NOT NEW.created_at_ms OR OLD.updated_at_ms IS NOT NEW.updated_at_ms
  OR OLD.avatar IS NOT NEW.avatar OR OLD.may_ask IS NOT NEW.may_ask
BEGIN
  SELECT feed_audit_changed('bot', NEW.bot_id, 'bots');
END;
CREATE TEMP TRIGGER feed_audit_bots_delete AFTER DELETE ON bots BEGIN
  SELECT feed_audit_changed('bot', OLD.bot_id, 'bots');
END;
CREATE TEMP TRIGGER feed_audit_pages_insert AFTER INSERT ON bot_selected_pages BEGIN
  SELECT feed_audit_changed('bot', NEW.bot_id, 'bot_selected_pages');
END;
CREATE TEMP TRIGGER feed_audit_pages_delete AFTER DELETE ON bot_selected_pages BEGIN
  SELECT feed_audit_changed('bot', OLD.bot_id, 'bot_selected_pages');
END;
CREATE TEMP TRIGGER feed_audit_grants_insert AFTER INSERT ON bot_connection_grants BEGIN
  SELECT feed_audit_changed('bot', NEW.bot_id, 'bot_connection_grants');
END;
CREATE TEMP TRIGGER feed_audit_grants_delete AFTER DELETE ON bot_connection_grants BEGIN
  SELECT feed_audit_changed('bot', OLD.bot_id, 'bot_connection_grants');
END;

-- A conversation becomes a Bot's: its marker and one per message it has.
CREATE TEMP TRIGGER feed_audit_bot_chats_insert AFTER INSERT ON bot_chats BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_chats');
  SELECT feed_audit_changed('message', m.id, 'bot_chats') FROM messages AS m WHERE m.chat_id = NEW.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_bot_chats_update AFTER UPDATE ON bot_chats
WHEN OLD.bot_id IS NOT NEW.bot_id OR OLD.purpose IS NOT NEW.purpose OR OLD.chat_id IS NOT NEW.chat_id
BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_chats');
  SELECT feed_audit_changed('message', m.id, 'bot_chats') FROM messages AS m WHERE m.chat_id = NEW.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_bot_chats_delete AFTER DELETE ON bot_chats BEGIN
  SELECT feed_audit_changed('chat', OLD.chat_id, 'bot_chats');
  SELECT feed_audit_changed('message', m.id, 'bot_chats') FROM messages AS m WHERE m.chat_id = OLD.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_trash_insert AFTER INSERT ON bot_chat_trash BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_chat_trash');
END;
CREATE TEMP TRIGGER feed_audit_trash_delete AFTER DELETE ON bot_chat_trash BEGIN
  SELECT feed_audit_changed('chat', OLD.chat_id, 'bot_chat_trash');
END;

-- A Run is bound: the conversation names the Run of its running reply, and
-- a reply that has ended names its own.
CREATE TEMP TRIGGER feed_audit_runs_insert AFTER INSERT ON bot_chat_runs BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_chat_runs');
  SELECT feed_audit_changed('message', m.id, 'bot_chat_runs') FROM messages AS m
   WHERE m.id = NEW.assistant_message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_runs_update AFTER UPDATE ON bot_chat_runs
WHEN OLD.run_id IS NOT NEW.run_id OR OLD.record_workspace_id IS NOT NEW.record_workspace_id
  OR OLD.assistant_message_id IS NOT NEW.assistant_message_id OR OLD.chat_id IS NOT NEW.chat_id
  OR OLD.bot_id IS NOT NEW.bot_id OR OLD.submission_id IS NOT NEW.submission_id
BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_chat_runs');
  SELECT feed_audit_changed('message', NEW.assistant_message_id, 'bot_chat_runs');
END;
CREATE TEMP TRIGGER feed_audit_runs_delete AFTER DELETE ON bot_chat_runs BEGIN
  SELECT feed_audit_changed('chat', OLD.chat_id, 'bot_chat_runs');
  SELECT feed_audit_changed('message', OLD.assistant_message_id, 'bot_chat_runs');
END;
CREATE TEMP TRIGGER feed_audit_run_refs_insert AFTER INSERT ON bot_historical_run_refs BEGIN
  SELECT feed_audit_changed('message', NEW.message_id, 'bot_historical_run_refs');
END;
CREATE TEMP TRIGGER feed_audit_run_refs_delete AFTER DELETE ON bot_historical_run_refs BEGIN
  SELECT feed_audit_changed('message', OLD.message_id, 'bot_historical_run_refs');
END;

-- A conversation one Bot opened in another is shown once its owner admitted
-- it: the row that makes it such a conversation, and its receipt's stage.
CREATE TEMP TRIGGER feed_audit_child_insert AFTER INSERT ON bot_child_chats BEGIN
  SELECT feed_audit_changed('chat', NEW.chat_id, 'bot_child_chats');
  SELECT feed_audit_changed('message', m.id, 'bot_child_chats') FROM messages AS m WHERE m.chat_id = NEW.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_child_delete AFTER DELETE ON bot_child_chats BEGIN
  SELECT feed_audit_changed('chat', OLD.chat_id, 'bot_child_chats');
  SELECT feed_audit_changed('message', m.id, 'bot_child_chats') FROM messages AS m WHERE m.chat_id = OLD.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_receipts_shown AFTER UPDATE OF stage ON bot_command_receipts
WHEN (OLD.stage IN ('accepted','completed')) IS NOT (NEW.stage IN ('accepted','completed'))
BEGIN
  SELECT feed_audit_changed('chat', cc.chat_id, 'bot_command_receipts') FROM bot_child_chats AS cc
   WHERE cc.user_scope_id = NEW.user_scope_id AND cc.child_request_id = NEW.request_id;
  SELECT feed_audit_changed('message', m.id, 'bot_command_receipts') FROM bot_child_chats AS cc
    JOIN messages AS m ON m.chat_id = cc.chat_id
   WHERE cc.user_scope_id = NEW.user_scope_id AND cc.child_request_id = NEW.request_id;
END;
-- A reply names its Run once the Run's submission was admitted.
CREATE TEMP TRIGGER feed_audit_receipts_admitted AFTER UPDATE OF stage ON bot_command_receipts
WHEN (OLD.stage = 'completed') IS NOT (NEW.stage = 'completed')
BEGIN
  SELECT feed_audit_changed('message', m.id, 'bot_command_receipts') FROM bot_chat_runs AS r
    JOIN messages AS m ON m.id = r.assistant_message_id
   WHERE r.user_scope_id = NEW.user_scope_id AND r.submission_id = NEW.request_id AND m.state <> 'pending';
END;

CREATE TEMP TRIGGER feed_audit_team_insert AFTER INSERT ON bot_team_pause BEGIN
  SELECT feed_audit_changed('team_pause', NEW.owner_device_id, 'bot_team_pause');
END;
CREATE TEMP TRIGGER feed_audit_team_update AFTER UPDATE ON bot_team_pause
WHEN OLD.paused IS NOT NEW.paused OR OLD.revision IS NOT NEW.revision OR OLD.stage IS NOT NEW.stage
  OR OLD.changed_at_ms IS NOT NEW.changed_at_ms
BEGIN
  SELECT feed_audit_changed('team_pause', NEW.owner_device_id, 'bot_team_pause');
END;
CREATE TEMP TRIGGER feed_audit_team_delete AFTER DELETE ON bot_team_pause BEGIN
  SELECT feed_audit_changed('team_pause', OLD.owner_device_id, 'bot_team_pause');
END;

-- A conversation's own row: every column its record shows. Its tool mode is
-- not copied, so a change of that names nobody.
CREATE TEMP TRIGGER feed_audit_chats_update AFTER UPDATE ON chats
WHEN OLD.title IS NOT NEW.title OR OLD.current_provider IS NOT NEW.current_provider
  OR OLD.current_model IS NOT NEW.current_model OR OLD.current_reasoning IS NOT NEW.current_reasoning
  OR OLD.branched_from_chat_id IS NOT NEW.branched_from_chat_id
  OR OLD.branched_from_message_id IS NOT NEW.branched_from_message_id
  OR OLD.created_at_ms IS NOT NEW.created_at_ms OR OLD.updated_at_ms IS NOT NEW.updated_at_ms
  OR OLD.private IS NOT NEW.private OR OLD.agent_working_folder IS NOT NEW.agent_working_folder
  OR OLD.agent_mode IS NOT NEW.agent_mode OR OLD.agent_fast IS NOT NEW.agent_fast
  OR OLD.pinned_at_ms IS NOT NEW.pinned_at_ms OR OLD.profile_id IS NOT NEW.profile_id
BEGIN
  SELECT feed_audit_changed('chat', bc.chat_id, 'chats') FROM bot_chats AS bc WHERE bc.chat_id = NEW.id;
  SELECT feed_audit_changed('message', m.id, 'chats') FROM bot_chats AS bc
    JOIN messages AS m ON m.chat_id = bc.chat_id
   WHERE bc.chat_id = NEW.id AND OLD.private IS NOT NEW.private;
END;

-- A message. While a reply is pending its record is a placeholder with no
-- text, so a write that leaves it pending names nobody; the write that ends
-- it does, and the conversation with it, which stops running.
CREATE TEMP TRIGGER feed_audit_messages_insert AFTER INSERT ON messages BEGIN
  SELECT feed_audit_changed('message', NEW.id, 'messages') FROM bot_chats AS bc WHERE bc.chat_id = NEW.chat_id;
  SELECT feed_audit_changed('chat', bc.chat_id, 'messages') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id AND NEW.role = 'assistant' AND NEW.state = 'pending';
END;
CREATE TEMP TRIGGER feed_audit_messages_update AFTER UPDATE ON messages
WHEN NOT (OLD.state = 'pending' AND NEW.state = 'pending')
 AND (OLD.state IS NOT NEW.state OR OLD.markdown IS NOT NEW.markdown OR OLD.ordinal IS NOT NEW.ordinal
   OR OLD.role IS NOT NEW.role OR OLD.origin_message_id IS NOT NEW.origin_message_id
   OR OLD.provider IS NOT NEW.provider OR OLD.requested_model IS NOT NEW.requested_model
   OR OLD.resolved_model IS NOT NEW.resolved_model OR OLD.error_class IS NOT NEW.error_class
   OR OLD.created_at_ms IS NOT NEW.created_at_ms OR OLD.completed_at_ms IS NOT NEW.completed_at_ms
   OR OLD.chat_id IS NOT NEW.chat_id)
BEGIN
  SELECT feed_audit_changed('message', NEW.id, 'messages') FROM bot_chats AS bc WHERE bc.chat_id = NEW.chat_id;
  SELECT feed_audit_changed('chat', bc.chat_id, 'messages') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id AND OLD.role = 'assistant' AND OLD.state = 'pending';
END;
CREATE TEMP TRIGGER feed_audit_messages_delete AFTER DELETE ON messages BEGIN
  SELECT feed_audit_changed('message', OLD.id, 'messages') FROM bot_chats AS bc WHERE bc.chat_id = OLD.chat_id;
END;

-- What is shown beside a message that has ended: its blocks, its sources,
-- its usage. Each is written before or with the write that ends the reply.
CREATE TEMP TRIGGER feed_audit_blocks_insert AFTER INSERT ON message_blocks BEGIN
  SELECT feed_audit_changed('message', m.id, 'message_blocks') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = NEW.message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_blocks_delete AFTER DELETE ON message_blocks BEGIN
  SELECT feed_audit_changed('message', m.id, 'message_blocks') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = OLD.message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_citations_insert AFTER INSERT ON message_citations BEGIN
  SELECT feed_audit_changed('message', m.id, 'message_citations') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = NEW.message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_citations_delete AFTER DELETE ON message_citations BEGIN
  SELECT feed_audit_changed('message', m.id, 'message_citations') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = OLD.message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_attachments_insert AFTER INSERT ON message_attachments
WHEN NEW.message_id IS NOT NULL
BEGIN
  SELECT feed_audit_changed('message', NEW.message_id, 'message_attachments') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_attachments_update AFTER UPDATE OF message_id ON message_attachments
WHEN OLD.message_id IS NOT NEW.message_id
BEGIN
  SELECT feed_audit_changed('message', NEW.message_id, 'message_attachments') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id;
  SELECT feed_audit_changed('message', OLD.message_id, 'message_attachments') FROM bot_chats AS bc
   WHERE bc.chat_id = OLD.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_attachments_delete AFTER DELETE ON message_attachments
WHEN OLD.message_id IS NOT NULL
BEGIN
  SELECT feed_audit_changed('message', OLD.message_id, 'message_attachments') FROM bot_chats AS bc
   WHERE bc.chat_id = OLD.chat_id;
END;
CREATE TEMP TRIGGER feed_audit_attempts_insert AFTER INSERT ON inference_attempts BEGIN
  SELECT feed_audit_changed('message', m.id, 'inference_attempts') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = NEW.assistant_message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_attempts_update AFTER UPDATE ON inference_attempts
WHEN OLD.prompt_tokens IS NOT NEW.prompt_tokens OR OLD.cached_prompt_tokens IS NOT NEW.cached_prompt_tokens
  OR OLD.completion_tokens IS NOT NEW.completion_tokens OR OLD.reasoning_tokens IS NOT NEW.reasoning_tokens
  OR OLD.total_tokens IS NOT NEW.total_tokens OR OLD.usage_source IS NOT NEW.usage_source
  OR OLD.context_used_tokens IS NOT NEW.context_used_tokens
  OR OLD.context_limit_tokens IS NOT NEW.context_limit_tokens
  OR OLD.stop_reason IS NOT NEW.stop_reason
  OR OLD.assistant_message_id IS NOT NEW.assistant_message_id
BEGIN
  SELECT feed_audit_changed('message', m.id, 'inference_attempts') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = NEW.assistant_message_id AND m.state <> 'pending';
END;
CREATE TEMP TRIGGER feed_audit_attempts_delete AFTER DELETE ON inference_attempts BEGIN
  SELECT feed_audit_changed('message', m.id, 'inference_attempts') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = OLD.assistant_message_id;
END;

-- A tool call: shown from the moment it exists, pending reply or not. Its
-- record moves when its status or its approval changes value, and the
-- conversation's with it when the call starts or stops needing the person.
CREATE TEMP TRIGGER feed_audit_tools_insert AFTER INSERT ON tool_invocations BEGIN
  SELECT feed_audit_changed('message', a.assistant_message_id, 'tool_invocations')
    FROM inference_attempts AS a JOIN bot_chats AS bc ON bc.chat_id = NEW.chat_id WHERE a.id = NEW.attempt_id;
  SELECT feed_audit_changed('chat', bc.chat_id, 'tool_invocations') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id AND NEW.status = 'approval_required';
END;
CREATE TEMP TRIGGER feed_audit_tools_update AFTER UPDATE ON tool_invocations
WHEN OLD.status IS NOT NEW.status OR OLD.approval IS NOT NEW.approval
BEGIN
  SELECT feed_audit_changed('message', a.assistant_message_id, 'tool_invocations')
    FROM inference_attempts AS a JOIN bot_chats AS bc ON bc.chat_id = NEW.chat_id WHERE a.id = NEW.attempt_id;
  SELECT feed_audit_changed('chat', bc.chat_id, 'tool_invocations') FROM bot_chats AS bc
   WHERE bc.chat_id = NEW.chat_id
     AND (OLD.status = 'approval_required') IS NOT (NEW.status = 'approval_required');
END;
CREATE TEMP TRIGGER feed_audit_tools_delete AFTER DELETE ON tool_invocations BEGIN
  SELECT feed_audit_changed('message', a.assistant_message_id, 'tool_invocations')
    FROM inference_attempts AS a JOIN bot_chats AS bc ON bc.chat_id = OLD.chat_id WHERE a.id = OLD.attempt_id;
END;
CREATE TEMP TRIGGER feed_audit_notices_insert AFTER INSERT ON agent_notices BEGIN
  SELECT feed_audit_changed('message', a.assistant_message_id, 'agent_notices')
    FROM inference_attempts AS a JOIN bot_chats AS bc ON bc.chat_id = NEW.chat_id WHERE a.id = NEW.attempt_id;
END;
CREATE TEMP TRIGGER feed_audit_notices_delete AFTER DELETE ON agent_notices BEGIN
  SELECT feed_audit_changed('message', a.assistant_message_id, 'agent_notices')
    FROM inference_attempts AS a JOIN bot_chats AS bc ON bc.chat_id = OLD.chat_id WHERE a.id = OLD.attempt_id;
END;
CREATE TEMP TRIGGER feed_audit_feedback_insert AFTER INSERT ON feedback_events
WHEN NEW.message_id IS NOT NULL
BEGIN
  SELECT feed_audit_changed('message', m.id, 'feedback_events') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = NEW.message_id;
END;
CREATE TEMP TRIGGER feed_audit_feedback_delete AFTER DELETE ON feedback_events
WHEN OLD.message_id IS NOT NULL
BEGIN
  SELECT feed_audit_changed('message', m.id, 'feedback_events') FROM messages AS m
    JOIN bot_chats AS bc ON bc.chat_id = m.chat_id WHERE m.id = OLD.message_id;
END;
";
