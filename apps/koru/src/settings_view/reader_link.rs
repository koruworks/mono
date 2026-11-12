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
    ReaderBlocked, ReaderDownloadEnd, ReaderDownloadStage, ReaderItemRow, ReaderRow, ReaderStatus,
};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::reader::{
    CatalogStage, InstallFrom, InstallReader, InstallStage, ReaderItems, ReaderState,
    ReaderUnavailable,
};

/// What the page asks the thread to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Read the state, and the items when there are any.
    Refresh,
    /// **Turn on** from Settings, which installs whatever *Not now* said.
    TurnOn,
    Stop,
    /// **Turn off** and **Remove** are one call: the Store removes the model
    /// and keeps every reading made.
    Remove,
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
                while let Ok(command) = command_rx.recv() {
                    // Only the newest refresh matters; a press is never
                    // dropped.
                    let mut queue = vec![command];
                    queue.extend(command_rx.try_iter());
                    for command in dedup_refreshes(queue) {
                        let answer = perform(&mut client, root.as_deref(), command);
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
    let state = match command {
        Command::Refresh => store.reader(),
        Command::TurnOn => store.reader().and_then(|state| match state.offer.clone() {
            Some(terms) => store.install_reader(&InstallReader {
                terms,
                from: InstallFrom::Settings,
            }),
            None => Ok(state),
        }),
        Command::Stop => store.stop_reader_install(),
        Command::Remove => store.remove_reader(),
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
    let wants_items = answer
        .state
        .as_ref()
        .is_some_and(|state| state.counts.could_not_be_read > 0);
    if wants_items && let Ok(items) = store.reader_items() {
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

/// A *could not be read* item's token: where picking it goes.
pub(crate) fn item_key(workspace: WorkspaceId, document: DocumentId) -> String {
    format!("{}/{}", workspace.to_storage(), document.to_storage())
}

/// The token back into a navigation target: the document route, which the
/// shell resolves through the Store to wherever the entry lives.
pub(crate) fn item_target(key: &str) -> Option<koru_workspace_host::NavigationTarget> {
    let (_, document) = key.split_once('/')?;
    let document = DocumentId::parse(document)?;
    Some(koru_workspace_host::NavigationTarget::Document {
        document: koru_core::DocumentId::from_uuid(document.as_uuid()),
        node: None,
    })
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
    let items = answer
        .items
        .items
        .iter()
        .map(|item| {
            let named = answer.names.get(&(item.workspace, item.document));
            ReaderItemRow {
                key: item_key(item.workspace, item.document),
                name: named.map(|(title, _)| title.clone()),
                // An entry that is not a file holds the picture.
                picture: named.is_some_and(|(_, kind)| kind != koru_core::explore::KIND_FILE),
                reason: item.reason.clone(),
            }
        })
        .collect::<Vec<_>>();
    // The count is the whole number; what the list does not hold is "more".
    let more = state
        .counts
        .could_not_be_read
        .saturating_sub(items.len() as u64);
    Some(ReaderRow {
        status,
        languages: state
            .offer
            .as_ref()
            .map(|terms| terms.languages.clone())
            .unwrap_or_default(),
        items,
        more: if answer.items.items.is_empty() {
            0
        } else {
            more
        },
        not_here: state.counts.not_here,
    })
}

/// Whether the card should be asked about again soon: while a download runs.
pub(crate) fn running(answer_state: Option<&ReaderState>) -> bool {
    answer_state
        .and_then(|state| state.install.as_ref())
        .is_some_and(|install| install.stage.is_running())
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
