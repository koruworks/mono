// SPDX-License-Identifier: MPL-2.0

//! **The window's half of a Bot's quick setup** (QUESTIONS O155,
//! `bots::onboard`, `bots::view::onboard`).
//!
//! Three things only the window can do for the card a Bot tab draws over
//! its empty main conversation:
//!
//! 1. **Say whether the card was ended on this install**, and keep that.
//!    Every Bot tab is told from the process's book
//!    ([`crate::bots::onboard::dismissed`]) on the window's pass, and a tab
//!    that just ended or reopened its card is written into it. The book
//!    rides the session file's `bot_setup=` line ([`restore`], [`capture`]):
//!    not ai.db, not the wire.
//! 2. **Read whether the Role page is empty.** The card is offered only for
//!    a Role page with nothing under its title ([`role_is_blank`]): a Role a
//!    person wrote is never the card's to replace. The read is the Store's
//!    snapshot of the page, on a worker.
//! 3. **Write the Role page.** Step 1's Save leaves one
//!    [`RoleWrite`](crate::bots::onboard::RoleWrite); a worker replaces what
//!    is under the page's title, as another writer over the Store, between
//!    two History markers (`canvas_chain::replace_page_text`, the path a
//!    canvas's output page already takes). **The card's first write looks
//!    again first** and writes nothing when the page is no longer empty
//!    (`role_not_empty`).
//!
//! The window thread never waits: each worker answers on a channel and
//! wakes the window, and the answer goes to the tab of that Bot, wherever
//! it now is.
//!
//! **No Bot command was added.** The Role is an ordinary page of the Bot's
//! home workspace, and writing it is the person's own edit of that page.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use koru_ai_core::bots::{BotId, BotPage};
use koru_core::schema::{Parent, WorkspaceDoc};

use super::MainView;
use super::canvas_runs::Reach;
use super::tab::TabView;
use crate::bots::onboard::{self, News, RoleRead, RoleWrite};

/// What a worker says when the window has no Store client to reach with.
const UNAVAILABLE: &str = "store_unavailable";
/// What a write says when the page it would have filled is not empty.
pub(crate) const NOT_EMPTY: &str = "role_not_empty";

/// **Whether a page has nothing under its title**: every row under its
/// root is empty text with nothing nested in it. A nested page, a file and
/// the page's own Trash are not the page's text and are not counted (a
/// write keeps them). Anything this cannot read as empty text — an image, a
/// table, a row it does not know — counts as content: the card must never
/// take a page for empty by mistake.
pub(crate) fn role_is_blank(doc: &WorkspaceDoc) -> bool {
    let Some(root) = koru_workspace::document_root(doc) else {
        return false;
    };
    let Ok(rows) = doc.children(Parent::Node(root)) else {
        return false;
    };
    rows.into_iter().all(|row| {
        if matches!(
            doc.kind_of(row),
            Some(koru_core::Kind::Document | koru_core::Kind::File | koru_core::Kind::Trash)
        ) {
            return true;
        }
        doc.text_len(row).is_ok_and(|len| len == 0)
            && doc
                .children(Parent::Node(row))
                .is_ok_and(|nested| nested.is_empty())
    })
}

/// The Store's snapshot of `page`, read for whether it is blank.
fn read_blank(client: &koru_api_client::Client, page: &BotPage) -> Result<bool, String> {
    use koru_workspace::explore::{store_document, store_workspace};
    let bytes = client
        .snapshot(
            store_workspace(koru_core::WorkspaceId::from_uuid(page.workspace.as_uuid())),
            store_document(koru_core::DocumentId::from_uuid(page.document.as_uuid())),
        )
        .map_err(|error| error.code.as_str().to_owned())?;
    let doc = WorkspaceDoc::from_snapshot(&bytes).map_err(|_| "internal".to_owned())?;
    Ok(role_is_blank(&doc))
}

/// **Reading and writing a Bot's Role page**, each on a worker. The Store
/// answers in the app ([`StorePort`]); a test answers from memory.
pub(crate) trait RolePort: Send {
    /// Whether the page has nothing under its title, answered once.
    fn read(&mut self, reach: Reach<'_>, read: RoleRead) -> Receiver<Result<bool, String>>;
    /// Write the page, answering once: `Ok`, or the refusal's code.
    fn write(&mut self, reach: Reach<'_>, write: RoleWrite) -> Receiver<Result<(), String>>;
}

/// [`RolePort`] over the window's gateway client: a thread per request,
/// answering once, queue first and wake second.
struct StorePort;

/// Run `work` on a worker named `name` and answer on the channel returned.
/// A thread that cannot start drops the sender, and the pass reads the
/// closed channel as a failure.
fn on_worker<T: Send + 'static>(
    reach: Reach<'_>,
    name: &str,
    work: impl FnOnce(koru_api_client::Client) -> Result<T, String> + Send + 'static,
) -> Receiver<Result<T, String>> {
    let (send, receive) = mpsc::channel();
    let Some(client) = reach.client.cloned() else {
        let _ = send.send(Err(UNAVAILABLE.to_owned()));
        return receive;
    };
    let wake = reach.wake.cloned();
    let _ = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if send.send(work(client)).is_ok()
                && let Some(proxy) = wake
            {
                let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
            }
        });
    receive
}

impl RolePort for StorePort {
    fn read(&mut self, reach: Reach<'_>, read: RoleRead) -> Receiver<Result<bool, String>> {
        on_worker(reach, "koru-bot-role-read", move |client| {
            read_blank(&client, &read.page)
        })
    }

    fn write(&mut self, reach: Reach<'_>, write: RoleWrite) -> Receiver<Result<(), String>> {
        on_worker(reach, "koru-bot-role-write", move |client| {
            // The card's first write: only into a page still empty. Someone
            // may have written a Role since the card read it.
            if write.only_if_blank && !read_blank(&client, &write.page)? {
                return Err(NOT_EMPTY.to_owned());
            }
            super::canvas_chain::replace_page_text(
                &client,
                koru_core::WorkspaceId::from_uuid(write.page.workspace.as_uuid()),
                koru_core::DocumentId::from_uuid(write.page.document.as_uuid()),
                &write.title,
                &write.markdown,
                (&write.before, &write.after),
            )
            .map_err(|error| error.code.as_str().to_owned())
        })
    }
}

/// One request with a worker: whose, which, and where its answer comes.
struct Out<T> {
    bot: BotId,
    serial: u64,
    answer: Receiver<Result<T, String>>,
}

/// What became of the requests in `out` that have answered: each taken out
/// with its answer, a dead worker's as a failure.
fn answered<T>(out: &mut Vec<Out<T>>) -> Vec<(BotId, u64, Result<T, String>)> {
    let mut done = Vec::new();
    out.retain(|request| match request.answer.try_recv() {
        Ok(answer) => {
            done.push((request.bot, request.serial, answer));
            false
        }
        Err(TryRecvError::Empty) => true,
        // The worker never started, or died before it answered.
        Err(TryRecvError::Disconnected) => {
            done.push((request.bot, request.serial, Err(UNAVAILABLE.to_owned())));
            false
        }
    });
    done
}

/// **The Role reads and writes this window has out.** Nothing here is the
/// only copy of anything: the card holds what was typed and asks again.
pub(crate) struct BotSetups {
    port: Box<dyn RolePort>,
    reading: Vec<Out<bool>>,
    writing: Vec<Out<()>>,
}

impl Default for BotSetups {
    fn default() -> Self {
        Self::with_port(Box::new(StorePort))
    }
}

impl BotSetups {
    pub(crate) fn with_port(port: Box<dyn RolePort>) -> Self {
        Self {
            port,
            reading: Vec::new(),
            writing: Vec::new(),
        }
    }
}

/// Fold in the session file's `bot_setup=` line.
pub(super) fn restore(line: &str) {
    onboard::restore(line);
}

/// The `bot_setup=` line for the session file.
pub(super) fn capture() -> String {
    onboard::capture()
}

impl MainView {
    /// Do `f` to every tab of `bot`, in every pane. `true` when any said
    /// it changed.
    fn each_tab_of(
        &mut self,
        bot: BotId,
        mut f: impl FnMut(&mut crate::bots::BotView) -> bool,
    ) -> bool {
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                if let TabView::Bot(view) = &mut tab.view
                    && view.bot() == Some(bot)
                {
                    changed |= f(view);
                }
            }
        }
        changed
    }

    /// **The quick setup's pass**, over every Bot tab in every pane: the
    /// book takes what a tab just ended or reopened and tells each tab
    /// where its Bot stands; a Role read and a Save's write go to workers;
    /// a worker's answer goes to its Bot's tab. A comparison per Bot tab
    /// when nothing moved. `true` when a repaint is owed.
    pub(super) fn maintain_bot_setup(&mut self) -> bool {
        let mut changed = false;
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                let TabView::Bot(view) = &mut tab.view else {
                    continue;
                };
                let Some(bot) = view.bot() else {
                    continue;
                };
                if let Some(news) = view.take_setup_news() {
                    onboard::note(bot, news == News::Dismissed);
                    changed = true;
                }
                changed |= view.tell_setup(onboard::dismissed(bot));
                reads.extend(view.take_role_read());
                writes.extend(view.take_role_write());
            }
        }
        if !reads.is_empty() || !writes.is_empty() {
            let wake = self.agent.wake_proxy();
            for read in reads {
                let (bot, serial) = (read.bot, read.serial);
                let answer = self.bot_setups.port.read(
                    Reach {
                        client: self.delegate_client.as_ref(),
                        wake: wake.as_ref(),
                        workspace: None,
                    },
                    read,
                );
                self.bot_setups.reading.push(Out {
                    bot,
                    serial,
                    answer,
                });
            }
            for write in writes {
                let (bot, serial) = (write.bot, write.serial);
                let answer = self.bot_setups.port.write(
                    Reach {
                        client: self.delegate_client.as_ref(),
                        wake: wake.as_ref(),
                        workspace: None,
                    },
                    write,
                );
                self.bot_setups.writing.push(Out {
                    bot,
                    serial,
                    answer,
                });
            }
            changed = true;
        }
        // A failed read is `None`: never taken for an empty page.
        for (bot, serial, answer) in answered(&mut self.bot_setups.reading) {
            changed |= self.each_tab_of(bot, |view| view.role_read(serial, answer.clone().ok()));
        }
        for (bot, serial, answer) in answered(&mut self.bot_setups.writing) {
            changed |= self.each_tab_of(bot, |view| match &answer {
                Err(code) if code == NOT_EMPTY => view.role_found_written(serial),
                answer => view.role_written(serial, answer.is_ok()),
            });
        }
        changed
    }

    /// The window's Role reads and writes, for a test to answer.
    #[cfg(test)]
    pub(super) fn bot_setups_mut(&mut self) -> &mut BotSetups {
        &mut self.bot_setups
    }
}

#[cfg(test)]
mod tests;
