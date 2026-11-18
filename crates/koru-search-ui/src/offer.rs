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
//!    sentence is not a line at all but the empty page itself
//!    ([`empty_progress`], OCR Plan 3.3): a progress report in place of *No
//!    results*, the rule search already follows while a workspace is still
//!    being indexed.
//! 4. **Below *No results*, nothing left to read here, and files whose
//!    contents are on another device**: *38 files are not on this computer*
//!    (EXPERIENCE "On a second device"), so the counts never call them unread.
//!    Below results it says nothing: a device that never fetches those files
//!    would carry the sentence under every list.
//!
//! ## The three empty pages
//!
//! When a search found nothing (EXPERIENCE "Before there is anything"):
//!
//! | the counts say | the page shows |
//! | --- | --- |
//! | the reader is not here and items wait for it (`offer`) | *No results*, and the offer directly below it |
//! | something here is still being read (`left > 0`) | *Still reading N of M pictures and PDFs. Results appear as they are read.* in place of *No results* |
//! | everything in scope has been read | *No results*, the ordinary page |
//!
//! `N` is [`left`]: the items in scope being read on this computer now —
//! `unread` less those waiting for the reader and those whose contents are
//! elsewhere, both of which nothing here is reading. It falls to zero as the
//! reading finishes, and the page becomes the ordinary verdict.
//!
//! ## The same line in an import's summary
//!
//! [`import_summary`] is the line an import's own summary gains (EXPERIENCE
//! §3 "The first time"): the same sentences and the same two answers, counted
//! from what the import brought rather than from a search's scope.
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
pub enum LineAction {
    TurnOn,
    NotNow,
    Stop,
    TryAgain,
}

impl LineAction {
    /// The button's words.
    pub fn label(self, lang: Lang) -> &'static str {
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
pub struct LineView {
    pub sentence: String,
    pub actions: Vec<LineAction>,
    /// `Some` while a download with a known total runs.
    pub fraction: Option<f32>,
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
    let left = left(counts);
    if !with_results {
        // Something still being read is the empty page itself
        // ([`empty_progress`]), not a line under it. Once nothing here is
        // left, the files whose contents are elsewhere are named as such.
        // **Files whose contents are on another device are not announced**
        // (owner, 2026-11-16). The line stood under every search that found
        // nothing and gave nothing to do about it. They are still left out
        // of the unread counts, which is the part that mattered.
        return None;
    }
    (left > 0).then(|| quiet(still_reading(lang, left, counts.total)))
}

/// **What a search that found nothing shows in place of *No results*** while
/// something in its scope is still being read here: *Still reading N of M
/// pictures and PDFs.* and, under it, *Results appear as they are read.*
/// (EXPERIENCE "Before there is anything"). A progress report, not a verdict —
/// the rule the surface already follows while a workspace is being indexed.
///
/// `None` when the page is a verdict after all: nothing left to read here, a
/// store that counts nothing, a download running (its progress is the line),
/// or the offer standing (the reader is not here; the offer sits below *No
/// results* and says why).
pub(crate) fn empty_progress(
    lang: Lang,
    counts: Option<&ReadingCounts>,
    reader: Option<&ReaderState>,
) -> Option<(String, String)> {
    let counts = counts?;
    if reader
        .and_then(|state| state.install.as_ref())
        .is_some_and(|install| install.stage.is_running())
        || counts.offer
    {
        return None;
    }
    let left = left(counts);
    (left > 0).then(|| {
        (
            still_reading(lang, left, counts.total),
            t(lang, Key::ReaderResultsAppear).to_owned(),
        )
    })
}

/// **The items in scope being read on this computer now**: `unread`, less
/// those waiting for the reader (nothing reads them until it is installed)
/// and those whose contents are on another device (nothing here ever will).
/// The `N` of *Still reading N of M*, which falls to zero as reading
/// finishes.
pub(crate) fn left(counts: &ReadingCounts) -> u64 {
    counts
        .unread
        .saturating_sub(counts.not_here)
        .saturating_sub(counts.needs_reader)
}

fn still_reading(lang: Lang, left: u64, total: u64) -> String {
    fill(
        t(lang, Key::ReaderStillReading),
        &[&number::decimal(lang, left), &number::decimal(lang, total)],
    )
}

/// *38 files are not on this computer.* — a whole sentence for one.
#[allow(dead_code)]
pub(crate) fn not_here(lang: Lang, count: u64) -> String {
    match count {
        1 => t(lang, Key::ReaderNotHereOne).to_owned(),
        _ => fill(
            t(lang, Key::ReaderNotHere),
            &[&number::decimal(lang, count)],
        ),
    }
}

/// A line that reports and offers nothing to press.
fn quiet(sentence: String) -> LineView {
    LineView {
        sentence,
        actions: Vec::new(),
        fraction: None,
    }
}

/// **The line an import's summary gains** (EXPERIENCE §3 "The first time"):
/// *412 pictures and scanned PDFs can be found by the words in them. Koru
/// reads them on this computer. One-time download, 24 MB.* with **Turn on**
/// and **Not now**.
///
/// `pictures_and_scans` is the import's own count; `state` is the reader's
/// state as the Store answered the import's report of that count — which is
/// what cleared a *Not now*, so the state already says whether to offer.
///
/// Only when the import brought some, no reader is installed, the reader can
/// be used on this computer, and no download is already running (whoever
/// started it has answered). Offline, the sentence and *needs a one-time
/// download* in place of the actions. No size known yet means no line: the
/// size is never a number written into the text.
pub fn import_summary(
    lang: Lang,
    pictures_and_scans: u64,
    state: &ReaderState,
) -> Option<LineView> {
    // A *Not now* still standing means the Store did not take this import as
    // the next one; the line stays quiet with it.
    if pictures_and_scans == 0
        || state.installed.is_some()
        || state.unavailable.is_some()
        || state.offer_declined
    {
        return None;
    }
    if state
        .install
        .as_ref()
        .is_some_and(|install| install.stage.is_running())
    {
        return None;
    }
    let found = lead(lang, pictures_and_scans, true);
    let here = t(
        lang,
        if pictures_and_scans == 1 {
            Key::ReaderOfferHereOne
        } else {
            Key::ReaderOfferHere
        },
    );
    let lead = sentences(lang, &found, here);
    if offline(state) {
        return Some(quiet(sentences(
            lang,
            &lead,
            t(lang, Key::ReaderOfferOffline),
        )));
    }
    let terms = state.offer.as_ref()?;
    Some(LineView {
        sentence: sentences(lang, &lead, &size(lang, terms.download_bytes)),
        actions: vec![LineAction::TurnOn, LineAction::NotNow],
        fraction: None,
    })
}

/// **What a download of the reader says once somebody pressed Turn on**: its
/// progress with **Stop** while it runs; that it failed or was stopped, with
/// **Try again** and **Not now**, once it ended that way. `None` for a state
/// with nothing to say about a download — installed, removed, never started.
/// The same words the line in search and the Settings row use.
pub fn install_line(lang: Lang, state: &ReaderState) -> Option<LineView> {
    let install = state.install.as_ref()?;
    if install.stage.is_running() {
        return Some(progress(
            lang,
            install.stage,
            install.received,
            install.total,
        ));
    }
    ended(lang, install.stage)
}

/// A download that ended without installing: what happened, and **Try
/// again** (which resumes) with **Not now**.
fn ended(lang: Lang, stage: InstallStage) -> Option<LineView> {
    let key = match stage {
        InstallStage::Failed => Key::ReaderDownloadFailed,
        InstallStage::Stopped => Key::ReaderDownloadStopped,
        _ => return None,
    };
    Some(LineView {
        sentence: t(lang, key).to_owned(),
        actions: vec![LineAction::TryAgain, LineAction::NotNow],
        fraction: None,
    })
}

fn offline(state: &ReaderState) -> bool {
    state.catalog.state == CatalogStage::Failed && state.catalog.code.as_deref() == Some("offline")
}

/// *One-time download, 24 MB.* — the real size, rounded the way voice's
/// consent rounds it.
fn size(lang: Lang, bytes: u64) -> String {
    fill(
        t(lang, Key::ReaderOfferSize),
        &[&number::decimal(lang, koru_voice_consent::megabytes(bytes))],
    )
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
    // What, then where: the reading happens on this computer and nothing is
    // sent away — the first thing somebody asked to download a reader wants
    // to know.
    let lead = sentences(
        lang,
        &lead(lang, waiting, with_results),
        t(
            lang,
            if waiting == 1 {
                Key::ReaderOfferHereOne
            } else {
                Key::ReaderOfferHere
            },
        ),
    );
    if let Some(line) = reader
        .install
        .as_ref()
        .and_then(|install| ended(lang, install.stage))
    {
        return Some(line);
    }
    if offline(reader) {
        return Some(quiet(sentences(
            lang,
            &lead,
            t(lang, Key::ReaderOfferOffline),
        )));
    }
    let terms = reader.offer.as_ref()?;
    Some(LineView {
        sentence: sentences(lang, &lead, &size(lang, terms.download_bytes)),
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
