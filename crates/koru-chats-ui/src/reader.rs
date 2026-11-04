// SPDX-License-Identifier: MPL-2.0

//! Everything this surface asks the AI service, off the window thread.
//!
//! **Two threads, for the reason the Artifacts column has two.** The list
//! thread coalesces to the newest request, which is right for a refresh and
//! wrong for a delete: a delete queued behind two refreshes would simply be
//! dropped. So deletes have their own channel and thread, and a slow listing
//! can never sit in front of one.
//!
//! **Dead under test, and that is the point.** [`Reader::start`] is never
//! called under `cfg(test)` or with the `no-reader` feature, so no unit test
//! dials the developer's own gateway.
#![cfg_attr(test, allow(dead_code))]

use std::sync::mpsc::{self, Receiver, Sender};

use koru_ai_core::ChatId;
use koru_ai_wire::Failure;

use crate::Listing;
use crate::gateway::Gateway;

pub(crate) enum Answer {
    Listed {
        seq: u64,
        result: Result<Listing, Failure>,
    },
    Deleted {
        chat: ChatId,
        result: Result<(), Failure>,
    },
}

pub(crate) struct Reader {
    lists: Sender<u64>,
    deletes: Sender<ChatId>,
    answers: Receiver<Answer>,
}

impl Reader {
    pub(crate) fn start(root: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        let (lists, list_rx) = mpsc::channel::<u64>();
        let (deletes, delete_rx) = mpsc::channel::<ChatId>();
        let (answer_tx, answers) = mpsc::channel::<Answer>();
        let delete_tx = answer_tx.clone();

        // **One handle, two threads.** Two clients would mint two gateway
        // sessions per window.
        let gateway = Gateway::new(root);
        let deleting = gateway.clone();

        std::thread::Builder::new()
            .name("koru-chats-read".into())
            .spawn(move || {
                while let Ok(seq) = list_rx.recv() {
                    let seq = list_rx.try_iter().last().unwrap_or(seq);
                    let result = list(&gateway);
                    if answer_tx.send(Answer::Listed { seq, result }).is_err() {
                        break;
                    }
                }
            })?;

        std::thread::Builder::new()
            .name("koru-chats-delete".into())
            .spawn(move || {
                while let Ok(chat) = delete_rx.recv() {
                    // Not logged here: a refusal is the AI worker's news, and
                    // it already says so (content-free) where it happens.
                    let result = deleting
                        .call(|client| client.ai().delete_chat(chat))
                        .map(|_| ());
                    if delete_tx.send(Answer::Deleted { chat, result }).is_err() {
                        break;
                    }
                }
            })?;

        Ok(Self {
            lists,
            deletes,
            answers,
        })
    }

    pub(crate) fn list(&self, seq: u64) -> bool {
        self.lists.send(seq).is_ok()
    }

    pub(crate) fn delete(&self, chat: ChatId) -> bool {
        self.deletes.send(chat).is_ok()
    }

    pub(crate) fn drain(&self) -> Vec<Answer> {
        self.answers.try_iter().collect()
    }
}

/// One refresh: the chats, and the agents' names.
///
/// The names may fail on their own: without them an agent chat says *Local
/// agent* rather than its name, which is a better answer than an empty list
/// because one request did not land.
fn list(gateway: &Gateway) -> Result<Listing, Failure> {
    let chats = gateway.call(|client| client.ai().list_chats(None))?.chats;
    let agents = gateway
        .call(|client| client.ai().provider_settings())
        .map(|settings| settings.agents)
        .unwrap_or_default();
    Ok(Listing { chats, agents })
}
