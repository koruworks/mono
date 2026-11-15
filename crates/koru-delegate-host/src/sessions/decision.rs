// SPDX-License-Identifier: MPL-2.0

//! Decision-only bounded merge. Ordinary subscriptions keep their established
//! behavior. A producer can hold at most its current item plus the one merged
//! channel slot; the actor pump owns the single delivery acknowledgement.

use super::*;
use koru_delegate_core::decisions::DecisionSourceFrontiers;

impl HostSessions {
    pub(super) fn decision_stream(
        &mut self,
        session: SessionId,
        after: &DecisionSourceFrontiers,
    ) -> PortResult<Box<dyn SessionEvents>> {
        let client = Arc::new(connect(&self.endpoint).map_err(|e| session_error("connect", e))?);
        let agent = client
            .subscribe_agent_events_from(session, after.agent_after)
            .map_err(|e| session_error("subscribe", e))?;
        let session_events = client
            .subscribe_events_from(session, after.session_after)
            .map_err(|e| session_error("subscribe", e))?;
        let (send, receive) = mpsc::sync_channel(1);
        for (name, subscription) in [
            ("koru-delegate decision agent", agent),
            ("koru-delegate decision session", session_events),
        ] {
            let send = send.clone();
            std::thread::Builder::new()
                .name(name.into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    loop {
                        let update = match subscription.recv() {
                            Ok(update) => update,
                            Err(_) => {
                                let _ = send.send(SessionUpdate::Closed { error: None });
                                return;
                            }
                        };
                        let Some(update) = normalized(update) else {
                            continue;
                        };
                        let closed = matches!(update, SessionUpdate::Closed { .. });
                        if send.send(update).is_err() || closed {
                            return;
                        }
                    }
                })
                .map_err(|e| PortError::new(Port::Sessions, "thread", e.to_string()))?;
        }
        Ok(Box::new(LiveEvents {
            receive,
            _client: client,
        }))
    }
}

fn normalized(update: SubscriptionUpdate) -> Option<SessionUpdate> {
    match update {
        SubscriptionUpdate::AgentEvent { event } => Some(SessionUpdate::Agent(event)),
        SubscriptionUpdate::AgentEventGap { expected, actual } => {
            Some(SessionUpdate::AgentGap { expected, actual })
        }
        SubscriptionUpdate::Event { event } => Some(SessionUpdate::Session(event)),
        SubscriptionUpdate::EventGap { expected, actual } => {
            Some(SessionUpdate::SessionGap { expected, actual })
        }
        // Upstream bounded queue overflow is Closed(StreamGap); preserve its
        // error as loss/reobserve evidence, never proof that a process stopped.
        SubscriptionUpdate::Closed { error } => Some(SessionUpdate::Closed { error }),
        _ => None,
    }
}
