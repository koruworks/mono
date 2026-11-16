// SPDX-License-Identifier: MPL-2.0

//! The line's every state, in words, in every language.

use super::*;
use koru_store_wire::reader::{CatalogState, InstallProgress, ReaderTerms};

fn counts(needs_reader: u64, unread: u64, total: u64, offer: bool) -> ReadingCounts {
    ReadingCounts {
        unread,
        total,
        needs_reader,
        not_here: 0,
        offer,
    }
}

fn terms() -> ReaderTerms {
    ReaderTerms {
        model: "pp-ocrv6-small".into(),
        version: "1".into(),
        licence: "Apache-2.0".into(),
        attribution: "PaddleOCR".into(),
        // 23.4 MiB: rounds up to 24, as voice's sizes do.
        download_bytes: 24_536_678,
        languages: vec!["und-Latn".into(), "zh".into(), "ja".into()],
    }
}

fn offered() -> ReaderState {
    ReaderState {
        offer: Some(terms()),
        catalog: CatalogState {
            state: CatalogStage::Ready,
            code: None,
        },
        ..ReaderState::default()
    }
}

fn installing(stage: InstallStage, received: u64, total: u64) -> ReaderState {
    ReaderState {
        install: Some(InstallProgress {
            stage,
            received,
            total,
            code: None,
        }),
        ..offered()
    }
}

/// **The two positions say the two sentences** of EXPERIENCE §3, each with
/// the real size, **Turn on** and **Not now**.
#[test]
fn the_offer_says_its_sentence_for_where_it_sits() {
    let waiting = counts(412, 412, 500, true);
    let below = view(Lang::En, Some(&waiting), Some(&offered()), true).unwrap();
    assert_eq!(
        below.sentence,
        "412 pictures and scanned PDFs can be found by the words in them. One-time download, 24 MB."
    );
    assert_eq!(below.actions, [LineAction::TurnOn, LineAction::NotNow]);
    let empty = view(Lang::En, Some(&waiting), Some(&offered()), false).unwrap();
    assert_eq!(
        empty.sentence,
        "412 pictures and scanned PDFs have not been read, so they cannot be found by their contents yet. One-time download, 24 MB."
    );
    assert_eq!(empty.actions, [LineAction::TurnOn, LineAction::NotNow]);
}

/// One item is a sentence about one item, not *1 pictures*.
#[test]
fn one_item_is_a_sentence_of_its_own() {
    let one = counts(1, 1, 1, true);
    let line = view(Lang::En, Some(&one), Some(&offered()), true).unwrap();
    assert!(
        line.sentence
            .starts_with("One picture or scanned PDF can be found"),
        "{}",
        line.sentence
    );
}

/// Counts and megabytes are written the way each language writes numbers,
/// and every language gets a whole sentence with nothing left unfilled.
#[test]
fn every_language_writes_its_own_numbers() {
    let waiting = counts(1_240, 1_240, 8_300, true);
    for lang in Lang::ALL {
        for with_results in [true, false] {
            let line = view(lang, Some(&waiting), Some(&offered()), with_results).unwrap();
            assert!(
                !line.sentence.contains("{}"),
                "{}: {}",
                lang.tag(),
                line.sentence
            );
            assert!(
                line.sentence.contains(&number::decimal(lang, 1_240)),
                "{}: {}",
                lang.tag(),
                line.sentence
            );
            assert!(
                line.sentence.contains(&number::decimal(lang, 24)),
                "{}: {}",
                lang.tag(),
                line.sentence
            );
            for action in &line.actions {
                assert!(!action.label(lang).trim().is_empty());
            }
        }
    }
    let arabic = view(Lang::Ar, Some(&waiting), Some(&offered()), true).unwrap();
    assert!(arabic.sentence.contains("١٬٢٤٠"), "{}", arabic.sentence);
}

/// **No size, no line**: the size is the real one or nothing. A state not
/// fetched yet, or a catalog still being read, draws nothing yet.
#[test]
fn no_line_until_the_size_is_known() {
    let waiting = counts(5, 5, 5, true);
    assert_eq!(view(Lang::En, Some(&waiting), None, true), None);
    let reading = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Reading,
            code: None,
        },
        ..ReaderState::default()
    };
    assert_eq!(view(Lang::En, Some(&waiting), Some(&reading), true), None);
}

/// Offline: the sentence, and *needs a one-time download* in place of the
/// actions.
#[test]
fn offline_the_line_says_so_and_offers_nothing_to_press() {
    let offline = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Failed,
            code: Some("offline".into()),
        },
        ..ReaderState::default()
    };
    let line = view(
        Lang::En,
        Some(&counts(3, 3, 3, true)),
        Some(&offline),
        false,
    )
    .unwrap();
    assert!(line.actions.is_empty());
    assert!(
        line.sentence
            .ends_with("Needs a one-time download; Koru will offer it when you are online."),
        "{}",
        line.sentence
    );
}

/// **Not now is respected**: once the Store stops offering, neither position
/// shows the offer — and with nothing being read, nothing at all.
#[test]
fn after_not_now_neither_position_offers() {
    let declined = counts(412, 412, 500, false);
    let state = ReaderState {
        offer_declined: true,
        ..offered()
    };
    assert_eq!(view(Lang::En, Some(&declined), Some(&state), true), None);
    assert_eq!(view(Lang::En, Some(&declined), Some(&state), false), None);
}

/// A store that counts nothing — a phone, an older Store — gets no line.
#[test]
fn a_store_that_counts_nothing_gets_no_line() {
    assert_eq!(view(Lang::En, None, Some(&offered()), true), None);
}

/// **Turn on shows progress with Stop**, the megabytes as the language writes
/// them, and the bar's fraction; checking and finishing are their own words,
/// and a Stop being carried out offers no second Stop.
#[test]
fn a_running_download_shows_its_progress_and_stop() {
    let waiting = counts(412, 412, 500, true);
    let mib = 1024 * 1024;
    let line = view(
        Lang::En,
        Some(&waiting),
        Some(&installing(InstallStage::Downloading, 12 * mib, 24 * mib)),
        true,
    )
    .unwrap();
    assert_eq!(line.sentence, "Downloading — 12 of 24 MB");
    assert_eq!(line.actions, [LineAction::Stop]);
    assert_eq!(line.fraction, Some(0.5));
    let checking = view(
        Lang::En,
        Some(&waiting),
        Some(&installing(InstallStage::Verifying, 0, 0)),
        false,
    )
    .unwrap();
    assert_eq!(checking.sentence, "Checking the download…");
    let stopping = view(
        Lang::En,
        Some(&waiting),
        Some(&installing(InstallStage::Stopping, 0, 0)),
        true,
    )
    .unwrap();
    assert!(stopping.actions.is_empty());
    // Progress outranks the counts, which predate the download.
    let stale = counts(412, 412, 500, false);
    assert!(
        view(
            Lang::En,
            Some(&stale),
            Some(&installing(InstallStage::Starting, 0, 0)),
            true
        )
        .is_some()
    );
}

/// The download failed or was stopped: the line says so, with **Try again**
/// (which resumes) and **Not now**.
#[test]
fn a_failed_or_stopped_download_offers_try_again() {
    let waiting = counts(4, 4, 4, true);
    for (stage, words) in [
        (InstallStage::Failed, "The download did not finish."),
        (InstallStage::Stopped, "The download was stopped."),
    ] {
        let line = view(
            Lang::En,
            Some(&waiting),
            Some(&installing(stage, 0, 0)),
            true,
        )
        .unwrap();
        assert_eq!(line.sentence, words);
        assert_eq!(line.actions, [LineAction::TryAgain, LineAction::NotNow]);
    }
}

/// **Reading under way, below results**: the quiet progress line, counting
/// only what is being read here. With no results it is OCR Plan 3.3's.
#[test]
fn reading_under_way_is_a_quiet_line_below_results_only() {
    let mut reading = counts(0, 1_240, 8_300, false);
    reading.not_here = 40;
    let line = view(
        Lang::En,
        Some(&reading),
        Some(&ReaderState::default()),
        true,
    )
    .unwrap();
    assert_eq!(
        line.sentence,
        "Still reading 1,200 of 8,300 pictures and PDFs."
    );
    assert!(line.actions.is_empty());
    assert_eq!(view(Lang::En, Some(&reading), None, false), None);
    // Nothing left to read: no line.
    assert_eq!(
        view(Lang::En, Some(&counts(0, 0, 8_300, false)), None, true),
        None
    );
}

/// **Turn on sends exactly the terms the person was shown**, from the offer
/// line; Not now declines; Stop stops.
#[test]
fn each_action_asks_the_store_what_the_design_says() {
    let mut offer = Offer::default();
    assert_eq!(
        offer.command(LineAction::TurnOn),
        None,
        "no terms, no install"
    );
    offer.answered(offered(), Instant::now());
    assert_eq!(
        offer.command(LineAction::TurnOn),
        Some(ReaderCommand::Install(InstallReader {
            terms: terms(),
            from: InstallFrom::Offer,
        }))
    );
    assert_eq!(
        offer.command(LineAction::TryAgain),
        offer.command(LineAction::TurnOn)
    );
    assert_eq!(
        offer.command(LineAction::NotNow),
        Some(ReaderCommand::Decline)
    );
    assert_eq!(offer.command(LineAction::Stop), Some(ReaderCommand::Stop));
}

/// **Nothing polls a settled line.** A running download is asked about on a
/// short deadline; a catalog being read a few times; otherwise never.
#[test]
fn only_a_running_download_or_a_catalog_being_read_is_polled() {
    let now = Instant::now();
    let mut offer = Offer::default();
    assert_eq!(offer.poll_deadline(), None);
    offer.answered(offered(), now);
    assert_eq!(offer.poll_deadline(), None, "settled");
    offer.answered(installing(InstallStage::Downloading, 1, 2), now);
    assert_eq!(offer.poll_deadline(), Some(now + INSTALL_POLL));
    offer.asked();
    assert_eq!(offer.poll_deadline(), None, "one request at a time");
    let finished = offer.answered(installing(InstallStage::Installed, 2, 2), now);
    assert!(finished, "the counts the last answer carried are stale now");
    assert_eq!(offer.poll_deadline(), None);

    let mut catalog = Offer::default();
    let reading = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Reading,
            code: None,
        },
        ..ReaderState::default()
    };
    for _ in 0..CATALOG_POLLS {
        catalog.answered(reading.clone(), now);
        assert!(catalog.poll_deadline().is_some());
        catalog.poll_taken();
    }
    catalog.answered(reading, now);
    assert_eq!(catalog.poll_deadline(), None, "a bounded number of times");
}

/// The state is fetched only when an answer could put a line on screen, and
/// not again while a fresh one is in hand.
#[test]
fn the_state_is_fetched_only_when_the_line_needs_it() {
    let now = Instant::now();
    let mut offer = Offer::default();
    assert!(!offer.wants_state(None, now));
    assert!(!offer.wants_state(Some(&counts(0, 3, 3, false)), now));
    let waiting = counts(3, 3, 3, true);
    assert!(offer.wants_state(Some(&waiting), now));
    offer.asked();
    assert!(!offer.wants_state(Some(&waiting), now), "one is on its way");
    offer.answered(offered(), now);
    assert!(
        !offer.wants_state(Some(&waiting), now),
        "a fresh one is in hand"
    );
    assert!(offer.wants_state(Some(&waiting), now + STATE_FRESH));
}

// ================================================ the empty page (OCR Plan 3.3)

/// **The three empty pages** of EXPERIENCE "Before there is anything", each
/// decided from the counts alone.
#[test]
fn a_search_that_found_nothing_is_one_of_three_pages() {
    // 1. The reader is not here and items wait for it: *No results*, and the
    //    offer directly below it saying why.
    let waiting = counts(412, 412, 500, true);
    assert_eq!(
        empty_progress(Lang::En, Some(&waiting), Some(&offered())),
        None
    );
    let offer = view(Lang::En, Some(&waiting), Some(&offered()), false).unwrap();
    assert!(
        offer
            .sentence
            .starts_with("412 pictures and scanned PDFs have not been read"),
        "{}",
        offer.sentence
    );

    // 2. Reading is under way: a progress report in place of *No results*,
    //    and no line under it.
    let reading = counts(0, 1_240, 8_300, false);
    assert_eq!(
        empty_progress(Lang::En, Some(&reading), Some(&ReaderState::default())),
        Some((
            "Still reading 1,240 of 8,300 pictures and PDFs.".to_owned(),
            "Results appear as they are read.".to_owned()
        ))
    );
    assert_eq!(view(Lang::En, Some(&reading), None, false), None);

    // 3. Everything in scope has been read: the ordinary page, nothing else.
    let read = counts(0, 0, 8_300, false);
    assert_eq!(empty_progress(Lang::En, Some(&read), None), None);
    assert_eq!(view(Lang::En, Some(&read), None, false), None);

    // A store that counts nothing says nothing either way.
    assert_eq!(empty_progress(Lang::En, None, None), None);
}

/// **`N` is what is being read here**, so it falls to zero and the page
/// becomes the verdict: items waiting for the reader (after a *Not now*) and
/// items whose contents are elsewhere are never "still being read".
#[test]
fn the_number_counts_only_what_is_being_read_here_and_reaches_zero() {
    let mut counts = counts(300, 340, 1_000, false);
    counts.not_here = 30;
    let (heading, _) = empty_progress(Lang::En, Some(&counts), None).unwrap();
    assert_eq!(heading, "Still reading 10 of 1,000 pictures and PDFs.");
    assert_eq!(left(&counts), 10);
    // The last of them read: only the waiting and the absent remain.
    counts.unread = 330;
    assert_eq!(left(&counts), 0);
    assert_eq!(empty_progress(Lang::En, Some(&counts), None), None);
}

/// A download running is the line (its progress), not the page: the counts
/// predate it.
#[test]
fn a_running_download_keeps_the_page_a_verdict_with_its_progress_below() {
    let reading = counts(0, 5, 5, false);
    let running = installing(InstallStage::Downloading, 1, 4);
    assert_eq!(
        empty_progress(Lang::En, Some(&reading), Some(&running)),
        None
    );
    assert!(view(Lang::En, Some(&reading), Some(&running), false).is_some());
}

/// The progress page in every language: the counts written the language's
/// way, nothing left unfilled, and the second sentence there too.
#[test]
fn the_progress_page_writes_its_numbers_in_every_language() {
    let reading = counts(0, 1_240, 8_300, false);
    for lang in Lang::ALL {
        let (heading, detail) = empty_progress(lang, Some(&reading), None).unwrap();
        assert!(!heading.contains("{}"), "{}: {heading}", lang.tag());
        assert!(
            heading.contains(&number::decimal(lang, 1_240))
                && heading.contains(&number::decimal(lang, 8_300)),
            "{}: {heading}",
            lang.tag()
        );
        assert!(!detail.trim().is_empty(), "{}", lang.tag());
    }
}

// ======================================== not on this computer (OCR Plan 3.3)

/// **Files whose contents are on another device are named as such**, never
/// counted as unread (EXPERIENCE "On a second device"): below *No results*
/// once nothing here is left to read, and not under every list of results.
#[test]
fn files_on_another_device_are_named_below_no_results() {
    let mut elsewhere = counts(0, 38, 100, false);
    elsewhere.not_here = 38;
    let line = view(Lang::En, Some(&elsewhere), None, false).unwrap();
    assert_eq!(line.sentence, "38 files are not on this computer.");
    assert!(line.actions.is_empty());
    assert_eq!(
        empty_progress(Lang::En, Some(&elsewhere), None),
        None,
        "nothing here is being read, so the page is the verdict"
    );
    assert_eq!(
        view(Lang::En, Some(&elsewhere), None, true),
        None,
        "quiet below results"
    );

    elsewhere.unread = 1;
    elsewhere.not_here = 1;
    assert_eq!(
        view(Lang::En, Some(&elsewhere), None, false)
            .unwrap()
            .sentence,
        "One file is not on this computer."
    );

    // Still reading here: the progress page comes first.
    let mut both = counts(0, 50, 100, false);
    both.not_here = 38;
    assert_eq!(view(Lang::En, Some(&both), None, false), None);
    assert!(empty_progress(Lang::En, Some(&both), None).is_some());
}

#[test]
fn not_on_this_computer_writes_its_number_in_every_language() {
    let mut elsewhere = counts(0, 1_240, 2_000, false);
    elsewhere.not_here = 1_240;
    for lang in Lang::ALL {
        let line = view(lang, Some(&elsewhere), None, false).unwrap();
        assert!(
            line.sentence.contains(&number::decimal(lang, 1_240)) && !line.sentence.contains("{}"),
            "{}: {}",
            lang.tag(),
            line.sentence
        );
        assert!(!not_here(lang, 1).trim().is_empty());
    }
}

// ===================================== the import's summary (OCR Plan 3.3)

/// **EXPERIENCE §3's sentence, word for word**, with the real size, **Turn
/// on** and **Not now**.
#[test]
fn the_summary_says_the_designs_sentence() {
    let line = import_summary(Lang::En, 412, &offered()).unwrap();
    assert_eq!(
        line.sentence,
        "412 pictures and scanned PDFs can be found by the words in them. Koru reads them on this computer. One-time download, 24 MB."
    );
    assert_eq!(line.actions, [LineAction::TurnOn, LineAction::NotNow]);
    let one = import_summary(Lang::En, 1, &offered()).unwrap();
    assert_eq!(
        one.sentence,
        "One picture or scanned PDF can be found by the words in it. Koru reads it on this computer. One-time download, 24 MB."
    );
}

/// **Only under its conditions**: the import brought some, no reader is
/// installed, the reader can be used here, nothing already answered (a
/// download running, a *Not now* the Store still holds), and a size to say.
#[test]
fn the_summary_carries_the_line_only_under_its_conditions() {
    assert!(import_summary(Lang::En, 3, &offered()).is_some());
    assert_eq!(
        import_summary(Lang::En, 0, &offered()),
        None,
        "brought none"
    );
    let installed = ReaderState {
        installed: Some(koru_store_wire::reader::InstalledReader {
            model: "pp-ocrv6-small".into(),
            version: "1".into(),
            download_bytes: 24_536_678,
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
        }),
        ..offered()
    };
    assert_eq!(
        import_summary(Lang::En, 3, &installed),
        None,
        "already here"
    );
    let unavailable = ReaderState {
        unavailable: Some(koru_store_wire::reader::ReaderUnavailable::NotBuilt),
        ..offered()
    };
    assert_eq!(
        import_summary(Lang::En, 3, &unavailable),
        None,
        "cannot be used here"
    );
    assert_eq!(
        import_summary(Lang::En, 3, &installing(InstallStage::Downloading, 1, 4)),
        None,
        "somebody already pressed Turn on"
    );
    let declined = ReaderState {
        offer_declined: true,
        ..offered()
    };
    assert_eq!(
        import_summary(Lang::En, 3, &declined),
        None,
        "Not now still stands"
    );
    let no_size = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Reading,
            code: None,
        },
        ..ReaderState::default()
    };
    assert_eq!(
        import_summary(Lang::En, 3, &no_size),
        None,
        "no size, no line"
    );
    // A download that failed before is offered again from the summary.
    assert!(import_summary(Lang::En, 3, &installing(InstallStage::Failed, 0, 0)).is_some());
}

/// Offline: the sentence, and *needs a one-time download* in place of the
/// actions (EXPERIENCE "Offline").
#[test]
fn offline_the_summary_says_so_and_offers_nothing_to_press() {
    let offline = ReaderState {
        catalog: CatalogState {
            state: CatalogStage::Failed,
            code: Some("offline".into()),
        },
        ..ReaderState::default()
    };
    let line = import_summary(Lang::En, 7, &offline).unwrap();
    assert!(line.actions.is_empty());
    assert_eq!(
        line.sentence,
        "7 pictures and scanned PDFs can be found by the words in them. Koru reads them on this computer. Needs a one-time download; Koru will offer it when you are online."
    );
}

#[test]
fn the_summary_writes_its_numbers_in_every_language() {
    for lang in Lang::ALL {
        let line = import_summary(lang, 1_240, &offered()).unwrap();
        assert!(
            line.sentence.contains(&number::decimal(lang, 1_240))
                && line.sentence.contains(&number::decimal(lang, 24))
                && !line.sentence.contains("{}"),
            "{}: {}",
            lang.tag(),
            line.sentence
        );
    }
}

/// **Turn on shows progress with Stop**, as the line in search does; a
/// download that ended says how, with **Try again** and **Not now**; one that
/// installed has nothing more to say.
#[test]
fn a_download_started_from_the_summary_says_what_the_line_says() {
    let mib = 1024 * 1024;
    let running = install_line(
        Lang::En,
        &installing(InstallStage::Downloading, 6 * mib, 24 * mib),
    )
    .unwrap();
    assert_eq!(running.sentence, "Downloading — 6 of 24 MB");
    assert_eq!(running.actions, [LineAction::Stop]);
    assert_eq!(running.fraction, Some(0.25));
    let stopped = install_line(Lang::En, &installing(InstallStage::Stopped, 0, 0)).unwrap();
    assert_eq!(stopped.actions, [LineAction::TryAgain, LineAction::NotNow]);
    assert_eq!(
        install_line(Lang::En, &installing(InstallStage::Installed, 0, 0)),
        None
    );
    assert_eq!(install_line(Lang::En, &offered()), None);
}

// ============================================================ on the surface

mod on_the_surface {
    use super::*;
    use crate::reader::{BackgroundWork, SearchReader};
    use crate::{SearchModal, SearchSurface};
    use koru_api_client::SearchHit;
    use koru_draw::{inspect, scene};
    use koru_store_core::ids::{DocumentId, WorkspaceId};
    use koru_store_wire::search::{Lifecycle, field};
    use std::sync::mpsc::Receiver;
    use vello::Scene;
    use vello::kurbo::Affine;
    use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

    fn a_hit(n: usize) -> SearchHit {
        let document = DocumentId::new().to_storage();
        SearchHit {
            workspace: WorkspaceId::new().to_string(),
            workspace_name: "Acme".into(),
            document: Some(document.clone()),
            node: Some(document),
            document_kind: "page".into(),
            mime: None,
            kind: "paragraph".into(),
            field: field::BODY.into(),
            title: format!("Page {n}"),
            breadcrumb: "Acme".into(),
            breadcrumb_spans: Vec::new(),
            snippet: "the pump".into(),
            spans: Vec::new(),
            lifecycle: Lifecycle::Live,
            trashed_at: None,
            trashed_by: None,
            score: 1.0,
            observed_modified_at_ms: None,
            workspace_last_used_at_ms: None,
            page: None,
            picture_name: None,
            list: None,
            task: None,
        }
    }

    /// A surface whose last answer offered the reader, with the reader's
    /// state in hand and a background channel a test can read.
    fn offering(hits: usize) -> (SearchSurface, Receiver<BackgroundWork>) {
        let (reader, _responses, _requests, background) = SearchReader::test_pair_with_background();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        surface.set_viewport(720.0, 520.0);
        surface.query.set_text(&mut surface.engine, "pump");
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits = (0..hits).map(a_hit).collect();
        surface.page.reading = Some(counts(412, 412, 500, true));
        surface.offer.answered(offered(), Instant::now());
        (surface, background)
    }

    fn key(surface: &mut SearchSurface, key: NamedKey) -> bool {
        surface.key(&WinitKey::Named(key), None, ModifiersState::empty())
    }

    fn reader_commands(background: &Receiver<BackgroundWork>) -> Vec<ReaderCommand> {
        background
            .try_iter()
            .filter_map(|work| match work {
                BackgroundWork::Reader(command) => Some(command),
                _ => None,
            })
            .collect()
    }

    /// **Keyboard only**: Tab reaches *Turn on*, the arrows move to *Not now*
    /// and back, Tab past the last action hands the keyboard back to the
    /// results, Shift+Tab comes in from the end, and Escape leaves the line
    /// without clearing the query.
    #[test]
    fn the_line_is_reached_and_left_by_keyboard_alone() {
        let (mut surface, _background) = offering(3);
        assert_eq!(surface.offer_focus(), None);
        assert!(key(&mut surface, NamedKey::Tab));
        assert_eq!(surface.offer_focus(), Some(0), "Tab lands on Turn on");
        key(&mut surface, NamedKey::ArrowRight);
        assert_eq!(surface.offer_focus(), Some(1), "Right reaches Not now");
        key(&mut surface, NamedKey::ArrowRight);
        assert_eq!(surface.offer_focus(), Some(1), "and stops there");
        key(&mut surface, NamedKey::ArrowLeft);
        assert_eq!(surface.offer_focus(), Some(0));
        key(&mut surface, NamedKey::Tab);
        assert_eq!(surface.offer_focus(), Some(1));
        key(&mut surface, NamedKey::Tab);
        assert_eq!(
            surface.offer_focus(),
            None,
            "past the last, back to the results"
        );
        surface.key(&WinitKey::Named(NamedKey::Tab), None, ModifiersState::SHIFT);
        assert_eq!(
            surface.offer_focus(),
            Some(1),
            "Shift+Tab comes in from the end"
        );
        key(&mut surface, NamedKey::Escape);
        assert_eq!(surface.offer_focus(), None);
        assert_eq!(
            surface.query.raw_text(),
            "pump",
            "Escape left the line, not the query"
        );
        // Arrows on the results still walk the results.
        let selected = surface.list.selected;
        key(&mut surface, NamedKey::ArrowDown);
        assert_eq!(surface.list.selected, selected + 1);
    }

    /// **Enter on *Turn on*** sends the terms the line was showing, through
    /// the background channel, never from the key handler.
    #[test]
    fn enter_on_turn_on_installs_with_the_terms_shown() {
        let (mut surface, background) = offering(2);
        key(&mut surface, NamedKey::Tab);
        assert!(key(&mut surface, NamedKey::Enter));
        assert_eq!(
            reader_commands(&background),
            [ReaderCommand::Install(InstallReader {
                terms: terms(),
                from: InstallFrom::Offer,
            })]
        );
        assert!(
            surface.target.is_none(),
            "Enter pressed the button, not a result"
        );
    }

    /// **Space on *Not now*** declines, and the line leaves search in both
    /// positions at once.
    #[test]
    fn not_now_removes_the_line_in_both_positions() {
        for hits in [3, 0] {
            let (mut surface, background) = offering(hits);
            assert!(
                surface.offer_view().is_some(),
                "{hits} hits: the line is up"
            );
            key(&mut surface, NamedKey::Tab);
            key(&mut surface, NamedKey::ArrowRight);
            key(&mut surface, NamedKey::Space);
            assert_eq!(reader_commands(&background), [ReaderCommand::Decline]);
            assert_eq!(surface.offer_view(), None, "{hits} hits: gone");
            assert_eq!(surface.offer_focus(), None);
        }
    }

    /// Typing while the keyboard is on the line gives it back to the query
    /// box: the line never swallows a letter.
    #[test]
    fn typing_on_the_line_goes_to_the_query() {
        let (mut surface, _background) = offering(1);
        key(&mut surface, NamedKey::Tab);
        surface.key(
            &WinitKey::Character("s".into()),
            Some("s"),
            ModifiersState::empty(),
        );
        assert_eq!(surface.offer_focus(), None);
        assert_eq!(surface.query.raw_text(), "pumps");
    }

    /// With no line, Tab still changes nothing — the surface's old answer.
    #[test]
    fn with_no_line_tab_changes_nothing() {
        let (mut surface, _background) = offering(2);
        surface.page.reading = None;
        assert!(key(&mut surface, NamedKey::Tab));
        assert_eq!(surface.offer_focus(), None);
        assert_eq!(surface.query.raw_text(), "pump");
    }

    /// **Below the list, the list gives way**: the results end where the band
    /// begins, the band ends at the footer, and each button is pressable
    /// exactly where it is drawn.
    #[test]
    fn below_results_the_band_sits_under_the_list_and_its_buttons_hit_test() {
        let (mut surface, background) = offering(30);
        let band = surface.offer_band().expect("a band");
        let results = surface.results_rect();
        assert!(
            (results.y + results.h - band.y).abs() < 0.5,
            "{results:?} {band:?}"
        );
        assert!((band.y + band.h - (surface.viewport.1 - crate::FOOTER_H)).abs() < 0.5);
        let (_, _, text, buttons) = surface.offer_layout().unwrap();
        assert_eq!(buttons.len(), 2);
        assert!(
            text.x + text.w <= buttons[0].x,
            "the sentence never runs under a button"
        );
        for (index, rect) in buttons.iter().enumerate() {
            let (x, y) = (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
            assert_eq!(surface.offer_action_at(x, y), Some(index));
        }
        let first = buttons[0];
        assert!(surface.mouse_down(first.x + 2.0, first.y + 2.0, false));
        assert!(matches!(
            reader_commands(&background).as_slice(),
            [ReaderCommand::Install(_)]
        ));
    }

    /// **Below *No results***, the line sits directly under its text and the
    /// page's own links move down past it rather than under it.
    #[test]
    fn with_no_results_the_line_sits_under_the_text_and_the_links_move_down() {
        let (surface, _background) = offering(0);
        let band = surface.offer_band().expect("a band");
        assert!((band.y - (surface.results_top() + crate::OFFER_EMPTY_Y)).abs() < 0.5);
        let deleted = surface.deleted_pages_rect();
        assert!(deleted.y >= band.y + band.h, "{deleted:?} {band:?}");
    }

    /// The F12 tree: the line is one component under the modal, its buttons
    /// the shared `KoruButton` nested inside it.
    #[test]
    fn the_inspector_nests_the_line_and_its_buttons_under_the_modal() {
        let viewport = (900.0, 650.0);
        let modal = SearchModal::new(viewport);
        let (mut search, _background) = offering(4);
        search.set_viewport(modal.bounds().w, modal.bounds().h);
        let root = Affine::IDENTITY;
        let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, viewport.0, viewport.1));
        modal.draw(&mut search, &mut Scene::new(), root, true, (0.0, 0.0));
        let records = capture.finish().records;
        let line = records
            .iter()
            .position(|record| record.name == "Reading offer")
            .expect("the line is drawn");
        let buttons = records
            .iter()
            .filter(|record| record.name == "KoruButton" && record.parent == Some(line))
            .count();
        assert_eq!(buttons, 2, "Turn on and Not now");
        let mut at = records[line].parent;
        let mut owner = None;
        while let Some(index) = at {
            owner = Some(index);
            at = records[index].parent;
        }
        assert_eq!(owner.map(|index| records[index].name), Some("SearchModal"));
    }

    /// **Nothing found while reading is a progress report** (OCR Plan 3.3):
    /// the surface's empty page is *Still reading N of M…*, with no line
    /// under it and none of the verdict's links pressable; once the reading
    /// is done the ordinary page and its links are back.
    #[test]
    fn nothing_found_while_reading_is_the_progress_page_until_it_is_done() {
        let (mut surface, _background) = offering(0);
        surface.page.reading = Some(counts(0, 1_240, 8_300, false));
        assert_eq!(
            surface.reading_heading().map(|(heading, _)| heading),
            Some(koru_i18n::fill(
                koru_i18n::t(koru_i18n::current(), koru_i18n::Key::ReaderStillReading),
                &[
                    &number::decimal(koru_i18n::current(), 1_240),
                    &number::decimal(koru_i18n::current(), 8_300),
                ],
            ))
        );
        assert_eq!(
            surface.offer_view(),
            None,
            "the page is the report, not a line"
        );
        let link = surface.deleted_pages_rect();
        let (x, y) = (link.x + 4.0, link.y + link.h * 0.5);
        surface.mouse_down(x, y, false);
        assert_eq!(
            surface.lifecycle,
            koru_store_wire::search::LifecycleFilter::Live,
            "a link that is not drawn is not pressed"
        );

        surface.page.reading = Some(counts(0, 0, 8_300, false));
        assert_eq!(
            surface.reading_heading(),
            None,
            "everything read: the verdict"
        );
        assert!(surface.empty_page_is_verdict());
        surface.mouse_down(x, y, false);
        assert_eq!(
            surface.lifecycle,
            koru_store_wire::search::LifecycleFilter::Trash
        );
    }

    /// The offer still owns the empty page while the reader is not here:
    /// *No results*, and the offer directly below it.
    #[test]
    fn while_the_reader_is_offered_the_empty_page_is_the_verdict_with_the_offer() {
        let (surface, _background) = offering(0);
        assert_eq!(surface.reading_heading(), None);
        assert!(surface.offer_view().is_some());
    }

    /// **The counts ask for the state, once**; a running download is polled
    /// on its deadline from `maintain`, never from a draw.
    #[test]
    fn an_answer_that_offers_asks_for_the_state_and_a_download_is_polled() {
        let (reader, _responses, _requests, background) = SearchReader::test_pair_with_background();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        surface.page.reading = Some(counts(3, 3, 3, true));
        surface.ask_reader_state();
        surface.ask_reader_state();
        assert_eq!(reader_commands(&background), [ReaderCommand::Read]);
        let now = Instant::now();
        surface
            .offer
            .answered(installing(InstallStage::Downloading, 1, 4), now);
        let due = surface
            .maintenance_deadline()
            .expect("a download is polled");
        assert!(due <= now + INSTALL_POLL);
        surface.maintain(now + INSTALL_POLL);
        assert_eq!(reader_commands(&background), [ReaderCommand::Read]);
    }
}

/// **While the reader is downloaded and off, search says nothing about what
/// waits for it** (OCR Plan 3.5): the Store's answer carries `offer: false`
/// (it is downloaded) and the items waiting for the reader in
/// `needs_reader`, which *Still reading* never counts — so no line below
/// results, the ordinary *No results*, and no line in an import's summary. A
/// person who turned it off was not undecided.
#[test]
fn a_reader_turned_off_quiets_the_line_and_the_summary() {
    let off = ReaderState {
        installed: Some(koru_store_wire::reader::InstalledReader {
            model: "pp-ocrv6-small".into(),
            version: "1".into(),
            download_bytes: 24_536_678,
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
        }),
        turned_off: true,
        ..offered()
    };
    // Three pictures wait for the reader; nothing else is left to read.
    let waiting = counts(3, 3, 10, false);
    for with_results in [true, false] {
        assert_eq!(
            view(Lang::En, Some(&waiting), Some(&off), with_results),
            None,
            "with results: {with_results}"
        );
    }
    assert_eq!(empty_progress(Lang::En, Some(&waiting), Some(&off)), None);
    assert_eq!(import_summary(Lang::En, 3, &off), None);
    // What is still being read is still reported: a text-layer PDF.
    let reading = counts(3, 5, 10, false);
    assert!(view(Lang::En, Some(&reading), Some(&off), true).is_some());
}
