// SPDX-License-Identifier: MPL-2.0

//! **A Bot's tab while another computer runs the Bot** (Bots M8, P8.3;
//! contract 6.3): the tab's half of `bots::linked::page`.
//!
//! The tab learns that its Bot is another computer's in one of two ways:
//! the roster's row says so ([`RosterBot::owner`](crate::bots::roster::RosterBot)),
//! or the Bot's own read names an owner that is not this device, once the
//! window has said which device this is ([`BotView::set_here`]). From then
//! on it holds a [`CopyPage`] and:
//!
//! - **reads the copy's state on its own link**, when shown and every few
//!   seconds while shown, never while hidden. A read that says that
//!   computer's copy does not list the Bot ends it: the tab is an ordinary
//!   Bot's from there on, and asks no more;
//! - **tells its conversation** what it may send and what a refusal about
//!   the other computer reads as (`ChatSurface::set_saved_copy`);
//! - **says, in its header**, that this is a saved copy and as of when, and
//!   what the last command came to.
//!
//! A tab whose Bot this computer runs holds no page, reads nothing more and
//! draws nothing more: it is exactly the tab it was.

use std::time::Instant;

use koru_ai_core::bots::{Bot, BotId, BotRequestId, DeviceId, LinkedOwnerState};
use koru_draw::el::Key;

use super::{BotView, SURFACE};
use crate::bots::link::{Command, Failure};
use crate::bots::linked::Names;
use crate::bots::linked::page::{CopyPage, Read};

/// The sentence saying the page is a saved copy, in the header.
pub(crate) const COPY_BANNER: Key = Key::surface(SURFACE, "copy-banner");
/// What the last command to the other computer came to, under it.
pub(crate) const COPY_LINE: Key = Key::surface(SURFACE, "copy-line");

/// What the tab holds about the other computer. All absent for a Bot this
/// computer runs.
#[derive(Debug, Default)]
pub(crate) struct Copy {
    /// This device, as the window knows it: what a Bot's owner is compared
    /// with. `None` until the window says.
    here: Option<DeviceId>,
    /// The other computer's name, as the window's device list has it.
    name: Option<String>,
    page: Option<CopyPage>,
    /// A read said that computer's copy does not list this Bot: the owner
    /// it was read for, so the same answer is not asked for again.
    not_a_copy: Option<DeviceId>,
}

impl Copy {
    pub(crate) fn page(&self) -> Option<&CopyPage> {
        self.page.as_ref()
    }

    /// Take `owner` as the computer that runs the Bot, or none.
    fn set_owner(&mut self, owner: Option<DeviceId>) -> bool {
        let owner = owner.filter(|owner| Some(*owner) != self.not_a_copy);
        if self.page.as_ref().map(CopyPage::owner) == owner {
            return false;
        }
        self.page = owner.map(|owner| CopyPage::new(owner, None));
        true
    }
}

impl BotView {
    /// The saved-copy state of this tab, while another computer runs its
    /// Bot.
    pub(crate) fn copy(&self) -> Option<&CopyPage> {
        self.copy.page()
    }

    /// **Which device this is**, as the window knows it: a Bot whose owner
    /// is another device is that computer's. `true` when it changed.
    pub(crate) fn set_here(&mut self, here: Option<DeviceId>) -> bool {
        if self.copy.here == here {
            return false;
        }
        self.copy.here = here;
        true
    }

    /// **The names the window knows its linked devices by**: the one this
    /// tab's other computer wears. `true` when the tab's words moved.
    pub(crate) fn set_computer_names(&mut self, names: &Names) -> bool {
        let Some(owner) = self.copy.page().map(CopyPage::owner) else {
            return false;
        };
        let name = names.known(owner).map(str::to_owned);
        if self.copy.name == name {
            return false;
        }
        self.copy.name = name;
        // The conversation's sentences name the computer too.
        self.push_saved_copy();
        true
    }

    /// **What the roster's row says of the copy now**, when it lists this
    /// Bot under another computer. `true` when what the tab says changed.
    pub(crate) fn know_copy(
        &mut self,
        owner: DeviceId,
        state: LinkedOwnerState,
        last_confirmed_at_ms: Option<i64>,
    ) -> bool {
        let changed = match &mut self.copy.page {
            Some(page) if page.owner() == owner => page.know(state, last_confirmed_at_ms),
            _ => false,
        };
        if changed {
            // Held or open from this moment, not from the next pass.
            self.push_saved_copy();
        }
        changed
    }

    /// The computer that runs `bot`, when the window has said which device
    /// this is and it is another — and that computer's copy has not already
    /// said it does not list the Bot.
    pub(super) fn remote_owner(&self, bot: &Bot) -> Option<DeviceId> {
        self.copy
            .here
            .filter(|here| *here != bot.owner_device)
            .map(|_| bot.owner_device)
            .filter(|owner| Some(*owner) != self.copy.not_a_copy)
    }

    /// The tab's Bot is `owner`'s (or this computer's, `None`).
    pub(super) fn set_copy_owner(&mut self, owner: Option<DeviceId>) {
        if self.copy.set_owner(owner) {
            self.copy.name = None;
            self.push_saved_copy();
        }
    }

    /// The other computer's name, or *another computer*.
    fn copy_computer(&self) -> String {
        match self.copy.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => koru_i18n::t(self.lang, koru_i18n::Key::BotsLinkedAnotherComputer).to_owned(),
        }
    }

    /// The header's saved-copy sentence, while there is one to say.
    pub(crate) fn copy_banner(&self) -> Option<String> {
        self.copy
            .page()?
            .banner(self.lang, &self.copy_computer(), |at| {
                crate::format::absolute_local(self.lang, at)
            })
    }

    /// What the last command to the other computer came to.
    pub(crate) fn copy_line(&self) -> Option<String> {
        self.copy.page()?.line(self.lang, &self.copy_computer())
    }

    /// Tell the conversation what the copy lets it do. `true` when what it
    /// draws changed.
    pub(super) fn push_saved_copy(&mut self) -> bool {
        let copy = self
            .copy
            .page()
            .map(|page| page.surface(self.lang, &self.copy_computer()));
        self.conversation_mut()
            .is_some_and(|conversation| conversation.set_saved_copy(copy))
    }

    /// **The shown tab's pass over its copy**: read its state when a read is
    /// owed, and keep the conversation told. Nothing for a Bot this computer
    /// runs. `true` when a repaint is owed.
    pub(super) fn copy_pass(&mut self) -> bool {
        let pushed = self.push_saved_copy();
        let (Some(bot), Some(home)) = (self.bot, self.facts.as_ref().map(|facts| facts.home))
        else {
            return pushed;
        };
        let Some(page) = &mut self.copy.page else {
            return pushed;
        };
        if !page.read_due(Instant::now()) {
            return pushed;
        }
        let seq = page.reading();
        let command = Command::LinkedOwner {
            seq,
            bot,
            owner: page.owner(),
            workspace: home,
        };
        // A full queue leaves the read owed: the next pass asks again.
        if !self.link.as_ref().is_some_and(|link| link.send(command)) {
            page.unsent();
        }
        true
    }

    /// What a read of the copy's state answered. `true` when it was this
    /// tab's and what it says changed.
    pub(super) fn copy_answer(
        &mut self,
        seq: u64,
        bot: BotId,
        result: Result<Option<(LinkedOwnerState, Option<i64>)>, Failure>,
    ) -> bool {
        if Some(bot) != self.bot {
            return false;
        }
        let Some(page) = &mut self.copy.page else {
            return false;
        };
        let owner = page.owner();
        match page.take(seq, result, Instant::now()) {
            Read::Same => false,
            Read::Changed => {
                self.push_saved_copy();
                true
            }
            // That computer does not run this Bot as far as this one
            // knows: an ordinary Bot's tab, and not asked again.
            Read::NotACopy => {
                self.copy.not_a_copy = Some(owner);
                self.copy.page = None;
                self.copy.name = None;
                if let Some(facts) = &mut self.facts {
                    facts.owner = None;
                }
                self.push_saved_copy();
                true
            }
        }
    }

    /// The link noted how far a refused command of this Bot got.
    pub(super) fn copy_delivery(&mut self, bot: BotId, request: BotRequestId, delivery: String) {
        if Some(bot) == self.bot
            && let Some(page) = &mut self.copy.page
        {
            page.note_delivery(request, delivery);
        }
    }

    /// A command of this Bot was answered: what the header says of it.
    pub(super) fn copy_wrote<T>(
        &mut self,
        request: BotRequestId,
        result: &Result<T, Failure>,
    ) -> bool {
        self.copy
            .page
            .as_mut()
            .is_some_and(|page| page.wrote(request, result))
    }

    /// The tab was hidden: shown again, the copy's state is read once.
    pub(super) fn copy_hidden(&mut self) {
        if let Some(page) = &mut self.copy.page {
            page.hidden();
        }
    }

    /// **When the tab next needs the clock for its copy**, while a page is
    /// held: soon while a read is owed or on its way, as the tab's other
    /// reads are paced, else when the state is read again.
    pub(super) fn copy_deadline(&self, now: Instant) -> Option<Instant> {
        let page = self.copy.page()?;
        if page.read_due(now) || page.in_flight() {
            return Some(
                now.checked_add(std::time::Duration::from_millis(50))
                    .unwrap_or(now),
            );
        }
        page.deadline()
    }

    /// The tab's Bot is another computer's, as a test needs it.
    #[cfg(test)]
    pub(crate) fn copy_for_test(
        &mut self,
        owner: DeviceId,
        state: LinkedOwnerState,
        last_confirmed_at_ms: Option<i64>,
    ) {
        self.set_copy_owner(Some(owner));
        self.know_copy(owner, state, last_confirmed_at_ms);
        self.push_saved_copy();
    }
}

#[cfg(test)]
mod tests;
