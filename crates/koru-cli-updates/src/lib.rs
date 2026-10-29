// SPDX-License-Identifier: MPL-2.0

//! **Keeping the coding-agent CLIs on this machine up to date.**
//!
//! Koru launches other people's command-line agents — `claude`, `codex`,
//! `grok` and the rest of the terminal's `+` menu — and each of them ships
//! often and updates itself through a different channel: a native installer
//! with its own `update` verb, an npm package, or both. A person who has four
//! of them installed finds out one is stale when a session misbehaves. This
//! crate finds out for them: which of the known CLIs are installed, what
//! version each is, what its channel offers, and — when they say so — runs the
//! CLI's own updater and reports what it did.
//!
//! ## What it is, and what it is not
//!
//! - **It only knows the CLIs in [`catalogue::KNOWN`]**, each with a verified
//!   way to read the latest version and a verified way to update. A CLI with
//!   no published latest version is not listed, because a card that says
//!   "update available" has to be able to say to what.
//! - **It never installs anything by itself.** The update is the CLI's own
//!   `update` command where the CLI has one, and `npm install -g` where the
//!   package is only on npm. Koru runs the command a person would have typed
//!   and shows what came out of it.
//! - **It is not Koru's own updater.** `koru-update` replaces this
//!   installation and has consent steps, signatures and a swap helper; this
//!   runs a third party's updater on a third party's install.
//! - **A newer CLI is not a CLI Koru's managed routes accept.** Those routes
//!   pin exact versions, and a version past the pin runs raw. The update card
//!   says what the channel offers; the readiness rows say what Koru admits.
//!
//! ## Shape
//!
//! The same split `koru-update` uses: [`Controller`] owns the state and the
//! worker threads, every check and every update runs on a thread of its own
//! and reports back over a channel, and [`Controller::tick`] drains that
//! channel on the owner's loop. Nothing here blocks the thread that calls it.
//! The decisions — what a row says given a status, an activity and a
//! dismissal — are the pure functions in [`report`], and those are what the
//! tests exercise.

pub mod catalogue;
pub mod controller;
pub mod latest;
pub mod report;
pub mod run;
pub mod state;
pub mod version;

pub use catalogue::{KnownCli, LatestSource, UpdateCommand};
pub use controller::{ChoiceError, Controller};
pub use report::{CliReport, CliStage, Report};
pub use version::Version;

#[cfg(test)]
mod tests;
