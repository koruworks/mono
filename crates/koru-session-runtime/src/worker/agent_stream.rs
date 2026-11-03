// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;

use koru_session_observability::{Counter, SessionStats};
use koru_session_protocol::{
    AgentEvent, AgentEventObservation, SequencedAgentEvent, SessionId, StreamSequence,
};

use crate::BoundedStream;
use crate::stream::{PushError, Queue};

const LIVE_QUEUE_CAPACITY: usize = 32;
const REPLAY_EVENT_CAPACITY: usize = 256;
const REPLAY_BYTE_CAPACITY: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEventUpdate {
    Event(SequencedAgentEvent),
    Gap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
}

struct Subscriber {
    queue: Queue<AgentEventUpdate>,
    gapped: bool,
}

pub(super) struct AgentStreamState {
    sequence: StreamSequence,
    replay: VecDeque<SequencedAgentEvent>,
    replay_bytes: usize,
    subscribers: Vec<Subscriber>,
}

impl AgentStreamState {
    pub(super) fn new(sequence: StreamSequence) -> Self {
        Self {
            sequence,
            replay: VecDeque::with_capacity(REPLAY_EVENT_CAPACITY),
            replay_bytes: 0,
            subscribers: Vec::new(),
        }
    }

    pub(super) fn sequence(&self) -> StreamSequence {
        self.sequence
    }

    pub(super) fn publish(
        &mut self,
        session_id: SessionId,
        event: AgentEvent,
        stats: &SessionStats,
    ) -> StreamSequence {
        self.sequence = self.sequence.next().unwrap_or(self.sequence);
        let event = SequencedAgentEvent {
            session_id,
            sequence: self.sequence,
            event,
        };
        self.retain_for_replay(event.clone());
        self.subscribers.retain_mut(|subscriber| {
            if subscriber.gapped || !subscriber.queue.is_alive() {
                return subscriber.queue.is_alive();
            }
            match subscriber
                .queue
                .try_push(AgentEventUpdate::Event(event.clone()))
            {
                Ok(depth) => {
                    stats.observe_high_water(Counter::QueueHighWater, depth as u64);
                    true
                }
                Err(PushError::Full(_)) => {
                    let expected = subscriber
                        .queue
                        .first()
                        .and_then(update_sequence)
                        .unwrap_or(event.sequence);
                    let _ = subscriber.queue.replace_with(AgentEventUpdate::Gap {
                        expected,
                        actual: event.sequence,
                    });
                    subscriber.gapped = true;
                    stats.add(Counter::GappedSubscribers, 1);
                    true
                }
                Err(PushError::Closed(_)) => false,
            }
        });
        self.sequence
    }

    pub(super) fn observe(&self) -> AgentEventObservation {
        AgentEventObservation {
            events: self.replay.iter().cloned().collect(),
            cursor: self.sequence,
            earlier_truncated: self
                .replay
                .front()
                .is_some_and(|event| event.sequence.get() > 2),
        }
    }

    pub(super) fn subscribe(
        &mut self,
        after: Option<StreamSequence>,
        stats: &SessionStats,
    ) -> BoundedStream<AgentEventUpdate> {
        let (queue, stream) = Queue::new(LIVE_QUEUE_CAPACITY);
        let mut gapped = false;
        if let Some(after) = after {
            let expected = after.next().unwrap_or(after);
            if after > self.sequence {
                let _ = queue.try_push(AgentEventUpdate::Gap {
                    expected,
                    actual: self.sequence,
                });
                gapped = true;
            } else if after != self.sequence {
                let earliest = self.replay.front().map(|event| event.sequence);
                if earliest.is_none_or(|earliest| earliest > expected) {
                    let _ = queue.try_push(AgentEventUpdate::Gap {
                        expected,
                        actual: self.sequence,
                    });
                    gapped = true;
                } else {
                    for event in self.replay.iter().filter(|event| event.sequence > after) {
                        if queue
                            .try_push(AgentEventUpdate::Event(event.clone()))
                            .is_err()
                        {
                            let _ = queue.replace_with(AgentEventUpdate::Gap {
                                expected,
                                actual: self.sequence,
                            });
                            gapped = true;
                            break;
                        }
                    }
                }
            }
        }
        if gapped {
            stats.add(Counter::GappedSubscribers, 1);
        }
        self.subscribers.push(Subscriber { queue, gapped });
        stream
    }

    pub(super) fn close(&mut self) {
        for subscriber in &self.subscribers {
            subscriber.queue.close();
        }
        self.subscribers.clear();
        self.replay.clear();
        self.replay_bytes = 0;
    }

    fn retain_for_replay(&mut self, event: SequencedAgentEvent) {
        let bytes = event_bytes(&event.event);
        if bytes > REPLAY_BYTE_CAPACITY {
            self.replay.clear();
            self.replay_bytes = 0;
            return;
        }
        while self.replay.len() >= REPLAY_EVENT_CAPACITY
            || self.replay_bytes.saturating_add(bytes) > REPLAY_BYTE_CAPACITY
        {
            let Some(removed) = self.replay.pop_front() else {
                break;
            };
            self.replay_bytes = self
                .replay_bytes
                .saturating_sub(event_bytes(&removed.event));
        }
        self.replay.push_back(event);
        self.replay_bytes = self.replay_bytes.saturating_add(bytes);
    }
}

fn event_bytes(event: &AgentEvent) -> usize {
    const FIXED: usize = 128;
    FIXED
        + match event {
            AgentEvent::MessageDelta { text, .. } => text.len(),
            AgentEvent::ToolStarted { label, .. } => label.len(),
            AgentEvent::ApprovalRequired {
                summary, choices, ..
            } => choices.iter().fold(summary.len(), |bytes, choice| {
                bytes
                    .saturating_add(std::mem::size_of::<koru_session_protocol::ApprovalChoice>())
                    .saturating_add(choice.label.len())
            }),
            AgentEvent::TurnFailed { error, .. } => error.message.len(),
            AgentEvent::ModeChanged { mode, .. } => mode.as_str().len(),
            AgentEvent::Notice { text, .. } => text.len(),
            AgentEvent::TurnStarted { .. }
            | AgentEvent::ToolCompleted { .. }
            | AgentEvent::ApprovalResolved { .. }
            | AgentEvent::ApprovalExpired { .. }
            | AgentEvent::WaitingForInput { .. }
            | AgentEvent::AttachmentAccepted { .. }
            | AgentEvent::AttachmentRejected { .. }
            | AgentEvent::Usage { .. }
            | AgentEvent::ChildSessionCreated { .. }
            | AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::ProviderState { .. } => 0,
        }
}

fn update_sequence(update: AgentEventUpdate) -> Option<StreamSequence> {
    match update {
        AgentEventUpdate::Event(event) => Some(event.sequence),
        AgentEventUpdate::Gap { expected, .. } => Some(expected),
    }
}

#[cfg(test)]
mod tests;
