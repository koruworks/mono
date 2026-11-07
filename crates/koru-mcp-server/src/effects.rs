// SPDX-License-Identifier: MPL-2.0

//! What a mutating call answers when its writes did not all land.
//!
//! [`contracts/hosted-tools-v1.md`] §1–§2: **an effect is one submission** —
//! one persist of one unit — and a mutating call makes its effects in a fixed
//! order, stopping at the first that does not end [`EffectState::Saved`].
//! Every check runs before the first effect, so a call refused by one has no
//! effect at all and answers with the refusal's own code. Once an effect has
//! landed, a later one that does not is reported per effect:
//!
//! | Effects | Answer |
//! | --- | --- |
//! | all `saved` | success, as the tool answers today |
//! | all `not_saved` | the refusal's own code, or [`EDIT_NOT_SAVED`] |
//! | a `saved` prefix, the rest `not_saved` | [`PARTIALLY_SAVED`] |
//! | any `unknown` | [`SAVE_STATUS_UNKNOWN`] |
//!
//! **The same on every host** (QUESTIONS Q5, item 3). A local host's Store
//! answers every persist, so locally an effect is `saved` or `not_saved`; a
//! local extraction whose source write fails after the destination landed
//! answers `partially_saved` with its recovery fields, where it used to answer
//! the Store's error. The hosted server's port classifies Cloud's answers into
//! the same three states, and the tool layer builds the same envelope.
//!
//! **How a Store port reports an effect.** A port call that returns `Ok` saved
//! everything it was asked to. One that returns an error refused it: the
//! effect is `not_saved` unless the error's code is [`SAVE_STATUS_UNKNOWN`]
//! (submitted, then unconfirmed) — the convention a hosted port answers a
//! timeout or an unclassified refusal with. A port call covering two effects
//! (`seed_document`: the document and its manifest entry; `create_document`:
//! an extraction's empty destination and its entry) says how each ended with
//! [`ToolError::with_effects`], naming its two units `document` and
//! `manifest`; without that, each takes the error's state.
//!
//! [`contracts/hosted-tools-v1.md`]: ../../../contracts/hosted-tools-v1.md

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::ToolError;

/// Nothing was saved, and a retry is safe once the cause is gone. For a
/// refusal that has no code of its own — the hosted server's generation fence,
/// a unit Cloud already holds.
pub const EDIT_NOT_SAVED: &str = "edit_not_saved";
/// A prefix of the call's effects landed and the rest did not.
pub const PARTIALLY_SAVED: &str = "partially_saved";
/// At least one effect was submitted and never confirmed.
pub const SAVE_STATUS_UNKNOWN: &str = "save_status_unknown";

/// [`SAVE_STATUS_UNKNOWN`]'s message, exactly as the contract writes it.
pub const SAVE_STATUS_UNKNOWN_MESSAGE: &str = "Koru could not confirm whether this change was saved. Read the documents listed before retrying; if your change is there, do not repeat it. `create_document` with the same `idempotency_key` is always safe to retry.";

/// One submission's name, in [`contracts/hosted-tools-v1.md`]'s table.
///
/// [`contracts/hosted-tools-v1.md`]: ../../../contracts/hosted-tools-v1.md
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// The workspace system document: the tags an edit creates.
    System,
    /// The edited or created document.
    Document,
    /// The workspace manifest: a created document's entry.
    Manifest,
    /// An extraction's new, empty destination.
    DestinationSeed,
    /// An extraction's destination, with the moved content.
    Destination,
    /// An extraction's source, with the content replaced by a link.
    Source,
}

/// How one effect ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    /// Durably acknowledged.
    Saved,
    /// Proven to have no effect: refused before submission, or refused with a
    /// code defined to leave the unit unchanged.
    NotSaved,
    /// Submitted, then not confirmed.
    Unknown,
}

impl EffectState {
    /// The state a port's refusal of a submission leaves an effect in.
    ///
    /// **`durability_timeout` is `unknown`, on a local host too.** The desktop
    /// bridge waits ten seconds for the Store to say an edit is on disk; when
    /// it does not, the edit may still land, so calling it `not_saved` would
    /// claim a proof nobody has (QUESTIONS Q2.1-2).
    pub fn of_refusal(refusal: &ToolError) -> Self {
        match refusal.code.as_str() {
            SAVE_STATUS_UNKNOWN | "durability_timeout" => Self::Unknown,
            _ => Self::NotSaved,
        }
    }
}

/// One effect as a mutating call reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub effect: EffectKind,
    /// The unit's document — the workspace's system document or manifest for
    /// [`EffectKind::System`] and [`EffectKind::Manifest`]. `null` only for a
    /// unit no id was ever minted for: a creation refused before the Store
    /// named it.
    pub document_id: Option<String>,
    pub state: EffectState,
}

impl ToolError {
    /// Say how each effect of one port call ended, for a call that covered
    /// more than one — the convention a Store port answers a partial
    /// `seed_document` or `create_document` with.
    pub fn with_effects(mut self, effects: &[Effect]) -> Self {
        let mut details = match self.details.take() {
            Some(Value::Object(details)) => details,
            Some(other) => Map::from_iter([("cause".to_owned(), other)]),
            None => Map::new(),
        };
        details.insert("effects".into(), json!(effects));
        self.details = Some(Value::Object(details));
        self
    }

    /// The per-effect report a port attached, if it attached one.
    fn reported_effects(&self) -> Option<Vec<Effect>> {
        serde_json::from_value(self.details.as_ref()?.get("effects")?.clone()).ok()
    }
}

/// A tool's recovery advice for a saved prefix: [`PARTIALLY_SAVED`]'s message,
/// and the fields its details add.
pub struct Recovery {
    pub message: String,
    pub details: Map<String, Value>,
}

impl Recovery {
    pub fn sentence(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: Map::new(),
        }
    }
}

/// The answer for a call whose effects did not all end `saved`, by the
/// contract's table. `refusal` is what stopped the sequence; `extra` is added
/// to every envelope (a hosted creation's `idempotency_key`).
pub fn outcome(
    operation_id: Uuid,
    effects: &[Effect],
    refusal: ToolError,
    recovery: impl FnOnce(&[Effect]) -> Recovery,
    extra: &Map<String, Value>,
) -> ToolError {
    let envelope = |mut details: Map<String, Value>| {
        details.insert("operation_id".into(), json!(operation_id.to_string()));
        details.insert("effects".into(), json!(effects));
        details.extend(extra.clone());
        Some(Value::Object(details))
    };
    let state = |state| effects.iter().any(|effect| effect.state == state);
    if state(EffectState::Unknown) {
        return ToolError {
            code: SAVE_STATUS_UNKNOWN.into(),
            message: SAVE_STATUS_UNKNOWN_MESSAGE.into(),
            details: envelope(Map::new()),
        };
    }
    if !state(EffectState::Saved) {
        if refusal.code != EDIT_NOT_SAVED {
            // A refusal with a code of its own keeps it, and its details.
            return refusal;
        }
        let own = match refusal.details {
            Some(Value::Object(details)) => details,
            _ => Map::new(),
        };
        return ToolError {
            details: envelope(own),
            ..refusal
        };
    }
    let Recovery { message, details } = recovery(effects);
    ToolError {
        code: PARTIALLY_SAVED.into(),
        message,
        details: envelope(details),
    }
}

/// One mutating call's effects, in submission order, as they end.
pub(crate) struct Ledger {
    operation: Uuid,
    effects: Vec<Effect>,
    extra: Map<String, Value>,
}

impl Ledger {
    pub(crate) fn new(operation: Uuid) -> Self {
        Self {
            operation,
            effects: Vec::new(),
            extra: Map::new(),
        }
    }

    /// The next effect the call means to make. Planned effects that are never
    /// submitted are reported `not_saved`.
    pub(crate) fn plan(&mut self, effect: EffectKind, document_id: Option<String>) {
        self.effects.push(Effect {
            effect,
            document_id,
            state: EffectState::NotSaved,
        });
    }

    /// An effect the call planned and then had nothing to submit for.
    pub(crate) fn drop_plan(&mut self, effect: EffectKind) {
        self.effects.retain(|planned| planned.effect != effect);
    }

    /// Name the unit an effect writes, once the call knows it.
    pub(crate) fn name(&mut self, effect: EffectKind, document_id: String) {
        if let Some(planned) = self.find(effect) {
            planned.document_id = Some(document_id);
        }
    }

    pub(crate) fn saved(&mut self, effect: EffectKind) {
        if let Some(planned) = self.find(effect) {
            planned.state = EffectState::Saved;
        }
    }

    /// Added to every envelope this call answers.
    pub(crate) fn always(&mut self, key: &str, value: Value) {
        self.extra.insert(key.into(), value);
    }

    pub(crate) fn plans(&self, effect: EffectKind) -> bool {
        self.effects.iter().any(|planned| planned.effect == effect)
    }

    pub(crate) fn unnamed(&self, effect: EffectKind) -> bool {
        self.effects
            .iter()
            .any(|planned| planned.effect == effect && planned.document_id.is_none())
    }

    /// `effects`, submitted in one port call, did not all land: each takes the
    /// state the port reported for it, or the refusal's. Everything after
    /// them was never submitted.
    ///
    /// A port reports the two units of a two-effect call as `document` and
    /// `manifest`, because it cannot know what the tool calls them: the
    /// `document` report is the content unit, whichever effect the tool
    /// planned for it — `document` for a creation, `destination_seed` for an
    /// extraction's new page. A one-effect call takes the refusal's state.
    pub(crate) fn failed(
        &mut self,
        effects: &[EffectKind],
        refusal: ToolError,
        recovery: impl FnOnce(&[Effect]) -> Recovery,
    ) -> ToolError {
        let fallback = EffectState::of_refusal(&refusal);
        let reported = match effects.len() {
            1 => Vec::new(),
            _ => refusal.reported_effects().unwrap_or_default(),
        };
        for kind in effects {
            let unit = match kind {
                EffectKind::Manifest => EffectKind::Manifest,
                _ => EffectKind::Document,
            };
            let report = reported.iter().find(|effect| effect.effect == unit);
            if let Some(planned) = self.find(*kind) {
                planned.state = report.map_or(fallback, |report| report.state);
                if planned.document_id.is_none() {
                    planned.document_id = report.and_then(|report| report.document_id.clone());
                }
            }
        }
        self.answer(refusal, recovery)
    }

    /// The call stopped before submitting its next effect: that one, and
    /// everything after it, are `not_saved`.
    pub(crate) fn refused(
        &self,
        refusal: ToolError,
        recovery: impl FnOnce(&[Effect]) -> Recovery,
    ) -> ToolError {
        self.answer(refusal, recovery)
    }

    fn answer(
        &self,
        refusal: ToolError,
        recovery: impl FnOnce(&[Effect]) -> Recovery,
    ) -> ToolError {
        outcome(
            self.operation,
            &self.effects,
            refusal,
            recovery,
            &self.extra,
        )
    }

    fn find(&mut self, effect: EffectKind) -> Option<&mut Effect> {
        self.effects
            .iter_mut()
            .find(|planned| planned.effect == effect)
    }
}

#[cfg(test)]
mod tests;
