// SPDX-License-Identifier: MPL-2.0
//! The shared surface's adapter for engine-owned conversations.
use super::*;
use koru_ai_wire::bot_threads::{Command as C, Response as R};

#[cfg(test)]
mod tests;

impl ChatSurface {
    pub(super) fn response_action_count(&self) -> usize {
        // Engine blocks are local layout keys, not durable chat-block links.
        // Branch and Store need their engine-owned command paths.
        if self.managed.is_some() {
            1
        } else {
            RESPONSE_ACTIONS
        }
    }
    pub(crate) fn managed_line(&self) -> Option<&'static str> {
        let state = self.managed.as_ref()?;
        let key = if state.retry_send {
            koru_i18n::Key::BotsEngineSendUncertain
        } else if state.gap {
            koru_i18n::Key::BotsEngineLiveGap
        } else if state.snapshot.team_paused {
            koru_i18n::Key::BotsTeamPausedLine
        } else if state.snapshot.paused {
            koru_i18n::Key::BotsPaused
        } else if state.snapshot.waiting_messages > 0 && !state.running() {
            koru_i18n::Key::BotsEngineWaiting
        } else {
            return None;
        };
        Some(koru_i18n::t(self.lang, key))
    }

    pub(crate) fn leave_managed(&mut self) {
        self.managed_cards.clear();
        if let Some(mut state) = self.managed.take() {
            let thread = state.thread();
            if let Some(command) = state.pending_send.take() {
                self.managed_outbox
                    .insert(thread, (command, self.sent_message.take()));
            }
            self.pending_user = None;
            self.pending_user_tiles.clear();
            self.transcript = None;
            self.messages_dirty = true;
        }
    }

    pub(crate) fn managed_command(&mut self, command: Command) -> Option<Command> {
        let Some(state) = &self.managed else {
            return Some(command);
        };
        let thread = state.thread();
        let bot = state.snapshot.conversation.label.bot_id;
        let chat = &state.snapshot.conversation.chat;
        let command = match command {
            Command::Send(request) => {
                // These inputs need their engine carrier before they can be
                // admitted. Never silently discard part of a person's send.
                if request.chat_id != thread
                    || !request.attachment_ids.is_empty()
                    || request.answers.is_some()
                    || !request.skills.is_empty()
                    || request.system_prompt_override.is_some()
                {
                    self.status = Some(
                        koru_i18n::t(self.lang, koru_i18n::Key::BotsConversationUnavailable).into(),
                    );
                    return None;
                }
                let references = request
                    .explicit_refs
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                C::Send {
                    id: request.operation_id,
                    bot,
                    thread,
                    body: request.markdown,
                    in_reply_to: None,
                    attachments: vec![],
                    references,
                }
            }
            Command::UpdateModel(id, request) if id == thread => C::Settings {
                id: uuid::Uuid::new_v4(),
                bot,
                thread,
                revision: state.snapshot.settings_revision,
                model: request.model,
                mode: chat.agent_mode.clone(),
                fast: chat.agent_fast,
            },
            Command::UpdateAgentSettings(id, request) if id == thread => C::Settings {
                id: uuid::Uuid::new_v4(),
                bot,
                thread,
                revision: state.snapshot.settings_revision,
                model: chat.current_model.clone(),
                mode: request.mode.or_else(|| chat.agent_mode.clone()),
                fast: request.fast.or(chat.agent_fast),
            },
            Command::Retry(id, request) if id == thread => {
                let turn = state.snapshot.turns.iter().find(|turn| {
                    turn.reply_message == Some(request.message_id.as_uuid())
                        && turn.state == "needs_you"
                        && turn.details.get("setup_failure").is_some()
                })?;
                C::Retry {
                    id: request.operation_id,
                    thread,
                    turn: turn.id,
                }
            }
            other => return Some(other),
        };
        Some(Command::Engine { thread, command })
    }

    pub(crate) fn managed_opened(&mut self, mut opened: managed::Opened) -> bool {
        if self
            .owned
            .as_ref()
            .is_some_and(|owned| owned.chat != opened.snapshot.conversation.chat.id)
        {
            return false;
        }
        let now = Instant::now();
        let mut state = managed::State::new(&mut opened, now);
        if let Some(old) = self
            .managed
            .take()
            .filter(|old| old.thread() == state.thread())
        {
            state.pending_send = old.pending_send;
            state.retry_send = state.pending_send.is_some();
        }
        if let Some((command, sent)) = self.managed_outbox.remove(&state.thread()) {
            state.pending_send = Some(command);
            state.retry_send = true;
            self.sent_message = sent;
        }
        let transcript = state.transcript();
        self.managed = Some(state);
        self.unavailable = None;
        let pending_user = self.pending_user.take();
        let changed = if let Some(settings) = opened.settings {
            self.apply_update(Update::Boot {
                settings,
                chats: vec![],
                transcript: Some(transcript),
                link_missing: false,
                destinations: vec![],
                destination_error: None,
            })
        } else {
            self.apply_update(Update::Transcript(transcript))
        };
        if self
            .managed
            .as_ref()
            .is_some_and(|s| s.pending_send.is_some())
        {
            self.pending_user =
                pending_user.or_else(|| self.sent_message.as_ref().map(|s| s.markdown.clone()));
        }
        changed
    }

    pub(crate) fn managed_refreshed(&mut self, update: managed::Refreshed) -> bool {
        let Some(state) = &mut self.managed else {
            return false;
        };
        let changed = state.refresh(update, Instant::now());
        if !changed {
            return false;
        }
        if state.history_changed {
            let mut next = state.transcript();
            if let Some(old) = &self.transcript
                && old.chat.current_model == next.chat.current_model
            {
                next.reasoning_control = old.reasoning_control.clone();
                next.agent_commands = old.agent_commands.clone();
            }
            self.sent_inputs.retain(|sent| {
                sent.chat != next.chat.id
                    || sent
                        .message_id
                        .is_none_or(|id| !next.messages.iter().any(|m| m.id == id))
            });
            self.transcript = Some(next);
        } else if let Some(transcript) = &mut self.transcript {
            transcript.chat = state.snapshot.conversation.chat.clone();
        }
        self.messages_dirty = true;
        true
    }

    pub(crate) fn managed_reply(&mut self, thread: ChatId, reply: R) -> bool {
        self.settle_managed_request(thread, &reply);
        let Some(state) = &mut self.managed else {
            return false;
        };
        if state.thread() != thread {
            return false;
        }
        let accepted = matches!(reply, R::Accepted { .. });
        match reply {
            R::History {
                messages, before, ..
            } => {
                if !state.history_pending {
                    return false;
                }
                state.history_pending = false;
                state.before = before;
                for message in messages {
                    state.ingest(message);
                }
                let mut transcript = state.transcript();
                if let Some(old) = &self.transcript {
                    transcript.reasoning_control = old.reasoning_control.clone();
                    transcript.agent_commands = old.agent_commands.clone();
                }
                self.transcript = Some(transcript);
            }
            R::Accepted {
                request,
                message,
                thread: accepted_thread,
                ..
            } => {
                if accepted_thread != thread.as_uuid()
                    || !matches!(&state.pending_send,
                    Some(C::Send { id, .. }) if *id == request)
                {
                    return false;
                }
                state.pending_send = None;
                state.retry_send = false;
                if let Some(sent) = self
                    .sent_inputs
                    .iter_mut()
                    .rev()
                    .find(|sent| sent.chat == thread && sent.message_id.is_none())
                {
                    sent.message_id = Some(MessageId::from_uuid(message));
                }
                self.sent_message = None;
                self.pending_user = None;
                self.pending_user_tiles.clear();
                self.pulse = None;
                // An accepted input is not evidence that any worker runs.
                self.attempt = None;
                self.pending_assistant_message_id = None;
            }
            R::Settings {
                revision,
                model,
                mode,
                fast,
                ..
            } => {
                if revision >= state.snapshot.settings_revision {
                    state.snapshot.settings_revision = revision;
                    let chat = &mut state.snapshot.conversation.chat;
                    chat.current_model = model;
                    chat.agent_mode = mode;
                    chat.agent_fast = fast;
                    if let Some(transcript) = &mut self.transcript {
                        transcript.chat = chat.clone();
                    }
                }
            }
            R::Stopped { .. } => self.queues.pause(thread),
            R::Answered { .. } | R::Changed { .. } | R::Continued { .. } => {}
            _ => return false,
        }
        state.poll_at = Instant::now();
        self.waiting = false;
        self.status = None;
        self.messages_dirty = true;
        if accepted {
            self.note_activity();
            if !self.reply_busy() && !self.queues.is_paused(thread) {
                self.send_queue_head();
            }
        }
        true
    }

    pub(crate) fn managed_failed(
        &mut self,
        thread: ChatId,
        command: C,
        failure: koru_ai_wire::Failure,
    ) -> bool {
        if self.failed_managed_request(thread, &command, &failure) {
            return true;
        }
        let Some(state) = &mut self.managed else {
            return false;
        };
        if state.thread() != thread {
            return false;
        }
        if matches!(command, C::History { .. }) {
            state.history_pending = false;
            state.history_anchor = None;
        }
        if let C::Send { id, .. } = command {
            if !matches!(&state.pending_send, Some(C::Send { id: pending, .. }) if *pending == id) {
                return false;
            }
            if failure.retryable
                || matches!(
                    failure.code.as_str(),
                    "ai_unavailable"
                        | "gateway_unavailable"
                        | "engine_unavailable"
                        | "storage_unavailable"
                        | "invalid_response"
                )
            {
                // Preserve the exact id AND payload across an uncertain reply.
                state.retry_send = true;
                state.retry_at = Instant::now() + Duration::from_secs(2);
            } else {
                state.pending_send = None;
                state.retry_send = false;
                self.give_back_sent(Some(&failure));
            }
        }
        self.waiting = false;
        self.status = Some(self.failure_words(&failure));
        true
    }

    pub(crate) fn maintain_managed(&mut self, now: Instant) -> bool {
        self.maintain_managed_requests(now);
        let Some(reader) = &self.reader else {
            return false;
        };
        let Some(state) = &mut self.managed else {
            return false;
        };
        if !self.follow_tail
            && self.conversation_scroll <= 1.0
            && !state.history_pending
            && let Some(before) = state.before
        {
            let command = C::History {
                thread: state.thread(),
                before: Some(before),
            };
            if reader
                .send(Command::Engine {
                    thread: state.thread(),
                    command,
                })
                .is_ok()
            {
                state.history_pending = true;
                state.history_anchor = self.message_layouts.iter().find_map(|message| {
                    message
                        .id
                        .map(|id| (id, message.top - self.conversation_scroll))
                });
            }
        }
        if state.retry_send && now >= state.retry_at {
            if let Some(command) = state.pending_send.clone() {
                if reader
                    .send(Command::Engine {
                        thread: state.thread(),
                        command,
                    })
                    .is_ok()
                {
                    state.retry_send = false;
                }
                state.retry_at = now + Duration::from_secs(2);
            }
        }
        if let Some(poll) = state.poll(now) {
            if reader.send(Command::EnginePoll(poll)).is_err() {
                state.settled(now);
            }
        }
        false
    }

    pub(crate) fn stop_managed(&mut self) -> bool {
        let Some(state) = &self.managed else {
            return false;
        };
        let Some(turn) = state.primary_turn() else {
            return false;
        };
        self.queue_command(Command::Engine {
            thread: state.thread(),
            command: C::Stop {
                id: uuid::Uuid::new_v4(),
                turn,
            },
        })
    }

    pub(crate) fn append_managed_live(
        &mut self,
        messages: &mut Vec<MessageLayout>,
        old: Vec<MessageLayout>,
        width: f32,
    ) {
        if self.managed.is_none() {
            return;
        }
        if let Some(markdown) = self.pending_user.clone() {
            messages.push(self.user_card(&markdown, self.pending_user_tiles.clone(), width));
        }
        let state = self.managed.as_mut().expect("checked above");
        let mut old = old;
        for turn in &state.snapshot.turns {
            let Some(live) = state.live.get_mut(&turn.id) else {
                continue;
            };
            if live.bot != turn.bot {
                continue;
            }
            let blocks = live.markdown.blocks(live.reply, 0, &live.text);
            let tail = live.markdown.tail_id();
            let cached = old
                .iter()
                .position(|m| m.id == Some(live.reply))
                .map(|i| old.swap_remove(i));
            let (laid, stats) = layout_cache::reconcile(
                &mut self.engine,
                cached.into_iter().collect(),
                vec![layout_cache::MessageInput {
                    id: live.reply,
                    role: ChatRole::Assistant,
                    state: if turn.state == "running" {
                        MessageState::Pending
                    } else {
                        MessageState::Interrupted
                    },
                    markdown: live.text.clone(),
                    display: live.text.clone(),
                    links: vec![],
                    wrap_width: message_wrap_width(ChatRole::Assistant, width),
                    usage_text: None,
                    sources: vec![],
                    blocks,
                    attachments: vec![],
                }],
                |engine, block, markdown, width| {
                    if block == tail {
                        live_markdown::plain_block(engine, block, markdown, width)
                    } else {
                        render_chat_block(engine, block, markdown, width)
                    }
                },
            );
            self.last_rebuild_stats += stats;
            messages.extend(laid);
        }
    }
}
