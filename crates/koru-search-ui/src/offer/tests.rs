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
