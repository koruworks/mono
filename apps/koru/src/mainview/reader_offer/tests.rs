// SPDX-License-Identifier: MPL-2.0

//! The import summary's line on the toast: when it appears, what each of its
//! two answers asks the shell, the download Turn on starts, and all of it
//! from the keyboard alone. No Store: the shell's half is a pane request.

use koru_store_wire::reader::{
    CatalogStage, CatalogState, InstallProgress, InstallStage, InstalledReader,
};
use koru_workspace_host::View;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::mainview::harness::on_document;

fn terms() -> ReaderTerms {
    ReaderTerms {
        model: "pp-ocrv6-small".into(),
        version: "1".into(),
        licence: "Apache-2.0".into(),
        attribution: "PaddleOCR".into(),
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

fn reading(pictures_and_scans: u64, state: ReaderState) -> ImportReading {
    ImportReading {
        pictures_and_scans,
        state,
    }
}

fn requests(v: &mut MainView) -> Vec<PaneRequest> {
    std::iter::from_fn(|| View::take_pane_request(v)).collect()
}

fn reader_answers(v: &mut MainView) -> Vec<ReaderAnswer> {
    requests(v)
        .into_iter()
        .filter_map(|request| match request {
            PaneRequest::ReaderAnswer(answer) => Some(answer),
            _ => None,
        })
        .collect()
}

/// An imported notebook's summary, carrying the line.
fn summary(v: &mut MainView) -> koru_core::DocumentId {
    let notebook = koru_core::DocumentId::new();
    assert!(View::show_import_notice_reading(
        v,
        "Import succeeded.".into(),
        koru_workspace_host::ImportNoticeTarget::Notebook(notebook),
        reading(412, offered()),
    ));
    notebook
}

fn press(v: &mut MainView, ws: &mut koru_core::WorkspaceDoc, key: NamedKey) -> bool {
    v.key(ws, &Key::Named(key), None, ModifiersState::empty())
}

/// **The summary gains its line** with the design's sentence, **Turn on**
/// and **Not now**, and keeps its own action.
#[test]
fn the_summary_gains_its_line_and_keeps_its_action() {
    let (mut v, _workspace) = on_document();
    let notebook = summary(&mut v);
    let (toast, payload) = v.toast.as_ref().expect("a summary");
    assert_eq!(toast.message(), "Import succeeded.");
    assert_eq!(toast.action(), Some("View Notebook"));
    assert_eq!(
        toast.line_sentence(),
        Some(
            "412 pictures and scanned PDFs can be found by the words in them. Koru reads them on this computer, not in the cloud. One-time download, 24 MB."
        )
    );
    assert_eq!(toast.line_actions(), ["Turn on", "Not now"]);
    assert!(matches!(
        payload,
        ToastPayload::ImportReading { go, .. } if **go == ToastPayload::Open(notebook)
    ));
}

/// **Only under its conditions**: an import that brought none, a reader
/// already here, or one that cannot be used here is the plain summary.
#[test]
fn without_its_conditions_the_summary_is_the_plain_one() {
    let (mut v, _workspace) = on_document();
    let installed = ReaderState {
        installed: Some(InstalledReader {
            model: "pp-ocrv6-small".into(),
            version: "1".into(),
            download_bytes: 24_536_678,
            licence: "Apache-2.0".into(),
            attribution: "PaddleOCR".into(),
        }),
        ..offered()
    };
    let unavailable = ReaderState {
        unavailable: Some(koru_store_wire::reader::ReaderUnavailable::NoTrustRoot),
        ..offered()
    };
    for (count, state) in [(0, offered()), (5, installed), (5, unavailable)] {
        let file = koru_core::DocumentId::new();
        assert!(View::show_import_notice_reading(
            &mut v,
            "Import succeeded.".into(),
            koru_workspace_host::ImportNoticeTarget::File(file),
            reading(count, state),
        ));
        let (toast, payload) = v.toast.as_ref().unwrap();
        assert_eq!(toast.line_sentence(), None);
        assert_eq!(toast.action(), Some("View file"));
        assert_eq!(*payload, ToastPayload::ShowFile(file));
    }
}

/// **Turn on** asks the shell to install with exactly the terms the line
/// showed, from the offer, and the toast becomes the download's progress
/// with **Stop**; each state the shell hands back moves it on.
#[test]
fn turn_on_installs_with_the_terms_shown_and_shows_progress_with_stop() {
    let (mut v, _workspace) = on_document();
    summary(&mut v);
    assert!(v.press_toast_line(0));
    assert_eq!(
        reader_answers(&mut v),
        [ReaderAnswer::TurnOn(InstallReader {
            terms: terms(),
            from: InstallFrom::Offer,
        })]
    );
    let (toast, payload) = v.toast.as_ref().unwrap();
    assert_eq!(*payload, ToastPayload::ReaderDownload);
    assert_eq!(toast.action(), Some("Stop"));
    assert_eq!(toast.message(), "Preparing the download…");

    let mib = 1024 * 1024;
    assert!(View::reader_answered(
        &mut v,
        Some(&installing(InstallStage::Downloading, 12 * mib, 24 * mib)),
    ));
    assert_eq!(
        v.toast.as_ref().unwrap().0.message(),
        "Downloading — 12 of 24 MB"
    );

    // Stop: asked once, and offered no more while it stops.
    v.stop_reader_download();
    assert_eq!(reader_answers(&mut v), [ReaderAnswer::Stop]);
    assert_eq!(v.toast.as_ref().unwrap().0.action(), None);

    // It stopped: Try again (which resumes) and Not now.
    assert!(View::reader_answered(
        &mut v,
        Some(&installing(InstallStage::Stopped, 12 * mib, 24 * mib)),
    ));
    let (toast, payload) = v.toast.as_ref().unwrap();
    assert_eq!(toast.message(), "The download was stopped.");
    assert_eq!(toast.action(), Some("Try again"));
    assert_eq!(toast.secondary(), Some("Not now"));
    assert!(matches!(
        payload,
        ToastPayload::ReaderEnded { terms: Some(_) }
    ));

    // Try again, and this time it installs: the toast goes; reading starts
    // on its own.
    let ToastPayload::ReaderEnded { terms: again } = payload.clone() else {
        unreachable!()
    };
    assert!(v.turn_on_reader(again));
    assert!(matches!(
        reader_answers(&mut v).as_slice(),
        [ReaderAnswer::TurnOn(_)]
    ));
    assert!(View::reader_answered(
        &mut v,
        Some(&installing(InstallStage::Installed, 24 * mib, 24 * mib)),
    ));
    assert!(v.toast.is_none());
}

/// **Not now** asks the shell to decline; the line goes and the summary
/// stays, its action still going where it went.
#[test]
fn not_now_declines_and_leaves_the_summary() {
    let (mut v, _workspace) = on_document();
    let notebook = summary(&mut v);
    assert!(v.press_toast_line(1));
    assert_eq!(reader_answers(&mut v), [ReaderAnswer::NotNow]);
    let (toast, payload) = v.toast.as_ref().expect("the summary stays");
    assert_eq!(toast.line_sentence(), None);
    assert_eq!(toast.action(), Some("View Notebook"));
    assert_eq!(*payload, ToastPayload::Open(notebook));
    assert!(!v.press_toast_line(0), "nothing left to answer");
}

/// A toast somebody closed is not brought back by the download's news, and
/// a Store that did not answer takes the progress away.
#[test]
fn the_download_never_recreates_a_closed_toast() {
    let (mut v, _workspace) = on_document();
    assert!(!View::reader_answered(&mut v, Some(&offered())));
    summary(&mut v);
    v.press_toast_line(0);
    v.toast = None;
    assert!(!View::reader_answered(
        &mut v,
        Some(&installing(InstallStage::Downloading, 1, 4)),
    ));
    assert!(v.toast.is_none());
    v.turn_on_reader(Some(terms()));
    assert!(View::reader_answered(&mut v, None));
    assert!(v.toast.is_none());
}

/// **By pointer**: each answer is pressed where the toast draws it.
#[test]
fn each_answer_is_pressed_where_it_is_drawn() {
    let (mut v, mut workspace) = on_document();
    summary(&mut v);
    let content = v.regions.content;
    let floor = v.toast_floor();
    let rects = {
        let (toast, _) = v.toast.as_ref().unwrap();
        toast.line_rects(&mut v.engine, content, floor)
    };
    assert_eq!(rects.len(), 2);
    let (x, y) = rects[1].centre();
    assert!(v.mouse_down(&mut workspace.ws, x, y, false));
    assert_eq!(reader_answers(&mut v), [ReaderAnswer::NotNow]);
}

/// **Keyboard only**: `F6` lands on *Turn on* first while the summary is up,
/// → reaches *Not now*, Enter presses it, and the keyboard leaves with the
/// line. Escape leaves without answering.
#[test]
fn f6_reaches_the_answers_and_enter_presses_them() {
    let (mut v, mut workspace) = on_document();
    summary(&mut v);
    assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
    assert_eq!(v.toast_line_focus(), Some(0), "F6 lands on Turn on");
    assert!(press(&mut v, &mut workspace.ws, NamedKey::ArrowRight));
    assert_eq!(v.toast_line_focus(), Some(1), "→ reaches Not now");
    assert!(press(&mut v, &mut workspace.ws, NamedKey::ArrowRight));
    assert_eq!(
        v.toast_line_focus(),
        Some(0),
        "and wraps, as every stop does"
    );
    assert!(press(&mut v, &mut workspace.ws, NamedKey::ArrowLeft));
    assert_eq!(v.toast_line_focus(), Some(1));
    assert!(press(&mut v, &mut workspace.ws, NamedKey::Enter));
    assert_eq!(reader_answers(&mut v), [ReaderAnswer::NotNow]);
    assert_eq!(
        v.toast_line_focus(),
        None,
        "the keyboard left with the line"
    );
    assert_eq!(v.toast_line_count(), 0);

    summary(&mut v);
    assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
    assert!(press(&mut v, &mut workspace.ws, NamedKey::Escape));
    assert_eq!(v.toast_line_focus(), None);
    assert!(reader_answers(&mut v).is_empty(), "Escape answers nothing");

    assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
    assert!(press(&mut v, &mut workspace.ws, NamedKey::Space));
    assert!(matches!(
        reader_answers(&mut v).as_slice(),
        [ReaderAnswer::TurnOn(InstallReader {
            from: InstallFrom::Offer,
            ..
        })]
    ));
    assert_eq!(v.toast.as_ref().unwrap().1, ToastPayload::ReaderDownload);
}

/// The stop is there only while the line is: a faded or replaced summary
/// leaves no dead stop behind, and `F6` goes on to the chrome.
#[test]
fn a_summary_that_went_leaves_no_stop_behind() {
    let (mut v, mut workspace) = on_document();
    summary(&mut v);
    assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
    assert_eq!(v.toast_line_focus(), Some(0));
    v.toast = None;
    press(&mut v, &mut workspace.ws, NamedKey::Escape);
    assert_eq!(v.toast_line_focus(), None, "the stop went with the toast");
    assert!(reader_answers(&mut v).is_empty());
    assert!(press(&mut v, &mut workspace.ws, NamedKey::F6));
    assert!(
        !matches!(v.chrome_focus, Some(ChromeStop::Toast(_))),
        "{:?}",
        v.chrome_focus
    );
}
