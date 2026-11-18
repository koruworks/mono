// SPDX-License-Identifier: MPL-2.0

//! **The line in an import's summary, and the download it starts** (OCR Plan
//! 3.3; EXPERIENCE §3 "The first time", *Not now*, "Offline").
//!
//! An import through an importer tells the reader how many pictures and
//! scanned PDFs it brought, and the shell hands this window the count and the
//! reader's state the Store answered with. When that state says to offer, the
//! summary's toast gains one line — *412 pictures and scanned PDFs can be
//! found by the words in them. Koru reads them on this computer. One-time
//! download, 24 MB.* — with **Turn on** and **Not now**. The words are the
//! line in search's (`koru_search_ui::import_summary`); this module only puts
//! them on the toast and carries out what is pressed.
//!
//! - **Turn on** sends the terms the line showed the size of, and the toast
//!   becomes the download's progress with **Stop**, as the line in search
//!   does. The shell watches the download and hands each state back
//!   ([`MainView::reader_answered_toast`]); a download that ends without
//!   installing says so with **Try again** and **Not now**, and one that
//!   installs takes the toast away — reading starts on its own.
//! - **Not now** takes the line away and leaves the summary: the Store
//!   quiets the line in search until the next import that brings pictures.
//!
//! **From the keyboard**, `F6` reaches the line's answers first while the
//! toast is up (`chrome_focus.rs`): ← and → move between them, Enter or
//! Space presses, Escape gives the keyboard back. The chip holds while the
//! keyboard is on it.

use std::time::Instant;

use koru_i18n::{Key, t};
use koru_search_ui::LineAction;
use koru_store_wire::reader::{InstallFrom, InstallReader, ReaderState, ReaderTerms};
use koru_workspace_host::PaneRequest;
use koru_workspace_host::reader_offer::{ImportReading, ReaderAnswer};

use super::chrome_focus::ChromeStop;
use super::{MainView, ToastPayload};

/// The summary's own action and where it goes, by what the import made — in
/// the person's language, like the line under it (OCR Plan 3.5).
pub(super) fn import_notice_action(
    target: koru_workspace_host::ImportNoticeTarget,
) -> (&'static str, ToastPayload) {
    let lang = crate::format::lang();
    match target {
        koru_workspace_host::ImportNoticeTarget::Notebook(document) => (
            t(lang, Key::ImportNoticeViewNotebook),
            ToastPayload::Open(document),
        ),
        koru_workspace_host::ImportNoticeTarget::Document(document) => (
            t(lang, Key::ImportNoticeViewFile),
            ToastPayload::Open(document),
        ),
        koru_workspace_host::ImportNoticeTarget::File(file) => (
            t(lang, Key::ImportNoticeViewFile),
            ToastPayload::ShowFile(file),
        ),
    }
}

impl MainView {
    /// **An importer's summary**, with its line when the reader's state says
    /// to offer; the plain summary otherwise.
    pub(super) fn show_import_reading(
        &mut self,
        message: String,
        target: koru_workspace_host::ImportNoticeTarget,
        reading: ImportReading,
    ) -> bool {
        let lang = crate::format::lang();
        let Some(line) =
            koru_search_ui::import_summary(lang, reading.pictures_and_scans, &reading.state)
        else {
            return koru_workspace_host::View::show_import_notice(self, message, target);
        };
        let (action, go) = import_notice_action(target);
        let labels = line
            .actions
            .iter()
            .map(|action| action.label(lang))
            .collect();
        self.toast = Some((
            koru_ui::Toast::new(message, Some(action), Instant::now())
                .with_line(line.sentence, labels),
            ToastPayload::ImportReading {
                go: Box::new(go),
                line: line.actions,
                terms: reading.state.offer,
            },
        ));
        true
    }

    /// The line's answer the keyboard is on, when `F6` put it there.
    pub(super) fn toast_line_focus(&self) -> Option<usize> {
        match self.chrome_focus {
            Some(ChromeStop::Toast(index)) => Some(index),
            _ => None,
        }
    }

    /// How many answers the toast's line has: none without a line.
    pub(super) fn toast_line_count(&self) -> usize {
        self.toast
            .as_ref()
            .map_or(0, |(toast, _)| toast.line_actions().len())
    }

    /// **Press the line's answer `index`**, from the pointer or the keyboard.
    pub(super) fn press_toast_line(&mut self, index: usize) -> bool {
        let Some((_, ToastPayload::ImportReading { line, terms, .. })) = self.toast.as_ref() else {
            return false;
        };
        let Some(&action) = line.get(index) else {
            return false;
        };
        let terms = terms.clone();
        if self.toast_line_focus().is_some() {
            self.chrome_focus = None;
        }
        match action {
            LineAction::TurnOn | LineAction::TryAgain => self.turn_on_reader(terms),
            LineAction::NotNow => {
                self.ask_pane(PaneRequest::ReaderAnswer(ReaderAnswer::NotNow));
                // The answer was given: the line goes, the summary stays, and
                // its action goes where it always did.
                if let Some((toast, payload)) = self.toast.as_mut() {
                    toast.clear_line();
                    if let ToastPayload::ImportReading { go, .. } = payload {
                        let go = std::mem::replace(go, Box::new(ToastPayload::None));
                        *payload = *go;
                    }
                }
                true
            }
            LineAction::Stop => {
                self.stop_reader_download();
                true
            }
        }
    }

    /// **Turn on** (or **Try again**) with the terms the person was shown: the
    /// shell installs, and the toast becomes the download's progress with
    /// **Stop** until the first state arrives. No terms, nothing to agree to.
    pub(super) fn turn_on_reader(&mut self, terms: Option<ReaderTerms>) -> bool {
        let Some(terms) = terms else {
            return false;
        };
        self.ask_pane(PaneRequest::ReaderAnswer(ReaderAnswer::TurnOn(
            InstallReader {
                terms,
                from: InstallFrom::Offer,
            },
        )));
        let lang = crate::format::lang();
        self.toast = Some((
            koru_ui::Toast::new(
                t(lang, Key::ReaderDownloadPreparing).to_owned(),
                Some(LineAction::Stop.label(lang)),
                Instant::now(),
            )
            .with_progress(koru_ui::ProgressMode::Indeterminate),
            ToastPayload::ReaderDownload,
        ));
        true
    }

    /// **Stop** on the download's toast. The toast stays, saying it is
    /// stopping, and its action goes at once: an accepted Stop cannot be
    /// asked for twice.
    pub(super) fn stop_reader_download(&mut self) {
        self.ask_pane(PaneRequest::ReaderAnswer(ReaderAnswer::Stop));
        let lang = crate::format::lang();
        if let Some((toast, ToastPayload::ReaderDownload)) = self.toast.as_mut() {
            toast.set_progress(
                t(lang, Key::ReaderDownloadStopping).to_owned(),
                koru_ui::ProgressMode::Indeterminate,
                Instant::now(),
            );
            toast.set_action(None);
        }
    }

    /// **Not now** on a download that ended: the line goes quiet, and so
    /// does the toast.
    pub(super) fn decline_reader_from_toast(&mut self) {
        self.ask_pane(PaneRequest::ReaderAnswer(ReaderAnswer::NotNow));
        self.toast = None;
    }

    /// **The Store's answer about the download Turn on started**: its
    /// progress while it runs, *did not finish* or *was stopped* with **Try
    /// again** and **Not now** once it ended that way, and no toast once it
    /// installed — reading then starts on its own. A toast somebody closed is
    /// not brought back; `None` (the Store did not answer) takes the progress
    /// away rather than leaving it standing, and the line in search is still
    /// there.
    pub(super) fn reader_answered_toast(&mut self, state: Option<&ReaderState>) -> bool {
        let Some((toast, ToastPayload::ReaderDownload)) = self.toast.as_mut() else {
            return false;
        };
        let lang = crate::format::lang();
        let Some(state) = state else {
            self.toast = None;
            return true;
        };
        let running = state
            .install
            .as_ref()
            .is_some_and(|install| install.stage.is_running());
        match koru_search_ui::install_line(lang, state) {
            Some(line) if running => {
                let progress = line.fraction.map_or(
                    koru_ui::ProgressMode::Indeterminate,
                    koru_ui::ProgressMode::Determinate,
                );
                toast.set_progress(line.sentence, progress, Instant::now());
                toast.set_action(line.actions.first().map(|action| action.label(lang)));
            }
            Some(line) => {
                self.toast = Some((
                    koru_ui::Toast::new(
                        line.sentence,
                        Some(LineAction::TryAgain.label(lang)),
                        Instant::now(),
                    )
                    .with_secondary(LineAction::NotNow.label(lang)),
                    ToastPayload::ReaderEnded {
                        terms: state.offer.clone(),
                    },
                ));
            }
            None => self.toast = None,
        }
        true
    }
}

#[cfg(test)]
mod tests;
