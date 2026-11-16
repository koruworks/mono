// SPDX-License-Identifier: MPL-2.0

//! **The apps' own records**: the controls, painters and surfaces an
//! application draws for itself — the workspace app's mini app, Tasks tab,
//! container, side columns and pages, the document app's window — and the
//! app's implementations of the host contracts the domain crates define. A
//! file per area, written from the source each record names.
//!
//! **Every path is a string literal on purpose.** These records describe the
//! crate the Lab itself is part of, and the Lab must stay isolated from that
//! crate's startup code: nothing here `use`s or calls an app module. Each file
//! is on `ui_lab/tests.rs`'s `RECORD_FILES` list, so its literals may spell
//! the path they record while its code is still read whole.
//!
//! **App symbols are written as the crate's own paths**, `koru::` standing
//! for the `crate::` a caller inside the workspace app writes
//! (`koru::mini_app::MiniApp`), and `koru_document::` / `koru_agent::` for
//! the other app crates. Most are `pub(crate)` or private: the record names
//! where an item is declared, not a promise that anything outside the app can
//! reach it. A module the app keeps private (`mini_app::chrome`) is spelled
//! through it.
//!
//! The modules of each app that draw nothing — routing, workers, persistence,
//! data — have no record; the module baselines in `tests/drift/app.rs` and
//! `MANIFEST.md` say why each was left out.

use super::domain::{boundary, host_only, painter, record, source, support, support_with};
use super::*;
use koru_i18n::Key;

mod companion_apps;
mod leaf;
mod mainview;
mod mini_app;
mod pages;
mod panes;
mod tasks_view;

pub(super) const COMPANION_APPS: &[Entry] = companion_apps::RECORDS;
pub(super) const LEAF: &[Entry] = leaf::RECORDS;
pub(super) const MAINVIEW: &[Entry] = mainview::RECORDS;
pub(super) const MINI_APP: &[Entry] = mini_app::RECORDS;
pub(super) const PAGES: &[Entry] = pages::RECORDS;
pub(super) const PANES: &[Entry] = panes::RECORDS;
pub(super) const TASKS_VIEW: &[Entry] = tasks_view::RECORDS;

/// The workspace app's owner name, as its manifest spells it.
const KORU: &str = "koru";

/// Where the fixed and composition catalogs' native host runs. PLATFORMS.md
/// ("Task/document catalog and chat host", "A2UI element composition",
/// "Mini-app pin group"): the desktop host is shared Rust, verified on
/// Windows only; Android and iOS have no generated-host integration and are
/// unavailable for these catalogs.
const A2UI_DESKTOP: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::Existing),
    (Platform::MacOs, Availability::SharedSourceUnverified),
    (Platform::Linux, Availability::SharedSourceUnverified),
    (Platform::Android, Availability::Unavailable),
    (Platform::Ios, Availability::Unavailable),
];

/// The symbol a declaration's catalog component becomes in the host. The
/// story is owed: the composition Lab (L5a) is what will run it.
const fn component(symbol: &'static str, name: &'static str) -> Api {
    api(symbol, &[], A2ui::Component(name), Story::NoStoryYet)
}

/// A painter the host's adapter draws a declaration with; a declaration
/// never names it.
const fn host_painter(symbol: &'static str) -> Api {
    api(symbol, &[], A2ui::HostOnly, Story::NoStoryYet)
}
