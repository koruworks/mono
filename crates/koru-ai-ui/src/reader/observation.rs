// SPDX-License-Identifier: MPL-2.0
//! A reopened reply has an authoritative attempt ID but no owned event stream.
//! Observe that exact attempt off the window thread until it ends or navigation
//! drops the observation. This never sends or retries a turn.

use super::*;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(super) struct Observation {
    selected: Option<ChatId>,
    active: Option<Active>,
}

struct Active {
    chat: ChatId,
    attempt: InferenceAttemptId,
    generation: u64,
    _life: Arc<()>,
    running: Weak<()>,
}

impl Observation {
    pub(super) fn select(&mut self, command: &Command) {
        if let Command::StopObserving(chat, attempt, generation) = command {
            if self.selected == Some(*chat)
                && self.active.as_ref().is_none_or(|active| {
                    active.chat == *chat
                        && active.attempt == *attempt
                        && active.generation == *generation
                })
            {
                self.stop();
            }
            return;
        }
        let selected = match command {
            Command::Boot(chat, _) => Some(*chat),
            Command::Read(chat) => Some(Some(*chat)),
            Command::Create(request) => Some(Some(request.chat_id)),
            _ => None,
        };
        if let Some(selected) = selected {
            self.selected = selected;
            if self
                .active
                .as_ref()
                .is_some_and(|active| Some(active.chat) != selected)
            {
                self.active = None;
            }
        }
    }

    pub(super) fn stop(&mut self) {
        self.active = None;
    }

    pub(super) fn start(
        &mut self,
        client: &koru_api_client::Client,
        updates: &SyncSender<Update>,
        chat: ChatId,
        attempt: InferenceAttemptId,
        generation: u64,
    ) {
        let Some((alive, running)) = self.begin(chat, attempt, generation) else {
            return;
        };
        let client = client.clone();
        let updates = updates.clone();
        let failed = updates.clone();
        let started = std::thread::Builder::new()
            .name("koru-ai-ui-observe".into())
            .spawn(move || {
                // Deduplicate only while the worker exists. After a refusal or
                // spawn failure a successful refresh may observe this same ID.
                let mut running = Some(running);
                observe(
                    || {
                        client.ai().read_attempt(attempt).and_then(|response| {
                            if response.attempt.id == attempt
                                && response.attempt.chat_id == Some(chat)
                            {
                                Ok(response.attempt.status)
                            } else {
                                Err(mismatched_response())
                            }
                        })
                    },
                    || {
                        client.ai().read_chat(chat).and_then(|transcript| {
                            if transcript.chat.id == chat {
                                Ok(decorate_transcript(&client, transcript))
                            } else {
                                Err(mismatched_response())
                            }
                        })
                    },
                    |update| {
                        // A refresh triggered by this terminal receipt can start
                        // observing immediately, before this worker is scheduled
                        // again. Clear its running proof before publishing it.
                        if !matches!(update, Update::Observed { .. }) {
                            drop(running.take());
                        }
                        updates
                            .send(Update::Observation {
                                generation,
                                attempt,
                                update: Box::new(update),
                            })
                            .is_ok()
                    },
                    || Weak::upgrade(&alive).is_some(),
                    std::thread::sleep,
                    attempt,
                );
            });
        if started.is_err() {
            self.stop();
            let _ = failed.send(Update::Observation {
                generation, attempt,
                update: Box::new(Update::RecoveryFailed {
                    attempt,
                    failure: unreachable_failure(
                        "The response could not be refreshed. Reopen the conversation to try again."
                            .into(),
                    ),
                }),
            });
        }
    }

    fn begin(
        &mut self,
        chat: ChatId,
        attempt: InferenceAttemptId,
        generation: u64,
    ) -> Option<(Weak<()>, Arc<()>)> {
        if self.selected.is_some_and(|selected| selected != chat)
            || self.active.as_ref().is_some_and(|active| {
                active.chat == chat
                    && active.attempt == attempt
                    && active.generation == generation
                    && active.running.upgrade().is_some()
            })
        {
            return None;
        }
        self.selected = Some(chat);
        let life = Arc::new(());
        let alive = Arc::downgrade(&life);
        let running = Arc::new(());
        self.active = Some(Active {
            chat,
            attempt,
            generation,
            _life: life,
            running: Arc::downgrade(&running),
        });
        Some((alive, running))
    }
}

fn mismatched_response() -> koru_ai_wire::Failure {
    koru_ai_wire::Failure {
        code: "invalid_response".into(),
        message: "The response did not belong to this conversation.".into(),
        retryable: false,
        details: Default::default(),
    }
}

/// Read at the existing bounded recovery cadence. Unchanged snapshots do not
/// reach the window thread, so idle queued work does not reflow the chat.
fn observe(
    mut read_attempt: impl FnMut() -> Result<AttemptStatus, koru_ai_wire::Failure>,
    mut read_chat: impl FnMut() -> Result<ChatTranscript, koru_ai_wire::Failure>,
    mut publish: impl FnMut(Update) -> bool,
    alive: impl Fn() -> bool,
    mut pause: impl FnMut(Duration),
    attempt: InferenceAttemptId,
) {
    let mut previous = None;
    let mut wait = RECOVERY_FIRST;
    while alive() {
        let result = read_attempt().and_then(|status| read_chat().map(|chat| (status, chat)));
        // A blocked read can finish after the person navigated elsewhere.
        if !alive() {
            return;
        }
        match result {
            Ok((status, transcript)) if status.is_terminal() => {
                publish(Update::Recovered {
                    attempt,
                    status,
                    transcript,
                });
                return;
            }
            Ok((_, transcript)) => {
                if previous.as_ref() != Some(&transcript) {
                    previous = Some(transcript.clone());
                    if !publish(Update::Observed {
                        attempt,
                        transcript,
                    }) {
                        return;
                    }
                }
            }
            Err(failure) if !failure.retryable => {
                publish(Update::RecoveryFailed { attempt, failure });
                return;
            }
            Err(_) => {}
        }
        pause(wait);
        wait = (wait * 2).min(RECOVERY_MAX);
    }
}

#[cfg(test)]
mod tests;
