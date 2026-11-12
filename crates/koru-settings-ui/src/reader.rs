// SPDX-License-Identifier: MPL-2.0

//! **The *Text in pictures and PDFs* card** (OCR EXPERIENCE §4, OCR Plan 2.5):
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
}

/// Why the reader cannot be used here — the Store's
/// `ReaderUnavailable`, in this crate's words.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReaderBlocked {
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
}

/// The card. Absent when the host says nothing about the reader — Koru
/// Terminal and Koru AI host this page too.
pub(crate) fn lay_out(b: &mut Builder<'_>, lang: Lang, row: &ReaderRow) {
    b.card_with_aside(
        Some(t(lang, Key::SettingsReaderTitle)),
        Some(t(lang, Key::SettingsReaderAside)),
    );
    let status = t(lang, Key::SettingsReaderStatus);
    match status_line(lang, &row.status) {
        (line, Some((action, label))) => b.row_button(status, Some(&line), action, label),
        (line, None) => b.row_text(status, Some(&line)),
    }
    match &row.status {
        ReaderStatus::On { megabytes, .. } => b.row_text_button(
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
    b.row_text(
        t(lang, Key::SettingsReaderWhatIsRead),
        Some(&what_is_read(lang, &row.languages)),
    );
    if row.items.is_empty() {
        return;
    }
    b.card(Some(t(lang, Key::SettingsReaderCouldNotRead)));
    for item in row.items.iter().take(READER_ITEMS_SHOWN) {
        b.row_text_button(
            &item_name(lang, item),
            Some(reason(lang, &item.reason)),
            Action::ShowReaderItem(item.key.clone()),
            t(lang, Key::SettingsReaderShow),
        );
    }
    let more = row.more + row.items.len().saturating_sub(READER_ITEMS_SHOWN) as u64;
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
                (None, false, None) => t(lang, Key::SettingsReaderNotDownloaded).to_owned(),
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
    }
}

fn blocked(lang: Lang, why: ReaderBlocked) -> &'static str {
    t(
        lang,
        match why {
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
