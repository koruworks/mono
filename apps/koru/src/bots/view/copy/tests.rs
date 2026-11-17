// SPDX-License-Identifier: MPL-2.0

//! A Bot's tab while another computer runs its Bot, over a link the test
//! answers: what the header says, what the conversation is told, what is
//! read and when — and that a tab of this computer's own Bot is untouched.

use std::sync::mpsc::{Receiver, SyncSender};

use koru_store_core::ids::WorkspaceId;
use koru_workspace_host::View;

use super::*;
use crate::bots::link::{Answer, BotsLink};
use crate::bots::linked::{EXECUTING_COMPUTER_ONLY, NOT_CONFIRMED, NOT_SENT, OWNER_UNREACHABLE};
use crate::bots::roster::RosterBot;
use crate::bots::tests::{assert_publishable, bot, bot_tab_tree};

struct Fixture {
    view: BotView,
    commands: Receiver<Command>,
    answers: SyncSender<Answer>,
    bot: Bot,
}

/// A Bot tab that knows its Bot, over a link the test answers.
fn fixture() -> Fixture {
    let (link, commands, answers) = BotsLink::test_pair();
    let bot = bot(BotId::mint(), WorkspaceId::new(), "Scout", false);
    let mut view = BotView::with_link(bot.id, bot.main_chat, link);
    view.set_viewport(640.0, 480.0);
    view.know_bot_for_test(bot.clone());
    Fixture {
        view,
        commands,
        answers,
        bot,
    }
}

impl Fixture {
    /// The reads of a copy's state the tab sent since the last look, as
    /// their sequence, Bot, computer and workspace. Anything else it asked
    /// is not this test's.
    fn copy_reads(&self) -> Vec<(u64, BotId, DeviceId, WorkspaceId)> {
        std::iter::from_fn(|| self.commands.try_recv().ok())
            .filter_map(|command| match command {
                Command::LinkedOwner {
                    seq,
                    bot,
                    owner,
                    workspace,
                } => Some((seq, bot, owner, workspace)),
                _ => None,
            })
            .collect()
    }

    fn answer(&mut self, answer: Answer) -> bool {
        self.answers.send(answer).expect("the tab's end is open");
        let changed = self.view.poll();
        assert_publishable("a Bot tab", &[bot_tab_tree(&self.view)]);
        changed
    }

    /// The header's line under `key`, as it is published.
    fn line(&self, key: Key) -> Option<String> {
        self.view
            .header_lines()
            .into_iter()
            .find(|(line, _, _)| *line == key)
            .map(|(_, _, words)| words)
    }

    /// What the conversation was told its copy lets it do.
    fn told(&self) -> Option<koru_ai_ui::SavedCopy> {
        self.view
            .conversation()
            .and_then(|conversation| conversation.saved_copy().cloned())
    }

    fn desk(&mut self, owner: DeviceId) {
        self.view
            .set_computer_names(&Names::new([(owner, "Desk".to_owned())]));
    }
}

/// **A tab of a Bot this computer runs is the tab it was**: it holds no
/// copy, reads nothing more, says nothing more in its header, and its
/// conversation is told nothing — also once the window has said which
/// device this is and the Bot's read names that same device.
#[test]
fn m8_ui_copy_banner_a_tab_of_this_computers_own_bot_is_unchanged() {
    let mut f = fixture();
    assert!(f.view.copy().is_none());
    assert!(!f.view.copy_pass(), "nothing to read, nothing to say");
    assert!(f.copy_reads().is_empty());
    assert_eq!(f.line(COPY_BANNER), None);
    assert_eq!(f.line(COPY_LINE), None);
    assert_eq!(f.told(), None);
    assert_eq!(f.view.copy_deadline(std::time::Instant::now()), None);

    assert!(f.view.set_here(Some(f.bot.owner_device)));
    assert!(!f.view.set_here(Some(f.bot.owner_device)), "said already");
    f.view.take(Ok(f.bot.clone()));
    assert!(f.view.copy().is_none(), "this device's own Bot");
    assert!(!f.view.copy_pass());
    assert!(f.copy_reads().is_empty());
    assert_eq!(f.told(), None);
    assert!(!f.view.set_computer_names(&Names::default()));
}

/// **A Bot whose owner is another device is that computer's**: the tab
/// reads that computer's copy on its own link, and from the answer says
/// that this is a saved copy and as of when, holds the conversation's sends
/// with the sentence that keeps the draft, and names the computer as the
/// window does. Connected, it says nothing and holds nothing. A read that
/// says that computer's copy does not list the Bot ends it for good.
#[test]
fn m8_ui_copy_banner_another_devices_bot_is_a_saved_copys_tab() {
    let mut f = fixture();
    let owner = f.bot.owner_device;
    f.view.set_here(Some(DeviceId::new()));
    f.view.take(Ok(f.bot.clone()));
    let page = f.view.copy().expect("another device runs it");
    assert_eq!(page.owner(), owner);
    assert_eq!(page.state(), None, "not read yet");
    assert_eq!(f.line(COPY_BANNER), None, "nothing is said on a guess");
    assert_eq!(f.told().and_then(|copy| copy.held), None, "nor held");

    // Its own read, for that computer and the Bot's home.
    while f.commands.try_recv().is_ok() {}
    assert!(f.view.copy_pass());
    let reads = f.copy_reads();
    assert_eq!(reads.len(), 1);
    let (seq, bot, asked, workspace) = reads[0];
    assert_eq!(
        (bot, asked, workspace),
        (f.bot.id, owner, f.bot.home_workspace)
    );
    assert!(!f.view.copy_pass(), "one read at a time");
    assert!(f.copy_reads().is_empty());

    // Not reachable, as of a time.
    assert!(f.answer(Answer::LinkedOwner {
        seq,
        bot,
        result: Ok(Some((LinkedOwnerState::Stale, Some(0)))),
    }));
    let banner = f.line(COPY_BANNER).expect("a saved copy says so");
    assert!(
        banner.starts_with("This is a saved copy from another computer, last updated "),
        "{banner}"
    );
    assert_eq!(
        f.told().and_then(|copy| copy.held).as_deref(),
        Some("another computer is not reachable. Your draft is kept here.")
    );

    // Named as the window's device list names it.
    f.desk(owner);
    f.view.copy_pass();
    let banner = f.line(COPY_BANNER).expect("still a saved copy");
    assert!(banner.starts_with("This is a saved copy from Desk, last updated "));
    assert!(
        banner.ends_with("Desk must be on and reachable."),
        "{banner}"
    );
    assert_eq!(
        f.told().and_then(|copy| copy.held).as_deref(),
        Some("Desk is not reachable. Your draft is kept here.")
    );

    // An answer about another Bot, or to an older read, is not this tab's.
    assert!(!f.answer(Answer::LinkedOwner {
        seq,
        bot: BotId::mint(),
        result: Ok(None),
    }));
    assert!(f.view.copy().is_some());

    // Connected: an ordinary page, whose commands go to that computer.
    let page = f.view.copy.page.as_mut().expect("a page");
    let seq = page.reading();
    assert!(f.answer(Answer::LinkedOwner {
        seq,
        bot,
        result: Ok(Some((LinkedOwnerState::Current, Some(9)))),
    }));
    assert_eq!(f.line(COPY_BANNER), None);
    let told = f.told().expect("still a copy's conversation");
    assert_eq!(told.held, None, "sending is open");
    assert!(
        !told.outcomes.is_empty(),
        "and a refusal still has its words"
    );

    // That computer's copy does not list the Bot: not a copy after all.
    let seq = f.view.copy.page.as_mut().expect("a page").reading();
    assert!(f.answer(Answer::LinkedOwner {
        seq,
        bot,
        result: Ok(None),
    }));
    assert!(f.view.copy().is_none());
    assert_eq!(f.told(), None, "the conversation is an ordinary Bot's");
    assert!(!f.view.copy_pass());
    f.view.take(Ok(f.bot.clone()));
    assert!(f.view.copy().is_none(), "and it is not asked about again");
    assert!(!f.view.copy_pass());
    assert!(f.copy_reads().is_empty());
}

/// **A roster row of another computer's group opens a tab that already
/// says what the row said**, before any read of its own: held at once when
/// that computer is out of reach, so a message typed in the first moment is
/// a kept draft and not a refused send.
#[test]
fn m8_ui_offline_draft_a_remote_row_opens_a_tab_that_holds_at_once() {
    let owner = DeviceId::new();
    let mut listed = bot(BotId::mint(), WorkspaceId::new(), "Otter", false);
    listed.owner_device = owner;
    let mut row = RosterBot::from(listed);
    row.owner = Some(owner);

    let mut view = BotView::opened(&row);
    view.set_viewport(640.0, 480.0);
    assert_eq!(view.copy().map(|page| page.owner()), Some(owner));
    assert!(view.know_copy(owner, LinkedOwnerState::Stale, Some(3)));
    assert!(!view.know_copy(owner, LinkedOwnerState::Stale, Some(3)));
    assert!(
        !view.know_copy(DeviceId::new(), LinkedOwnerState::Current, None),
        "another computer's word is not this tab's"
    );
    let told = view
        .conversation()
        .and_then(|conversation| conversation.saved_copy().cloned())
        .expect("the conversation is told with the tab");
    assert!(told.held.is_some(), "held from the first frame");
    assert!(view.copy_banner().is_some());
    assert_publishable("a remote Bot's tab", &[bot_tab_tree(&view)]);

    // A later row or read that does not say which computer keeps it.
    let mut plain = row.clone();
    plain.owner = None;
    view.refresh_from(&plain);
    assert_eq!(view.copy().map(|page| page.owner()), Some(owner));
}

/// **The header says what the last command to the other computer came
/// to**, from the refusal's code and the delivery the link noted for that
/// request: not saved, not confirmed, a change only that computer makes.
/// The owner's own refusal says nothing here.
#[test]
fn m8_ui_not_confirmed_a_tab_says_what_a_command_came_to() {
    let mut f = fixture();
    let owner = DeviceId::new();
    f.view.copy_for_test(owner, LinkedOwnerState::Current, None);
    f.desk(owner);
    let bot = f.bot.id;
    let refused = |code: &str| Err(Failure::Refused(code.to_owned()));

    let request = BotRequestId::mint();
    f.answer(Answer::Delivery {
        bot,
        request,
        delivery: NOT_SENT.to_owned(),
    });
    assert_eq!(f.line(COPY_LINE), None, "nothing until the refusal itself");
    assert!(f.answer(Answer::Wrote {
        bot,
        request,
        result: refused(OWNER_UNREACHABLE),
    }));
    assert_eq!(
        f.line(COPY_LINE).as_deref(),
        Some("Not saved. Desk is not reachable.")
    );

    let request = BotRequestId::mint();
    f.answer(Answer::Delivery {
        bot,
        request,
        delivery: NOT_CONFIRMED.to_owned(),
    });
    f.answer(Answer::Wrote {
        bot,
        request,
        result: refused(OWNER_UNREACHABLE),
    });
    assert_eq!(
        f.line(COPY_LINE).as_deref(),
        Some("Not confirmed. Koru will check when Desk is reachable.")
    );

    // No delivery noted: never read as not sent.
    f.answer(Answer::Wrote {
        bot,
        request: BotRequestId::mint(),
        result: refused(OWNER_UNREACHABLE),
    });
    assert_eq!(
        f.line(COPY_LINE).as_deref(),
        Some("Not confirmed. Check the conversation.")
    );

    f.answer(Answer::Wrote {
        bot,
        request: BotRequestId::mint(),
        result: refused(EXECUTING_COMPUTER_ONLY),
    });
    assert_eq!(f.line(COPY_LINE).as_deref(), Some("Change this on Desk."));

    // A delivery or an answer about another Bot is not this tab's.
    let other = BotId::mint();
    f.answer(Answer::Delivery {
        bot: other,
        request: BotRequestId::mint(),
        delivery: NOT_SENT.to_owned(),
    });
    f.answer(Answer::Wrote {
        bot: other,
        request: BotRequestId::mint(),
        result: refused(OWNER_UNREACHABLE),
    });
    assert_eq!(f.line(COPY_LINE).as_deref(), Some("Change this on Desk."));

    // The owner's own refusal: the line of the last command goes.
    f.answer(Answer::Wrote {
        bot,
        request: BotRequestId::mint(),
        result: refused("revision_conflict"),
    });
    assert_eq!(f.line(COPY_LINE), None);
}

/// **Hidden, the tab reads nothing; shown again, it reads its copy once.**
/// While shown it asks for a wake when its clock comes due, and for none
/// once hidden.
#[test]
fn m8_ui_copy_banner_a_hidden_tab_reads_nothing_and_a_shown_one_reads_once() {
    let mut f = fixture();
    let owner = DeviceId::new();
    f.view
        .copy_for_test(owner, LinkedOwnerState::Stale, Some(1));
    while f.commands.try_recv().is_ok() {}
    let now = std::time::Instant::now();
    let soon = now + std::time::Duration::from_millis(50);
    assert_eq!(f.view.copy_deadline(now), Some(soon), "a read is owed");
    assert!(f.view.copy_pass());
    let (seq, bot, _, _) = f.copy_reads()[0];
    f.answer(Answer::LinkedOwner {
        seq,
        bot,
        result: Ok(Some((LinkedOwnerState::Stale, Some(1)))),
    });
    assert!(!f.view.copy_pass(), "read: nothing until its clock");
    assert!(f.view.copy_deadline(now).is_some_and(|at| at > now));

    f.view.copy_hidden();
    assert!(f.view.copy_pass(), "shown again: once");
    assert_eq!(f.copy_reads().len(), 1);
    assert!(!f.view.copy_pass());
}
