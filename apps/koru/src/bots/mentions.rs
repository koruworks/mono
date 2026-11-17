// SPDX-License-Identifier: MPL-2.0

//! **The Bots a Bot's conversation lets the person mention.**
//!
//! Typing `@` in a Bot's conversation lists, above the pages, the Bots that
//! Bot may ask. Picking one puts a chip in the message; sending it tells the
//! Bot's agent the person wants that Bot asked. **A mention is a hint.** The
//! agent makes the call (`ask_bot`) and every check on that call applies as
//! it always did: this module decides only what the picker offers, never
//! what is allowed.
//!
//! ## Who is offered
//!
//! The set the service itself answers to an agent that lists whom it may ask
//! (`list_askable_bots`): the person's own Bots **with the same home
//! workspace, run by this computer, active, and not the Bot itself**. A
//! paused Bot is listed and marked *Paused*, because asking it is refused
//! until it is resumed. **None at all** when this Bot's *May ask other bots*
//! is off, when asking is closed in this build, while the tab has not read
//! its Bot (it does not know the setting), for an archived or trashed Bot,
//! and for a Bot another computer runs (its askable Bots are that
//! computer's, which this one does not list).
//!
//! ## Where the list comes from, and how it stays fresh
//!
//! - **At once, from what the window already holds**: the roster's rows, as
//!   it last listed them ([`Mentions::seed`]). The roster lists only while it
//!   is shown, so this may be old — it is what makes the first `@` instant.
//! - **Then from the service**, on the tab's own link thread: when the
//!   picker is open and what is held is older than [`STALE_AFTER`], the tab
//!   asks for the workspace's Bots (`link::Command::List`, the roster's own
//!   read) and the answer replaces the list, in the open picker too. One
//!   ask at a time; a failed one keeps what was listed and is asked again
//!   on the next `@` after [`ASK_PATIENCE`] at most. **Nothing here blocks**:
//!   the window thread only queues the ask and folds the answer.
//!
//! The ordinary AI chat is never fed: only a Bot's tab calls this.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use koru_ai_core::bots::{Bot, BotId, BotLifecycle, DeviceId};
use koru_ai_ui::{ExtraChoice, ExtraSection};
use koru_i18n::{Key as Words, Lang, t};
use koru_store_core::ids::WorkspaceId;

use super::link::Failure;
use super::roster::RosterBot;

/// A list older than this is asked for again when the picker is open.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(5);
/// How long an ask that has not been answered holds off another.
pub(crate) const ASK_PATIENCE: Duration = Duration::from_secs(30);

/// One Bot a listing named, as far as a mention cares.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Listed {
    pub id: BotId,
    pub name: String,
    pub paused: bool,
    pub home: WorkspaceId,
    pub archived: bool,
    /// The other computer that runs it; `None` for one this computer runs.
    pub owner: Option<DeviceId>,
}

impl From<&RosterBot> for Listed {
    fn from(row: &RosterBot) -> Self {
        Self {
            id: row.id,
            name: row.name.clone(),
            paused: row.paused,
            home: row.home,
            archived: row.archived,
            owner: row.owner,
        }
    }
}

impl Listed {
    /// A Bot of the service's own listing. `here` is this computer, when
    /// the window knows which it is: a Bot another device owns is that
    /// computer's.
    pub(crate) fn of(bot: &Bot, here: Option<DeviceId>) -> Self {
        Self {
            id: bot.id,
            name: bot.config.name.clone(),
            paused: bot.paused,
            home: bot.home_workspace,
            archived: bot.lifecycle != BotLifecycle::Active,
            owner: here
                .filter(|here| *here != bot.owner_device)
                .map(|_| bot.owner_device),
        }
    }
}

/// The Bot whose conversation it is, as its tab knows it from the owner's
/// own read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Asker {
    pub id: BotId,
    pub home: WorkspaceId,
    /// Its *May ask other bots*, as the owner holds it.
    pub may_ask: bool,
}

/// **Who the picker offers**, in the order they were listed. See the module
/// documentation for the rule; `asker` is `None` whenever the tab may not
/// offer anybody.
pub(crate) fn askable<'a>(asker: Option<&Asker>, listed: &'a [Listed]) -> Vec<&'a Listed> {
    let Some(asker) = asker.filter(|asker| asker.may_ask) else {
        return Vec::new();
    };
    listed
        .iter()
        .filter(|bot| {
            bot.id != asker.id && bot.home == asker.home && !bot.archived && bot.owner.is_none()
        })
        .collect()
}

/// The picker's section for `asker`: the askable Bots under the word *Bot*,
/// a paused one marked. Empty — the picker with pages only — when nobody is
/// offered.
pub(crate) fn section(lang: Lang, asker: Option<&Asker>, listed: &[Listed]) -> ExtraSection {
    let choices: Vec<ExtraChoice> = askable(asker, listed)
        .into_iter()
        .map(|bot| ExtraChoice {
            id: bot.id.as_uuid().to_string(),
            title: bot.name.trim().to_owned(),
            note: bot
                .paused
                .then(|| t(lang, Words::BotsMentionPaused).to_owned()),
        })
        // A Bot with no name cannot be picked by name.
        .filter(|choice| !choice.title.is_empty())
        .collect();
    if choices.is_empty() {
        return ExtraSection::default();
    }
    ExtraSection {
        label: t(lang, Words::BotsMentionKind).to_owned(),
        role: koru_core::attributes::ROLE_BOT.to_owned(),
        icon: Some(koru_draw::icons::ROBOT),
        choices,
    }
}

/// The names the window knows Bots by now, by id: what a mention in a sent
/// message reads as. Every listed Bot, askable or not — a mention sent
/// earlier still reads as its Bot's current name.
pub(crate) fn names(listed: &[Listed]) -> HashMap<uuid::Uuid, String> {
    listed
        .iter()
        .filter(|bot| !bot.name.trim().is_empty())
        .map(|bot| (bot.id.as_uuid(), bot.name.trim().to_owned()))
        .collect()
}

/// What a Bot's tab holds for its conversation's mentions.
#[derive(Default)]
pub(crate) struct Mentions {
    listed: Vec<Listed>,
    /// When the tab's own read last answered. `None` while what is held is
    /// only what the window's roster listed.
    read_at: Option<Instant>,
    seq: u64,
    /// An ask on its way, and since when.
    asked: Option<Instant>,
    /// What was last handed to the conversation: rebuilt only when what it
    /// is made from moved.
    fed: Option<(Lang, Option<Asker>)>,
    section: ExtraSection,
    names: HashMap<uuid::Uuid, String>,
}

impl Mentions {
    /// The Bots held, as last listed. Test only.
    #[cfg(test)]
    pub(crate) fn listed(&self) -> &[Listed] {
        &self.listed
    }

    /// **What the window's roster lists**, taken only until the tab's own
    /// read has answered: after that the tab's read is the fresher one.
    /// `true` when the list changed.
    pub(crate) fn seed(&mut self, rows: impl IntoIterator<Item = Listed>) -> bool {
        if self.read_at.is_some() {
            return false;
        }
        let rows: Vec<Listed> = rows.into_iter().collect();
        if self.listed == rows {
            return false;
        }
        self.listed = rows;
        self.fed = None;
        true
    }

    /// Whether an ask should go now: what is held is the roster's or older
    /// than [`STALE_AFTER`], and no ask is on its way within
    /// [`ASK_PATIENCE`].
    pub(crate) fn due(&self, now: Instant) -> bool {
        let fresh = self
            .read_at
            .is_some_and(|at| now.saturating_duration_since(at) < STALE_AFTER);
        let waiting = self
            .asked
            .is_some_and(|at| now.saturating_duration_since(at) < ASK_PATIENCE);
        !fresh && !waiting
    }

    /// An ask is being sent: its sequence number.
    pub(crate) fn asking(&mut self, now: Instant) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.asked = Some(now);
        self.seq
    }

    /// The link did not take the ask: nothing is on its way.
    pub(crate) fn unsent(&mut self) {
        self.asked = None;
    }

    /// Whether an ask is on its way: the tab polls for its answer.
    pub(crate) fn waiting(&self, now: Instant) -> bool {
        self.asked
            .is_some_and(|at| now.saturating_duration_since(at) < ASK_PATIENCE)
    }

    /// **The service's listing.** Only the latest ask's answer, for the
    /// workspace asked about, is folded; a failed one keeps what was listed
    /// (a failed read is never *there are no Bots*). `true` when the list
    /// changed.
    pub(crate) fn answer(
        &mut self,
        seq: u64,
        asked_for: Option<WorkspaceId>,
        workspace: WorkspaceId,
        result: Result<Vec<Bot>, Failure>,
        here: Option<DeviceId>,
        now: Instant,
    ) -> bool {
        if seq != self.seq || self.asked.is_none() {
            return false;
        }
        self.asked = None;
        let Ok(bots) = result else {
            return false;
        };
        if asked_for != Some(workspace) {
            return false;
        }
        self.read_at = Some(now);
        let rows: Vec<Listed> = bots.iter().map(|bot| Listed::of(bot, here)).collect();
        if self.listed == rows {
            return false;
        }
        self.listed = rows;
        self.fed = None;
        true
    }

    /// **What the conversation is fed**: the picker's section and the
    /// names, rebuilt only when the language, the asker or the list moved.
    pub(crate) fn feed(
        &mut self,
        lang: Lang,
        asker: Option<Asker>,
    ) -> (&ExtraSection, &HashMap<uuid::Uuid, String>) {
        if self.fed != Some((lang, asker)) {
            self.section = section(lang, asker.as_ref(), &self.listed);
            self.names = names(&self.listed);
            self.fed = Some((lang, asker));
        }
        (&self.section, &self.names)
    }
}

#[cfg(test)]
mod tests;
