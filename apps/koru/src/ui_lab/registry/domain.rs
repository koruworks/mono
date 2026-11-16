// SPDX-License-Identifier: MPL-2.0

//! **The domain crates' records**: the surfaces, controls, painters and
//! support code of `koru-ai-ui`, `koru-search-ui`, `koru-shell-chrome` and
//! the other domain UI crates, a file per crate. Each record names the group
//! it is shown under (most are compositions) and the [`Kind`] it honestly is.
//! A crate's modules that draw nothing — controllers, readers, gateways, data
//! books — have no record; the module baselines in `tests/drift.rs` and
//! `MANIFEST.md` say why each was left out, and which surfaces need services
//! to be built.

use super::*;
use koru_i18n::Key;

mod agents;
mod ai;
mod app_shell;
mod appearance;
mod artifacts;
mod canvas;
mod chats;
mod markdown_view;
mod runs;
mod search;
mod session_view;
mod settings;
mod shell_chrome;
mod socials;
mod voice;

pub(super) const AGENTS: &[Entry] = agents::RECORDS;
pub(super) const AI: &[Entry] = ai::RECORDS;
pub(super) const APP_SHELL: &[Entry] = app_shell::RECORDS;
pub(super) const APPEARANCE: &[Entry] = appearance::RECORDS;
pub(super) const ARTIFACTS: &[Entry] = artifacts::RECORDS;
pub(super) const CANVAS: &[Entry] = canvas::RECORDS;
pub(super) const CHATS: &[Entry] = chats::RECORDS;
pub(super) const MARKDOWN_VIEW: &[Entry] = markdown_view::RECORDS;
pub(super) const RUNS: &[Entry] = runs::RECORDS;
pub(super) const SEARCH: &[Entry] = search::RECORDS;
pub(super) const SESSION_VIEW: &[Entry] = session_view::RECORDS;
pub(super) const SETTINGS: &[Entry] = settings::RECORDS;
pub(super) const SHELL_CHROME: &[Entry] = shell_chrome::RECORDS;
pub(super) const SOCIALS: &[Entry] = socials::RECORDS;
pub(super) const VOICE: &[Entry] = voice::RECORDS;

const fn source(owner: &'static str, module: &'static str, file: &'static str) -> Source {
    Source {
        owner,
        module,
        file,
    }
}

#[allow(clippy::too_many_arguments)]
const fn record(
    catalog_id: &'static str,
    public_name: &'static str,
    name: Key,
    purpose: Key,
    family: Family,
    kind: Kind,
    catalog_family: Option<&'static str>,
    identities: &'static [&'static str],
    source: Source,
    platforms: [(Platform, Availability); 5],
    apis: &'static [Api],
) -> Entry {
    Entry {
        catalog_id,
        public_name,
        name,
        purpose,
        family,
        kind,
        catalog_family,
        identities,
        source,
        platforms,
        recipes: &[],
        apis,
    }
}

/// A symbol that paints and has no story yet.
const fn painter(symbol: &'static str) -> Api {
    plain(symbol, Story::NoStoryYet)
}

/// A painting symbol whose production values are listed.
const fn painter_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::NoStoryYet)
}

/// Geometry, input, outcomes, state or constants beside a surface.
const fn support(symbol: &'static str) -> Api {
    plain(symbol, Story::Nonvisual)
}

/// Nonvisual, with the production values it carries.
const fn support_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::Nonvisual)
}

/// Content a surface draws as its own — canvas objects, a document — shown
/// through that surface rather than as a gallery story.
const fn boundary(symbol: &'static str) -> Api {
    plain(symbol, Story::Boundary)
}

/// Content, with the kinds of it the source declares.
const fn boundary_with(symbol: &'static str, options: &'static [OptionSet]) -> Api {
    plain_with(symbol, options, Story::Boundary)
}

/// Nonvisual code the A2UI host uses to serve a fixed-catalog component; a
/// declaration never names it.
const fn host_only(symbol: &'static str) -> Api {
    api(symbol, &[], A2ui::HostOnly, Story::Nonvisual)
}
