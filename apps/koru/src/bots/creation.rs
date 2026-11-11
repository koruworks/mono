// SPDX-License-Identifier: MPL-2.0

//! **Creating a Bot** (Bots P1.8 S8): the create form's model — the
//! person's draft, the creation the Bot owner holds for it, and what is on
//! its way — with no drawing and no thread. The Bot tab in create mode asks
//! it for the [`BotCommand`] a press means, hands it the link's answers, and
//! draws what it says ([`crate::bots::view`]).
//!
//! - **One request and one BotId for the life of a draft.** Both are minted
//!   when the draft is made and sent with every Create, Read, Retry and
//!   Abandon, so a lost reply, a second press or a Retry can only ever be
//!   the same creation: the owner replays it. They are replaced only when
//!   the owner has ended that creation — refused it before it made anything
//!   (`rejected`) or the person abandoned it — and the person then creates
//!   again from the same inputs. When Koru cannot confirm whether the owner
//!   made it, they are replaced only by the person's *Start over*.
//! - **A duplicate press sends nothing.** While a command is with the link,
//!   or while the owner holds the creation, Create sends nothing new.
//! - **The draft survives every failure.** A refusal, an unanswered command
//!   or a stopped creation keeps every input; nothing is cleared until the
//!   tab becomes the Bot's.
//! - **Everything a person reads is a sentence by a stable code**: the
//!   creation's reason, the refusal's code. The server's English is never
//!   shown.
//! - **Progress is read, never assumed.** The owner sends no event when a
//!   creation moves, so it is read when the tab is shown and, while it is
//!   moving and shown, on the lifecycle UI's bounded clock (250 ms doubling
//!   to two seconds; nothing while hidden or stopped).

use std::time::{Duration, Instant};

use koru_ai_core::bots::{
    BOT_ROLE_MARKDOWN_LIMIT, BOT_ROLE_TITLE_LIMIT, Bot, BotAccess, BotCommand, BotCreation,
    BotCreationConfig, BotCreationReason, BotCreationStage, BotFolderChoice, BotId, BotPage,
    BotRequestId, BotResponse, BotRoleInput,
};
use koru_ai_ui::agent_catalog::config::modes::NewChatPermission;
use koru_i18n::Key as Words;
use koru_store_core::ids::WorkspaceId;

use super::link::Failure;
use super::settings::{AccessDraft, ModelDraft};

/// **The permission a new Bot starts in** (QUESTIONS O42, recommended and
/// built): the runtime's asking mode, never the person's own default
/// permission for new chats, so no Bot starts by bypassing approvals
/// unasked. One constant, so a change of mind is one line.
pub(crate) const NEW_BOT_PERMISSION: NewChatPermission = NewChatPermission::Manual;

/// The longest name the owner takes (`BotCreationConfig::validate`), counted
/// as it counts: bytes.
const NAME_MAX_BYTES: usize = 256;

/// How soon a moving creation is read again, doubling to [`READ_MAX`]: the
/// lifecycle UI's clock (`bots::lifecycle`), reused rather than invented.
const READ_FIRST: Duration = Duration::from_millis(250);
const READ_MAX: Duration = Duration::from_secs(2);

/// Where the new Bot's standing role comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum RoleKind {
    /// A new ordinary page, written here.
    Authored,
    /// An ordinary page that already exists in the home workspace.
    Existing,
}

/// An example's wording, which only fills in the role text (EXPERIENCE:
/// examples create nothing).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Example {
    /// *Start your own*: the text emptied.
    StartOwn,
    Research,
    Assessment,
}

impl Example {
    /// The words an example fills in, in the person's language.
    pub(crate) fn text(self) -> Option<Words> {
        match self {
            Self::StartOwn => None,
            Self::Research => Some(Words::BotsExampleResearchText),
            Self::Assessment => Some(Words::BotsExampleAssessmentText),
        }
    }
}

/// Which command is with the link.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Create,
    Read,
    Retry,
    Abandon,
}

/// **The person's draft**: every input of the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Draft {
    pub name: String,
    pub role: RoleKind,
    /// The new page's title; `None` while it follows the name.
    pub title: Option<String>,
    pub text: String,
    pub existing: Option<BotPage>,
    /// The agent, model, effort, mode and Fast; `None` until an agent is
    /// known to offer.
    pub model: Option<ModelDraft>,
    pub access: AccessDraft,
    /// A folder the person chose; `None` is the managed default.
    pub folder: Option<String>,
}

/// What an answer settled, for the tab to act on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    /// The answer was about this draft's creation.
    pub matched: bool,
    /// The owner made the Bot, and it is readable: the tab becomes its tab.
    pub created: Option<Bot>,
}

/// **A create tab's model.**
pub(crate) struct CreateForm {
    request: BotRequestId,
    bot: BotId,
    home: WorkspaceId,
    draft: Draft,
    /// The owner's latest word on this draft's creation, while it holds it:
    /// pending, incomplete, or completed.
    live: Option<BotCreation>,
    /// The previous creation the owner ended — rejected before it made
    /// anything, or abandoned — kept for what the tab says about it.
    ended: Option<BotCreation>,
    in_flight: Option<Sent>,
    /// A Create has gone at least once with these identities: the owner may
    /// hold the creation even if no answer came.
    sent: bool,
    /// **A Create went and no answer said whether the owner took it.** The
    /// inputs stay as they were sent and a read is owed: the owner's
    /// creation, or `not_found` (never taken — the draft is the person's
    /// again, under the same identities). An edit sent under the same
    /// request could otherwise be refused, or worse, the form could say
    /// one thing while the owner makes another.
    uncertain: bool,
    /// **The read that would have said was refused for good**: Koru could
    /// not confirm whether the owner made the Bot. The identities and the
    /// lock stay — the owner may hold the creation and go on to complete
    /// it, and a Create under new identities could make a second Bot and a
    /// second role page — until the person chooses *Start over*.
    unconfirmed: bool,
    notice: Option<Words>,
    /// A read is owed: the tab was shown again, or an answer said the
    /// creation may be other than it looks.
    read_owed: bool,
    next_read: Option<Instant>,
    backoff: Duration,
}

impl CreateForm {
    /// **A new draft for `home`**: fresh identities, the managed folder, the
    /// approved access default, a role to write, no agent until the
    /// catalog offers one.
    pub(crate) fn new(home: WorkspaceId) -> Self {
        Self {
            request: BotRequestId::mint(),
            bot: BotId::mint(),
            home,
            draft: Draft {
                name: String::new(),
                role: RoleKind::Authored,
                title: None,
                text: String::new(),
                existing: None,
                model: None,
                access: AccessDraft {
                    access: BotAccess::Default,
                    pages: Vec::new(),
                },
                folder: None,
            },
            live: None,
            ended: None,
            in_flight: None,
            sent: false,
            uncertain: false,
            unconfirmed: false,
            notice: None,
            read_owed: false,
            next_read: None,
            backoff: READ_FIRST,
        }
    }

    pub(crate) fn request(&self) -> BotRequestId {
        self.request
    }

    /// The BotId this draft reserves: what the link's answers and the
    /// window's asks find the tab by.
    pub(crate) fn bot(&self) -> BotId {
        self.bot
    }

    pub(crate) fn home(&self) -> WorkspaceId {
        self.home
    }

    pub(crate) fn draft(&self) -> &Draft {
        &self.draft
    }

    pub(crate) fn live(&self) -> Option<&BotCreation> {
        self.live.as_ref()
    }

    pub(crate) fn ended(&self) -> Option<&BotCreation> {
        self.ended.as_ref()
    }

    pub(crate) fn notice(&self) -> Option<Words> {
        self.notice
    }

    pub(crate) fn in_flight(&self) -> Option<Sent> {
        self.in_flight
    }

    /// Whether nothing has been sent yet: a draft the window may reuse
    /// rather than open a second form beside it.
    pub(crate) fn untouched(&self) -> bool {
        !self.sent && self.in_flight.is_none() && self.live.is_none() && self.ended.is_none()
    }

    /// **Whether the inputs may change**: nothing on its way, and the owner
    /// holds no creation for this draft. Once it does, Retry resumes what
    /// was recorded, so an edit could not reach it.
    pub(crate) fn editable(&self) -> bool {
        self.in_flight.is_none() && self.live.is_none() && !self.uncertain
    }

    // ------------------------------------------------------------ inputs

    pub(crate) fn edit_name(&mut self, name: &str) {
        if self.editable() {
            self.draft.name = name.to_owned();
        }
    }

    /// The new page's title: the person's, else the name.
    pub(crate) fn title_shown(&self) -> String {
        self.draft
            .title
            .clone()
            .unwrap_or_else(|| self.draft.name.trim().to_owned())
    }

    /// The title after a keystroke. A title back to the name follows it
    /// again.
    pub(crate) fn edit_title(&mut self, title: &str) {
        if self.editable() {
            self.draft.title = (title != self.draft.name.trim()).then(|| title.to_owned());
        }
    }

    pub(crate) fn edit_text(&mut self, text: &str) {
        if self.editable() {
            self.draft.text = text.to_owned();
        }
    }

    /// An example's wording, or *Start your own*'s empty text: only the
    /// text changes, nothing is created.
    pub(crate) fn fill_text(&mut self, text: &str) -> bool {
        if !self.editable() || self.draft.role != RoleKind::Authored {
            return false;
        }
        self.draft.text = text.to_owned();
        true
    }

    pub(crate) fn choose_role(&mut self, role: RoleKind) -> bool {
        if !self.editable() || self.draft.role == role {
            return false;
        }
        self.draft.role = role;
        true
    }

    /// The existing page picked for the role: only a page of the home
    /// workspace, which is where the owner looks for it.
    pub(crate) fn choose_existing(&mut self, page: BotPage) -> bool {
        if !self.editable() || page.workspace != self.home {
            return false;
        }
        self.draft.existing = Some(page);
        true
    }

    /// The agent, model, effort, mode and Fast as the control left them.
    pub(crate) fn model_shown(&self) -> Option<ModelDraft> {
        self.draft.model.clone()
    }

    pub(crate) fn edit_model(&mut self, model: ModelDraft) {
        if self.editable() {
            self.draft.model = Some(model);
        }
    }

    pub(crate) fn access_shown(&self) -> BotAccess {
        self.draft.access.access
    }

    pub(crate) fn pages_shown(&self) -> &[BotPage] {
        &self.draft.access.pages
    }

    pub(crate) fn choose_access(&mut self, access: BotAccess) {
        if self.editable() {
            self.draft.access.access = access;
        }
    }

    /// A page the person selected: only one of the home workspace, once.
    pub(crate) fn add_page(&mut self, page: BotPage) -> bool {
        if !self.editable() || page.workspace != self.home || self.pages_shown().contains(&page) {
            return false;
        }
        self.draft.access.pages.push(page);
        true
    }

    pub(crate) fn remove_page(&mut self, page: &BotPage) -> bool {
        if !self.editable() || !self.pages_shown().contains(page) {
            return false;
        }
        self.draft.access.pages.retain(|kept| kept != page);
        true
    }

    /// A folder the person chose in the folder dialog: an absolute path
    /// the owner then checks.
    pub(crate) fn choose_folder(&mut self, folder: &std::path::Path) -> bool {
        if !self.editable() || !folder.is_absolute() {
            return false;
        }
        let Some(path) = folder.to_str() else {
            return false;
        };
        self.draft.folder = Some(path.to_owned());
        self.notice = None;
        true
    }

    pub(crate) fn use_managed_folder(&mut self) -> bool {
        if !self.editable() || self.draft.folder.is_none() {
            return false;
        }
        self.draft.folder = None;
        true
    }

    /// **The folder the owner bound**, when it differs from what was
    /// chosen — always, for the managed default, which names no path.
    pub(crate) fn resolved_folder(&self) -> Option<&str> {
        let binding = self.live.as_ref()?.folder_binding.as_deref()?;
        (self.draft.folder.as_deref() != Some(binding)).then_some(binding)
    }

    // ----------------------------------------------------------- checks

    /// **The first thing the form refuses, in words**, before anything is
    /// sent: the owner's own limits (`BotCreationConfig::validate`,
    /// `validate_role_text`), said rather than discovered by a refusal.
    pub(crate) fn problem(&self) -> Option<Words> {
        let name = self.draft.name.trim();
        if name.is_empty() {
            return Some(Words::BotsNameRequired);
        }
        if name.len() > NAME_MAX_BYTES {
            return Some(Words::BotsNameTooLong);
        }
        match self.draft.role {
            RoleKind::Authored => {
                let title = self.title_shown();
                if title.trim().is_empty()
                    || title.chars().count() > BOT_ROLE_TITLE_LIMIT
                    || title.chars().any(char::is_control)
                {
                    return Some(Words::BotsRoleTitleInvalid);
                }
                if self.draft.text.len() > BOT_ROLE_MARKDOWN_LIMIT || self.draft.text.contains('\0')
                {
                    return Some(Words::BotsRoleTextTooLong);
                }
            }
            RoleKind::Existing if self.draft.existing.is_none() => {
                return Some(Words::BotsRolePageRequired);
            }
            RoleKind::Existing => {}
        }
        if self.draft.model.is_none() {
            return Some(Words::BotsAgentRequired);
        }
        None
    }

    /// Whether *Create bot* would send: the inputs pass, and nothing is on
    /// its way or held.
    pub(crate) fn can_create(&self) -> bool {
        self.editable() && self.problem().is_none()
    }

    // --------------------------------------------------------- commands

    /// **Create bot**: `CreateBotWithRole` with this draft's request and
    /// BotId. `asking` is the agent's asking mode, the mode a new Bot
    /// starts in unless the person chose one ([`NEW_BOT_PERMISSION`]).
    /// `None` — and nothing sent — on a duplicate press, a held creation or
    /// an input the form refuses.
    pub(crate) fn create(&mut self, asking: Option<String>) -> Option<BotCommand> {
        if !self.can_create() {
            return None;
        }
        let draft = &self.draft;
        let model = draft.model.clone()?;
        let role = match draft.role {
            RoleKind::Authored => BotRoleInput::Authored {
                title: self.title_shown().trim().to_owned(),
                markdown: draft.text.clone(),
            },
            RoleKind::Existing => BotRoleInput::Existing {
                page: draft.existing.clone()?,
            },
        };
        let folder = match &draft.folder {
            Some(path) => BotFolderChoice::Selected { path: path.clone() },
            None => BotFolderChoice::Managed {},
        };
        let mut pages = draft.access.pages.clone();
        pages.sort();
        pages.dedup();
        let command = BotCommand::CreateBotWithRole {
            request_id: self.request,
            bot_id: self.bot,
            home_workspace: self.home,
            role,
            folder,
            config: BotCreationConfig {
                name: draft.name.trim().to_owned(),
                model: model.model,
                mode: model.mode.or(asking),
                fast: model.fast,
            },
            access: draft.access.access,
            selected_pages: pages,
        };
        self.in_flight = Some(Sent::Create);
        self.sent = true;
        self.notice = None;
        self.ended = None;
        Some(command)
    }

    /// **Retry**: resume the stopped creation from what the owner recorded,
    /// by its request alone. Only when the owner says Retry can resolve it.
    pub(crate) fn retry(&mut self) -> Option<BotCommand> {
        let live = self.live.as_ref()?;
        if self.in_flight.is_some() || !live.retryable {
            return None;
        }
        self.in_flight = Some(Sent::Retry);
        self.notice = None;
        Some(BotCommand::RetryBotCreation {
            request_id: self.request,
        })
    }

    /// Whether Abandon is offered: the owner holds a creation that has not
    /// finished.
    pub(crate) fn can_abandon(&self) -> bool {
        self.in_flight.is_none()
            && self.live.as_ref().is_some_and(|live| {
                matches!(
                    live.stage,
                    BotCreationStage::Pending | BotCreationStage::Incomplete
                )
            })
    }

    /// **Abandon**, once the person confirmed it: nothing is deleted, and
    /// the answer names what it leaves.
    pub(crate) fn abandon(&mut self) -> Option<BotCommand> {
        if !self.can_abandon() {
            return None;
        }
        self.in_flight = Some(Sent::Abandon);
        self.notice = None;
        Some(BotCommand::AbandonBotCreation {
            request_id: self.request,
        })
    }

    /// **The tab was shown**, for the first time or after being hidden: a
    /// creation the owner may hold is read once.
    pub(crate) fn shown(&mut self) {
        let finished = self
            .live
            .as_ref()
            .is_some_and(|live| live.stage == BotCreationStage::Completed);
        if self.sent && !finished {
            self.read_owed = true;
        }
    }

    /// The `ReadBotCreation` owed by `now`: once after showing, or on the
    /// bounded clock while the creation moves. Never while another command
    /// is with the link.
    pub(crate) fn read(&mut self, now: Instant) -> Option<BotCommand> {
        if self.in_flight.is_some() {
            return None;
        }
        let due = self.next_read.is_some_and(|at| at <= now);
        if !self.read_owed && !due {
            return None;
        }
        self.read_owed = false;
        self.next_read = None;
        self.in_flight = Some(Sent::Read);
        Some(BotCommand::ReadBotCreation {
            request_id: self.request,
        })
    }

    /// The clock stops while the tab is hidden: the next showing reads once
    /// and starts it again from its first step.
    pub(crate) fn hidden(&mut self) {
        self.next_read = None;
        self.backoff = READ_FIRST;
    }

    /// When a pass is next owed: a short tick while a command is with the
    /// link, else the clock while the creation moves.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        if self.in_flight.is_some() {
            return now.checked_add(Duration::from_millis(50));
        }
        self.next_read
    }

    /// The link would not take the command: nothing is on its way.
    pub(crate) fn unsent(&mut self) {
        if self.in_flight.take().is_some() {
            self.notice = Some(Words::BotsErrUnreachable);
        }
    }

    /// **What the owner said about `request`.** An answer about another
    /// request settles nothing.
    pub(crate) fn answer(
        &mut self,
        request: BotRequestId,
        result: Result<BotResponse, Failure>,
        now: Instant,
    ) -> Settled {
        if request != self.request {
            return Settled::default();
        }
        let sent = self.in_flight.take();
        let mut settled = Settled {
            matched: true,
            created: None,
        };
        match result {
            Ok(BotResponse::Creation { creation })
                if creation.request_id == self.request && creation.bot_id == self.bot =>
            {
                self.notice = None;
                self.uncertain = false;
                self.unconfirmed = false;
                match creation.stage {
                    BotCreationStage::Pending => {
                        let wait = if self
                            .live
                            .as_ref()
                            .is_some_and(|live| live.stage == BotCreationStage::Pending)
                            && sent == Some(Sent::Read)
                        {
                            (self.backoff * 2).min(READ_MAX)
                        } else {
                            READ_FIRST
                        };
                        self.backoff = wait;
                        self.next_read = Some(now.checked_add(wait).unwrap_or(now));
                        self.live = Some(creation);
                    }
                    BotCreationStage::Incomplete => {
                        self.next_read = None;
                        self.backoff = READ_FIRST;
                        self.live = Some(creation);
                    }
                    BotCreationStage::Completed => {
                        self.next_read = None;
                        settled.created = creation.bot.clone();
                        self.live = Some(creation);
                    }
                    // Ended by the owner: the draft stays, under new
                    // identities, for the person to create again.
                    BotCreationStage::Rejected | BotCreationStage::Abandoned => {
                        self.next_read = None;
                        self.ended = Some(creation);
                        self.renew();
                    }
                }
            }
            // **The outcome is unknown**: no answer, or one about something
            // else, which is no answer either — a lost connection, a worker
            // that timed out. The owner may hold the creation, so nothing
            // is said to be not applied: what it holds is read.
            Ok(_) | Err(Failure::Unanswered) => match sent {
                // Only the question about it went unanswered: ask again on
                // the clock.
                Some(Sent::Read) => self.read_again(now),
                // A Create unanswered: the owner may hold it. Read what it
                // holds before the person can change anything.
                Some(Sent::Create) => {
                    self.notice = Some(Words::BotsErrUnreachable);
                    self.uncertain = true;
                    self.read_owed = true;
                }
                // A Retry or an Abandon unanswered: the creation's stage
                // says what happened.
                _ => {
                    self.notice = Some(Words::BotsErrUnreachable);
                    self.read_owed = self.sent || self.live.is_some();
                }
            },
            Err(Failure::Refused(code)) => {
                let again = Failure::Refused(code.clone()).again();
                match (sent, code.as_str()) {
                    // A read of a creation the owner never accepted: there
                    // is none, and the draft is the person's to send.
                    (Some(Sent::Read), "not_found") if self.live.is_none() => {
                        self.uncertain = false;
                        self.unconfirmed = false;
                    }
                    // A read the owner turned away for now — busy, or what
                    // the Bot needs not reachable: asked again on the clock.
                    (Some(Sent::Read), _) if again => self.read_again(now),
                    // **A read refused for good while the form does not know
                    // whether the owner took the Create**: the owner may
                    // hold it and complete it, so nothing is said to be not
                    // done and the identities and the lock stay. The form
                    // says it could not confirm, and to look in Bots — a
                    // creation the owner holds is listed there — and offers
                    // *Start over*, which alone takes new identities.
                    (Some(Sent::Read), _) if self.uncertain => {
                        self.notice = Some(Words::BotsCreateUnconfirmed);
                        self.unconfirmed = true;
                    }
                    // The same request with other inputs: show what the
                    // owner holds.
                    (_, "request_conflict") => {
                        self.notice = Some(Words::BotsErrRequestConflict);
                        self.read_owed = true;
                    }
                    (Some(Sent::Create), "invalid_request") if self.draft.folder.is_some() => {
                        self.notice = Some(Words::BotsFolderRefused);
                    }
                    // **The chosen agent cannot run here**, as the owner's
                    // creation check says when it asks the agent for its
                    // models (`validate_model_evidence`, `apps/koru-ai-server/
                    // src/worker/bots.rs`): its own sentence, naming the
                    // agent at draw time, never *That wasn't done*.
                    (_, code) if agent_refusal(code).is_some() => {
                        self.notice = agent_refusal(code);
                    }
                    _ => self.notice = Some(super::settings::general_refusal_words(&code)),
                }
            }
        }
        settled
    }

    /// Whether *Start over* is offered: Koru could not confirm whether the
    /// owner made the Bot, and nothing is on its way.
    pub(crate) fn can_start_over(&self) -> bool {
        self.unconfirmed && self.in_flight.is_none()
    }

    /// **Start over**, the person's choice after Koru could not confirm the
    /// creation: the draft stays, under new identities, editable again.
    /// The creation the owner may hold is left to the roster's Unfinished
    /// group. `false` when it is not offered.
    pub(crate) fn start_over(&mut self) -> bool {
        if !self.can_start_over() {
            return false;
        }
        self.notice = None;
        self.renew();
        true
    }

    /// The read went unanswered, or was turned away for now: asked again
    /// on the bounded clock (doubling to two seconds), never while hidden.
    fn read_again(&mut self, now: Instant) {
        self.backoff = (self.backoff * 2).min(READ_MAX);
        self.next_read = Some(now.checked_add(self.backoff).unwrap_or(now));
    }

    /// New identities for the next attempt, after the owner ended the last.
    fn renew(&mut self) {
        self.request = BotRequestId::mint();
        self.bot = BotId::mint();
        self.live = None;
        self.sent = false;
        self.uncertain = false;
        self.unconfirmed = false;
        self.read_owed = false;
        self.next_read = None;
        self.backoff = READ_FIRST;
    }
}

/// **A creation's reason as a sentence**, by its stable code: every one of
/// the twelve.
pub(crate) fn reason_words(reason: BotCreationReason) -> Words {
    match reason {
        BotCreationReason::OwnerUnavailable => Words::BotsCreateOwnerUnavailable,
        BotCreationReason::StoreUnavailable => Words::BotsCreateStoreUnavailable,
        BotCreationReason::FolderUnavailable => Words::BotsCreateFolderUnavailable,
        BotCreationReason::FolderOccupied => Words::BotsCreateFolderOccupied,
        BotCreationReason::FolderMismatch => Words::BotsCreateFolderMismatch,
        BotCreationReason::FolderInvalid => Words::BotsCreateFolderInvalid,
        BotCreationReason::RoleConflict => Words::BotsCreateRoleConflict,
        BotCreationReason::RoleUnavailable => Words::BotsCreateRoleUnavailable,
        BotCreationReason::PageUnavailable => Words::BotsCreatePageUnavailable,
        BotCreationReason::StorageFailed => Words::BotsCreateStorageFailed,
        BotCreationReason::RoleMissing => Words::BotsCreateRoleMissing,
        BotCreationReason::StoreRefused => Words::BotsCreateStoreRefused,
    }
}

/// **Where a creation stands, in sentences**: the reason it stopped, then
/// what Retry or Abandon can do — or that it is still being made.
pub(crate) fn stage_words(creation: &BotCreation) -> Vec<Words> {
    let mut words = Vec::new();
    match creation.stage {
        BotCreationStage::Pending => words.push(Words::BotsCreateStillWorking),
        BotCreationStage::Incomplete => {
            if let Some(reason) = creation.reason {
                words.push(reason_words(reason));
            }
            words.push(if creation.retryable {
                Words::BotsCreateIncomplete
            } else {
                Words::BotsCreateTerminal
            });
        }
        BotCreationStage::Rejected => {
            if let Some(reason) = creation.reason {
                words.push(reason_words(reason));
            }
            words.push(Words::BotsCreateRejected);
        }
        BotCreationStage::Completed => words.push(Words::BotsCreateCreated),
        BotCreationStage::Abandoned => {}
    }
    words
}

/// **The codes a Create is refused with when its agent cannot run**, and
/// the sentence each says, naming the agent (`{}`). The owner asks the
/// agent for its models before it creates anything
/// (`validate_model_evidence`, `apps/koru-ai-server/src/worker/bots.rs`):
/// no session host or a model it does not list is `runtime_unavailable`, a
/// listing that failed is `agent_models_failed` (the session host's
/// `provider_unavailable` beneath it, which the owner does not pass on but
/// is mapped too should it ever be), and one still being built is
/// `agent_models_listing`. **None of them says why** — not installed, not
/// signed in, too old — so the sentence says what is true of all of them.
pub(crate) const AGENT_REFUSALS: [(&str, Words); 4] = [
    ("runtime_unavailable", Words::BotsCreateAgentNotReady),
    ("agent_models_failed", Words::BotsCreateAgentNotReady),
    ("provider_unavailable", Words::BotsCreateAgentNotReady),
    ("agent_models_listing", Words::BotsCreateAgentStillListing),
];

/// The agent sentence for a Create refused with `code`, if it is one.
pub(crate) fn agent_refusal(code: &str) -> Option<Words> {
    AGENT_REFUSALS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, words)| *words)
}

/// Whether `words` names the draft's agent (`{}`), to be filled at draw.
pub(crate) fn names_the_agent(words: Words) -> bool {
    AGENT_REFUSALS.iter().any(|(_, known)| *known == words)
}

#[cfg(test)]
mod tests;
