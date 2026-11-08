// SPDX-License-Identifier: MPL-2.0

//! **The exact tree this window handed the platform, once, on request.**
//!
//! One sentence, and what it excludes: *this module serialises the update
//! [`super::projection::build`] just produced — ids, locators, parent and
//! order, roles, bounds, states, actions, run and selection references, and the
//! window's counters — and it never contains anything a person wrote.* It
//! excludes deciding *when*: [`super::lifecycle::PaneAccessibility`] owns the
//! request, because it owns the publish.
//!
//! ## Why this exists at all
//!
//! `guide/contributing/PRINCIPLES.md`: *"a component nobody can inspect is a component nobody
//! can trust, and by the time somebody wants to look, it is too late to add the
//! seam."* An accessibility tree is the worst case of that — it is invisible by
//! construction, the only other way to see it is to attach a Windows inspector
//! to a running process, and the question a developer actually has ("did Koru
//! publish what I think it published?") is unanswerable from a screenshot, a
//! log or a diff.
//!
//! ## The four rules, and how each is held
//!
//! - **One-shot, on request.** There is no timer, frame or poll in this file
//!   and none in its caller: a dump is armed by
//!   [`PaneAccessibility::request_dump`](super::lifecycle::PaneAccessibility::request_dump)
//!   and taken by the next publish. `a_dump_is_taken_once_by_the_next_publish_and_never_again` pins it.
//! - **Zero work when off.** [`armed`] is a `OnceLock<Option<_>>` — one atomic
//!   load — and nothing here is called unless it answered `Some`. Not one
//!   string is formatted, not one node is copied. The same shape as
//!   `KORU_FRAME_STATS`, and `a_dump_costs_nothing_when_nobody_asked_for_one` is
//!   the counter test that says so.
//! - **Redacted by default.** [`Redaction::Structure`] writes no label, no
//!   title and no run text. §7.4 of the roadmap is absolute: operational logs
//!   and diagnostics never contain document text or titles. What is written
//!   instead is a *count* of characters, because a text position is meaningless
//!   without one and a count is not text.
//! - **Content is a separate, explicit, local opt-in.**
//!   [`Redaction::Content`] needs its own variable on top of the first, is
//!   meant for synthetic fixtures, and says so in its own banner.
//!
//! ## Which developer-command surface this reuses, and why it is a flag
//!
//! The house precedent named by the roadmap is `koru-store-server projection`,
//! and its live half is `GET /v1/debug/projection`. Both work because the store
//! is a **daemon**: it has a socket, an HTTP surface and its own CLI over its
//! own files. `koru.exe` has none of the three — it is a GUI process with an
//! outbound store client, an inbound agent IPC inbox carrying `PaneRequest`s
//! (a product surface, not a developer one), and a set of start-time flags
//! (`--envs`, `--where`, `--profiles`) that answer *before* a window exists.
//! There is no running-process developer command to extend, and inventing one
//! for this would be a second inbound surface on the app for a diagnostic.
//!
//! So the request surface is the one the roadmap itself names for a new
//! diagnostic — a `koru-env` switch — plus the direct
//! [`request_dump`](super::lifecycle::PaneAccessibility::request_dump) call the
//! interactive proof uses to compare a dump against what UIA reports for the
//! same update.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use accesskit::{Action, Node, NodeId, TreeUpdate};

use super::identity::{Occurrence, WindowSlot};
use super::lifecycle::Counters;
use super::projection::Scope;

/// How much of the tree a dump is allowed to say.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Redaction {
    /// Structure only: ids, locators, parent and order, roles, bounds, states,
    /// actions, run and selection references, and counts. **The default, and
    /// the only thing routine diagnostics may produce.**
    Structure,
    /// Structure *and* every label and run value. For a synthetic fixture on
    /// the machine that is debugging it, and nothing else.
    Content,
}

impl Redaction {
    fn label(self) -> &'static str {
        match self {
            Self::Structure => "structure",
            Self::Content => "content",
        }
    }
}

/// Whether a dump is armed for this process, and how much it may say.
///
/// **One atomic load when off.** Read once: an environment variable cannot
/// change under a running process, and asking the OS on a publish would be a
/// round trip on a path that must not make one — the same reasoning
/// `accessibility::ui_lang` uses for the locale.
pub fn armed() -> Option<Redaction> {
    static ARMED: OnceLock<Option<Redaction>> = OnceLock::new();
    *ARMED.get_or_init(|| {
        if !koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DUMP) {
            // The content switch on its own does nothing. Content is *more* of
            // a dump, never a way to get one, so there is no spelling of the
            // environment that produces document text without the structural
            // switch being deliberately on as well.
            return None;
        }
        Some(
            if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DUMP_CONTENT) {
                Redaction::Content
            } else {
                Redaction::Structure
            },
        )
    })
}

/// One prepared snapshot, and everything needed to describe it.
pub struct Dump<'a> {
    pub window: WindowSlot,
    pub scope: Scope,
    pub redaction: Redaction,
    /// The update as handed to the adapter — the exact bytes the platform got.
    pub update: &'a TreeUpdate,
    /// What the ids in this update meant.
    pub occurrences: &'a HashMap<NodeId, Occurrence>,
    /// Every occurrence the client is holding after this update, which is what
    /// a later action will be resolved against.
    pub known: &'a HashMap<NodeId, Occurrence>,
    pub retired: &'a [NodeId],
    pub counters: Counters,
}

/// The dump, as text.
///
/// **A `String` rather than a `println!`**, for two reasons: a test can read it
/// without a console, and the interactive proof can hand the same text to a
/// UI Automation client and check that the two agree about the update they are
/// both describing. Its caller writes it to **stderr** — not to a file, and for
/// the reason `lifecycle::note` gives: a diagnostic that opens a file is a
/// diagnostic that can become the stall being diagnosed. A person who wants it
/// in a file redirects one.
pub fn render(dump: &Dump<'_>) -> String {
    let mut out = String::with_capacity(256 + dump.update.nodes.len() * 160);
    let _ = writeln!(
        out,
        "[koru:a11y] dump window=w{} scope={} redaction={} nodes={} retired={} map={} focus=#{}",
        dump.window.raw(),
        match dump.scope {
            Scope::Whole => "whole",
            Scope::Delta => "delta",
        },
        dump.redaction.label(),
        dump.update.nodes.len(),
        dump.retired.len(),
        dump.known.len(),
        dump.update.focus.0,
    );
    if dump.redaction == Redaction::Content {
        let _ = writeln!(
            out,
            "[koru:a11y] dump CONTENT IS INCLUDED — labels and document text are in the lines \
             below. Synthetic fixtures only; this must not reach a bug report."
        );
    }

    // Parent and order are not properties of a node — they are a consequence of
    // somebody else's `children` list — so they are derived from this update
    // rather than guessed. A delta's parent may be absent, which is correct and
    // is said as `parent=-`.
    let mut parents: HashMap<NodeId, (NodeId, usize)> = HashMap::new();
    for (id, node) in &dump.update.nodes {
        for (order, child) in node.children().iter().enumerate() {
            parents.insert(*child, (*id, order));
        }
    }

    for (id, node) in &dump.update.nodes {
        let _ = writeln!(out, "[koru:a11y]   {}", one(dump, &parents, *id, node));
    }
    for id in dump.retired {
        let _ = writeln!(out, "[koru:a11y]   retired #{}", id.0);
    }

    let counters = dump.counters;
    let _ = writeln!(
        out,
        "[koru:a11y] dump counters rebuilds={} activations={} deltas={} nodes_published={} \
         actions_routed={} actions_refused={} actions_dropped={} duplicate_ids={} \
         unresolved_focus={} dumps={}",
        counters.rebuilds,
        counters.activations,
        counters.deltas,
        counters.nodes_published,
        counters.actions_routed,
        counters.actions_refused,
        counters.actions_dropped,
        counters.duplicate_ids,
        counters.unresolved_focus,
        counters.dumps,
    );
    out
}

/// Every `accesskit::Action` in the pinned version, because there is no way to
/// ask a node which ones it carries.
///
/// `Node::supports_action` is the only query, `Action::n` is private, and the
/// mask is not exposed — so a dump that wanted to list what a node advertises
/// has to name the variants. Listing them all rather than only the ones Koru
/// uses is deliberate: a dump that quietly omitted an advertised action would
/// be the one place a person looks when an AT is offering something nobody
/// meant to offer. `accesskit_actions_are_all_listed` fails if the crate grows
/// one.
const EVERY_ACTION: [Action; 22] = [
    Action::Click,
    Action::Focus,
    Action::Blur,
    Action::Collapse,
    Action::Expand,
    Action::CustomAction,
    Action::Decrement,
    Action::Increment,
    Action::HideTooltip,
    Action::ShowTooltip,
    Action::ReplaceSelectedText,
    Action::ScrollDown,
    Action::ScrollLeft,
    Action::ScrollRight,
    Action::ScrollUp,
    Action::ScrollIntoView,
    Action::ScrollToPoint,
    Action::SetScrollOffset,
    Action::SetTextSelection,
    Action::SetSequentialFocusNavigationStartingPoint,
    Action::SetValue,
    Action::ShowContextMenu,
];

fn one(
    dump: &Dump<'_>,
    parents: &HashMap<NodeId, (NodeId, usize)>,
    id: NodeId,
    node: &Node,
) -> String {
    let mut line = String::with_capacity(160);
    let locator = dump
        .occurrences
        .get(&id)
        .map(Occurrence::locator)
        // A text run is not a control and has no occurrence: the platform's
        // consumer filters `Role::TextRun` out of the control view, and a
        // client meets one only inside a text position. Saying so is more
        // useful than an empty field.
        .unwrap_or_else(|| "(not a control)".to_string());
    let _ = write!(line, "#{} {} role={:?}", id.0, locator, node.role());
    match parents.get(&id) {
        Some((parent, order)) => {
            let _ = write!(line, " parent=#{} order={order}", parent.0);
        }
        None => line.push_str(" parent=-"),
    }
    match node.bounds() {
        Some(rect) => {
            let _ = write!(
                line,
                " bounds={:.1},{:.1},{:.1}x{:.1}",
                rect.x0,
                rect.y0,
                rect.x1 - rect.x0,
                rect.y1 - rect.y0
            );
        }
        // Legal, and load-bearing here: an off-screen paragraph publishes its
        // text without a rectangle, which is what stops a scroll from
        // republishing the whole document.
        None => line.push_str(" bounds=-"),
    }
    let mut states = Vec::new();
    if node.is_disabled() {
        states.push("disabled");
    }
    if node.is_read_only() {
        states.push("read-only");
    }
    if node.is_hidden() {
        states.push("hidden");
    }
    // A checkbox's own state. Structural, not content: it says whether a box is
    // ticked and never what the row it belongs to says.
    match node.toggled() {
        Some(accesskit::Toggled::True) => states.push("checked"),
        Some(accesskit::Toggled::False) => states.push("unchecked"),
        Some(accesskit::Toggled::Mixed) => states.push("mixed"),
        None => {}
    }
    let _ = write!(line, " states=[{}]", states.join(","));

    let actions: Vec<String> = EVERY_ACTION
        .iter()
        .filter(|action| node.supports_action(**action))
        .map(|action| format!("{action:?}"))
        .collect();
    let _ = write!(line, " actions=[{}]", actions.join(","));

    let children = node.children();
    if !children.is_empty() {
        let _ = write!(line, " children={}", children.len());
    }
    if let Some(selection) = node.text_selection() {
        let _ = write!(
            line,
            " selection=#{}[{}]..#{}[{}]",
            selection.anchor.node.0,
            selection.anchor.character_index,
            selection.focus.node.0,
            selection.focus.character_index
        );
    }
    if let Some(value) = node.value() {
        // **A character count, never the characters.** A text position is
        // meaningless without knowing how many there are, and a count is
        // structure rather than content — but the string itself is what a
        // person wrote, and §7.4 forbids it here by default.
        if !node.character_lengths().is_empty() {
            let _ = write!(line, " chars={}", node.character_lengths().len());
        }
        let _ = write!(line, " bytes={}", value.len());
        if dump.redaction == Redaction::Content {
            let _ = write!(line, " value={value:?}");
        } else {
            line.push_str(" value=<redacted>");
        }
    }
    if let Some(label) = node.label() {
        if dump.redaction == Redaction::Content {
            let _ = write!(line, " label={label:?}");
        } else {
            let _ = write!(line, " label=<redacted:{}>", label.chars().count());
        }
    }
    line
}

#[cfg(test)]
mod tests;
