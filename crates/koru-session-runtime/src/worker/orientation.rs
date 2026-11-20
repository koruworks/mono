// SPDX-License-Identifier: MPL-2.0

//! An immutable manual-session snapshot, owned by the first real input.
//! Caller fingerprints remain raw; only the private provider carrier changes.

use koru_delegate_preamble::{Notice, Resolved};
use koru_session_protocol::{ErrorCode, IdempotencyKey};

const MODE: &str = "Session mode: a person is working directly with a managed agent in Koru. Follow the person's request and the agent's configured instructions.";
const FACTS: &str = "Koru has not injected its desktop tool bridge into this manual session. The agent may have user-configured tools or MCP connections; use only the capabilities actually offered. Do not invent Koru tools or claim an app operation was performed without a confirmed result.";
const REQUEST: &str = "\n\nPerson's request:\n";

#[derive(Default)]
pub(super) struct Orientation {
    source: Option<Resolved>,
    carrier: Option<Carrier>,
}

struct Carrier {
    key: IdempotencyKey,
    text: String,
    may_have_sent: bool,
}

impl Orientation {
    pub(super) fn owner(&self) -> Option<IdempotencyKey> {
        self.carrier.as_ref().map(|carrier| carrier.key)
    }
    pub(super) fn new(source: Option<Resolved>) -> Self {
        Self {
            source,
            carrier: None,
        }
    }

    /// No allocation or ownership change. Strict Begin must run this before
    /// inserting an initial turn that deliberately survives ambiguous failure.
    pub(super) fn preflight(&self, key: IdempotencyKey, raw: &str) -> Result<(), ErrorCode> {
        let Some(source) = &self.source else {
            return Ok(());
        };
        if let Some(carrier) = &self.carrier {
            return if carrier.key == key {
                Ok(())
            } else {
                Err(ErrorCode::Backpressure)
            };
        }
        // compose adds two newlines before each nonempty mode/facts block.
        let bytes = source
            .text
            .len()
            .saturating_add(4)
            .saturating_add(MODE.len())
            .saturating_add(FACTS.len())
            .saturating_add(REQUEST.len())
            .saturating_add(raw.len());
        if bytes > crate::MAX_QUEUED_WRITE_BYTES {
            return Err(ErrorCode::InputTooLarge);
        }
        Ok(())
    }

    pub(super) fn reserve(&mut self, key: IdempotencyKey, raw: &str) -> Result<String, ErrorCode> {
        self.preflight(key, raw)?;
        let Some(source) = &self.source else {
            return Ok(raw.to_owned());
        };
        if let Some(carrier) = &self.carrier {
            return Ok(carrier.text.clone());
        }
        let instructions = koru_delegate_preamble::session::compose(&source.text, MODE, FACTS);
        let text = format!("{instructions}{REQUEST}{raw}");
        self.carrier = Some(Carrier {
            key,
            text: text.clone(),
            may_have_sent: false,
        });
        Ok(text)
    }

    /// `known_unsent` is attempt-local. A prior ambiguous dispatch can never
    /// be released by a later local queue/lease refusal.
    pub(super) fn failed(&mut self, key: IdempotencyKey, known_unsent: bool) {
        if let Some(carrier) = &mut self.carrier {
            if carrier.key != key {
                return;
            }
            carrier.may_have_sent |= !known_unsent;
            if !carrier.may_have_sent {
                self.carrier = None;
            }
        }
    }

    pub(super) fn accepted(&mut self, key: IdempotencyKey) -> Option<Notice> {
        if self
            .carrier
            .as_ref()
            .is_none_or(|carrier| carrier.key != key)
        {
            return None;
        }
        self.carrier = None;
        self.source.take().and_then(|source| source.notice)
    }
}

#[cfg(test)]
mod tests;
