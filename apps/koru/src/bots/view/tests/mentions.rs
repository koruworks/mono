// SPDX-License-Identifier: MPL-2.0

//! **Mentioning another Bot, in the Bot's tab**: what the tab feeds its
//! conversation's `@` picker, from the roster's rows and from its own read
//! on a link the test answers. The rule itself is `bots::mentions`'.

use super::*;
use crate::bots::roster::RosterBot;

/// A Bot tab that knows its Bot, as after its first read.
fn known(may_ask: bool) -> Fixture {
    let mut f = fixture();
    f.bot.may_ask = may_ask;
    f.view.know_bot_for_test(f.bot.clone());
    assert!(f.view.conversation().is_some(), "its conversation is bound");
    f
}

/// What the tab's conversation is offering: titles and notes, in order.
fn offered(f: &Fixture) -> Vec<(String, Option<String>)> {
    f.view
        .conversation()
        .expect("a bound conversation")
        .mention_choices()
        .choices
        .iter()
        .map(|choice| (choice.title.clone(), choice.note.clone()))
        .collect()
}

fn row(name: &str, home: WorkspaceId, paused: bool) -> RosterBot {
    RosterBot::from(bot(BotId::mint(), home, name, paused))
}

/// The roster's rows around the tab's Bot: itself, two it may ask (one
/// paused), an archived one, one of another workspace and one another
/// computer runs.
fn roster(f: &Fixture) -> Vec<RosterBot> {
    let home = f.bot.home_workspace;
    let mut archived = row("Retired", home, false);
    archived.archived = true;
    let mut remote = row("On the laptop", home, false);
    remote.owner = Some(koru_ai_core::bots::DeviceId::new());
    vec![
        RosterBot::from(f.bot.clone()),
        row("CFO", home, false),
        archived,
        row("Elsewhere", WorkspaceId::new(), false),
        remote,
        row("Legal", home, true),
    ]
}

/// **The tab feeds its conversation the Bots its Bot may ask, and only
/// those**: not itself, not an archived Bot, not another workspace's, not
/// another computer's; a paused one is offered and marked.
#[test]
fn bot_mention_tab_feeds_the_askable_bots_and_marks_a_paused_one() {
    let mut f = known(true);
    assert!(offered(&f).is_empty(), "nothing listed yet");
    let rows = roster(&f);
    assert!(f.view.know_mention_roster(&rows));
    assert!(
        f.view.mentions_pass(std::time::Instant::now()),
        "sent mentions may read differently now: the conversation is drawn again"
    );
    assert_eq!(
        offered(&f),
        vec![
            ("CFO".to_owned(), None),
            ("Legal".to_owned(), Some("Paused".to_owned())),
        ]
    );
    let section = f.view.conversation().unwrap().mention_choices().clone();
    assert_eq!(section.role, koru_core::attributes::ROLE_BOT);
    assert!(
        section
            .choices
            .iter()
            .all(|choice| choice.id.parse::<uuid::Uuid>().is_ok()),
        "each choice is named by its Bot's id"
    );
    // The same rows again change nothing, and nothing is drawn again.
    assert!(!f.view.know_mention_roster(&rows));
    assert!(!f.view.mentions_pass(std::time::Instant::now()));
}

/// **With *May ask other bots* off the picker is fed nothing**: pages only,
/// as it was. Switched on again (the owner's read says so), they are back.
#[test]
fn bot_mention_tab_feeds_nothing_when_the_setting_is_off() {
    let mut f = known(false);
    f.view.know_mention_roster(&roster(&f));
    f.view.mentions_pass(std::time::Instant::now());
    assert!(offered(&f).is_empty());
    assert_eq!(
        *f.view.conversation().unwrap().mention_choices(),
        koru_ai_ui::ExtraSection::default(),
        "exactly the section of a chat nobody feeds"
    );

    f.bot.may_ask = true;
    f.view.know_bot_for_test(f.bot.clone());
    // Kept for the next `@`: a shut picker has nothing to draw again.
    f.view.mentions_pass(std::time::Instant::now());
    assert_eq!(offered(&f).len(), 2);
}

/// A tab that has not read its Bot does not know the setting: it offers
/// nobody. An archived Bot offers nobody either.
#[test]
fn bot_mention_tab_feeds_nothing_until_it_knows_its_bot_and_for_an_archived_one() {
    let mut f = fixture();
    f.view.know_mention_roster(&roster(&f));
    assert!(!f.view.mentions_pass(std::time::Instant::now()));
    assert!(f.view.mention_asker().is_none());

    let mut f = known(true);
    f.view.know_mention_roster(&roster(&f));
    f.bot.lifecycle = BotLifecycle::Archived;
    f.view.know_bot_for_test(f.bot.clone());
    assert!(f.view.mention_asker().is_none());
}

/// **Nothing is asked while the picker is shut**, and the tab's own read —
/// answered on its link — replaces the roster's rows: a Bot made since the
/// roster last listed is offered, one archived since is not.
#[test]
fn bot_mention_tab_reads_the_bots_again_on_its_own_link() {
    let mut f = known(true);
    let rows = roster(&f);
    f.view.know_mention_roster(&rows);
    f.view.mentions_pass(std::time::Instant::now());
    while f.commands.try_recv().is_ok() {}
    f.view.mentions_pass(std::time::Instant::now());
    assert!(
        f.commands.try_recv().is_err(),
        "a shut picker asks for nothing"
    );

    // The ask the open picker makes, answered as the service answers it.
    let home = f.bot.home_workspace;
    let seq = f.view.mentions.asking(std::time::Instant::now());
    let mut mine = f.bot.clone();
    mine.owner_device = f.bot.owner_device;
    let mut cfo = bot(BotId::mint(), home, "CFO", false);
    cfo.owner_device = f.bot.owner_device;
    let mut newcomer = bot(BotId::mint(), home, "Researcher", false);
    newcomer.owner_device = f.bot.owner_device;
    f.answers
        .send(Answer::Listed {
            seq,
            workspace: home,
            result: Ok(vec![mine, cfo, newcomer]),
        })
        .unwrap();
    assert!(f.view.poll(), "the listing was folded");
    f.view.mentions_pass(std::time::Instant::now());
    assert_eq!(
        offered(&f),
        vec![("CFO".to_owned(), None), ("Researcher".to_owned(), None)]
    );
    assert!(
        !f.view.know_mention_roster(&rows),
        "the roster's older rows no longer replace the tab's own read"
    );
    f.view.mentions_pass(std::time::Instant::now());
    assert_eq!(offered(&f).len(), 2, "still the tab's own read");
}

/// **The ordinary AI chat is never fed.** Feeding is one call, and only a
/// Bot's tab makes it: the chat tab's own module only passes it on, and the
/// window's code that draws and feeds every chat view (its recents) never
/// names it.
#[test]
fn bot_mention_ordinary_ai_chat_is_never_fed() {
    const FEED: &str = "set_mention_choices(";
    let chat_view = include_str!("../../../ai_chat_view.rs");
    assert_eq!(
        chat_view.matches(FEED).count(),
        2,
        "the wrapper's definition and its one call into the surface"
    );
    for (name, source) in [
        (
            "mainview/draw.rs",
            include_str!("../../../mainview/draw.rs"),
        ),
        (
            "mainview/bots.rs",
            include_str!("../../../mainview/bots.rs"),
        ),
        (
            "mainview/panes.rs",
            include_str!("../../../mainview/panes.rs"),
        ),
        ("mainview/mod.rs", include_str!("../../../mainview/mod.rs")),
    ] {
        assert!(!source.contains(FEED), "{name} feeds a chat's `@` picker");
    }
    let tab = include_str!("../../view.rs");
    assert_eq!(
        tab.matches(FEED).count(),
        1,
        "the Bot tab feeds its own conversation, in one place"
    );
}
