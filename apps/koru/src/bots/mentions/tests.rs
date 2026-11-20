// SPDX-License-Identifier: MPL-2.0

//! Who a Bot's conversation offers under `@`, and how the list stays fresh.

use super::*;
use crate::bots::tests::bot;

fn listed(name: &str, home: WorkspaceId) -> Listed {
    Listed {
        id: BotId::mint(),
        name: name.to_owned(),
        paused: false,
        home,
        archived: false,
        owner: None,
    }
}

fn asker(home: WorkspaceId) -> Asker {
    Asker {
        id: BotId::mint(),
        home,
        may_ask: true,
    }
}

fn titles(section: &ExtraSection) -> Vec<&str> {
    section
        .choices
        .iter()
        .map(|choice| choice.title.as_str())
        .collect()
}

/// **Only Bots that can actually be asked are offered**: the same person's
/// Bots in the same home workspace, run by this computer, active, and not
/// the Bot itself — the set supplied by the native roster.
#[test]
fn bot_mention_feed_excludes_itself_archived_other_workspaces_and_other_computers() {
    let home = WorkspaceId::new();
    let me = asker(home);
    let myself = Listed {
        id: me.id,
        ..listed("Scout", home)
    };
    let cfo = listed("CFO", home);
    let legal = listed("Legal", home);
    let archived = Listed {
        archived: true,
        ..listed("Retired", home)
    };
    let elsewhere = listed("Other workspace", WorkspaceId::new());
    let remote = Listed {
        owner: Some(DeviceId::new()),
        ..listed("On the laptop", home)
    };
    let all = [
        myself,
        cfo.clone(),
        archived,
        elsewhere,
        remote,
        legal.clone(),
    ];

    assert_eq!(askable(Some(&me), &all), vec![&cfo, &legal]);

    let section = section(Lang::En, Some(&me), &all);
    assert_eq!(
        titles(&section),
        vec!["CFO", "Legal"],
        "in the order listed"
    );
    assert_eq!(section.label, "Bot");
    assert_eq!(section.role, koru_core::attributes::ROLE_BOT);
    assert_eq!(
        section
            .choices
            .iter()
            .map(|choice| choice.id.clone())
            .collect::<Vec<_>>(),
        vec![cfo.id.as_uuid().to_string(), legal.id.as_uuid().to_string()],
        "a choice carries the Bot's id, never only its name"
    );
}

/// A paused Bot is offered and marked: asking it is refused until it is
/// resumed, and the row says why before the person finds out.
#[test]
fn bot_mention_feed_lists_a_paused_bot_and_marks_it() {
    let home = WorkspaceId::new();
    let me = asker(home);
    let paused = Listed {
        paused: true,
        ..listed("Legal", home)
    };
    let all = [listed("CFO", home), paused];
    let section = section(Lang::En, Some(&me), &all);
    assert_eq!(titles(&section), vec!["CFO", "Legal"]);
    assert_eq!(section.choices[0].note, None);
    assert_eq!(section.choices[1].note.as_deref(), Some("Paused"));
    // In the person's language, never blank.
    for lang in Lang::ALL {
        let section = super::section(lang, Some(&me), &all);
        assert!(!section.label.trim().is_empty(), "{lang:?}");
        assert!(
            section.choices[1]
                .note
                .as_deref()
                .is_some_and(|note| !note.trim().is_empty()),
            "{lang:?}"
        );
    }
}

/// **With *May ask other bots* off nobody is offered** — the picker is the
/// pages alone — and the same when the tab may not offer anybody at all.
#[test]
fn bot_mention_feed_is_empty_when_the_setting_is_off() {
    let home = WorkspaceId::new();
    let all = [listed("CFO", home), listed("Legal", home)];
    let off = Asker {
        may_ask: false,
        ..asker(home)
    };
    assert!(askable(Some(&off), &all).is_empty());
    assert_eq!(section(Lang::En, Some(&off), &all), ExtraSection::default());
    assert!(askable(None, &all).is_empty());
    assert_eq!(section(Lang::En, None, &all), ExtraSection::default());
    // Nobody to offer is the empty section too: no label with no rows.
    assert_eq!(
        section(Lang::En, Some(&asker(home)), &[]),
        ExtraSection::default()
    );
    // Sent mentions still read as their Bots' names, whatever the setting.
    assert_eq!(names(&all).len(), 2);
}

/// A Bot of the service's own listing: another device's is that
/// computer's, an archived one is archived.
#[test]
fn bot_mention_listing_tells_this_computers_bots_from_another_computers() {
    let home = WorkspaceId::new();
    let mine = bot(BotId::mint(), home, "CFO", false);
    let mut theirs = bot(BotId::mint(), home, "Laptop", true);
    theirs.owner_device = DeviceId::new();
    let mut archived = bot(BotId::mint(), home, "Retired", false);
    archived.lifecycle = BotLifecycle::Archived;
    let here = Some(mine.owner_device);

    assert_eq!(Listed::of(&mine, here).owner, None);
    assert_eq!(Listed::of(&theirs, here).owner, Some(theirs.owner_device));
    assert!(Listed::of(&theirs, here).paused);
    assert!(Listed::of(&archived, Some(archived.owner_device)).archived);
    // A window that does not know which device it is takes the listing as
    // this computer's, as the roster does.
    assert_eq!(Listed::of(&theirs, None).owner, None);
}

/// **How the list stays fresh.** The roster's rows are what the first `@`
/// shows; the tab's own read replaces them and is the fresher from then on;
/// it is asked for again once it is old, one ask at a time; a failed or a
/// stale answer changes nothing.
#[test]
fn bot_mention_list_is_seeded_then_read_and_kept_fresh() {
    let home = WorkspaceId::new();
    let here = None;
    let t0 = Instant::now();
    let mut mentions = Mentions::default();
    assert!(mentions.due(t0), "nothing read yet");

    assert!(mentions.seed([listed("CFO", home)]));
    assert_eq!(mentions.listed().len(), 1);
    assert!(
        mentions.due(t0),
        "the roster's rows may be old: still asked"
    );

    let seq = mentions.asking(t0);
    assert!(!mentions.due(t0), "one ask at a time");
    assert!(mentions.waiting(t0));

    // An answer to an older ask, or about another workspace, is not this.
    let cfo = bot(BotId::mint(), home, "CFO", false);
    let legal = bot(BotId::mint(), home, "Legal", true);
    assert!(!mentions.answer(
        seq.wrapping_sub(1),
        Some(home),
        home,
        Ok(vec![legal.clone()]),
        here,
        t0
    ));
    assert_eq!(mentions.listed()[0].name, "CFO");

    assert!(mentions.answer(
        seq,
        Some(home),
        home,
        Ok(vec![cfo.clone(), legal.clone()]),
        here,
        t0
    ));
    assert_eq!(
        mentions
            .listed()
            .iter()
            .map(|bot| (bot.name.as_str(), bot.paused))
            .collect::<Vec<_>>(),
        vec![("CFO", false), ("Legal", true)]
    );
    assert!(!mentions.waiting(t0));
    assert!(!mentions.due(t0), "fresh");
    assert!(
        !mentions.seed([listed("Stale roster row", home)]),
        "the tab's own read is the fresher one"
    );

    // Old again: asked once more. A failed read keeps what was listed.
    let later = t0 + STALE_AFTER;
    assert!(mentions.due(later));
    let seq = mentions.asking(later);
    assert!(!mentions.answer(seq, Some(home), home, Err(Failure::Unanswered), here, later));
    assert_eq!(mentions.listed().len(), 2, "never *there are no Bots*");
    assert!(mentions.due(later), "and it is asked again");

    // The link would not take the ask: nothing is on its way.
    mentions.asking(later);
    mentions.unsent();
    assert!(mentions.due(later));

    // An answer about another workspace than the Bot's home is dropped.
    let seq = mentions.asking(later);
    assert!(!mentions.answer(
        seq,
        Some(home),
        WorkspaceId::new(),
        Ok(Vec::new()),
        here,
        later
    ));
    assert_eq!(mentions.listed().len(), 2);
}

/// What is fed is rebuilt only when what it is made from moved, and says
/// the same thing each time otherwise.
#[test]
fn bot_mention_feed_follows_the_list_the_asker_and_the_language() {
    let home = WorkspaceId::new();
    let me = asker(home);
    let mut mentions = Mentions::default();
    mentions.seed([listed("CFO", home)]);
    let (section, names) = mentions.feed(Lang::En, Some(me));
    assert_eq!(titles(section), vec!["CFO"]);
    assert_eq!(names.len(), 1);

    let off = Asker {
        may_ask: false,
        ..me
    };
    let (section, names) = mentions.feed(Lang::En, Some(off));
    assert!(section.choices.is_empty(), "switched off: pages only");
    assert_eq!(names.len(), 1, "sent mentions keep their names");

    mentions.seed([listed("CFO", home), listed("Legal", home)]);
    let (section, _) = mentions.feed(Lang::En, Some(me));
    assert_eq!(titles(section), vec!["CFO", "Legal"]);
}
