// SPDX-License-Identifier: MPL-2.0

//! Every state of contract 6.3 to 6.5, host-fed: the model is handed plain
//! values where a service would have answered, and what it says is read in
//! words. The roster's own drawing of the groups is in `roster/tests.rs`,
//! and a Bot tab's in `view/copy/tests.rs`.

use std::time::Instant;

use koru_ai_core::bots::{Bot, BotRequestId, TeamPauseStage};
use koru_i18n::Lang;

use super::page::{self, CopyPage, Outcome, Read, Uncopied};
use super::*;
use crate::bots::tests::bot;

/// A time as a test says it: never a clock's.
fn when(at: i64) -> String {
    format!("<{at}>")
}

fn owner_view(device: DeviceId, state: LinkedOwnerState, last: Option<i64>) -> LinkedOwnerView {
    LinkedOwnerView {
        owner_device: device,
        // Always absent on this side: nothing in the copy names a computer.
        display_name: None,
        state,
        last_confirmed_at_ms: last,
        team: None,
        withheld_bots: 0,
    }
}

fn bots_view(
    device: DeviceId,
    state: LinkedOwnerState,
    last: Option<i64>,
    bots: Vec<Bot>,
) -> LinkedBotsView {
    LinkedBotsView {
        owner_device: device,
        state,
        last_confirmed_at_ms: last,
        bots,
    }
}

/// A Bot `device` runs, whose home is `home`.
fn remote_bot(device: DeviceId, home: WorkspaceId, name: &str) -> Bot {
    let mut bot = bot(BotId::mint(), home, name, false);
    bot.owner_device = device;
    bot
}

pub(crate) fn computer(
    device: DeviceId,
    state: LinkedOwnerState,
    last: Option<i64>,
    bots: Vec<Bot>,
) -> Computer {
    Computer::of(
        owner_view(device, state, last),
        Some(bots_view(device, state, last, bots)),
    )
}

fn team(paused: bool, stage: TeamPauseStage) -> TeamPauseView {
    TeamPauseView {
        owner_device: DeviceId::new(),
        paused,
        revision: 3,
        stage,
        pending_since_ms: None,
    }
}

// ------------------------------------------------------------ the roster

/// **Each state of a copy is one heading state** (contract 6.3): connected,
/// last updated with its time, not reachable when no time is known, needs
/// an update, nothing received yet. A computer being forgotten has no
/// heading at all: it is not drawn.
#[test]
fn m8_ui_linked_roster_each_copy_state_is_one_heading_state() {
    use LinkedOwnerState::*;
    assert_eq!(heading(Current, Some(7)), Some(Heading::Connected));
    assert_eq!(heading(Current, None), Some(Heading::Connected));
    assert_eq!(heading(Stale, Some(7)), Some(Heading::LastUpdated(7)));
    assert_eq!(heading(Stale, None), Some(Heading::NotReachable));
    assert_eq!(heading(Unsupported, Some(7)), Some(Heading::NeedsUpdate));
    assert_eq!(heading(Never, None), Some(Heading::NothingYet));
    assert_eq!(heading(Withdrawing, Some(7)), None);

    let say = |heading| heading_words(Lang::En, heading, when);
    assert_eq!(say(Heading::Connected), "Connected");
    assert_eq!(say(Heading::LastUpdated(7)), "Last updated <7>");
    assert_eq!(say(Heading::NotReachable), "Not reachable");
    assert_eq!(say(Heading::NeedsUpdate), "Needs an update");
    assert_eq!(say(Heading::NothingYet), "Nothing received yet");
    assert_eq!(title(Lang::En, "Desk"), "On Desk");
    assert_eq!(
        unsupported_words(Lang::En, "Desk"),
        "Koru on Desk needs an update to show its Bots here."
    );
}

/// **A computer is what the two reads say of it**: the owners' list gives
/// its team pause and the Bots it does not copy, its own Bots' read gives
/// the state, the time and the rows, and every row names the computer that
/// runs it. What is drawn follows from that: a computer with nothing to say
/// in this workspace has no group.
#[test]
fn m8_ui_linked_roster_a_computer_is_built_from_the_two_reads() {
    let (device, home) = (DeviceId::new(), WorkspaceId::new());
    let mut owner = owner_view(device, LinkedOwnerState::Stale, Some(3));
    owner.withheld_bots = 2;
    owner.team = Some(team(true, TeamPauseStage::Acknowledged));
    let scout = remote_bot(device, home, "Scout");
    let built = Computer::of(
        owner.clone(),
        // Read after the owners: connected by then.
        Some(bots_view(
            device,
            LinkedOwnerState::Current,
            Some(9),
            vec![scout.clone()],
        )),
    );
    assert_eq!(built.device, device);
    assert_eq!(built.state, LinkedOwnerState::Current, "the later read's");
    assert_eq!(built.last_confirmed_at_ms, Some(9));
    assert_eq!(built.withheld, 2);
    assert!(built.connected());
    assert!(built.team_holds());
    assert_eq!(built.bots.len(), 1);
    assert_eq!(built.bots[0].id, scout.id);
    assert_eq!(
        built.bots[0].owner,
        Some(device),
        "the row names its computer"
    );
    assert_eq!(built.bots[0].home, home);

    // With no Bots' read, the owners' own word stands and no row is made.
    let bare = Computer::of(owner, None);
    assert_eq!(bare.state, LinkedOwnerState::Stale);
    assert!(bare.bots.is_empty());
    assert!(!bare.connected());

    // What is drawn.
    let listed = |state, bots: Vec<Bot>, withheld| {
        let mut computer = computer(DeviceId::new(), state, None, bots);
        computer.withheld = withheld;
        computer.listed()
    };
    let one = || vec![remote_bot(device, home, "Scout")];
    assert!(listed(LinkedOwnerState::Current, one(), 0));
    assert!(listed(LinkedOwnerState::Stale, one(), 0));
    assert!(
        !listed(LinkedOwnerState::Current, Vec::new(), 0),
        "its Bots live in other workspaces: nothing to say here"
    );
    assert!(!listed(LinkedOwnerState::Stale, Vec::new(), 0));
    assert!(listed(LinkedOwnerState::Current, Vec::new(), 1), "withheld");
    assert!(listed(LinkedOwnerState::Unsupported, Vec::new(), 0));
    assert!(listed(LinkedOwnerState::Never, Vec::new(), 0));
    assert!(
        !listed(LinkedOwnerState::Withdrawing, one(), 4),
        "never drawn"
    );
}

/// ***Paused with the team* is the other computer's own pause**, from its
/// confirmed pause until its resume is confirmed, as this computer's rows
/// say theirs. A computer that sent no team pause holds nothing.
#[test]
fn m8_ui_linked_roster_paused_with_the_team_follows_that_computers_pause() {
    let mut computer = computer(DeviceId::new(), LinkedOwnerState::Current, None, Vec::new());
    assert!(!computer.team_holds(), "nothing received: nothing claimed");
    for (paused, stage, holds) in [
        (false, TeamPauseStage::Acknowledged, false),
        (true, TeamPauseStage::Pending, false),
        (true, TeamPauseStage::Acknowledged, true),
        (false, TeamPauseStage::Pending, true),
    ] {
        computer.team = Some(team(paused, stage));
        assert_eq!(computer.team_holds(), holds, "{paused} {stage:?}");
    }
}

/// **The read asks for the owners, then for each owner's Bots in the
/// roster's workspace**, in that order, skipping a computer that is being
/// forgotten and never asking about more than the owner serves. Any read
/// that fails fails the answer whole, and an answer to another question is
/// never read as a list.
#[test]
fn m8_ui_linked_roster_the_read_asks_owners_then_each_computers_bots() {
    let home = WorkspaceId::new();
    let (desk, laptop, gone) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
    let owners = vec![
        owner_view(desk, LinkedOwnerState::Current, Some(1)),
        owner_view(gone, LinkedOwnerState::Withdrawing, Some(1)),
        owner_view(laptop, LinkedOwnerState::Stale, Some(2)),
    ];
    let mut asked = Vec::new();
    let answer = read(home, |command| {
        asked.push(format!("{command:?}"));
        Ok(match command {
            BotCommand::ListLinkedOwners {} => BotResponse::LinkedOwners {
                owners: owners.clone(),
            },
            BotCommand::ListLinkedBots {
                owner_device,
                workspace,
            } => {
                assert_eq!(workspace, Some(home), "the window's workspace");
                BotResponse::LinkedBots {
                    linked: bots_view(
                        owner_device,
                        LinkedOwnerState::Current,
                        Some(5),
                        vec![remote_bot(owner_device, home, "Scout")],
                    ),
                }
            }
            other => panic!("not a linked read: {other:?}"),
        })
    })
    .expect("both reads answered");
    assert_eq!(asked.len(), 3, "the owners, then two computers: {asked:?}");
    assert_eq!(
        answer.iter().map(|c| c.device).collect::<Vec<_>>(),
        [desk, laptop],
        "the service's order, without the one being forgotten"
    );
    assert!(answer.iter().all(|c| c.bots.len() == 1));

    // A Koru that keeps no copy: no computer, and one read.
    let mut reads = 0;
    let none = read(home, |_| {
        reads += 1;
        Ok(BotResponse::LinkedOwners { owners: Vec::new() })
    });
    assert_eq!(none, Ok(Vec::new()));
    assert_eq!(reads, 1);

    // Never more computers than the owner serves at once.
    let many: Vec<_> = (0..MAX_COMPUTERS + 3)
        .map(|_| owner_view(DeviceId::new(), LinkedOwnerState::Current, None))
        .collect();
    let capped = read(home, |command| {
        Ok(match command {
            BotCommand::ListLinkedOwners {} => BotResponse::LinkedOwners {
                owners: many.clone(),
            },
            BotCommand::ListLinkedBots { owner_device, .. } => BotResponse::LinkedBots {
                linked: bots_view(owner_device, LinkedOwnerState::Current, None, Vec::new()),
            },
            other => panic!("not a linked read: {other:?}"),
        })
    })
    .expect("answered");
    assert_eq!(capped.len(), MAX_COMPUTERS);

    // A refusal, a lost answer, an answer to another question, and Bots
    // answered for another computer than the one asked about.
    let failing = |second: Result<BotResponse, Said>| {
        read(home, |command| match command {
            BotCommand::ListLinkedOwners {} => Ok(BotResponse::LinkedOwners {
                owners: vec![owner_view(desk, LinkedOwnerState::Current, None)],
            }),
            _ => second.clone(),
        })
    };
    assert_eq!(
        failing(Err(Said::new("forbidden", false))),
        Err(Failure::Refused("forbidden".into()))
    );
    assert_eq!(
        failing(Err(Said::new("gateway_unavailable", true))),
        Err(Failure::Unanswered)
    );
    assert_eq!(
        failing(Ok(BotResponse::List { bots: Vec::new() })),
        Err(Failure::Unanswered)
    );
    assert_eq!(
        failing(Ok(BotResponse::LinkedBots {
            linked: bots_view(laptop, LinkedOwnerState::Current, None, Vec::new()),
        })),
        Err(Failure::Unanswered),
        "about another computer"
    );
    assert_eq!(
        read(home, |_| Ok(BotResponse::List { bots: Vec::new() })),
        Err(Failure::Unanswered)
    );
}

/// **A computer's name is the window's own**, from its list of linked
/// devices: the copy carries none, and nothing else is read to find one. A
/// computer the window cannot name is *another computer*, never an id.
#[test]
fn m8_ui_linked_roster_a_computer_is_named_by_the_windows_device_list() {
    let (desk, unnamed, blank) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
    let names = Names::new([(desk, "  Desk  ".to_owned()), (blank, "   ".to_owned())]);
    assert_eq!(names.known(desk), Some("  Desk  "));
    assert_eq!(names.of(Lang::En, desk), "Desk");
    assert_eq!(names.known(unnamed), None);
    assert_eq!(names.of(Lang::En, unnamed), "another computer");
    assert_eq!(
        names.of(Lang::En, blank),
        "another computer",
        "a blank name"
    );
    assert!(
        !names.of(Lang::En, unnamed).contains(&unnamed.to_storage()),
        "never the device's id"
    );

    let mut linked = Linked::default();
    assert!(linked.set_names(names.clone()));
    assert!(!linked.set_names(names), "the same names: nothing moved");
    assert_eq!(linked.name_of(Lang::En, desk), "Desk");
}

/// **The states are read again only while a computer is listed.** A Koru
/// that keeps no copy answers none, and then no clock runs and nothing is
/// owed. With one listed the clock runs; an older read's answer is not
/// taken; a read that fails keeps what was shown; hidden, nothing is owed.
#[test]
fn m8_ui_linked_roster_the_clock_runs_only_while_a_computer_is_listed() {
    let now = Instant::now();
    let home = WorkspaceId::new();
    let mut linked = Linked::default();
    assert!(!linked.awaiting());
    assert!(!linked.read_due(now), "no clock until something is listed");

    // No copy: the answer changes nothing and starts no clock.
    let seq = linked.reading();
    assert!(linked.awaiting(), "its answer is owed");
    assert!(!linked.take(seq, Ok(Vec::new()), now));
    assert!(linked.is_empty());
    assert!(!linked.awaiting(), "no computer: no clock, nothing owed");
    assert_eq!(linked.deadline(), None);
    assert!(!linked.read_due(now + POLL * 4));

    // One computer: listed, and read again when the clock comes due.
    let desk = DeviceId::new();
    let listed = vec![computer(
        desk,
        LinkedOwnerState::Current,
        Some(1),
        vec![remote_bot(desk, home, "Scout")],
    )];
    let one = || listed.clone();
    let seq = linked.reading();
    assert!(linked.take(seq, Ok(one()), now));
    assert_eq!(linked.computers().count(), 1);
    assert!(!linked.awaiting(), "answered: no waking until its clock");
    assert_eq!(linked.deadline(), Some(now + POLL), "one wake, then");
    assert!(!linked.read_due(now), "not before its time");
    assert!(linked.read_due(now + POLL));

    // An older read's answer is not this read's.
    let older = seq;
    let seq = linked.reading();
    assert!(!linked.read_due(now + POLL * 2), "one read at a time");
    assert!(!linked.take(older, Ok(Vec::new()), now));
    assert_eq!(linked.computers().count(), 1);

    // A read that failed keeps what was shown, and is asked again.
    assert!(!linked.take(seq, Err(Failure::lost()), now));
    assert_eq!(linked.computers().count(), 1, "a copy does not vanish");
    assert!(linked.read_due(now + POLL));

    // The same answer again draws nothing new.
    let seq = linked.reading();
    assert!(!linked.take(seq, Ok(one()), now));

    // Hidden: nothing is owed, and an answer on its way is not taken.
    let seq = linked.reading();
    linked.hidden();
    assert!(!linked.awaiting());
    assert!(!linked.take(seq, Ok(Vec::new()), now));
    assert_eq!(linked.computers().count(), 1);

    // Another workspace: its computers' Bots are not this one's.
    linked.clear();
    assert!(linked.is_empty());
}

/// ***Refresh this copy* is drawn only when the host offers it, and can be
/// pressed only while its computer is connected** (contract 6.3). The press
/// is an intent for the host, taken once.
#[test]
fn m8_ui_linked_roster_refresh_is_the_hosts_to_offer_and_needs_a_connection() {
    let home = WorkspaceId::new();
    let (desk, laptop) = (DeviceId::new(), DeviceId::new());
    let mut linked = Linked::default();
    linked.read_for_test(vec![
        computer(
            desk,
            LinkedOwnerState::Current,
            None,
            vec![remote_bot(desk, home, "Scout")],
        ),
        computer(
            laptop,
            LinkedOwnerState::Stale,
            Some(2),
            vec![remote_bot(laptop, home, "Otter")],
        ),
    ]);
    assert!(!linked.refresh_offered(), "off unless the host says");
    assert!(!linked.refresh_enabled(desk));
    assert!(!linked.press_refresh(desk));
    assert_eq!(linked.take_refresh(), None);

    assert!(linked.offer_refresh(true));
    assert!(!linked.offer_refresh(true), "said already");
    assert!(linked.refresh_enabled(desk), "connected");
    assert!(!linked.refresh_enabled(laptop), "not reachable");
    assert!(!linked.refresh_enabled(DeviceId::new()), "not listed");
    assert!(!linked.press_refresh(laptop));
    assert!(linked.press_refresh(desk));
    assert_eq!(linked.take_refresh(), Some(desk));
    assert_eq!(linked.take_refresh(), None, "taken once");
}

/// **Bots a computer does not copy are counted, never named** (contract
/// 6.3, D22): none says nothing, one and several each read as a sentence
/// of their own, and the count is written as the language writes numbers.
#[test]
fn m8_ui_withheld_bots_are_counted_and_never_named() {
    assert_eq!(withheld_words(Lang::En, 0, "Desk"), None);
    assert_eq!(
        withheld_words(Lang::En, 1, "Desk").as_deref(),
        Some(
            "1 Bot on Desk is in a workspace kept on that computer only. Its history is not \
             available here."
        )
    );
    assert_eq!(
        withheld_words(Lang::En, 3, "Desk").as_deref(),
        Some(
            "3 Bots on Desk are in a workspace kept on that computer only. Their history is \
             not available here."
        )
    );
    let many = withheld_words(Lang::En, 1200, "Desk").expect("a sentence");
    assert!(
        many.starts_with(&koru_i18n::number::decimal(Lang::En, 1200)),
        "{many}"
    );
    for lang in Lang::ALL {
        for count in [1, 2, 40] {
            let words = withheld_words(lang, count, "Desk").expect("a sentence");
            assert!(words.contains("Desk"), "{lang:?}: {words}");
            assert!(!words.contains("{}"), "{lang:?}: {words}");
        }
    }
}

// ------------------------------------------------------- a remote Bot's page

/// **What a remote Bot's page says over itself, by the copy's state**
/// (contract 6.3): nothing while connected; that it is a saved copy and as
/// of when while its computer is out of reach; that the other computer's
/// Koru needs an update. Until the state is read it says nothing and holds
/// nothing: never a guess.
#[test]
fn m8_ui_copy_banner_each_state_of_a_remote_bots_page() {
    let owner = DeviceId::new();
    let page = |state| CopyPage::new(owner, state);
    let banner = |state| page(state).banner(Lang::En, "Desk", when);

    let unread = page(None);
    assert_eq!(unread.state(), None);
    assert_eq!(unread.banner(Lang::En, "Desk", when), None);
    assert!(!unread.held(), "nothing is held on a guess");
    assert!(!unread.connected(), "nor claimed as connected");
    assert_eq!(unread.placeholder(Lang::En, "Desk"), None);

    assert_eq!(banner(Some((LinkedOwnerState::Current, Some(4)))), None);
    assert_eq!(
        banner(Some((LinkedOwnerState::Stale, Some(4)))).as_deref(),
        Some(
            "This is a saved copy from Desk, last updated <4>. You can read it. To change \
             anything, Desk must be on and reachable."
        )
    );
    assert_eq!(
        banner(Some((LinkedOwnerState::Stale, None))).as_deref(),
        Some(
            "This is a saved copy from Desk. You can read it. To change anything, Desk must \
             be on and reachable."
        )
    );
    assert_eq!(
        banner(Some((LinkedOwnerState::Unsupported, Some(4)))).as_deref(),
        Some("Koru on Desk needs an update to show its Bots here.")
    );

    // What the composer says while nothing can be sent.
    let connected = page(Some((LinkedOwnerState::Current, None)));
    assert!(connected.connected() && !connected.held());
    assert_eq!(connected.placeholder(Lang::En, "Desk"), None);
    let stale = page(Some((LinkedOwnerState::Stale, Some(4))));
    assert!(stale.held() && !stale.connected());
    assert_eq!(
        stale.placeholder(Lang::En, "Desk").as_deref(),
        Some("Desk is not reachable. Your draft is kept here.")
    );
    assert_eq!(
        page(Some((LinkedOwnerState::Unsupported, None)))
            .placeholder(Lang::En, "Desk")
            .as_deref(),
        Some("Koru on Desk needs an update to show its Bots here.")
    );
}

/// **A remote Bot's page reads its copy's state when shown and on its
/// clock, and stops being one when that computer's copy does not list the
/// Bot.** A read that failed keeps what the page says.
#[test]
fn m8_ui_copy_banner_a_page_reads_its_copy_and_stops_when_it_is_not_one() {
    let now = Instant::now();
    let mut page = CopyPage::new(DeviceId::new(), None);
    assert!(page.read_due(now), "shown: read once");
    let seq = page.reading();
    assert!(!page.read_due(now), "one read at a time");
    assert!(page.awaiting());
    assert_eq!(
        page.take(seq, Ok(Some((LinkedOwnerState::Stale, Some(2)))), now),
        Read::Changed
    );
    assert!(page.held());
    assert!(!page.read_due(now));
    assert_eq!(page.deadline(), Some(now + POLL), "then on its clock");
    assert!(page.read_due(now + POLL));

    // The same state again; an older read; a read that failed.
    let older = seq;
    let seq = page.reading();
    assert_eq!(
        page.take(older, Ok(Some((LinkedOwnerState::Current, None))), now),
        Read::Same
    );
    assert!(page.held(), "an older read's answer is not this read's");
    assert_eq!(
        page.take(seq, Ok(Some((LinkedOwnerState::Stale, Some(2)))), now),
        Read::Same
    );
    let seq = page.reading();
    assert_eq!(page.take(seq, Err(Failure::lost()), now), Read::Same);
    assert!(page.held(), "what it says stays");

    // Connected again.
    let seq = page.reading();
    assert_eq!(
        page.take(seq, Ok(Some((LinkedOwnerState::Current, Some(8)))), now),
        Read::Changed
    );
    assert!(page.connected());

    // Hidden, then shown: read once more. A full queue leaves it owed.
    page.hidden();
    assert!(page.read_due(now));
    let _ = page.reading();
    page.unsent();
    assert!(page.read_due(now));

    // That computer's copy does not list the Bot.
    let seq = page.reading();
    assert_eq!(page.take(seq, Ok(None), now), Read::NotACopy);
}

/// **What a command came to follows the refusal's code and its delivery**
/// (the mirror's table): nothing left this computer, it left and no answer
/// came, the other computer cannot say, only that computer makes the
/// change, the two share no version. Any other refusal is the owner's own
/// and is not this model's to word.
#[test]
fn m8_ui_not_confirmed_the_outcome_follows_the_code_and_the_delivery() {
    use Outcome::*;
    for (code, delivery, expected) in [
        (OWNER_UNREACHABLE, Some(NOT_SENT), Some(NotSaved)),
        (OWNER_UNREACHABLE, Some(NOT_CONFIRMED), Some(NotConfirmed)),
        (OWNER_UNREACHABLE, Some(UNKNOWN), Some(NotConfirmedFinal)),
        // No delivery, or a word this build does not know: never read as
        // not sent.
        (OWNER_UNREACHABLE, None, Some(NotConfirmedFinal)),
        (OWNER_UNREACHABLE, Some("later"), Some(NotConfirmedFinal)),
        (ALREADY_ANSWERED, None, Some(NotConfirmedFinal)),
        (OWNER_UNSUPPORTED, None, Some(NeedsUpdate)),
        (EXECUTING_COMPUTER_ONLY, None, Some(ChangeThere)),
        ("revision_conflict", None, None),
        ("team_paused", Some(NOT_SENT), None),
    ] {
        assert_eq!(
            page::outcome(code, delivery),
            expected,
            "{code} {delivery:?}"
        );
    }
    let say = |outcome| page::outcome_words(Lang::En, outcome, "Desk");
    assert_eq!(say(NotSaved), "Not saved. Desk is not reachable.");
    assert_eq!(
        say(NotConfirmed),
        "Not confirmed. Koru will check when Desk is reachable."
    );
    assert_eq!(
        say(NotConfirmedFinal),
        "Not confirmed. Check the conversation."
    );
    assert_eq!(say(ChangeThere), "Change this on Desk.");
    assert_eq!(
        say(NeedsUpdate),
        "Koru on Desk needs an update to show its Bots here."
    );
    assert_eq!(page::sending_words(Lang::En), "Sending…");
    assert_eq!(page::saving_words(Lang::En), "Saving…");
}

/// **A page says what its last command came to, and only that**: the
/// delivery the link noted belongs to one request, a refusal that is the
/// owner's own says nothing here, and the next answer replaces what the
/// last one said. A command that was not confirmed has the copy read again.
#[test]
fn m8_ui_not_confirmed_a_page_says_what_its_last_command_came_to() {
    let now = Instant::now();
    let mut page = CopyPage::new(DeviceId::new(), Some((LinkedOwnerState::Current, None)));
    let seq = page.reading();
    page.take(seq, Ok(Some((LinkedOwnerState::Current, None))), now);
    assert!(!page.read_due(now));
    let unreachable = || Err::<(), _>(Failure::Refused(OWNER_UNREACHABLE.into()));

    let request = BotRequestId::mint();
    page.note_delivery(request, NOT_SENT.into());
    assert!(page.wrote(request, &unreachable()));
    assert_eq!(page.said(), Some(Outcome::NotSaved));
    assert_eq!(
        page.line(Lang::En, "Desk").as_deref(),
        Some("Not saved. Desk is not reachable.")
    );
    assert!(page.read_due(now), "the copy's state is read again");

    // A delivery noted for another request is not this one's.
    let (noted, answered) = (BotRequestId::mint(), BotRequestId::mint());
    page.note_delivery(noted, NOT_SENT.into());
    assert!(page.wrote(answered, &unreachable()));
    assert_eq!(page.said(), Some(Outcome::NotConfirmedFinal));

    let request = BotRequestId::mint();
    page.note_delivery(request, NOT_CONFIRMED.into());
    assert!(page.wrote(request, &unreachable()));
    assert_eq!(
        page.line(Lang::En, "Desk").as_deref(),
        Some("Not confirmed. Koru will check when Desk is reachable.")
    );

    // Only that computer makes this change.
    let request = BotRequestId::mint();
    assert!(page.wrote(
        request,
        &Err::<(), _>(Failure::Refused(EXECUTING_COMPUTER_ONLY.into()))
    ));
    assert_eq!(
        page.line(Lang::En, "Desk").as_deref(),
        Some("Change this on Desk.")
    );

    // The owner's own refusal, a lost answer and a success say nothing
    // here, and take the last line away.
    assert!(page.wrote(
        BotRequestId::mint(),
        &Err::<(), _>(Failure::Refused("revision_conflict".into()))
    ));
    assert_eq!(page.line(Lang::En, "Desk"), None);
    assert!(!page.wrote(BotRequestId::mint(), &Err::<(), _>(Failure::Unanswered)));
    assert!(!page.wrote(BotRequestId::mint(), &Ok(())));
    assert_eq!(page.said(), None);
}

/// **What the conversation is told** (`ChatSurface::set_saved_copy`): held
/// with the sentence that keeps the draft while the computer is out of
/// reach, open while connected, and for each refusal about the other
/// computer its sentence and whether the message comes back to the
/// composer. Only a message that never left comes back.
#[test]
fn m8_ui_offline_draft_the_conversation_is_told_what_to_hold_and_what_comes_back() {
    use koru_ai_ui::CopyDraft;
    let owner = DeviceId::new();
    let stale = CopyPage::new(owner, Some((LinkedOwnerState::Stale, Some(1))));
    let told = stale.surface(Lang::En, "Desk");
    assert_eq!(
        told.held.as_deref(),
        Some("Desk is not reachable. Your draft is kept here.")
    );
    assert_eq!(told.sending, "Sending…");
    assert_eq!(told.page_missing, "This page is not on this computer.");
    assert!(told.notes.is_empty());

    let connected = CopyPage::new(owner, Some((LinkedOwnerState::Current, None)));
    assert_eq!(connected.surface(Lang::En, "Desk").held, None);
    let unread = CopyPage::new(owner, None);
    assert_eq!(unread.surface(Lang::En, "Desk").held, None);

    let find = |code: &str, delivery: Option<&str>| {
        told.outcomes
            .iter()
            .find(|outcome| {
                outcome.code == code
                    && outcome
                        .detail
                        .as_ref()
                        .map(|(key, value)| (key.as_str(), value.as_str()))
                        == delivery.map(|value| (DELIVERY, value))
            })
            .unwrap_or_else(|| panic!("no outcome for {code} {delivery:?}"))
    };
    let not_sent = find(OWNER_UNREACHABLE, Some(NOT_SENT));
    assert_eq!(not_sent.draft, CopyDraft::Kept);
    assert_eq!(
        not_sent.words,
        "Desk is not reachable. Your draft is kept here."
    );
    let not_confirmed = find(OWNER_UNREACHABLE, Some(NOT_CONFIRMED));
    assert_eq!(not_confirmed.draft, CopyDraft::Sent, "never typed again");
    assert_eq!(
        not_confirmed.words,
        "Not confirmed. Koru will check when Desk is reachable."
    );
    for outcome in [
        find(OWNER_UNREACHABLE, Some(UNKNOWN)),
        find(OWNER_UNREACHABLE, None),
        find(ALREADY_ANSWERED, None),
    ] {
        assert_eq!(outcome.draft, CopyDraft::Sent);
        assert_eq!(outcome.words, "Not confirmed. Check the conversation.");
    }
    let only_there = find(EXECUTING_COMPUTER_ONLY, None);
    assert_eq!(only_there.draft, CopyDraft::Kept);
    assert_eq!(only_there.words, "Change this on Desk.");
    let update = find(OWNER_UNSUPPORTED, None);
    assert_eq!(update.draft, CopyDraft::Kept);
    assert_eq!(
        update.words,
        "Koru on Desk needs an update to show its Bots here."
    );
    // Every sentence names the computer or needs no name; none is the
    // engine's text, and none is left with a hole in it.
    for outcome in &told.outcomes {
        assert!(!outcome.words.contains("{}"), "{}", outcome.words);
    }
}

/// **What only the other computer shows is said, not hidden** (contract
/// 6.3): a change it alone can make, its Routines, a page whose workspace
/// is not here, a message too large to copy, attachments that stayed
/// there. The last two reach the conversation as notes on their message.
#[test]
fn m8_ui_executing_only_what_stays_on_the_other_computer_is_said() {
    assert_eq!(
        page::outcome_words(Lang::En, Outcome::ChangeThere, "Desk"),
        "Change this on Desk."
    );
    assert_eq!(
        page::routines_words(Lang::En, "Desk"),
        "Routines are shown on Desk."
    );
    assert_eq!(
        page::page_missing_words(Lang::En),
        "This page is not on this computer."
    );
    assert_eq!(
        page::too_large_words(Lang::En, "Desk"),
        "This message is too large to copy. Open it on Desk."
    );
    assert_eq!(page::attachments_words(Lang::En, 0, "Desk"), None);
    assert_eq!(
        page::attachments_words(Lang::En, 1, "Desk").as_deref(),
        Some("1 attachment is on Desk.")
    );
    assert_eq!(
        page::attachments_words(Lang::En, 4, "Desk").as_deref(),
        Some("4 attachments are on Desk.")
    );

    let message = koru_ai_core::MessageId::new();
    let uncopied = |too_large, attachments| Uncopied {
        message,
        too_large,
        attachments,
    };
    assert_eq!(page::note(Lang::En, uncopied(false, 0), "Desk"), None);
    let large = page::note(Lang::En, uncopied(true, 0), "Desk").expect("a note");
    assert_eq!(large.message, message);
    assert_eq!(
        large.instead.as_deref(),
        Some("This message is too large to copy. Open it on Desk.")
    );
    assert_eq!(large.under, None);
    let attached = page::note(Lang::En, uncopied(false, 2), "Desk").expect("a note");
    assert_eq!(attached.instead, None);
    assert_eq!(
        attached.under.as_deref(),
        Some("2 attachments are on Desk.")
    );

    // And the conversation is told, once the page is.
    let mut copy = CopyPage::new(DeviceId::new(), Some((LinkedOwnerState::Current, None)));
    assert!(copy.set_uncopied(vec![uncopied(true, 0), uncopied(false, 0)]));
    assert!(!copy.set_uncopied(vec![uncopied(true, 0), uncopied(false, 0)]));
    let told = copy.surface(Lang::En, "Desk");
    assert_eq!(
        told.notes.len(),
        1,
        "a message that carried all has no note"
    );
    assert_eq!(told.notes[0].message, message);
}

// ------------------------------------------------- disclosure and languages

/// **The disclosure is said only by a build that copies** (contract 6.5,
/// D22). This build copies nothing — the gateway's switch is closed — so
/// neither sentence is drawn anywhere; a build that copies says both, in
/// every language.
#[test]
fn m8_ui_linked_roster_the_disclosure_is_said_only_by_a_build_that_copies() {
    assert!(
        !COPIES_DISCLOSED,
        "opened only together with the gateway's LINKED_BOT_CONTROL_AVAILABLE"
    );
    assert_eq!(disclosure(Lang::En), None);
    assert_eq!(unlink_disclosure(Lang::En), None);
    assert_eq!(
        disclosure_when(Lang::En, true),
        Some("Bot chats and results are copied to your linked desktops.")
    );
    assert_eq!(
        unlink_disclosure_when(Lang::En, true),
        Some("Unlinking removes the copy the next time that computer is on.")
    );
    for lang in Lang::ALL {
        assert!(disclosure_when(lang, true).is_some_and(|words| !words.trim().is_empty()));
        assert!(unlink_disclosure_when(lang, true).is_some_and(|words| !words.trim().is_empty()));
        assert_eq!(disclosure_when(lang, false), None);
    }
}

/// **Every sentence of 6.3 to 6.5 exists in each of the eight languages**,
/// with the places English has and in English's order — a name is filled
/// in by position — and a filled sentence names the computer and leaves no
/// hole. The seven translations are machine-written and await a native
/// read; this holds their shape, not their wording.
#[test]
fn m8_i18n_linked_every_sentence_exists_in_eight_languages_and_keeps_its_places() {
    const KEYS: [Words; 28] = [
        Words::BotsLinkedOn,
        Words::BotsLinkedConnected,
        Words::BotsLinkedLastUpdated,
        Words::BotsLinkedNotReachable,
        Words::BotsLinkedNeedsUpdate,
        Words::BotsLinkedNothingYet,
        Words::BotsLinkedRefresh,
        Words::BotsLinkedRefreshNamed,
        Words::BotsLinkedWithheld,
        Words::BotsLinkedWithheldOne,
        Words::BotsLinkedCopyBanner,
        Words::BotsLinkedCopyBannerNoTime,
        Words::BotsLinkedComposerHeld,
        Words::BotsLinkedNotSaved,
        Words::BotsLinkedSending,
        Words::BotsLinkedNotConfirmed,
        Words::BotsLinkedNotConfirmedFinal,
        Words::BotsLinkedChangeOn,
        Words::BotsLinkedRoutinesOn,
        Words::BotsLinkedPageNotHere,
        Words::BotsLinkedTooLarge,
        Words::BotsLinkedAttachments,
        Words::BotsLinkedAttachmentsOne,
        Words::BotsLinkedUnsupported,
        Words::BotsLinkedRunsOn,
        Words::BotsLinkedAnotherComputer,
        Words::BotsLinkedDisclosure,
        Words::BotsLinkedUnlinkDisclosure,
    ];
    assert_eq!(Lang::ALL.len(), 8);
    for key in KEYS {
        let places = t(Lang::En, key).matches("{}").count();
        for lang in Lang::ALL {
            let words = t(lang, key);
            assert!(!words.trim().is_empty(), "{lang:?} {key:?}");
            assert_eq!(words.matches("{}").count(), places, "{lang:?} {key:?}");
            assert_eq!(words, words.trim(), "{lang:?} {key:?}: no stray space");
        }
    }
    // Filled as the screens fill them: the computer is named, the time and
    // the count are where they were put, and nothing is left unfilled.
    for lang in Lang::ALL {
        let computer = "Desk-7";
        let page = CopyPage::new(DeviceId::new(), Some((LinkedOwnerState::Stale, Some(41))));
        let banner = page.banner(lang, computer, when).expect("a banner");
        let sentences = [
            title(lang, computer),
            heading_words(lang, Heading::LastUpdated(41), when),
            withheld_words(lang, 5, computer).expect("a sentence"),
            withheld_words(lang, 1, computer).expect("a sentence"),
            unsupported_words(lang, computer),
            banner.clone(),
            page.placeholder(lang, computer).expect("held"),
            page::outcome_words(lang, Outcome::NotSaved, computer),
            page::outcome_words(lang, Outcome::NotConfirmed, computer),
            page::outcome_words(lang, Outcome::ChangeThere, computer),
            page::routines_words(lang, computer),
            page::too_large_words(lang, computer),
            page::attachments_words(lang, 6, computer).expect("a sentence"),
            page::attachments_words(lang, 1, computer).expect("a sentence"),
        ];
        for (index, words) in sentences.iter().enumerate() {
            assert!(!words.contains("{}"), "{lang:?} #{index}: {words}");
            if index != 1 {
                assert!(words.contains(computer), "{lang:?} #{index}: {words}");
            }
        }
        assert!(sentences[1].contains("<41>"), "{lang:?}: {}", sentences[1]);
        // The banner names the computer twice and the time between: the
        // one sentence whose places could be confused.
        assert_eq!(banner.matches(computer).count(), 2, "{lang:?}: {banner}");
        let first = banner.find(computer).expect("named");
        let time = banner.find("<41>").expect("timed");
        let last = banner.rfind(computer).expect("named");
        assert!(first < time && time < last, "{lang:?}: {banner}");
    }
}
