// SPDX-License-Identifier: MPL-2.0

//! **The one line in search about pictures and scans** (OCR EXPERIENCE §3
//! "The first time", "After that", "Offline"; §4; OCR Plan 2.5).
//!
//! At most one line on the surface at a time, and this module decides which
//! and what it says. Everything it reads is a value the surface already has:
//! the counts the last search answer carried for *its* workspaces
//! ([`ReadingCounts`]), and the reader's state the surface's background
//! thread last fetched ([`ReaderState`]). Nothing here dials anything; the
//! surface does that off the frame loop, on the channel its catalog uses.
//!
//! ## What the line says, in order
//!
//! 1. **A download running** — its progress, and **Stop**. Whatever the last
//!    answer's counts said, because they predate the download.
//! 2. **The offer**, while the Store says to offer (`offer`: something here
//!    waits for the reader, it is not installed, *Not now* was not answered,
//!    nothing says it cannot be used):
//!    - the last download failed or was stopped: that, with **Try again** and
//!      **Not now**;
//!    - offline (the catalog could not be read for want of a network): the
//!      sentence, with *needs a one-time download; Koru will offer it when
//!      you are online* in place of the actions;
//!    - otherwise: the sentence for where it sits — below results, or below
//!      *No results* saying why — with the real size, **Turn on** and **Not
//!      now**. No size known yet means no line yet: the size is never a number
//!      written into the text.
//! 3. **Reading under way, below results**: *Still reading N of M pictures
//!    and PDFs*, while something here is left to read. With no results that
//!    sentence is the empty page's own progress report, which is OCR Plan
//!    3.3's, so this module says nothing there.
//!
//! ## Keys
//!
//! The surface's conventions: the arrows walk the results, Enter opens, Escape
//! backs out. The line's actions are buttons, so **Tab** reaches them — Tab
//! from the results lands on the first, Tab moves on, and Tab past the last
//! hands the keyboard back to the results (Shift+Tab the other way). On an
//! action, Left and Right move between actions, Enter or Space presses it, and
//! Escape leaves the line without clearing the query. Anything else gives the
//! keyboard back and is handled as it would have been.

use std::time::{Duration, Instant};

use koru_i18n::{Key, Lang, fill, number, t};
use koru_store_wire::reader::{
    CatalogStage, InstallFrom, InstallReader, InstallStage, ReaderState,
};
use koru_store_wire::search::ReadingCounts;

/// How often a running download is asked about.
pub(crate) const INSTALL_POLL: Duration = Duration::from_millis(500);

/// How often, and how many times, the state is asked again while the Store is
/// still reading its catalog — the first read of the state is what makes it
/// fetch one, so a second read soon after usually carries the size.
pub(crate) const CATALOG_POLL: Duration = Duration::from_secs(1);
pub(crate) const CATALOG_POLLS: u32 = 10;

/// A state older than this is asked for again when a new answer wants it.
pub(crate) const STATE_FRESH: Duration = Duration::from_secs(30);

/// What a person can press on the line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum LineAction {
    TurnOn,
    NotNow,
    Stop,
    TryAgain,
}

impl LineAction {
    /// The button's words.
    pub(crate) fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::TurnOn => t(lang, Key::ReaderTurnOn),
            Self::NotNow => t(lang, Key::ReaderNotNow),
            // The words voice's download uses for its own Stop.
            Self::Stop => t(lang, Key::VoiceStop),
            Self::TryAgain => t(lang, Key::TryAgain),
        }
    }

    /// The primary answer is filled; the others are quiet.
    pub(crate) fn primary(self) -> bool {
        matches!(self, Self::TurnOn | Self::TryAgain)
    }
}

/// The line as it is drawn: one sentence, the actions beside it, and how far a
/// download has got when it can say.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LineView {
    pub(crate) sentence: String,
    pub(crate) actions: Vec<LineAction>,
    /// `Some` while a download with a known total runs.
    pub(crate) fraction: Option<f32>,
}

/// **Which line, if any, and what it says.** Pure: the counts the surface's
/// last answer carried, the reader's state it last fetched, whether the page
/// has results, and the language.
pub(crate) fn view(
    lang: Lang,
    counts: Option<&ReadingCounts>,
    reader: Option<&ReaderState>,
    with_results: bool,
) -> Option<LineView> {
    // A store that says nothing about reading — a phone, an older Store —
    // gets no line: absence is never read as "everything is read".
    let counts = counts?;
    if let Some(install) = reader.and_then(|state| state.install.as_ref())
        && install.stage.is_running()
    {
        return Some(progress(
            lang,
            install.stage,
            install.received,
            install.total,
        ));
    }
    if counts.offer {
        return offer(lang, counts, reader, with_results);
    }
    if !with_results {
        // The empty page's own progress report is OCR Plan 3.3's.
        return None;
    }
    let left = counts
        .unread
        .saturating_sub(counts.not_here)
        .saturating_sub(counts.needs_reader);
    (left > 0).then(|| LineView {
        sentence: fill(
            t(lang, Key::ReaderStillReading),
            &[
                &number::decimal(lang, left),
                &number::decimal(lang, counts.total),
            ],
        ),
        actions: Vec::new(),
        fraction: None,
    })
}

fn offer(
    lang: Lang,
    counts: &ReadingCounts,
    reader: Option<&ReaderState>,
    with_results: bool,
) -> Option<LineView> {
    let reader = reader?;
    let waiting = counts.needs_reader;
    if waiting == 0 {
        return None;
    }
    let lead = lead(lang, waiting, with_results);
    match reader.install.as_ref().map(|install| install.stage) {
        Some(InstallStage::Failed) => {
            return Some(LineView {
                sentence: t(lang, Key::ReaderDownloadFailed).to_owned(),
                actions: vec![LineAction::TryAgain, LineAction::NotNow],
                fraction: None,
            });
        }
        Some(InstallStage::Stopped) => {
            return Some(LineView {
                sentence: t(lang, Key::ReaderDownloadStopped).to_owned(),
                actions: vec![LineAction::TryAgain, LineAction::NotNow],
                fraction: None,
            });
        }
        _ => {}
    }
    if reader.catalog.state == CatalogStage::Failed
        && reader.catalog.code.as_deref() == Some("offline")
    {
        return Some(LineView {
            sentence: sentences(lang, &lead, t(lang, Key::ReaderOfferOffline)),
            actions: Vec::new(),
            fraction: None,
        });
    }
    let terms = reader.offer.as_ref()?;
    let size = fill(
        t(lang, Key::ReaderOfferSize),
        &[&number::decimal(
            lang,
            koru_voice_consent::megabytes(terms.download_bytes),
        )],
    );
    Some(LineView {
        sentence: sentences(lang, &lead, &size),
        actions: vec![LineAction::TurnOn, LineAction::NotNow],
        fraction: None,
    })
}

/// The first sentence: what could be found, below results; why it was not,
/// below *No results*. A whole sentence for one item and one for many.
fn lead(lang: Lang, waiting: u64, with_results: bool) -> String {
    match (with_results, waiting) {
        (true, 1) => t(lang, Key::ReaderOfferFoundOne).to_owned(),
        (true, _) => fill(
            t(lang, Key::ReaderOfferFound),
            &[&number::decimal(lang, waiting)],
        ),
        (false, 1) => t(lang, Key::ReaderOfferNotReadOne).to_owned(),
        (false, _) => fill(
            t(lang, Key::ReaderOfferNotRead),
            &[&number::decimal(lang, waiting)],
        ),
    }
}

fn sentences(lang: Lang, first: &str, second: &str) -> String {
    fill(t(lang, Key::ReaderSentences), &[first, second])
}

fn progress(lang: Lang, stage: InstallStage, received: u64, total: u64) -> LineView {
    let (sentence, actions, fraction) = match stage {
        InstallStage::Downloading if total > 0 => (
            fill(
                t(lang, Key::ReaderDownloadProgress),
                &[
                    &number::decimal(lang, received.min(total) / (1024 * 1024)),
                    &number::decimal(lang, koru_voice_consent::megabytes(total)),
                ],
            ),
            vec![LineAction::Stop],
            Some((received.min(total) as f64 / total as f64) as f32),
        ),
        InstallStage::Starting | InstallStage::Downloading => (
            t(lang, Key::ReaderDownloadPreparing).to_owned(),
            vec![LineAction::Stop],
            None,
        ),
        InstallStage::Verifying => (
            t(lang, Key::ReaderDownloadChecking).to_owned(),
            vec![LineAction::Stop],
            None,
        ),
        InstallStage::Installing => (
            t(lang, Key::ReaderDownloadInstalling).to_owned(),
            vec![LineAction::Stop],
            None,
        ),
        // An accepted Stop cannot be asked for twice.
        InstallStage::Stopping => (
            t(lang, Key::ReaderDownloadStopping).to_owned(),
            Vec::new(),
            None,
        ),
        _ => (
            t(lang, Key::ReaderDownloadRemoving).to_owned(),
            Vec::new(),
            None,
        ),
    };
    LineView {
        sentence,
        actions,
        fraction,
    }
}

/// What the surface asks its background thread to do about the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReaderCommand {
    /// `GET /v1/readings/reader`.
    Read,
    /// **Turn on** or **Try again**: the terms the person was shown.
    Install(InstallReader),
    Stop,
    Decline,
}

/// **The line's own state on one surface**: the reader as last fetched, when
/// to ask again, and where the keyboard and the pointer are on it.
#[derive(Debug, Default)]
pub(crate) struct Offer {
    pub(crate) reader: Option<ReaderState>,
    fetched_at: Option<Instant>,
    in_flight: bool,
    catalog_polls: u32,
    /// The action the keyboard is on, when it is on the line.
    pub(crate) focus: Option<usize>,
    pub(crate) hover: Option<usize>,
}

impl Offer {
    /// Whether an answer carrying `counts` wants the reader's state fetched:
    /// only when there is something the line could say about it, and not
    /// again while a fresh one is in hand or one is on its way.
    pub(crate) fn wants_state(&self, counts: Option<&ReadingCounts>, now: Instant) -> bool {
        let Some(counts) = counts else {
            return false;
        };
        if !counts.offer {
            return false;
        }
        !self.in_flight
            && self
                .fetched_at
                .is_none_or(|at| now.saturating_duration_since(at) >= STATE_FRESH)
    }

    /// A request went out.
    pub(crate) fn asked(&mut self) {
        self.in_flight = true;
    }

    /// The Store answered with the reader's state. Returns whether a download
    /// that was running has just finished installing — the moment the counts
    /// in the last answer stop being true.
    pub(crate) fn answered(&mut self, state: ReaderState, now: Instant) -> bool {
        let was_running = self.running();
        self.in_flight = false;
        self.fetched_at = Some(now);
        if state.offer.is_some() || state.catalog.state != CatalogStage::Reading {
            self.catalog_polls = 0;
        }
        self.reader = Some(state);
        was_running
            && self
                .reader
                .as_ref()
                .and_then(|state| state.install.as_ref())
                .is_some_and(|install| install.stage == InstallStage::Installed)
    }

    /// The request failed; the next answer that wants a state asks again.
    pub(crate) fn failed(&mut self) {
        self.in_flight = false;
    }

    pub(crate) fn running(&self) -> bool {
        self.reader
            .as_ref()
            .and_then(|state| state.install.as_ref())
            .is_some_and(|install| install.stage.is_running())
    }

    /// When the state should be asked for again without anybody asking: while
    /// a download runs, and a few times while the catalog is still being
    /// read. `None` otherwise — nothing polls a settled line.
    pub(crate) fn poll_deadline(&self) -> Option<Instant> {
        if self.in_flight {
            return None;
        }
        let at = self.fetched_at?;
        if self.running() {
            return Some(at + INSTALL_POLL);
        }
        let reading_catalog = self.reader.as_ref().is_some_and(|state| {
            state.offer.is_none()
                && state.unavailable.is_none()
                && matches!(
                    state.catalog.state,
                    CatalogStage::Unread | CatalogStage::Reading
                )
        });
        (reading_catalog && self.catalog_polls < CATALOG_POLLS).then(|| at + CATALOG_POLL)
    }

    /// The deadline passed: take one of the catalog polls if that is what it
    /// was. The caller then asks.
    pub(crate) fn poll_taken(&mut self) {
        if !self.running() {
            self.catalog_polls += 1;
        }
    }

    /// What pressing `action` asks the Store, or `None` for an action that
    /// cannot be carried out from what is known (no terms to agree to).
    pub(crate) fn command(&self, action: LineAction) -> Option<ReaderCommand> {
        match action {
            LineAction::TurnOn | LineAction::TryAgain => {
                let terms = self.reader.as_ref()?.offer.clone()?;
                Some(ReaderCommand::Install(InstallReader {
                    terms,
                    from: InstallFrom::Offer,
                }))
            }
            LineAction::NotNow => Some(ReaderCommand::Decline),
            LineAction::Stop => Some(ReaderCommand::Stop),
        }
    }
}

#[cfg(test)]
mod tests;
