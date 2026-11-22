// SPDX-License-Identifier: MPL-2.0

//! The *Recognize text and images in PDFs* card in each of its states, what each
//! button asks the host, and every sentence in eight languages.

use super::*;
use crate::Request;
use crate::tests::{GLOBALS, goto, koru, lines, press};

fn row(status: ReaderStatus) -> ReaderRow {
    ReaderRow {
        status,
        languages: vec!["und-Latn".into(), "zh".into(), "ja".into()],
        items: Vec::new(),
        more: 0,
        partly: Vec::new(),
        partly_more: 0,
        not_here: 0,
    }
}

/// How many pressable controls on the Editor page carry `word`.
fn buttons(surface: &mut crate::SettingsSurface, word: &str) -> usize {
    surface.show_page(crate::PageId::Editor);
    surface.relayout();
    crate::tests::words(surface)
        .iter()
        .filter(|(w, ..)| w == word)
        .count()
}

fn partly(key: &str, name: &str, pages_done: u32, read_all: bool) -> ReaderPartlyRow {
    ReaderPartlyRow {
        key: key.into(),
        name: Some(name.into()),
        picture: false,
        pages_done,
        reading: false,
        read_all,
    }
}

fn not_downloaded() -> ReaderStatus {
    ReaderStatus::NotDownloaded {
        megabytes: Some(24),
        offline: false,
        ended: None,
    }
}

fn on(could_not_be_read: u64) -> ReaderStatus {
    ReaderStatus::On {
        read: 8_297,
        could_not_be_read,
        megabytes: 24,
    }
}

fn item(key: &str, name: Option<&str>, picture: bool, reason: &str) -> ReaderItemRow {
    ReaderItemRow {
        key: key.into(),
        name: name.map(str::to_owned),
        picture,
        reason: reason.into(),
        read_again: false,
    }
}

/// **The four states say what EXPERIENCE §4 says**, with the button each
/// one offers.
#[test]
fn each_state_says_its_sentence_and_offers_its_button() {
    let (line, button) = status_line(Lang::En, &not_downloaded());
    assert_eq!(line, "Not downloaded · 24 MB");
    assert!(matches!(button, Some((Action::ReaderTurnOn, "Turn on"))));

    let (line, button) = status_line(Lang::En, &on(3));
    assert_eq!(line, "On · 8,297 read · 3 could not be read");
    assert!(matches!(button, Some((Action::ReaderTurnOff, "Turn off"))));
    assert_eq!(status_line(Lang::En, &on(0)).0, "On · 8,297 read");

    let (line, button) = status_line(
        Lang::En,
        &ReaderStatus::Downloading {
            stage: ReaderDownloadStage::Downloading,
            received_megabytes: 12,
            total_megabytes: 24,
        },
    );
    assert_eq!(line, "Downloading — 12 of 24 MB");
    assert!(matches!(button, Some((Action::ReaderStop, "Stop"))));

    let (line, button) = status_line(
        Lang::En,
        &ReaderStatus::Unavailable(ReaderBlocked::NotBuilt),
    );
    assert_eq!(
        line,
        "Not available on this computer: this build reads PDFs with text and text files, but not pictures or scans."
    );
    assert!(button.is_none(), "no Turn on where it cannot be used");
}

/// Offline, or before the catalog has said how big it is, there is nothing
/// to agree to yet; after a download that did not finish, **Try again**.
#[test]
fn turn_on_needs_a_size_and_try_again_follows_a_failure() {
    let offline = ReaderStatus::NotDownloaded {
        megabytes: None,
        offline: true,
        ended: None,
    };
    let (line, button) = status_line(Lang::En, &offline);
    assert_eq!(
        line,
        "Not downloaded Needs a one-time download; Koru will offer it when you are online."
    );
    assert!(button.is_none());
    let unknown = ReaderStatus::NotDownloaded {
        megabytes: None,
        offline: false,
        ended: None,
    };
    let (line, button) = status_line(Lang::En, &unknown);
    assert_eq!(
        line,
        "Not downloaded. Waiting for download information before recognition can be turned on."
    );
    assert!(button.is_none());
    let failed = ReaderStatus::NotDownloaded {
        megabytes: Some(24),
        offline: false,
        ended: Some(ReaderDownloadEnd::Failed),
    };
    let (line, button) = status_line(Lang::En, &failed);
    assert_eq!(line, "The download did not finish.");
    assert!(matches!(button, Some((Action::ReaderTurnOn, "Try again"))));
}

/// One line says which languages and kinds of file are read; a person
/// reading Koru in Arabic is told Arabic script is not read yet.
#[test]
fn the_line_names_the_languages_and_the_kinds_of_file() {
    let line = what_is_read(Lang::En, &["und-Latn".into(), "zh".into(), "ja".into()]);
    assert!(
        line.starts_with("Reads Latin-script languages, Chinese, Japanese. Pictures (PNG"),
        "{line}"
    );
    assert!(line.contains("HEIC"), "{line}");
    assert!(!what_is_read(Lang::En, &[]).starts_with("Reads"));
    let arabic = what_is_read(Lang::Ar, &["und-Latn".into()]);
    assert!(
        arabic.ends_with(t(Lang::Ar, Key::SettingsReaderArabicNotRead)),
        "{arabic}"
    );
    assert!(!line.contains(t(Lang::En, Key::SettingsReaderArabicNotRead)));
}

/// The list names items from the host's listing, words each reason, and
/// says when it could not name one.
#[test]
fn the_list_names_each_item_and_words_its_reason() {
    assert_eq!(
        item_name(Lang::En, &item("a", Some("Lease.pdf"), false, "password")),
        "Lease.pdf"
    );
    assert_eq!(
        item_name(
            Lang::En,
            &item("b", Some("Site visit"), true, "unsupported")
        ),
        "A picture in Site visit"
    );
    assert_eq!(
        item_name(Lang::En, &item("c", None, false, "damaged")),
        "An item in another workspace"
    );
    assert_eq!(reason(Lang::En, "password"), "Protected with a password");
    assert_eq!(reason(Lang::En, "damaged"), "Damaged");
    assert_eq!(
        reason(Lang::En, "unsupported"),
        "A kind of file Koru cannot open"
    );
    for local in [
        "crashed",
        "memory-limit",
        "hostile",
        "internal",
        "unreadable",
        "new-token",
    ] {
        assert_eq!(
            reason(Lang::En, local),
            "Koru could not read it on this computer"
        );
    }
}

/// **Every sentence the card can say exists in all eight languages**, with
/// its numbers written the language's way and nothing left unfilled.
#[test]
fn every_state_is_worded_in_every_language() {
    let statuses = [
        not_downloaded(),
        on(3),
        on(0),
        ReaderStatus::Downloading {
            stage: ReaderDownloadStage::Downloading,
            received_megabytes: 1_234,
            total_megabytes: 2_048,
        },
        ReaderStatus::NotDownloaded {
            megabytes: None,
            offline: true,
            ended: None,
        },
        ReaderStatus::NotDownloaded {
            megabytes: Some(24),
            offline: false,
            ended: Some(ReaderDownloadEnd::Stopped),
        },
        ReaderStatus::Off { megabytes: 24 },
    ];
    let blocked = [
        ReaderBlocked::DownloadInformationUnavailable,
        ReaderBlocked::DownloadInformationInvalid,
        ReaderBlocked::NoTrustRoot,
        ReaderBlocked::NoWorker,
        ReaderBlocked::WorkerMissing,
        ReaderBlocked::NotBuilt,
        ReaderBlocked::NoModel,
        ReaderBlocked::WillNotLoad,
    ];
    for lang in Lang::ALL {
        let mut sentences: Vec<String> = statuses
            .iter()
            .map(|status| status_line(lang, status).0)
            .collect();
        sentences.extend(
            blocked
                .iter()
                .map(|why| status_line(lang, &ReaderStatus::Unavailable(*why)).0),
        );
        sentences.push(what_is_read(
            lang,
            &["und-Latn".into(), "zh".into(), "ja".into()],
        ));
        sentences.push(item_name(lang, &item("p", Some("Page"), true, "damaged")));
        // The partly read list (OCR Plan 3.5): how far it got, one page,
        // and a Read all pages under way.
        let capped = partly("k", "Scan.pdf", 1_200, true);
        sentences.push(how_far(lang, &capped));
        sentences.push(how_far(lang, &partly("k", "Scan.pdf", 1, true)));
        sentences.push(how_far(
            lang,
            &ReaderPartlyRow {
                reading: true,
                ..capped.clone()
            },
        ));
        for far in [
            how_far(lang, &capped),
            sentences[sentences.len() - 1].clone(),
        ] {
            assert!(
                far.contains(&number::decimal(lang, 1_200)),
                "{}: {far}",
                lang.tag()
            );
        }
        for sentence in &sentences {
            assert!(!sentence.trim().is_empty(), "{}", lang.tag());
            assert!(!sentence.contains("{}"), "{}: {sentence}", lang.tag());
        }
        assert!(
            sentences[0].contains(&number::decimal(lang, 24)),
            "{}: {}",
            lang.tag(),
            sentences[0]
        );
        assert!(
            sentences[1].contains(&number::decimal(lang, 8_297)),
            "{}: {}",
            lang.tag(),
            sentences[1]
        );
    }
}

/// **On the page**: the card sits on the Editor page with the other
/// downloads, and each button asks the host for what it says.
#[test]
fn the_card_is_on_the_editor_page_and_its_buttons_ask_the_host() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(
        !lines(&surface)
            .iter()
            .any(|l| l == "Recognize text and images in PDFs"),
        "a host that says nothing draws no card"
    );
    assert!(surface.set_reader(Some(row(not_downloaded()))));
    assert!(
        !surface.set_reader(Some(row(not_downloaded()))),
        "unchanged"
    );
    assert!(
        lines(&surface)
            .iter()
            .any(|l| l == "Recognize text and images in PDFs")
    );
    assert!(goto(&mut surface, "Turn on"));
    assert_eq!(surface.page(), crate::PageId::Editor);
    assert!(press(&mut surface, "Turn on"));
    assert_eq!(surface.take_requests(), vec![Request::TurnOnReader]);

    surface.set_reader(Some(row(ReaderStatus::Downloading {
        stage: ReaderDownloadStage::Checking,
        received_megabytes: 24,
        total_megabytes: 24,
    })));
    assert!(press(&mut surface, "Stop"));
    assert_eq!(surface.take_requests(), vec![Request::StopReaderInstall]);

    let mut with_items = row(on(2));
    with_items.items = vec![
        item("file-1", Some("Lease.pdf"), false, "password"),
        item("page-2", Some("Site visit"), true, "unsupported"),
    ];
    surface.set_reader(Some(with_items));
    // Voice's model row says *Remove* too; take its card away so the word
    // names this card's button alone.
    surface.set_voice_facts(crate::VoiceFacts::default());
    assert!(press(&mut surface, "Turn off"));
    assert_eq!(surface.take_requests(), vec![Request::TurnOffReader]);
    assert!(press(&mut surface, "Remove"));
    assert_eq!(surface.take_requests(), vec![Request::RemoveReader]);
    let all = lines(&surface);
    assert!(all.iter().any(|l| l == "Could not be read"), "{all:?}");
    assert!(all.iter().any(|l| l == "Lease.pdf"));
    assert!(all.iter().any(|l| l == "A picture in Site visit"));
    assert!(all.iter().any(|l| l == "Protected with a password"));
    assert!(press(&mut surface, "Show"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ShowReaderItem("file-1".into())]
    );
}

/// **Files whose contents are on another device** get a row of their own in
/// any state, in the words search uses, and no row when there are none (OCR
/// Plan 3.3, EXPERIENCE "On a second device").
#[test]
fn files_on_another_device_have_a_row_of_their_own() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_reader(Some(row(on(0))));
    assert!(
        !lines(&surface)
            .iter()
            .any(|l| l.contains("not on this computer")),
        "none, no row"
    );
    for status in [on(0), not_downloaded()] {
        let mut elsewhere = row(status);
        elsewhere.not_here = 38;
        surface.set_reader(Some(elsewhere));
        let all = lines(&surface);
        assert!(
            all.iter()
                .any(|l| l == "38 files are not on this computer."),
            "{all:?}"
        );
    }
    assert_eq!(not_here(Lang::En, 1), "One file is not on this computer.");
    for lang in Lang::ALL {
        let sentence = not_here(lang, 1_240);
        assert!(
            sentence.contains(&number::decimal(lang, 1_240)) && !sentence.contains("{}"),
            "{}: {sentence}",
            lang.tag()
        );
    }
}

/// **Off and still downloaded** (OCR Plan 3.5): the row says so, offers
/// **Turn on** with no size (nothing to download), and keeps **Remove**.
#[test]
fn off_says_so_offers_turn_on_without_a_size_and_keeps_remove() {
    let (line, button) = status_line(Lang::En, &ReaderStatus::Off { megabytes: 24 });
    assert_eq!(line, "Off. What was already read can still be found.");
    assert!(!line.contains("MB"), "nothing to download");
    assert!(matches!(button, Some((Action::ReaderTurnOn, "Turn on"))));

    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_voice_facts(crate::VoiceFacts::default());
    surface.set_reader(Some(row(ReaderStatus::Off { megabytes: 24 })));
    let all = lines(&surface);
    assert!(all.iter().any(|l| l == "24 MB on this computer"), "{all:?}");
    assert!(press(&mut surface, "Turn on"));
    assert_eq!(surface.take_requests(), vec![Request::TurnOnReader]);
    assert!(press(&mut surface, "Remove"));
    assert_eq!(surface.take_requests(), vec![Request::RemoveReader]);
}

/// **Read again beside Show** on an item where a menu would offer it, and
/// only there (OCR Plan 3.5).
#[test]
fn read_again_sits_beside_show_where_it_is_offered() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut with_items = row(on(2));
    with_items.items = vec![
        item("ws/odd/b3:01", Some("Odd.pdf"), false, "unsupported"),
        ReaderItemRow {
            read_again: true,
            ..item("ws/bad/b3:02", Some("Bad.pdf"), false, "crashed")
        },
    ];
    surface.set_reader(Some(with_items));
    assert_eq!(buttons(&mut surface, "Read again"), 1);
    assert_eq!(buttons(&mut surface, "Show"), 2);
    assert!(press(&mut surface, "Read again"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ReadReaderItemAgain("ws/bad/b3:02".into())]
    );
}

/// **The partly read list** (OCR Plan 3.5; EXPERIENCE §4): under the could
/// not be read list, each item with its name, how far it got, **Show**, and
/// **Read all pages** where offered; the button asks the host for that item.
#[test]
fn the_partly_read_list_offers_read_all_pages_beside_the_file() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut card = row(on(1));
    card.items = vec![item(
        "ws/locked/b3:03",
        Some("Locked.pdf"),
        false,
        "password",
    )];
    card.partly = vec![
        partly("ws/scan/b3:04", "Scan.pdf", 200, true),
        partly("ws/away/b3:05", "Away.pdf", 200, false),
        ReaderPartlyRow {
            reading: true,
            ..partly("ws/going/b3:06", "Going.pdf", 230, false)
        },
    ];
    surface.set_reader(Some(card));
    let all = lines(&surface);
    let at = |text: &str| all.iter().position(|l| l == text).expect(text);
    assert!(
        at("Could not be read") < at("Partly read"),
        "under the could-not-be-read list: {all:?}"
    );
    assert!(at("Partly read") < at("Scan.pdf"));
    assert_eq!(
        all.iter().filter(|l| *l == "First 200 pages read").count(),
        2
    );
    assert!(
        all.iter()
            .any(|l| l == "Reading all pages: 230 read so far")
    );
    assert_eq!(
        buttons(&mut surface, "Read all pages"),
        1,
        "only where offered"
    );
    assert_eq!(buttons(&mut surface, "Show"), 4, "every item, both lists");
    assert!(press(&mut surface, "Read all pages"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ReadAllReaderPages("ws/scan/b3:04".into())]
    );
}

/// The partly read list is cut as the other is, and counts the rest.
#[test]
fn a_long_partly_read_list_counts_the_rest() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut long = row(on(0));
    long.partly = (0..READER_ITEMS_SHOWN + 2)
        .map(|i| partly(&i.to_string(), &format!("Scan {i}.pdf"), 200, true))
        .collect();
    long.partly_more = 3;
    surface.set_reader(Some(long));
    let all = lines(&surface);
    assert!(all.iter().any(|l| l == "Scan 0.pdf"));
    assert!(
        !all.iter()
            .any(|l| l == &format!("Scan {}.pdf", READER_ITEMS_SHOWN))
    );
    assert!(all.iter().any(|l| l == "Not listed here: 5"), "{all:?}");
}

/// A long list shows its first ones and says how many more.
#[test]
fn a_long_list_shows_its_first_items_and_counts_the_rest() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let mut long = row(on(80));
    long.items = (0..READER_ITEMS_SHOWN + 5)
        .map(|i| {
            item(
                &i.to_string(),
                Some(&format!("File {i}.pdf")),
                false,
                "damaged",
            )
        })
        .collect();
    long.more = 25;
    surface.set_reader(Some(long));
    let all = lines(&surface);
    assert!(all.iter().any(|l| l == "File 0.pdf"));
    assert!(
        !all.iter()
            .any(|l| l == &format!("File {}.pdf", READER_ITEMS_SHOWN))
    );
    assert!(all.iter().any(|l| l == "Not listed here: 30"), "{all:?}");
}
