// SPDX-License-Identifier: MPL-2.0

//! **A Bot's settings** (Bots P1.8 S5): what the Bot owner holds, the
//! person's unsaved edits, and the changes on their way.
//!
//! This is the settings region's model, with no drawing and no thread: the
//! Bot tab asks it for the [`Command`] a press means, hands it the link's
//! answers, and draws what it says. Everything a person sees about a change
//! comes from the owner's reply, never from the press:
//!
//! - **Every write carries the revision it read.** The name and the working
//!   folder are an `Update` of the configuration at `expected_revision`;
//!   access is `UpdateAccess` at both revisions; pause and resume at the
//!   Bot's revision.
//! - **Nothing is applied optimistically.** A name or a folder is shown as
//!   saved when the owner answers with the Bot that holds it. Access, pause
//!   and resume are operations the owner holds as pending; each is shown as
//!   *Saving…*, *Pausing…* or *Resuming…* and asked about again
//!   (`ReadOperation`) until the owner says it is accepted, and the Bot is
//!   then read again, so what is shown is the owner's.
//! - **A conflict keeps the person's edit.** `revision_conflict` reads the
//!   Bot again and says the Bot changed elsewhere; the unsaved name or access
//!   choice stays as the person left it, over the current settings.
//! - **A refusal is a sentence by its code**, never the server's English
//!   message ([`refusal_words`]).
//! - **A change that was not answered is the same change when asked again.**
//!   Saving the same thing reuses its request, so a reply that was lost on
//!   the way back cannot become a second change: the owner replays its
//!   receipt.

use std::time::{Duration, Instant};

use koru_ai_core::ModelSelection;
use koru_ai_core::bots::{
    Bot, BotAccess, BotCommand, BotOperationStatus, BotPage, BotRequestId, BotResponse,
};
use koru_i18n::Key as Words;

use super::link::{Command, Failure};

/// How soon an operation the owner holds as pending is asked about again,
/// doubling to [`POLL_MAX`] while it stays pending.
const POLL_FIRST: Duration = Duration::from_millis(250);
const POLL_MAX: Duration = Duration::from_secs(2);

/// The longest name the owner takes (`validate_config`), counted as it
/// counts: bytes.
const NAME_MAX_BYTES: usize = 256;

/// Which change a write is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Name,
    Folder,
    /// The agent-and-model card's model, effort, mode and Fast (S7).
    Model,
    Access,
    Pause,
    Resume,
}

/// What the settings say above their cards, when they have something to say.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// The Bot changed elsewhere: its current settings are shown and the
    /// person's edit is kept.
    ChangedElsewhere,
    /// The owner refused, or did not answer: the sentence for why.
    Refused(Words),
}

/// The person's unsaved access choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccessDraft {
    pub access: BotAccess,
    pub pages: Vec<BotPage>,
}

/// **The person's unsaved agent-and-model choice** (S7): the Bot's model —
/// its agent, the model's id and the level — its mode and Fast, `None`
/// for the agent's own default. What the Bot's next conversations start
/// on; an existing conversation keeps its own (QUESTIONS O23).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelDraft {
    pub model: ModelSelection,
    pub mode: Option<String>,
    pub fast: Option<bool>,
}

impl ModelDraft {
    /// What the owner holds for `bot`.
    fn held(bot: &Bot) -> Self {
        Self {
            model: bot.config.model.clone(),
            mode: bot.config.mode.clone(),
            fast: bot.config.fast,
        }
    }
}

/// One change on its way.
#[derive(Clone, Debug)]
struct Pending {
    request: BotRequestId,
    /// What was sent, kept so the same change asked again is the same
    /// request.
    command: BotCommand,
    change: Change,
    /// The owner holds it as a pending operation: `ReadOperation` is owed.
    operation: bool,
    /// A command for it is with the link.
    in_flight: bool,
    next_poll: Option<Instant>,
    backoff: Duration,
}

impl Pending {
    /// The question about it went unanswered, or was turned away for now:
    /// asked again at the next step of the clock (doubling to [`POLL_MAX`]).
    fn poll_again(&mut self, now: Instant) {
        self.in_flight = false;
        self.backoff = (self.backoff * 2).min(POLL_MAX);
        self.next_poll = Some(now.checked_add(self.backoff).unwrap_or(now));
    }
}

/// What an answer settled, for the tab to act on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The answer was about a change this model holds.
    pub matched: bool,
    /// The owner answered with the Bot as it holds it now.
    pub bot: Option<Bot>,
}

#[derive(Default)]
pub(crate) struct Settings {
    bot: Option<Bot>,
    name_draft: Option<String>,
    access_draft: Option<AccessDraft>,
    model_draft: Option<ModelDraft>,
    pending: Vec<Pending>,
    notice: Option<Notice>,
    read_owed: bool,
    /// The last write the owner did not answer, by its request.
    unanswered: Option<(BotRequestId, BotCommand)>,
}

impl Settings {
    /// The Bot as the owner last said. **The person's edits are kept**; one
    /// the owner now holds is no longer an edit.
    pub(crate) fn know(&mut self, bot: Bot) {
        if self.name_draft.as_deref() == Some(bot.config.name.as_str()) {
            self.name_draft = None;
        }
        if self
            .access_draft
            .as_ref()
            .is_some_and(|draft| same_access(draft, &bot))
        {
            self.access_draft = None;
        }
        if self.model_draft.as_ref() == Some(&ModelDraft::held(&bot)) {
            self.model_draft = None;
        }
        // **A save Koru AI did not answer, now read as made**: the read
        // says which, so it is no longer unconfirmed.
        if let Some((_, BotCommand::Update { config, .. })) = &self.unanswered
            && *config == bot.config
        {
            self.unanswered = None;
            if self.notice == Some(Notice::Refused(Words::BotsErrUnreachable)) {
                self.notice = None;
            }
        }
        self.bot = Some(bot);
    }

    pub(crate) fn bot(&self) -> Option<&Bot> {
        self.bot.as_ref()
    }

    pub(crate) fn notice(&self) -> Option<Notice> {
        self.notice
    }

    /// Whether `change` is on its way: shown as pending, its control inert.
    pub(crate) fn is_pending(&self, change: Change) -> bool {
        self.pending.iter().any(|pending| pending.change == change)
    }

    /// A read of the Bot is owed — after a conflict, an accepted operation
    /// or a refusal that may mean the Bot moved. Taken once.
    pub(crate) fn take_read_owed(&mut self) -> bool {
        std::mem::take(&mut self.read_owed)
    }

    /// Ask for a read of the Bot: the settings were opened before the tab
    /// knew more than the roster's row.
    pub(crate) fn owe_read(&mut self) {
        self.read_owed = true;
    }

    // ------------------------------------------------------------- name

    /// The name the field shows: the person's edit, or the owner's.
    pub(crate) fn name_shown(&self) -> &str {
        match (&self.name_draft, &self.bot) {
            (Some(draft), _) => draft,
            (None, Some(bot)) => &bot.config.name,
            (None, None) => "",
        }
    }

    pub(crate) fn name_edited(&self) -> bool {
        self.name_draft.is_some()
    }

    /// The field's text after a keystroke. An edit back to the owner's name
    /// is no edit.
    pub(crate) fn edit_name(&mut self, text: &str) {
        let owners = self.bot.as_ref().map(|bot| bot.config.name.as_str());
        self.name_draft = (owners != Some(text)).then(|| text.to_owned());
    }

    /// Whether *Save* can send the name: an edit, a name the owner takes,
    /// and no name already on its way.
    pub(crate) fn can_save_name(&self) -> bool {
        self.bot.is_some()
            && !self.is_pending(Change::Name)
            && self.name_draft.as_deref().is_some_and(|draft| {
                let name = draft.trim();
                !name.is_empty()
                    && name.len() <= NAME_MAX_BYTES
                    && Some(name) != self.bot.as_ref().map(|bot| bot.config.name.as_str())
            })
    }

    /// *Save* on the name: an `Update` of the configuration the tab read,
    /// with the new name.
    pub(crate) fn save_name(&mut self) -> Option<Command> {
        if !self.can_save_name() {
            return None;
        }
        let bot = self.bot.as_ref()?;
        let mut config = bot.config.clone();
        config.name = self.name_draft.as_deref()?.trim().to_owned();
        let (bot_id, revision) = (bot.id, bot.revision);
        Some(self.send(Change::Name, |request| BotCommand::Update {
            request_id: request,
            bot_id,
            expected_revision: revision,
            config: config.clone(),
        }))
    }

    // ------------------------------------------------ agent and model

    /// The agent and model the card shows: the person's edit, or the
    /// owner's. `None` until the tab holds the Bot.
    pub(crate) fn model_shown(&self) -> Option<ModelDraft> {
        self.model_draft
            .clone()
            .or_else(|| self.bot.as_ref().map(ModelDraft::held))
    }

    /// Whether the agent-and-model choice is an unsaved edit. Test only.
    #[cfg(test)]
    pub(crate) fn model_edited(&self) -> bool {
        self.model_draft.is_some()
    }

    /// The choice after the person changed something in the card's
    /// control. An edit back to what the owner holds is no edit; nothing
    /// changes while a save is on its way.
    pub(crate) fn edit_model(&mut self, draft: ModelDraft) {
        if self.is_pending(Change::Model) {
            return;
        }
        let Some(bot) = &self.bot else {
            return;
        };
        self.model_draft = (draft != ModelDraft::held(bot)).then_some(draft);
    }

    pub(crate) fn can_save_model(&self) -> bool {
        self.bot.is_some() && self.model_draft.is_some() && !self.is_pending(Change::Model)
    }

    /// *Save* on the card: an `Update` of the configuration the tab read,
    /// with the chosen model, mode and Fast.
    pub(crate) fn save_model(&mut self) -> Option<Command> {
        if !self.can_save_model() {
            return None;
        }
        let bot = self.bot.as_ref()?;
        let draft = self.model_draft.clone()?;
        let mut config = bot.config.clone();
        config.model = draft.model;
        config.mode = draft.mode;
        config.fast = draft.fast;
        let (bot_id, revision) = (bot.id, bot.revision);
        Some(self.send(Change::Model, |request| BotCommand::Update {
            request_id: request,
            bot_id,
            expected_revision: revision,
            config: config.clone(),
        }))
    }

    // ----------------------------------------------------------- folder

    /// **Whether the folder may change**: only while the owner has neither
    /// begun locking it for a first task nor frozen it, and no change is on
    /// its way.
    pub(crate) fn folder_changeable(&self) -> bool {
        self.bot.as_ref().is_some_and(|bot| {
            bot.folder_lock_request.is_none() && bot.folder_frozen_at_ms.is_none()
        }) && !self.is_pending(Change::Folder)
    }

    /// The folder the person chose in the folder dialog: an `Update` of the
    /// configuration the tab read — the owner's name, not an unsaved edit —
    /// with the new folder.
    pub(crate) fn folder_chosen(&mut self, folder: &std::path::Path) -> Option<Command> {
        if !self.folder_changeable() || !folder.is_absolute() {
            return None;
        }
        let bot = self.bot.as_ref()?;
        let folder = folder.to_str()?.to_owned();
        if folder == bot.config.working_folder {
            return None;
        }
        let mut config = bot.config.clone();
        config.working_folder = folder;
        let (bot_id, revision) = (bot.id, bot.revision);
        Some(self.send(Change::Folder, |request| BotCommand::Update {
            request_id: request,
            bot_id,
            expected_revision: revision,
            config: config.clone(),
        }))
    }

    // ----------------------------------------------------------- access

    /// The access the choice shows: the person's, or the owner's.
    pub(crate) fn access_shown(&self) -> BotAccess {
        match (&self.access_draft, &self.bot) {
            (Some(draft), _) => draft.access,
            (None, Some(bot)) => bot.access,
            (None, None) => BotAccess::Default,
        }
    }

    /// The selected pages the list shows: the person's, or the owner's.
    pub(crate) fn pages_shown(&self) -> &[BotPage] {
        match (&self.access_draft, &self.bot) {
            (Some(draft), _) => &draft.pages,
            (None, Some(bot)) => &bot.selected_pages,
            (None, None) => &[],
        }
    }

    /// Whether the access choice is an unsaved edit. Test only.
    #[cfg(test)]
    pub(crate) fn access_edited(&self) -> bool {
        self.access_draft.is_some()
    }

    fn draft_access(&mut self, edit: impl FnOnce(&mut AccessDraft)) {
        let Some(bot) = &self.bot else {
            return;
        };
        let mut draft = self.access_draft.clone().unwrap_or_else(|| AccessDraft {
            access: bot.access,
            pages: bot.selected_pages.clone(),
        });
        edit(&mut draft);
        self.access_draft = (!same_access(&draft, bot)).then_some(draft);
    }

    /// One of the two approved choices.
    pub(crate) fn choose_access(&mut self, access: BotAccess) {
        if self.is_pending(Change::Access) {
            return;
        }
        self.draft_access(|draft| draft.access = access);
    }

    /// A page the person selected in the picker. Only a page of the Bot's
    /// home workspace: the owner takes no other (`normalize_pages`).
    pub(crate) fn add_page(&mut self, page: BotPage) -> bool {
        if self.is_pending(Change::Access)
            || self
                .bot
                .as_ref()
                .is_none_or(|bot| bot.home_workspace != page.workspace)
            || self.pages_shown().contains(&page)
        {
            return false;
        }
        self.draft_access(|draft| draft.pages.push(page));
        true
    }

    pub(crate) fn remove_page(&mut self, page: &BotPage) -> bool {
        if self.is_pending(Change::Access) || !self.pages_shown().contains(page) {
            return false;
        }
        self.draft_access(|draft| draft.pages.retain(|kept| kept != page));
        true
    }

    pub(crate) fn can_save_access(&self) -> bool {
        self.bot.is_some() && self.access_draft.is_some() && !self.is_pending(Change::Access)
    }

    /// *Save* on access: `UpdateAccess` at the revisions the tab read.
    pub(crate) fn save_access(&mut self) -> Option<Command> {
        if !self.can_save_access() {
            return None;
        }
        let bot = self.bot.as_ref()?;
        let draft = self.access_draft.clone()?;
        let (bot_id, revision, policy) = (bot.id, bot.revision, bot.policy_revision);
        Some(
            self.send(Change::Access, |request| BotCommand::UpdateAccess {
                request_id: request,
                bot_id,
                expected_revision: revision,
                expected_policy_revision: policy,
                access: draft.access,
                selected_pages: draft.pages.clone(),
            }),
        )
    }

    // ------------------------------------------------------------ pause

    /// *Pause bot* or *Resume bot*, whichever the owner's state calls for.
    pub(crate) fn toggle_pause(&mut self) -> Option<Command> {
        if self.is_pending(Change::Pause) || self.is_pending(Change::Resume) {
            return None;
        }
        let bot = self.bot.as_ref()?;
        let (bot_id, revision) = (bot.id, bot.revision);
        Some(if bot.paused {
            self.send(Change::Resume, |request| BotCommand::Resume {
                request_id: request,
                bot_id,
                expected_revision: revision,
            })
        } else {
            self.send(Change::Pause, |request| BotCommand::Pause {
                request_id: request,
                bot_id,
                expected_revision: revision,
            })
        })
    }

    // ----------------------------------------------------- the link side

    /// Build `change`'s command and hold it as on its way. The same change
    /// the owner did not answer reuses that request.
    fn send(&mut self, change: Change, command: impl Fn(BotRequestId) -> BotCommand) -> Command {
        let request = match &self.unanswered {
            Some((request, sent)) if command(*request) == *sent => *request,
            _ => BotRequestId::mint(),
        };
        let command = command(request);
        let bot = self
            .bot
            .as_ref()
            .map(|bot| bot.id)
            .expect("a Bot to change");
        self.notice = None;
        self.pending.retain(|pending| pending.change != change);
        self.pending.push(Pending {
            request,
            command: command.clone(),
            change,
            operation: false,
            in_flight: true,
            next_poll: None,
            backoff: POLL_FIRST,
        });
        Command::Write {
            bot,
            request,
            command: Box::new(command),
        }
    }

    /// The `ReadOperation`s owed by `now`, each marked as with the link.
    pub(crate) fn polls(&mut self, now: Instant) -> Vec<Command> {
        let Some(bot) = self.bot.as_ref().map(|bot| bot.id) else {
            return Vec::new();
        };
        self.pending
            .iter_mut()
            .filter(|pending| {
                pending.operation
                    && !pending.in_flight
                    && pending.next_poll.is_some_and(|at| at <= now)
            })
            .map(|pending| {
                pending.in_flight = true;
                Command::Write {
                    bot,
                    request: pending.request,
                    command: Box::new(BotCommand::ReadOperation {
                        request_id: pending.request,
                        bot_id: bot,
                    }),
                }
            })
            .collect()
    }

    /// A poll the link would not take: owed again on the next pass.
    pub(crate) fn unsent(&mut self, request: BotRequestId) {
        if let Some(pending) = self.pending.iter_mut().find(|p| p.request == request) {
            pending.in_flight = false;
            if !pending.operation {
                // The write itself never left: nothing is on its way.
                self.pending.retain(|p| p.request != request);
                self.notice = Some(Notice::Refused(Words::BotsErrUnreachable));
            }
        }
    }

    /// When a pass is next owed: the soonest poll, or a short tick while a
    /// command is with the link.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        self.pending
            .iter()
            .filter_map(|pending| {
                if pending.in_flight {
                    now.checked_add(Duration::from_millis(50))
                } else {
                    pending.next_poll
                }
            })
            .min()
    }

    /// **What the owner said about `request`**. A reply about no change
    /// this model holds settles nothing.
    pub(crate) fn answer(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
        now: Instant,
    ) -> Settled {
        let Some(index) = self.pending.iter().position(|p| p.request == request) else {
            return Settled::default();
        };
        let mut settled = Settled {
            matched: true,
            bot: None,
        };
        match result {
            Ok(BotResponse::Bot { bot })
                if matches!(
                    self.pending[index].change,
                    Change::Name | Change::Folder | Change::Model
                ) && self.bot.as_ref().is_none_or(|known| known.id == bot.id) =>
            {
                self.pending.remove(index);
                self.unanswered = None;
                self.notice = None;
                self.know(bot.clone());
                settled.bot = Some(bot);
            }
            Ok(BotResponse::Operation { operation })
                if operation.request_id == request
                    && matches!(
                        self.pending[index].change,
                        Change::Access | Change::Pause | Change::Resume
                    ) =>
            {
                match operation.status {
                    BotOperationStatus::Pending => {
                        let pending = &mut self.pending[index];
                        let wait = if pending.operation {
                            (pending.backoff * 2).min(POLL_MAX)
                        } else {
                            POLL_FIRST
                        };
                        pending.operation = true;
                        pending.in_flight = false;
                        pending.backoff = wait;
                        pending.next_poll = Some(now.checked_add(wait).unwrap_or(now));
                        self.unanswered = None;
                    }
                    BotOperationStatus::Accepted => {
                        self.pending.remove(index);
                        self.unanswered = None;
                        self.notice = None;
                        self.read_owed = true;
                    }
                    BotOperationStatus::Rejected => {
                        self.pending.remove(index);
                        self.unanswered = None;
                        self.notice = Some(Notice::Refused(Words::BotsErrNotApplied));
                        self.read_owed = true;
                    }
                }
            }
            // **The outcome is unknown**: no answer, or one about something
            // else, which is no answer either. The change may well have
            // been made, so it is never said to be not applied.
            Ok(_) | Err(Failure::Unanswered) => {
                let pending = &mut self.pending[index];
                if pending.operation {
                    // The owner holds the operation; only the question about
                    // it went unanswered. Still pending: ask again later.
                    pending.poll_again(now);
                } else {
                    // Not confirmed: the same change asked again is the same
                    // request (the owner replays it), and the Bot is read
                    // again, so a change that was made shows as made.
                    let pending = self.pending.remove(index);
                    self.unanswered = Some((pending.request, pending.command));
                    self.notice = Some(Notice::Refused(Words::BotsErrUnreachable));
                    self.read_owed = true;
                }
            }
            // **A poll turned away for now** (`ai_worker_saturated`,
            // `workspace_unavailable`, `storage_error`): the question about a
            // change the owner holds as pending, never the change. Still
            // pending, asked again on the clock.
            Err(failure) if self.pending[index].operation && failure.again() => {
                self.pending[index].poll_again(now);
            }
            // **A poll refused for good**: the owner accepted the change as
            // pending, so this refusal's words are not the change's. It is
            // no longer followed here, and the Bot's read says whether it
            // took effect.
            Err(Failure::Refused(_)) if self.pending[index].operation => {
                self.pending.remove(index);
                self.read_owed = true;
            }
            Err(Failure::Refused(code)) => {
                self.pending.remove(index);
                self.unanswered = None;
                if code == "revision_conflict" {
                    self.notice = Some(Notice::ChangedElsewhere);
                    self.read_owed = true;
                } else {
                    self.notice = Some(Notice::Refused(refusal_words(&code)));
                    // A lock, a missing Bot, an owner that moved or a Bot
                    // archived meanwhile: what is shown may be old.
                    self.read_owed |= matches!(
                        code.as_str(),
                        "folder_locked"
                            | "not_found"
                            | "wrong_owner"
                            | BOT_ARCHIVED
                            | BOT_TRASHED
                            | BOT_TRASHED_BOT
                            | BOT_PENDING
                    );
                }
            }
        }
        settled
    }
}

/// Whether `draft` is what the owner holds already. The owner keeps
/// selected pages sorted and without repeats, so they compare as sets.
fn same_access(draft: &AccessDraft, bot: &Bot) -> bool {
    let mut mine = draft.pages.clone();
    mine.sort();
    mine.dedup();
    let mut theirs = bot.selected_pages.clone();
    theirs.sort();
    theirs.dedup();
    draft.access == bot.access && mine == theirs
}

/// The owner's refusal of any change to an archived Bot (P1.7):
/// `koru_ai_wire::bot_lifecycle_codes::ARCHIVED`, which the app cannot name
/// (it does not depend on `koru-ai-wire`). Its history still reads.
pub(crate) const BOT_ARCHIVED: &str = "bot_archived";
/// The owner's refusal of a change to a conversation in Trash, or to any
/// part of a trashed Bot (P1.7): `koru_ai_wire::bot_lifecycle_codes::TRASHED`.
pub(crate) const BOT_TRASHED: &str = "bot_conversation_trashed";
/// The owner's refusal while a lifecycle operation of the Bot or the
/// conversation is moving or parked: `bot_lifecycle_codes::PENDING`.
pub(crate) const BOT_PENDING: &str = "bot_lifecycle_pending";
/// The owner's refusal of anything a Bot in Trash causes — a change to the
/// Bot, to one of its conversations, a conversation's Restore — since the
/// engine's Trash review fix: the Bot must be restored first.
/// `koru_ai_wire::bot_lifecycle_codes::BOT_TRASHED`, which the app cannot
/// name (it does not depend on `koru-ai-wire`). `bot_conversation_trashed`
/// stays for a conversation in Trash under a live Bot.
pub(crate) const BOT_TRASHED_BOT: &str = "bot_trashed";
/// **An earlier pause, resume or access change of the Bot is still being
/// delivered** to what runs it (`BotError::Settling`): the owner refuses a
/// pause, resume, settings save, access change, Start or lifecycle capture
/// until it is acknowledged, and offers it again at once, so a retry soon
/// succeeds. Retryable; never a reused request id.
pub(crate) const BOT_OPERATION_PENDING: &str = "bot_operation_pending";
/// **The folder chosen is Koru's own data**, or inside it
/// (`koru_ai_wire::bot_creation_codes::FOLDER_RESERVED`), on creation and on
/// a change of folder. Not retryable: another folder is. A folder that does
/// not exist stays `invalid_request`.
pub(crate) const FOLDER_RESERVED: &str = "folder_reserved";

/// **A refusal's sentence in the settings, by its stable code**
/// (`BotError::code`, and the lifecycle's `bot_archived`,
/// `bot_conversation_trashed` and `bot_lifecycle_pending`). The server's
/// message is English developer text and is never shown. Said where the
/// Bot's settings are shown: a revision conflict says the current
/// settings are shown with the edit kept, and a code this build does not
/// know reads as a change that was not applied, its current settings shown.
/// Everywhere else is [`general_refusal_words`].
///
/// `workspace_unavailable` is `BotError::Unavailable`, which the owner
/// answers for more than one cause: the Bot's delegate or the Koru Agent
/// that runs it is not ready (no delegate port, an agent not started), and
/// the server's Store authority maps every Store failure to it too
/// (`apps/koru-ai-server/src/bots.rs`, each `map_err` there). So
/// its sentence names no cause: Koru could not reach what the Bot needs,
/// and asking again may succeed.
pub(crate) fn refusal_words(code: &str) -> Words {
    match code {
        "revision_conflict" => Words::BotsChangedElsewhere,
        _ => known_refusal(code).unwrap_or(Words::BotsErrNotApplied),
    }
}

/// **A refusal's sentence anywhere the settings are not shown** — the
/// create form, the roster, a tab's lifecycle, the Trash: the same
/// sentences, but none that points at settings on screen. A revision
/// conflict says the Bot changed somewhere else; a code this build does
/// not know says it was not done.
pub(crate) fn general_refusal_words(code: &str) -> Words {
    match code {
        "revision_conflict" => Words::BotsErrRevisionConflict,
        _ => known_refusal(code).unwrap_or(Words::BotsErrNotDone),
    }
}

/// The sentences true wherever they are said, by code.
fn known_refusal(code: &str) -> Option<Words> {
    Some(match code {
        BOT_ARCHIVED => Words::BotsErrArchived,
        BOT_TRASHED => Words::BotsErrTrashed,
        BOT_TRASHED_BOT => Words::BotsErrBotTrashed,
        BOT_PENDING => Words::BotsErrPending,
        BOT_OPERATION_PENDING => Words::BotsErrOperationPending,
        FOLDER_RESERVED => Words::BotsErrFolderReserved,
        "invalid_request" => Words::BotsErrInvalid,
        "not_found" => Words::BotsErrNotFound,
        "wrong_owner" => Words::BotsErrWrongOwner,
        "workspace_unavailable" => Words::BotsErrNotReady,
        "request_conflict" => Words::BotsErrRequestConflict,
        "folder_locked" => Words::BotsErrFolderLocked,
        "storage_error" => Words::BotsErrStorage,
        // `queue_failure`: the AI worker's queue was full, and the command
        // was never queued.
        "ai_worker_saturated" => Words::BotsErrBusy,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
