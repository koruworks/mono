// SPDX-License-Identifier: MPL-2.0
//! Blocking carrier work for the owned conversation, outside the UI thread.
use super::*;
use crate::managed::{Feed, Opened, Poll, Refreshed};
use koru_ai_wire::bot_threads::{Command as C, Response as R};

#[derive(Default, PartialEq, Eq)]
pub(super) enum Backend {
    #[default]
    Unknown,
    Legacy,
    Engine,
}

fn malformed() -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: "invalid_response".into(),
        message: "The conversation response was invalid.".into(),
        retryable: true,
        details: Default::default(),
    }
}
fn navigation(
    client: &koru_api_client::Client,
    thread: ChatId,
    after: Option<uuid::Uuid>,
) -> Result<crate::managed_navigation::Page, koru_ai_wire::Failure> {
    match client.ai().bot_thread(&C::Cards { thread, after })? {
        R::Cards { items, next } => Ok(crate::managed_navigation::Page { after, items, next }),
        _ => Err(malformed()),
    }
}
fn read(
    client: &koru_api_client::Client,
    thread: ChatId,
) -> Result<koru_ai_wire::bot_threads::Snapshot, koru_ai_wire::Failure> {
    match client.ai().bot_thread(&C::Read { thread })? {
        R::Read { snapshot } if snapshot.conversation.chat.id == thread => Ok(snapshot),
        _ => Err(malformed()),
    }
}
fn attention(
    client: &koru_api_client::Client,
    thread: ChatId,
    after: Option<koru_ai_wire::bot_threads::OperationRef>,
) -> Result<crate::managed_attention::Page, koru_ai_wire::Failure> {
    match client.ai().bot_thread(&C::Attention {
        thread,
        after: after.clone(),
    })? {
        R::Attention { items, next } => Ok(crate::managed_attention::Page { after, items, next }),
        _ => Err(malformed()),
    }
}
fn refreshed(
    client: &koru_api_client::Client,
    poll: Poll,
) -> Result<Refreshed, koru_ai_wire::Failure> {
    // Read observations first, then their turn identities, then committed
    // messages. A newly started turn is present in the snapshot or has already
    // published into history; events never outrun the snapshot we send.
    let R::Observed {
        boot,
        cursor,
        gap,
        entries,
    } = client.ai().bot_thread(&C::Observe {
        thread: poll.thread,
        boot: poll.boot,
        after: poll.cursor,
    })?
    else {
        return Err(malformed());
    };
    let snapshot = read(client, poll.thread)?;
    let R::History {
        messages, after, ..
    } = client.ai().bot_thread(&C::Since {
        thread: poll.thread,
        after: poll.after,
    })?
    else {
        return Err(malformed());
    };
    let R::Questions { questions } = client.ai().bot_thread(&C::Questions {
        thread: poll.thread,
    })?
    else {
        return Err(malformed());
    };
    Ok(Refreshed {
        navigation: navigation(client, poll.thread, poll.navigation_after)?,
        attention: attention(client, poll.thread, poll.attention_after)?,
        snapshot,
        messages,
        after,
        questions,
        feed: Feed {
            boot,
            cursor,
            gap,
            entries,
        },
    })
}

pub(super) fn handle(
    client: &koru_api_client::Client,
    updates: &SyncSender<Update>,
    alive: &std::sync::Arc<()>,
    backend: &mut Backend,
    command: &Command,
) -> bool {
    match command {
        Command::Boot(Some(thread), _) | Command::Read(thread) if *backend != Backend::Legacy => {
            let snapshot = match read(client, *thread) {
                Ok(snapshot) => {
                    *backend = Backend::Engine;
                    snapshot
                }
                Err(failure)
                    if *backend == Backend::Unknown && failure.code == "engine_disabled" =>
                {
                    *backend = Backend::Legacy;
                    return false;
                }
                Err(_) => {
                    let _ = updates.send(Update::Unavailable {
                        chat: Some(*thread),
                        reason: crate::owned::Unavailable::Unreachable,
                    });
                    return true;
                }
            };
            let opened = (|| {
                let settings = if matches!(command, Command::Boot(..)) {
                    Some(client.ai().provider_settings()?)
                } else {
                    None
                };
                let R::History {
                    messages,
                    before,
                    after,
                } = client.ai().bot_thread(&C::History {
                    thread: *thread,
                    before: None,
                })?
                else {
                    return Err(malformed());
                };
                let R::Questions { questions } =
                    client.ai().bot_thread(&C::Questions { thread: *thread })?
                else {
                    return Err(malformed());
                };
                Ok(Opened {
                    navigation: navigation(client, *thread, None)?,
                    attention: attention(client, *thread, None)?,
                    snapshot,
                    messages,
                    before,
                    after,
                    settings,
                    questions,
                })
            })();
            let update = match opened {
                Ok(opened) => Update::EngineOpened(Box::new(opened)),
                Err(_) => Update::Unavailable {
                    chat: Some(*thread),
                    reason: crate::owned::Unavailable::Unreachable,
                },
            };
            let _ = updates.send(update);
            true
        }
        Command::Engine { thread, command } if *backend == Backend::Engine => {
            let update = match client.ai().bot_thread(command) {
                Ok(reply) => Update::EngineReply {
                    thread: *thread,
                    reply,
                },
                Err(failure) => Update::EngineFailed {
                    thread: *thread,
                    command: command.clone(),
                    failure,
                },
            };
            let _ = updates.send(update);
            true
        }
        Command::EnginePoll(poll) if *backend == Backend::Engine => {
            let poll = Poll {
                navigation_after: poll.navigation_after,
                attention_after: poll.attention_after.clone(),
                thread: poll.thread,
                after: poll.after,
                boot: poll.boot,
                cursor: poll.cursor,
            };
            let client = client.clone();
            let updates = updates.clone();
            let alive = std::sync::Arc::downgrade(alive);
            // The surface admits only one observation job. A slow catalog read
            // cannot hold its Send/Stop command thread behind that job.
            let thread = poll.thread;
            let refused = updates.clone();
            let started = std::thread::Builder::new()
                .name("koru-owned-conversation-read".into())
                .spawn(move || {
                    if alive.upgrade().is_none() {
                        return;
                    }
                    let thread = poll.thread;
                    let result = refreshed(&client, poll);
                    if alive.upgrade().is_none() {
                        return;
                    }
                    let update = match result {
                        Ok(value) => Update::EngineRefreshed(Box::new(value)),
                        Err(failure) => Update::EnginePollFailed { thread, failure },
                    };
                    let _ = updates.send(update);
                });
            if started.is_err() {
                let _ = refused.send(Update::EnginePollFailed {
                    thread,
                    failure: unreachable_failure("The conversation could not be refreshed.".into()),
                });
            }
            true
        }
        // A command whose ownership was not translated is never sent to the
        // ordinary AI repository after the engine has claimed this surface.
        Command::Send(_)
        | Command::Cancel(_)
        | Command::Steer(..)
        | Command::Retry(..)
        | Command::Branch(..)
        | Command::Rename(..)
        | Command::Pin(..)
        | Command::UpdateModel(..)
        | Command::UpdateAgentSettings(..)
        | Command::UpdatePrivacy(..)
        | Command::UpdateFolder(..)
        | Command::UpdateToolPolicy(..)
        | Command::AddGrants(..)
        | Command::Restage(..)
        | Command::CopyWorkspaceFile(..)
        | Command::ApproveTool(_)
        | Command::DenyTool(_)
        | Command::DecideTool(..)
        | Command::AnswerQuestions(..)
        | Command::Export(..)
            if *backend == Backend::Engine =>
        {
            let failure = koru_ai_wire::Failure {
                code: "not_supported".into(),
                message: "The conversation owner does not offer this action.".into(),
                retryable: false,
                details: Default::default(),
            };
            let update = if matches!(command, Command::Send(_)) {
                Update::SendFailed(failure)
            } else {
                Update::Failure(failure.message)
            };
            let _ = updates.send(update);
            true
        }
        _ => false,
    }
}
