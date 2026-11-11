// SPDX-License-Identifier: MPL-2.0

//! **The agent-configuration control** (Bots P1.8, QUESTIONS O39, O42): it
//! draws and drives the choice of how a local agent runs — which agent, which
//! model, what effort, Fast, the context window and the approval mode — from
//! plain data its host hands it, and hands back what the person chose. It
//! never reads a catalog, calls a service, stores a preference, or knows what
//! a chat or a Bot is.
//!
//! **What it refuses**: the composer's chip and split send button that open
//! it, where favorites and the last choice are stored, readiness probing,
//! a host's own focus model, and any catalog fetch. Those are the host's,
//! and the host is the AI chat (`koru-ai-ui`) or a Bot's settings.
//!
//! **What it links**: the design system (`koru-draw`, `koru-ui`), the
//! strings (`koru-i18n`) and the two platform libraries (`vello`, `winit`);
//! nothing of the AI stack, the API client or the Store. The fence is
//! `tests/dependency.rs`. The design system brings its own reach with it —
//! `koru-draw` links the font and brand settings, `koru-ui` the clipboard and
//! the key registry — and that is the whole of what a host takes on.
//!
//! The plan this is built to is `docs/features/bots/P1.8.PICKER.notes.md`.

#[cfg(test)]
mod tests;
