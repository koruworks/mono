// SPDX-License-Identifier: MPL-2.0

//! **One window's accessibility state, from before it is shown until after it
//! is gone.**
//!
//! `koru-window` owns the adapter's lifetime against a winit window; this
//! owns what Koru does with it — when a tree is worth building, what the ids
//! in it mean, and which of the requests arriving from a screen reader are
//! still about something that exists.
//!
//! ## Nothing is built while nobody is listening
//!
//! Two gates, in this order:
//!
//! 1. **Is a client attached?** A client attaching is the *only* thing that
//!    asks for a first tree — on Windows, a real `WM_GETOBJECT` on the window.
//!    Until one arrives, [`poll`](PaneAccessibility::poll) never says a
//!    snapshot is wanted, so no window title is read, no view is consulted and
//!    no tree is built. What an ordinary session pays is one drained mailbox
//!    per window per pass of the loop: a mutex taken and released.
//! 2. **Has anything changed?** [`invalidate`](PaneAccessibility::invalidate)
//!    is called by the handful of things this plan's tree actually depends on.
//!    A caret blink, a frame, a timer and a UIA property query are none of
//!    them, so they do not reach [`Counters::rebuilds`] — which is the number
//!    that says so, rather than a stopwatch that passes on a fast machine.
//!
//! ## A request for a window that is gone is refused
//!
//! Not routed to a survivor, and not silently dropped either. A slot is handed
//! out once and never reused; every published node id is remembered with the
//! occurrence it stood for; and an action naming an id that is not in the tree
//! *currently on screen* is counted as refused. That is one mechanism covering
//! three cases the roadmap names separately — a closed window, a closed menu
//! whose view slot will never come again, and a client replaying an old id.

use std::collections::HashMap;

use accesskit::{Action, ActionData, ActionRequest, NodeId, TreeId};
use koru_window::AccessibilityProvider as Provider;

use super::dump::{self, Dump, Redaction};
use super::identity::{Occurrence, ViewSlot, WindowSlot};
use super::projection::{Projection, Scope, WindowSemantics, build};

/// Where a window is in its life. The order is the invariant: a provider is
/// installed while the window is still hidden, and nothing is answered after
/// it is retired.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Created hidden. The provider, if the platform gave us one, exists.
    Hidden,
    /// On screen at least once.
    Revealed,
    /// Gone. Every request is refused from here on.
    Retired,
}

/// What this window has done, for somebody who wants to look.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Semantic trees actually built. The one that must not grow with frames.
    pub rebuilds: u64,
    /// Trees published as a coherent full snapshot because a client attached.
    pub activations: u64,
    pub actions_routed: u64,
    /// Actions that named nothing in the tree on screen — a closed menu, a
    /// closed window, a replay.
    pub actions_refused: u64,
    /// Actions the mailbox dropped because a client queued faster than the
    /// loop drained.
    pub actions_dropped: u64,
    /// Trees published as a difference rather than a whole window. With
    /// [`activations`](Self::activations), this is the ratio §5 asks for:
    /// activation may be O(document), and everything after it may not.
    pub deltas: u64,
    /// Node ids published, summed over every update. The delta's size, which is
    /// the number that must stay proportional to what changed rather than to
    /// the document.
    pub nodes_published: u64,
    /// Snapshot dumps written. One per request, and never more.
    pub dumps: u64,
    /// Node ids that collided or were published twice. Always a bug.
    pub duplicate_ids: u64,
    /// A published focus named a control that was not in the tree.
    pub unresolved_focus: u64,
}

/// One action, against something that still exists.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub occurrence: Occurrence,
    pub action: Action,
    pub data: Option<ActionData>,
}

/// What a drain concluded.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    /// A snapshot is worth taking: a client attached, or the window changed
    /// while one was listening.
    pub snapshot: bool,
    /// The snapshot owed is a **whole** one, not a difference: a client has
    /// just attached, or one detached and the tree it held is gone.
    ///
    /// The caller has to know this *before* it prepares the semantics, because
    /// preparing a delta is what makes an ordinary edit cheap and preparing a
    /// whole window is what makes an activation correct. Guessing it from
    /// [`Counters::activations`] after the fact would be a race with the
    /// adapter, which is the authority on when a client is really there.
    pub full: bool,
}

/// What one build produced, minus the update the adapter took.
struct Landed {
    /// A copy of the update, kept only when a dump was armed.
    copy: Option<accesskit::TreeUpdate>,
    occurrences: HashMap<NodeId, Occurrence>,
    nodes: usize,
    duplicate_ids: usize,
    unresolved_focus: bool,
    retired: Vec<NodeId>,
}

pub struct PaneAccessibility {
    slot: WindowSlot,
    view: ViewSlot,
    bridge: Option<Box<dyn Provider>>,
    stage: Stage,
    /// Set the first time a tree was actually built, which is the first time a
    /// client asked for one.
    active: bool,
    /// A client has asked for a whole tree and has not been given one yet.
    owes_full: bool,
    revision: u64,
    /// The view's own revision as of the last [`observe`](PaneAccessibility::observe).
    /// `None` until it has been asked once, so the first answer is not mistaken
    /// for a change.
    seen_view_revision: Option<u64>,
    /// The revision the last published tree was built from, or `None` when no
    /// tree is currently published.
    published: Option<u64>,
    /// **Every node id this window has published, and what it stood for.**
    ///
    /// Accumulated, not replaced. Before the tree became a delta, the map that
    /// resolved an action was the map of the *last update* — which was every
    /// node, so nothing noticed. The moment an update carries only what
    /// changed, that map is a handful of nodes, and a client acting on a
    /// control that did not change is refused while `actions_refused` climbs
    /// and everything looks healthy. Merged on publish, emptied on a whole
    /// snapshot (which is authoritative on its own), and pruned by
    /// [`WindowSemantics::retired`].
    occurrences: HashMap<NodeId, Occurrence>,
    actions: Vec<ActionRequest>,
    /// A one-shot snapshot dump has been asked for, and how much of it to say.
    /// Taken by the next publish and never re-armed by one.
    dump: Option<Redaction>,
    /// The text of the last dump, for a caller that wants to compare it with
    /// what a client sees rather than read it off a console. `None` whenever no
    /// dump was asked for, which is always in an ordinary session.
    last_dump: Option<String>,
    /// Whether this activation has already armed the environment's dump. One
    /// per client attach, never one per publish.
    dumped_this_activation: bool,
    counters: Counters,
}

impl PaneAccessibility {
    /// A window has just been created, hidden, and has been offered a provider.
    ///
    /// `bridge` is `None` on a platform with no adapter, or when the window was
    /// somehow already visible — `koru-window` has already said so loudly. The
    /// window still works; it is simply not on the accessibility tree.
    pub fn new(slot: WindowSlot, view: ViewSlot, bridge: Option<Box<dyn Provider>>) -> Self {
        Self {
            slot,
            view,
            bridge,
            stage: Stage::Hidden,
            active: false,
            owes_full: false,
            revision: 1,
            seen_view_revision: None,
            published: None,
            occurrences: HashMap::new(),
            actions: Vec::new(),
            dump: None,
            last_dump: None,
            dumped_this_activation: false,
            counters: Counters::default(),
        }
    }

    pub fn slot(&self) -> WindowSlot {
        self.slot
    }

    pub fn view_slot(&self) -> ViewSlot {
        self.view
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// **How many published node ids this window is still willing to route an
    /// action to.**
    ///
    /// The size of the accumulated occurrence map, which a delta merges into
    /// rather than replaces. It is the number that says whether the views are
    /// naming what they took away: it must settle while a person opens and
    /// closes the same surfaces, and grow only when the window really is
    /// showing more. A map that climbs for the life of a session is also a map
    /// that goes on routing requests to controls that left the screen, which is
    /// exactly the refusal this module promises.
    ///
    /// Public because a counter nobody can read is a counter nobody can trust;
    /// the one-shot dump already prints it under `known`.
    pub fn known_ids(&self) -> usize {
        self.occurrences.len()
    }

    /// Whether this window has a native provider at all.
    pub fn has_provider(&self) -> bool {
        self.bridge.is_some()
    }

    /// **Whether anybody is reading this window's tree.**
    ///
    /// The gate in front of [`observe`](Self::observe): a view's revision is
    /// cheap but not free — it reads the state the projection is built from —
    /// and an ordinary session with no screen reader must pay nothing at all.
    /// `active` is set by the first publish that actually landed; `owes_full`
    /// by a client asking for a tree it has not been given yet.
    pub fn listening(&self) -> bool {
        self.stage != Stage::Retired && (self.active || self.owes_full)
    }

    /// Hand a window event to the adapter **before** the application handles
    /// it. See `koru_window::accessibility` for why the order is the
    /// adapter's requirement and not a preference.
    pub fn process_event(
        &mut self,
        window: &winit::window::Window,
        event: &winit::event::WindowEvent,
    ) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.process_event(window, event);
        }
    }

    /// The window is about to be shown for the first time.
    pub fn revealed(&mut self) {
        if self.stage == Stage::Hidden {
            self.stage = Stage::Revealed;
        }
    }

    /// The window is going. After this every request is refused, and the
    /// adapter goes with the value.
    ///
    /// Anything already queued is refused rather than quietly dropped — a
    /// count nobody can see is the same as no count, and "the client asked and
    /// nothing happened" is exactly the shape of report this feature will get.
    pub fn retire(&mut self) {
        let queued = self.actions.len();
        self.stage = Stage::Retired;
        self.bridge = None;
        self.published = None;
        self.occurrences.clear();
        self.actions.clear();
        self.dump = None;
        self.last_dump = None;
        self.owes_full = false;
        if queued > 0 {
            self.refuse(queued, "the window closed before the request was resolved");
        }
    }

    /// Something this window's semantics depend on has changed.
    ///
    /// Cheap and unconditional: one increment. The decision about whether that
    /// costs anything is [`poll`](Self::poll)'s, and it is `no` unless a screen
    /// reader is attached.
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// A host overlay changed the root's child list. The next active publish
    /// must include the root; this never activates an otherwise idle provider.
    pub fn invalidate_structure(&mut self) {
        self.invalidate();
        self.published = None;
    }

    /// **The view's own answer to "has anything I publish changed?"**
    ///
    /// The host cannot tell: it would have to build the tree to compare it,
    /// which is the work this exists to avoid. So the view keeps one counter
    /// and bumps it in the handful of `&mut self` paths its projection depends
    /// on, and this notices a new number.
    ///
    /// **A counter rather than "invalidate on every handled input"**, which was
    /// the obvious alternative and is wrong: scrolling is a handled input, and
    /// scrolling changes which rows are *drawn* without changing the set of
    /// rows, so it must not rebuild. The counter is what makes
    /// [`Counters::rebuilds`] mean something a test can assert.
    ///
    /// Costs one `u64` comparison per window per pass of the loop, whether or
    /// not a screen reader is listening.
    pub fn observe(&mut self, view_revision: u64) {
        if self.seen_view_revision != Some(view_revision) {
            if self.seen_view_revision.is_some() {
                self.invalidate();
            }
            self.seen_view_revision = Some(view_revision);
        }
    }

    /// Drain what the platform has queued, and say whether a snapshot is owed.
    pub fn poll(&mut self) -> Poll {
        if self.stage == Stage::Retired {
            return Poll::default();
        }
        let Some(bridge) = self.bridge.as_mut() else {
            return Poll::default();
        };
        let pending = bridge.take_pending();
        if pending.dropped_actions > 0 {
            self.counters.actions_dropped += pending.dropped_actions;
            tracing::warn!(
                target: "koru::accessibility",
                window = self.slot.raw(),
                dropped = pending.dropped_actions,
                "accessibility action requests were dropped: a client queued faster than the \
                 window could resolve them"
            );
        }
        if pending.deactivated {
            // No client is listening any more. Forget the tree rather than
            // keep answering from it: the next activation must be a coherent
            // full snapshot of what is on screen then, not what was on screen
            // now.
            self.active = false;
            self.owes_full = false;
            self.published = None;
            self.occurrences.clear();
            self.dumped_this_activation = false;
            self.note(format_args!("a client detached"));
        }
        if pending.initial_tree {
            if !self.owes_full {
                self.note(format_args!("a client attached and asked for the tree"));
            }
            self.owes_full = true;
            // **A client attaching is a request, and it happens once.** Not a
            // frame, not a timer, not a property query — `initial_tree` is set
            // by a real `WM_GETOBJECT` and by nothing else. `armed` is one
            // atomic load when the switch is off, so an ordinary session never
            // reaches the line below.
            if !self.dumped_this_activation
                && let Some(redaction) = dump::armed()
            {
                self.dumped_this_activation = true;
                self.dump = Some(redaction);
            }
        }
        self.actions.extend(pending.actions);
        let changed = self
            .published
            .is_none_or(|revision| revision != self.revision);
        Poll {
            snapshot: self.owes_full || (self.active && changed),
            full: self.owes_full || self.published.is_none(),
        }
    }

    /// Publish a snapshot, if a client is in fact listening.
    ///
    /// Returns whether a tree was built. The adapter is the authority on that
    /// — `update_if_active` does not call its closure before a client has
    /// asked — so this is the honest answer even when [`poll`](Self::poll)
    /// thought one was owed.
    pub fn publish(&mut self, semantics: &WindowSemantics) -> bool {
        if self.stage == Stage::Retired {
            return false;
        }
        debug_assert_eq!(
            semantics.window, self.slot,
            "a window was handed another window's semantics"
        );
        let Some(bridge) = self.bridge.as_mut() else {
            return false;
        };
        debug_assert!(
            !self.owes_full || semantics.scope == Scope::Whole,
            "a client that asked for the tree was handed a difference"
        );
        let revision = self.revision;
        let whole = semantics.scope == Scope::Whole;
        // Taken out so the build closure may read it: a delta's focus can name
        // a control this update does not carry, and only the accumulated map
        // knows whether the client has it.
        let known = std::mem::take(&mut self.occurrences);
        let want_dump = self.dump.take();
        let mut landed: Option<Landed> = None;
        let built = bridge.publish(&mut || {
            let Projection {
                update,
                occurrences,
                nodes,
                duplicate_ids,
                unresolved_focus,
                retired,
            } = build(semantics, &known);
            // **The one copy, and only when somebody asked for it.** The
            // adapter takes the update, so describing it afterwards means
            // holding a copy — which is O(nodes) and would be paid on every
            // publish forever if it were unconditional. This is the whole of
            // "zero serialization and zero content-copy work when off".
            let copy = want_dump.is_some().then(|| update.clone());
            landed = Some(Landed {
                copy,
                occurrences,
                nodes,
                duplicate_ids,
                unresolved_focus,
                retired,
            });
            update
        });
        let Some(landed) = landed.filter(|_| built) else {
            // Nothing was published, so nothing was learned and nothing may be
            // forgotten — including the dump, which was asked for against a
            // tree that has not happened yet.
            self.occurrences = known;
            self.dump = want_dump;
            return false;
        };
        self.counters.rebuilds += 1;
        self.counters.duplicate_ids += landed.duplicate_ids as u64;
        self.counters.unresolved_focus += u64::from(landed.unresolved_focus);
        self.counters.nodes_published += landed.nodes as u64;
        if self.owes_full {
            self.counters.activations += 1;
            self.owes_full = false;
        }
        if !whole {
            self.counters.deltas += 1;
        }
        self.active = true;
        // **Merged, never replaced.** A whole snapshot starts from nothing
        // because it *is* everything; a delta adds to what the client already
        // holds and takes away only what this update retired. Replacing here is
        // the latent bug Plan 1b.1 named: the map is what an action is resolved
        // against, so the moment an update stops carrying every node, a client
        // acting on a control that did not change is refused.
        self.occurrences = if whole { HashMap::new() } else { known };
        for id in &landed.retired {
            self.occurrences.remove(id);
        }
        self.occurrences.extend(landed.occurrences.iter());
        self.note(format_args!(
            "published {} node(s) as a {}, {} known, rebuild {}",
            landed.nodes,
            if whole { "snapshot" } else { "difference" },
            self.occurrences.len(),
            self.counters.rebuilds
        ));
        if let (Some(redaction), Some(update)) = (want_dump, landed.copy.as_ref()) {
            self.counters.dumps += 1;
            let text = dump::render(&Dump {
                window: self.slot,
                scope: semantics.scope,
                redaction,
                update,
                occurrences: &landed.occurrences,
                known: &self.occurrences,
                retired: &landed.retired,
                counters: self.counters,
            });
            eprint!("{text}");
            self.last_dump = Some(text);
        }
        self.published = Some(revision);
        true
    }

    /// The text of the most recent dump, for a caller that asked for one and
    /// wants to compare it with what a client sees. Cleared by reading it, so
    /// nothing holds a tree's description longer than the request that asked
    /// for it.
    pub fn take_dump(&mut self) -> Option<String> {
        self.last_dump.take()
    }

    /// Ask for **one** dump of the next tree this window publishes.
    ///
    /// One-shot on purpose, and armed from outside rather than from anything
    /// this module does on its own: there is no frame, timer or poll that can
    /// reach it. A request that is never satisfied — because no client is
    /// listening, so nothing is published — stays armed rather than being lost,
    /// which is the behaviour somebody debugging an attach actually wants.
    ///
    /// See [`super::dump`] for what it says and what it refuses to say.
    pub fn request_dump(&mut self, redaction: Redaction) {
        self.dump = Some(redaction);
    }

    /// Whether a dump is armed. For a test, and for a caller that does not want
    /// to arm a second one behind the first.
    pub fn dump_armed(&self) -> bool {
        self.dump.is_some()
    }

    /// Turn queued requests into actions against controls that still exist.
    pub fn resolve(&mut self) -> Vec<Resolved> {
        let queued = std::mem::take(&mut self.actions);
        if queued.is_empty() {
            return Vec::new();
        }
        if self.stage == Stage::Retired {
            self.refuse(queued.len(), "the window has been retired");
            return Vec::new();
        }
        if self.published.is_none() {
            self.refuse(queued.len(), "no tree is currently published");
            return Vec::new();
        }
        let mut resolved = Vec::with_capacity(queued.len());
        let mut refused = 0;
        for request in queued {
            if request.target_tree != TreeId::ROOT {
                refused += 1;
                continue;
            }
            match self.occurrences.get(&request.target_node) {
                // The window check is defence in depth: the map belongs to one
                // window, so a mismatch would mean two windows shared a map.
                Some(occurrence) if occurrence.window == self.slot => {
                    resolved.push(Resolved {
                        occurrence: *occurrence,
                        action: request.action,
                        data: request.data,
                    });
                }
                _ => refused += 1,
            }
        }
        if refused > 0 {
            self.refuse(refused, "the target is not in the tree on screen");
        }
        self.counters.actions_routed += resolved.len() as u64;
        resolved
    }

    fn refuse(&mut self, count: usize, why: &'static str) {
        self.counters.actions_refused += count as u64;
        self.note(format_args!("refused {count} action(s): {why}"));
    }

    /// One line about this window, behind the switch.
    ///
    /// **Behind it rather than unconditional**, because every line here is
    /// per-event and predictable — a client that replays ids would write one
    /// per request — and the counters are always live whether or not anybody
    /// switched it on. Ids, slots, counts and stages only: never an accessible
    /// name, a label, a title, or anything a person wrote.
    fn note(&self, what: std::fmt::Arguments<'_>) {
        if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
            eprintln!("[koru:a11y] window={} {what}", self.slot.raw());
        }
    }
}

#[cfg(test)]
mod tests;
