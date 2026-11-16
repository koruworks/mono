// SPDX-License-Identifier: MPL-2.0

//! **The *Recognize text and images in PDFs* card** (OCR EXPERIENCE §4, OCR Plan 2.5):
//! beside the other downloads on the Editor page — spelling dictionaries and
//! voice input — built from the same rows they are.
//!
//! **Host-fed, like voice's card.** The reader's state lives in the Store
//! daemon, and a surface hosted by three applications may not dial it; the
//! host reads it off its own event loop and hands this crate a [`ReaderRow`]
//! of plain values: a status, the languages the reader reads, and the *could
//! not be read* items with the names the host resolved from its own workspace
//! listing. This crate words all of it, in all eight languages, and says what
//! was pressed with a [`Request`](crate::Request).
//!
//! | status | the row says | and offers |
//! | --- | --- | --- |
//! | unavailable | *Not available on this computer:* and why | nothing (Remove, for a download that will not load) |
//! | not downloaded | *Not downloaded · 24 MB* | **Turn on** (from Settings, whatever *Not now* said) |
//! | downloading | how far it has got | **Stop** |
//! | on | *On · 8,297 read · 3 could not be read* | **Turn off**; the download's own row, **Remove** |
//! | off (downloaded) | *Off. What was already read can still be found.* | **Turn on** (no size: nothing to download); the download's own row, **Remove** |
//!
//! In any state, files nobody has read whose contents are on another device
//! get a row of their own — *38 files are not on this computer* — so they are
//! never counted as waiting (OCR Plan 3.3).
//!
//! Under the card, two lists (OCR Plan 3.5; EXPERIENCE §4): *could not be
//! read*, each item with **Show** and, where a right-click menu on it would
//! offer it, **Read again**; and *partly read*, each with how far it got,
//! **Show**, and **Read all pages** where it can be finished here. Each shows
//! [`READER_ITEMS_SHOWN`] and says how many more there are.

use koru_i18n::{Key, Lang, fill, number, t};

use crate::action::Action;
use crate::page::Builder;

/// How many *could not be read* items the card lists before it says how
/// many more there are. A list is for finding the one file somebody wondered
/// about; past a screenful, the count says the rest.
pub const READER_ITEMS_SHOWN: usize = 50;

/// **What the host read about the reader on this computer**, as plain values.
#[derive(Clone, Debug, PartialEq)]
pub struct ReaderRow {
    pub status: ReaderStatus,
    /// The languages it reads, as the terms' BCP 47 tags (`und-Latn`, `zh`,
    /// `ja`). Empty while nothing has said; the row then names only the kinds
    /// of file.
    pub languages: Vec<String>,
    /// The *could not be read* list, in the host's order.
    pub items: Vec<ReaderItemRow>,
    /// Items the host knows of beyond `items`.
    pub more: u64,
    /// The *partly read* list, in the host's order (OCR Plan 3.5).
    pub partly: Vec<ReaderPartlyRow>,
    /// Partly read items the host knows of beyond `partly`.
    pub partly_more: u64,
    /// Items on this computer's workspaces that nobody has read and whose
    /// contents are on another device: nothing here can read them, so the
    /// card says so rather than counting them as waiting (EXPERIENCE "On a
    /// second device"). The total for this computer, as the row's other
    /// counts are.
    pub not_here: u64,
}

/// Where the reader stands on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderStatus {
    /// It cannot be used here, and why.
    Unavailable(ReaderBlocked),
    /// Not on this computer. `megabytes` is the real size of the download
    /// when the catalog has said; `offline` when it could not be read for want
    /// of a network; `ended` when the last download failed or was stopped.
    NotDownloaded {
        megabytes: Option<u64>,
        offline: bool,
        ended: Option<ReaderDownloadEnd>,
    },
    /// A download is running.
    Downloading {
        stage: ReaderDownloadStage,
        received_megabytes: u64,
        total_megabytes: u64,
    },
    /// On: how many items were read and how many could not be, and what the
    /// download takes on this computer.
    On {
        read: u64,
        could_not_be_read: u64,
        megabytes: u64,
    },
    /// Downloaded and turned off (OCR Plan 3.5): nothing is read with it, and
    /// what was read stays findable. `megabytes` is what the download takes
    /// on this computer.
    Off { megabytes: u64 },
}

/// Why the reader cannot be used here — the Store's
/// `ReaderUnavailable`, in this crate's words.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReaderBlocked {
    DownloadInformationUnavailable,
    DownloadInformationInvalid,
    NoTrustRoot,
    NoWorker,
    WorkerMissing,
    NotBuilt,
    NoModel,
    /// A download is here and could not be used: **Remove** is offered.
    WillNotLoad,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReaderDownloadStage {
    Preparing,
    Downloading,
    Checking,
    Finishing,
    Stopping,
    Removing,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReaderDownloadEnd {
    Failed,
    Stopped,
}

/// One item of the *could not be read* list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderItemRow {
    /// The host's own token for the item, handed back in
    /// [`Request::ShowReaderItem`](crate::Request::ShowReaderItem).
    pub key: String,
    /// The file's name, or the title of the page holding the picture; `None`
    /// when the host could not resolve it.
    pub name: Option<String>,
    /// The item is a picture in a page rather than a file.
    pub picture: bool,
    /// The Store's reason token: `password`, `damaged`, `unsupported`, or a
    /// failure on this computer.
    pub reason: String,
    /// **Read again** is offered: the Store would accept it now (OCR Plan
    /// 3.5).
    pub read_again: bool,
}

/// One item of the *partly read* list (OCR Plan 3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderPartlyRow {
    /// The host's own token for the item, handed back in
    /// [`Request::ShowReaderItem`](crate::Request::ShowReaderItem) and
    /// [`Request::ReadAllReaderPages`](crate::Request::ReadAllReaderPages).
    pub key: String,
    /// The file's name; `None` when the host could not resolve it.
    pub name: Option<String>,
    pub picture: bool,
    /// How far it got.
    pub pages_done: u32,
    /// A person's *Read all pages* is under way.
    pub reading: bool,
    /// **Read all pages** is offered: the Store would accept it now.
    pub read_all: bool,
}

/// The card. Absent when the host says nothing about the reader — Koru
/// Terminal and Koru AI host this page too.
pub(crate) fn lay_out(b: &mut Builder<'_>, lang: Lang, row: &ReaderRow) {
    b.card_with_aside(
        Some(t(lang, Key::SettingsReaderTitle)),
        Some(t(lang, Key::SettingsReaderAside)),
    );
    b.anchor(crate::page::Anchor::Reader);
    let status = t(lang, Key::SettingsReaderStatus);
    match status_line(lang, &row.status) {
        (line, Some((action, label))) => b.row_button(status, Some(&line), action, label),
        (line, None) => b.row_text(status, Some(&line)),
    }
    match &row.status {
        ReaderStatus::On { megabytes, .. } | ReaderStatus::Off { megabytes } => b.row_text_button(
            t(lang, Key::SettingsReaderDownloadRow),
            Some(&fill(
                t(lang, Key::SettingsReaderOnDisk),
                &[&number::decimal(lang, *megabytes)],
            )),
            Action::ReaderRemove,
            t(lang, Key::SettingsReaderRemove),
        ),
        ReaderStatus::Unavailable(ReaderBlocked::WillNotLoad) => b.row_text_button(
            t(lang, Key::SettingsReaderDownloadRow),
            None,
            Action::ReaderRemove,
            t(lang, Key::SettingsReaderRemove),
        ),
        _ => {}
    }
    if row.not_here > 0 {
        b.row_text(&not_here(lang, row.not_here), None);
    }
    b.row_text(
        t(lang, Key::SettingsReaderWhatIsRead),
        Some(&what_is_read(lang, &row.languages)),
    );
    if !row.items.is_empty() {
        b.card(Some(t(lang, Key::SettingsReaderCouldNotRead)));
        for item in row.items.iter().take(READER_ITEMS_SHOWN) {
            let name = item_name(lang, item);
            let why = Some(reason(lang, &item.reason));
            let show = Action::ShowReaderItem(item.key.clone());
            let show_label = t(lang, Key::SettingsReaderShow);
            if item.read_again {
                b.row_text_buttons(
                    &name,
                    why,
                    (
                        t(lang, Key::ReadingReadAgain),
                        Action::ReaderReadAgain(item.key.clone()),
                    ),
                    (show_label, show),
                );
            } else {
                b.row_text_button(&name, why, show, show_label);
            }
        }
        more_row(b, lang, row.more, row.items.len());
    }
    if !row.partly.is_empty() {
        b.card(Some(t(lang, Key::SettingsReaderPartlyRead)));
        for item in row.partly.iter().take(READER_ITEMS_SHOWN) {
            let name = partly_name(lang, item);
            let far = how_far(lang, item);
            let show = Action::ShowReaderItem(item.key.clone());
            let show_label = t(lang, Key::SettingsReaderShow);
            if item.read_all {
                b.row_text_buttons(
                    &name,
                    Some(&far),
                    (
                        t(lang, Key::ReadingReadAllPages),
                        Action::ReaderReadAll(item.key.clone()),
                    ),
                    (show_label, show),
                );
            } else {
                b.row_text_button(&name, Some(&far), show, show_label);
            }
        }
        more_row(b, lang, row.partly_more, row.partly.len());
    }
}

/// *Not listed here: N* under a list cut at [`READER_ITEMS_SHOWN`].
fn more_row(b: &mut Builder<'_>, lang: Lang, beyond: u64, listed: usize) {
    let more = beyond + listed.saturating_sub(READER_ITEMS_SHOWN) as u64;
    if more > 0 {
        b.row_text(
            &fill(
                t(lang, Key::SettingsReaderMore),
                &[&number::decimal(lang, more)],
            ),
            None,
        );
    }
}

/// What the *partly read* list calls an item: as the other list does.
pub(crate) fn partly_name(lang: Lang, item: &ReaderPartlyRow) -> String {
    match (&item.name, item.picture) {
        (Some(name), false) => name.clone(),
        (Some(page), true) => fill(t(lang, Key::ReaderItemPictureIn), &[page]),
        (None, _) => t(lang, Key::ReaderItemElsewhere).to_owned(),
    }
}

/// How far a partly read item got: *First 200 pages read*, or, while a
/// person's *Read all pages* is under way, *Reading all pages: 230 read so
/// far*.
pub(crate) fn how_far(lang: Lang, item: &ReaderPartlyRow) -> String {
    let pages = number::decimal(lang, u64::from(item.pages_done));
    match (item.reading, item.pages_done) {
        (true, _) => fill(t(lang, Key::SettingsReaderReadingRest), &[&pages]),
        (false, 1) => t(lang, Key::SettingsReaderPagesReadOne).to_owned(),
        (false, _) => fill(t(lang, Key::SettingsReaderPagesRead), &[&pages]),
    }
}

/// The status row's sentence, and its button when it has one.
pub(crate) fn status_line(
    lang: Lang,
    status: &ReaderStatus,
) -> (String, Option<(Action, &'static str)>) {
    match status {
        ReaderStatus::Unavailable(why) => (
            fill(
                t(lang, Key::SettingsReaderUnavailable),
                &[blocked(lang, *why)],
            ),
            None,
        ),
        ReaderStatus::NotDownloaded {
            megabytes,
            offline,
            ended,
        } => {
            let line = match (ended, offline, megabytes) {
                (Some(ReaderDownloadEnd::Failed), ..) => {
                    t(lang, Key::ReaderDownloadFailed).to_owned()
                }
                (Some(ReaderDownloadEnd::Stopped), ..) => {
                    t(lang, Key::ReaderDownloadStopped).to_owned()
                }
                (None, true, _) => fill(
                    t(lang, Key::ReaderSentences),
                    &[
                        t(lang, Key::SettingsReaderNotDownloaded),
                        t(lang, Key::ReaderOfferOffline),
                    ],
                ),
                (None, false, Some(mb)) => fill(
                    t(lang, Key::SettingsReaderNotDownloadedSize),
                    &[&number::decimal(lang, *mb)],
                ),
                (None, false, None) => t(lang, Key::SettingsReaderDownloadPending).to_owned(),
            };
            // **Turn on** needs the terms, and the terms carry the size: no
            // size known means nothing to agree to yet.
            let button = match (ended, megabytes) {
                (_, None) => None,
                (Some(_), Some(_)) => Some((Action::ReaderTurnOn, t(lang, Key::TryAgain))),
                (None, Some(_)) if *offline => None,
                (None, Some(_)) => Some((Action::ReaderTurnOn, t(lang, Key::ReaderTurnOn))),
            };
            (line, button)
        }
        ReaderStatus::Downloading {
            stage,
            received_megabytes,
            total_megabytes,
        } => {
            let line = match stage {
                ReaderDownloadStage::Downloading if *total_megabytes > 0 => fill(
                    t(lang, Key::ReaderDownloadProgress),
                    &[
                        &number::decimal(lang, *received_megabytes),
                        &number::decimal(lang, *total_megabytes),
                    ],
                ),
                ReaderDownloadStage::Preparing | ReaderDownloadStage::Downloading => {
                    t(lang, Key::ReaderDownloadPreparing).to_owned()
                }
                ReaderDownloadStage::Checking => t(lang, Key::ReaderDownloadChecking).to_owned(),
                ReaderDownloadStage::Finishing => t(lang, Key::ReaderDownloadInstalling).to_owned(),
                ReaderDownloadStage::Stopping => t(lang, Key::ReaderDownloadStopping).to_owned(),
                ReaderDownloadStage::Removing => t(lang, Key::ReaderDownloadRemoving).to_owned(),
            };
            let stop = !matches!(
                stage,
                ReaderDownloadStage::Stopping | ReaderDownloadStage::Removing
            );
            (
                line,
                stop.then(|| (Action::ReaderStop, t(lang, Key::VoiceStop))),
            )
        }
        ReaderStatus::On {
            read,
            could_not_be_read,
            ..
        } => {
            let line = if *could_not_be_read == 0 {
                fill(
                    t(lang, Key::SettingsReaderOn),
                    &[&number::decimal(lang, *read)],
                )
            } else {
                fill(
                    t(lang, Key::SettingsReaderOnWithFailures),
                    &[
                        &number::decimal(lang, *read),
                        &number::decimal(lang, *could_not_be_read),
                    ],
                )
            };
            (
                line,
                Some((Action::ReaderTurnOff, t(lang, Key::SettingsReaderTurnOff))),
            )
        }
        // **Turn on** with no size: it is downloaded, so there is nothing to
        // agree to a size of; the host turns it on rather than installing.
        ReaderStatus::Off { .. } => (
            t(lang, Key::SettingsReaderOff).to_owned(),
            Some((Action::ReaderTurnOn, t(lang, Key::ReaderTurnOn))),
        ),
    }
}

fn blocked(lang: Lang, why: ReaderBlocked) -> &'static str {
    t(
        lang,
        match why {
            ReaderBlocked::DownloadInformationUnavailable => Key::SettingsReaderDownloadUnavailable,
            ReaderBlocked::DownloadInformationInvalid => Key::SettingsReaderDownloadInvalid,
            ReaderBlocked::NoTrustRoot => Key::SettingsReaderUnavailableNoTrustRoot,
            ReaderBlocked::NoWorker => Key::SettingsReaderUnavailableNoWorker,
            ReaderBlocked::WorkerMissing => Key::SettingsReaderUnavailableWorkerMissing,
            ReaderBlocked::NotBuilt => Key::SettingsReaderUnavailableNotBuilt,
            ReaderBlocked::NoModel => Key::SettingsReaderUnavailableNoModel,
            ReaderBlocked::WillNotLoad => Key::SettingsReaderUnavailableWillNotLoad,
        },
    )
}

/// The one line for which languages and kinds of file are read. A person
/// reading Koru in Arabic is told plainly that Arabic script is not read yet
/// (EXPERIENCE §3 "In eight languages").
pub(crate) fn what_is_read(lang: Lang, languages: &[String]) -> String {
    let kinds = t(lang, Key::SettingsReaderKinds);
    let mut line = if languages.is_empty() {
        kinds.to_owned()
    } else {
        let names: Vec<&str> = languages
            .iter()
            .map(|tag| language_name(lang, tag))
            .collect();
        let list = names.join(t(lang, Key::SettingsReaderListSeparator));
        fill(
            t(lang, Key::ReaderSentences),
            &[
                &fill(t(lang, Key::SettingsReaderLanguages), &[&list]),
                kinds,
            ],
        )
    };
    if lang == Lang::Ar {
        line = fill(
            t(lang, Key::ReaderSentences),
            &[&line, t(lang, Key::SettingsReaderArabicNotRead)],
        );
    }
    line
}

/// A language the terms name, in the reader's language. A tag this build
/// has no name for is shown as the tag: a correct code beats a wrong word.
fn language_name<'a>(lang: Lang, tag: &'a str) -> &'a str {
    match tag {
        "und-Latn" | "Latn" => t(lang, Key::SettingsReaderLanguageLatin),
        "zh" | "zh-Hans" | "zh-Hant" => t(lang, Key::SettingsReaderLanguageChinese),
        "ja" => t(lang, Key::SettingsReaderLanguageJapanese),
        other => other,
    }
}

/// *38 files are not on this computer.* — the words search uses for the same
/// files, a whole sentence for one.
pub(crate) fn not_here(lang: Lang, count: u64) -> String {
    match count {
        1 => t(lang, Key::ReaderNotHereOne).to_owned(),
        _ => fill(
            t(lang, Key::ReaderNotHere),
            &[&number::decimal(lang, count)],
        ),
    }
}

/// What the list calls an item: the file's name; *A picture in* the page; or,
/// when the host could not name it, that it is in another workspace.
pub(crate) fn item_name(lang: Lang, item: &ReaderItemRow) -> String {
    match (&item.name, item.picture) {
        (Some(name), false) => name.clone(),
        (Some(page), true) => fill(t(lang, Key::ReaderItemPictureIn), &[page]),
        (None, _) => t(lang, Key::ReaderItemElsewhere).to_owned(),
    }
}

/// Why, in words: the two reasons true on every device by name, and every
/// failure true only on this computer as one sentence.
pub(crate) fn reason(lang: Lang, token: &str) -> &'static str {
    t(
        lang,
        match token {
            "password" => Key::ReaderReasonPassword,
            "damaged" => Key::ReaderReasonDamaged,
            "unsupported" => Key::ReaderReasonUnsupported,
            _ => Key::ReaderReasonHere,
        },
    )
}

#[cfg(test)]
mod tests;
