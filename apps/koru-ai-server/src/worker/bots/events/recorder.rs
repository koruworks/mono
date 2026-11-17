// SPDX-License-Identifier: MPL-2.0

//! A sink that keeps what it was handed, for tests of this crate and of the
//! gateway's Bot ingress. Nothing is stored, sent or journaled.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use koru_events::{AttrValue, Draft, EmitContext, EventId};

use super::BotEventSink;

/// Every row a worker published, in order.
#[derive(Default)]
pub struct BotEventRecorder(Mutex<Vec<Draft>>);

impl BotEventSink for BotEventRecorder {
    fn publish(&self, event: Draft) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
    }
}

/// One published row, as a test reads it: every attribute as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedBotEvent {
    pub name: String,
    /// The system component the row names as its actor.
    pub actor: String,
    /// An id as its uuid, a token as itself, a count as digits, a flag as
    /// `true` or `false`.
    pub attrs: BTreeMap<String, String>,
}

impl RecordedBotEvent {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }

    /// The attribute keys, in order.
    pub fn keys(&self) -> Vec<&str> {
        self.attrs.keys().map(String::as_str).collect()
    }
}

impl BotEventRecorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every row so far, oldest first. **Each is checked against its
    /// registry row as the journal would check it**, and a row that would be
    /// refused there panics here: an undeclared attribute, a token outside
    /// its vocabulary, a missing required attribute. A row may hold nothing
    /// but ids, counts, flags and declared tokens, and names no subject and
    /// no workspace.
    pub fn events(&self) -> Vec<RecordedBotEvent> {
        let drafts = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let context = EmitContext::system("recorder");
        drafts
            .into_iter()
            .map(|draft| {
                let envelope = draft.build(&context, EventId::mint(), 1);
                koru_events::validate(&envelope).unwrap_or_else(|violation| {
                    panic!("{} fails its registry row: {violation}", envelope.name)
                });
                assert!(
                    envelope.subject.is_none(),
                    "{} names a subject",
                    envelope.name
                );
                assert!(
                    envelope.scope.workspace_id.is_none() && envelope.scope.profile_id.is_none(),
                    "{} names a workspace or a profile",
                    envelope.name
                );
                let attrs = envelope
                    .attrs
                    .iter()
                    .map(|(key, value)| {
                        let text = match value {
                            AttrValue::Id(id) => id.to_string(),
                            AttrValue::Token(token) => token.clone(),
                            AttrValue::Count(count) => count.to_string(),
                            AttrValue::Flag(flag) => flag.to_string(),
                            other => panic!("{}.{key} holds {other:?}", envelope.name),
                        };
                        (key.clone(), text)
                    })
                    .collect();
                RecordedBotEvent {
                    name: envelope.name.as_str().to_owned(),
                    actor: envelope.actor.token.clone().unwrap_or_default(),
                    attrs,
                }
            })
            .collect()
    }

    /// The rows of one name, oldest first.
    pub fn named(&self, name: &str) -> Vec<RecordedBotEvent> {
        self.events()
            .into_iter()
            .filter(|event| event.name == name)
            .collect()
    }

    /// How many rows were published so far.
    pub fn count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

/// Fail when any row holds one of `content`: a Bot's name, a message, a
/// path, a credential, the name of a person or a computer. Compared without
/// regard to case against the row's name, its actor and every attribute key
/// and value.
pub fn assert_holds_none_of(events: &[RecordedBotEvent], content: &[&str]) {
    for event in events {
        let mut text = format!("{} {}", event.name, event.actor);
        for (key, value) in &event.attrs {
            text.push(' ');
            text.push_str(key);
            text.push(' ');
            text.push_str(value);
        }
        let text = text.to_lowercase();
        for forbidden in content {
            assert!(
                forbidden.is_empty() || !text.contains(&forbidden.to_lowercase()),
                "{} holds content: {forbidden:?} in {event:?}",
                event.name
            );
        }
    }
}
