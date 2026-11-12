// SPDX-License-Identifier: MPL-2.0

//! **The Settings page's line to the reader** (OCR Plan 2.5): the one thread
//! that asks the Store about *Text in pictures and PDFs* and carries out what
//! was pressed on its card, and the pure mapping from what the Store said to
//! the card's [`ReaderRow`].
//!
//! **A thread, because a view may not ask anything.** The page's `maintain`
//! is a `try_recv` and a clock comparison; the HTTP round trips, the listing
//! reads that put names on the *could not be read* items, and a download's
//! polling all happen here. The Store answers every one of them from memory
//! or from its derived read models; none of it is worth a frame.
//!
//! **Names come from the workspace listing**, never from the reading code:
//! the Store's items are ids and reason tokens, and this thread reads each
//! named workspace's document list — the same list Explore draws — to say
//! *Lease.pdf* or *A picture in Site visit*.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use koru_settings_ui::{
    ReaderBlocked, ReaderDownloadEnd, ReaderDownloadStage, ReaderItemRow, ReaderPartlyRow,
    ReaderRow, ReaderStatus,
};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::reader::{
    CatalogStage, InstallFrom, InstallReader, InstallStage, ReaderItem, ReaderItemKind,
    ReaderItems, ReaderState, ReaderUnavailable,
};

/// What the page asks the thread to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Read the state, and the items when there are any.
    Refresh,
    /// **Turn on** from Settings: a reader downloaded and off is turned on
    /// with no download (OCR Plan 3.5); otherwise it installs, whatever *Not
    /// now* said. Which, the Store's state says at the moment of the press.
    TurnOn,
    Stop,
    /// **Turn off**: reading stops and the download stays (OCR Plan 3.5).
    TurnOff,
    /// **Remove** the download, whether the reader was on or off.
    Remove,
    /// **Read again** an item that could not be read (OCR Plan 3.5).
    ReadAgain {
        workspace: WorkspaceId,
        hash: String,
    },
    /// **Read all pages** of a partly read item (OCR Plan 3.5).
    ReadAll {
        workspace: WorkspaceId,
        hash: String,
    },
}

/// What came back: the state, the items, and the names the listing gave
/// them. `None` state: the Store did not answer, and the card keeps what it
/// last showed.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub(crate) state: Option<ReaderState>,
    pub(crate) items: ReaderItems,
    /// `(workspace, document)` → (title, kind), for the items' entries.
    pub(crate) names: HashMap<(WorkspaceId, DocumentId), (String, String)>,
}

pub(crate) struct ReaderLink {
    commands: Sender<Command>,
    answers: Receiver<Answer>,
}

impl ReaderLink {
    /// Start the thread. `None` when it cannot be started, which leaves the
    /// card absent — the same as a host that says nothing.
    pub(crate) fn start(root: Option<std::path::PathBuf>) -> Option<Self> {
        let (commands, command_rx) = mpsc::channel::<Command>();
        let (answer_tx, answers) = mpsc::channel::<Answer>();
        std::thread::Builder::new()
            .name("koru-settings-reader".into())
            .spawn(move || {
                let mut client: Option<koru_api_client::Client> = None;
                // A request was waiting or under way at the last answer: the
                // list is read again even if the counts no longer name it.
                let mut asked = false;
                while let Ok(command) = command_rx.recv() {
                    // Only the newest refresh matters; a press is never
                    // dropped.
                    let mut queue = vec![command];
                    queue.extend(command_rx.try_iter());
                    for command in dedup_refreshes(queue) {
                        let answer = perform(&mut client, root.as_deref(), command, &mut asked);
                        if answer_tx.send(answer).is_err() {
                            return;
                        }
                    }
                }
            })
            .ok()?;
        Some(Self { commands, answers })
    }

    pub(crate) fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// The newest answer waiting, if any. Never blocks.
    pub(crate) fn take(&self) -> Option<Answer> {
        self.answers.try_iter().last()
    }
}

/// Refreshes queued behind each other are one refresh, placed last.
fn dedup_refreshes(queue: Vec<Command>) -> Vec<Command> {
    let refresh = queue.contains(&Command::Refresh);
    let mut out: Vec<Command> = queue
        .into_iter()
        .filter(|command| *command != Command::Refresh)
        .collect();
    if refresh || out.is_empty() {
        out.push(Command::Refresh);
    }
    out
}

fn perform(
    client: &mut Option<koru_api_client::Client>,
    root: Option<&std::path::Path>,
    command: Command,
    asked: &mut bool,
) -> Answer {
    if client.is_none() {
        let opened = match root {
            Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
            None => koru_api_client::Client::open(koru_channel::segment()),
        };
        *client = opened.ok();
    }
    let Some(open) = client.as_ref() else {
        return Answer::default();
    };
    let store = open.store();
    let requested = matches!(command, Command::ReadAgain { .. } | Command::ReadAll { .. });
    let state = match command {
        Command::Refresh => store.reader(),
        Command::TurnOn => store.reader().and_then(|state| {
            if state.turned_off {
                return store.turn_reader_on();
            }
            match state.offer.clone() {
                Some(terms) => store.install_reader(&InstallReader {
                    terms,
                    from: InstallFrom::Settings,
                }),
                None => Ok(state),
            }
        }),
        Command::Stop => store.stop_reader_install(),
        Command::TurnOff => store.turn_reader_off(),
        Command::Remove => store.remove_reader(),
        // A refusal (the item changed since the list was drawn) is not the
        // Store going away: the list read below shows what is true now.
        Command::ReadAgain { workspace, hash } => {
            if let Ok(blob) = koru_store_core::model::BlobHash::parse(&hash) {
                let _ = store.read_again(workspace, &blob);
            }
            store.reader()
        }
        Command::ReadAll { workspace, hash } => {
            if let Ok(blob) = koru_store_core::model::BlobHash::parse(&hash) {
                let _ = store.read_all(workspace, &blob);
            }
            store.reader()
        }
    };
    let Ok(state) = state else {
        // A Store that went away is found again on the next command.
        *client = None;
        return Answer::default();
    };
    let mut answer = Answer {
        state: Some(state),
        ..Answer::default()
    };
    let wants_items = requested
        || *asked
        || answer.state.as_ref().is_some_and(|state| {
            state.counts.could_not_be_read > 0 || state.counts.partly_read > 0
        });
    *asked = false;
    if wants_items && let Ok(items) = store.reader_items() {
        *asked = items.items.iter().any(|item| item.requested);
        let mut workspaces: Vec<WorkspaceId> = items.items.iter().map(|i| i.workspace).collect();
        workspaces.sort_by_key(|id| id.to_storage());
        workspaces.dedup();
        for workspace in workspaces {
            if let Ok(entries) = store.documents(workspace) {
                for entry in entries {
                    answer
                        .names
                        .insert((workspace, entry.id), (entry.title, entry.kind));
                }
            }
        }
        answer.items = items;
    }
    answer
}

/// An item's token on the card: `<workspace>/<document>/<hash>` — where
/// **Show** goes, and what **Read again** and **Read all pages** ask the
/// Store about (OCR Plan 3.5). Ids only.
pub(crate) fn item_key(workspace: WorkspaceId, document: DocumentId, hash: &str) -> String {
    format!(
        "{}/{}/{hash}",
        workspace.to_storage(),
        document.to_storage()
    )
}

/// The token back into a navigation target: the document route, which the
/// shell resolves through the Store to wherever the entry lives. A token of
/// two segments (before Plan 3.5) still goes where it went.
pub(crate) fn item_target(key: &str) -> Option<koru_workspace_host::NavigationTarget> {
    let document = DocumentId::parse(key.split('/').nth(1)?)?;
    Some(koru_workspace_host::NavigationTarget::Document {
        document: koru_core::DocumentId::from_uuid(document.as_uuid()),
        node: None,
    })
}

/// The token back into what a request names: the workspace and the bytes.
pub(crate) fn item_request(key: &str) -> Option<(WorkspaceId, String)> {
    let mut parts = key.split('/');
    let workspace = WorkspaceId::parse(parts.next()?)?;
    let _document = parts.next()?;
    let hash = parts.next().filter(|hash| !hash.is_empty())?;
    Some((workspace, hash.to_owned()))
}

/// **The card, from what the Store said.** Pure, so every state is tested
/// without a Store.
pub(crate) fn row(answer: &Answer) -> Option<ReaderRow> {
    let state = answer.state.as_ref()?;
    let status = if let Some(install) = state.install.as_ref().filter(|i| i.stage.is_running()) {
        ReaderStatus::Downloading {
            stage: match install.stage {
                InstallStage::Starting => ReaderDownloadStage::Preparing,
                InstallStage::Downloading => ReaderDownloadStage::Downloading,
                InstallStage::Verifying => ReaderDownloadStage::Checking,
                InstallStage::Installing => ReaderDownloadStage::Finishing,
                InstallStage::Stopping => ReaderDownloadStage::Stopping,
                _ => ReaderDownloadStage::Removing,
            },
            received_megabytes: install.received.min(install.total) / (1024 * 1024),
            total_megabytes: koru_voice_consent::megabytes(install.total),
        }
    } else if let Some(why) = state.unavailable {
        ReaderStatus::Unavailable(match why {
            ReaderUnavailable::NoTrustRoot => ReaderBlocked::NoTrustRoot,
            ReaderUnavailable::NoWorker => ReaderBlocked::NoWorker,
            ReaderUnavailable::WorkerMissing => ReaderBlocked::WorkerMissing,
            ReaderUnavailable::NotBuilt => ReaderBlocked::NotBuilt,
            ReaderUnavailable::NoModel => ReaderBlocked::NoModel,
            ReaderUnavailable::WillNotLoad => ReaderBlocked::WillNotLoad,
        })
    } else if let Some(installed) = state.installed.as_ref().filter(|_| state.turned_off) {
        ReaderStatus::Off {
            megabytes: koru_voice_consent::megabytes(installed.download_bytes),
        }
    } else if let Some(installed) = state.installed.as_ref() {
        ReaderStatus::On {
            read: state.counts.read,
            could_not_be_read: state.counts.could_not_be_read,
            megabytes: koru_voice_consent::megabytes(installed.download_bytes),
        }
    } else {
        ReaderStatus::NotDownloaded {
            megabytes: state
                .offer
                .as_ref()
                .map(|terms| koru_voice_consent::megabytes(terms.download_bytes)),
            offline: state.catalog.state == CatalogStage::Failed
                && state.catalog.code.as_deref() == Some("offline"),
            ended: match state.install.as_ref().map(|i| i.stage) {
                Some(InstallStage::Failed) => Some(ReaderDownloadEnd::Failed),
                Some(InstallStage::Stopped) => Some(ReaderDownloadEnd::Stopped),
                _ => None,
            },
        }
    };
    let named = |item: &ReaderItem| answer.names.get(&(item.workspace, item.document));
    // An entry that is not a file holds the picture.
    let picture = |item: &ReaderItem| {
        named(item).is_some_and(|(_, kind)| kind != koru_core::explore::KIND_FILE)
    };
    let items = answer
        .items
        .items
        .iter()
        .filter(|item| item.kind == ReaderItemKind::CouldNotBeRead)
        .map(|item| ReaderItemRow {
            key: item_key(item.workspace, item.document, &item.hash),
            name: named(item).map(|(title, _)| title.clone()),
            picture: picture(item),
            reason: item.reason.clone(),
            read_again: item.offers.read_again,
        })
        .collect::<Vec<_>>();
    let partly = answer
        .items
        .items
        .iter()
        .filter(|item| item.kind == ReaderItemKind::PartlyRead)
        .map(|item| ReaderPartlyRow {
            key: item_key(item.workspace, item.document, &item.hash),
            name: named(item).map(|(title, _)| title.clone()),
            picture: picture(item),
            pages_done: item.pages_done.unwrap_or(0),
            reading: item.requested,
            read_all: item.offers.read_all,
        })
        .collect::<Vec<_>>();
    // The counts are the whole numbers; what a list does not hold is "more".
    // An item being read in full is in the list and counted unread, not
    // partly read, so only the settled ones are taken off the count.
    let more = state
        .counts
        .could_not_be_read
        .saturating_sub(items.len() as u64);
    let settled_partly = partly.iter().filter(|item| !item.reading).count() as u64;
    let partly_more = state.counts.partly_read.saturating_sub(settled_partly);
    let listed = !answer.items.items.is_empty();
    Some(ReaderRow {
        status,
        languages: state
            .offer
            .as_ref()
            .map(|terms| terms.languages.clone())
            .unwrap_or_default(),
        items,
        more: if listed { more } else { 0 },
        partly,
        partly_more: if listed { partly_more } else { 0 },
        not_here: state.counts.not_here,
    })
}

/// Whether the card should be asked about again soon: while a download runs.
pub(crate) fn running(answer_state: Option<&ReaderState>) -> bool {
    answer_state
        .and_then(|state| state.install.as_ref())
        .is_some_and(|install| install.stage.is_running())
}

/// Whether a person's *Read again* or *Read all pages* is waiting or under
/// way, so the list is asked about again to show how far it got and to let
/// the item go when it finishes (OCR Plan 3.5).
pub(crate) fn reading_requested(items: &ReaderItems) -> bool {
    items.items.iter().any(|item| item.requested)
}

/// Whether the Store is still reading the reader's catalog, so the size the
/// card would offer is not known yet. Reading the state is what makes the
/// Store fetch it; a read a moment later usually carries the size.
pub(crate) fn catalog_pending(answer_state: Option<&ReaderState>) -> bool {
    answer_state.is_some_and(|state| {
        state.installed.is_none()
            && state.unavailable.is_none()
            && state.offer.is_none()
            && matches!(
                state.catalog.state,
                CatalogStage::Unread | CatalogStage::Reading
            )
    })
}

#[cfg(test)]
mod tests;
