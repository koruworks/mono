// SPDX-License-Identifier: MPL-2.0

//! **Where the six plans meet.**
//!
//! A second test module beside `tests.rs`, and it earns its own file because it
//! asks a different question. `tests.rs` asks whether the shell's projection is
//! right; `document/tests.rs` asks whether the document's is. Neither asks what
//! happens when a surface one plan built is standing over a surface another
//! plan built — a menu over a published document, a save status changing while
//! a paragraph is being typed into, two windows over one workspace, the same
//! document in two tabs. Those are the seams, and a seam is where the pieces
//! that are individually correct meet wrongly.
//!
//! Every test here drives a **real** [`PaneAccessibility`] and applies the real
//! `TreeUpdate`s into a small client, because the questions are about what the
//! *host* is holding — which ids it will still route an action to, how big an
//! update was, whether a difference was owed and a whole window was sent. A
//! test that read the view's own answer back could not see any of that.
//!
//! Nothing here touches disk, the Store, a browser or a desktop.

use std::sync::{Arc, Mutex};

use accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId, TreeUpdate};
use koru_core::NodeId as KoruNodeId;
use koru_core::node::Node as CoreNode;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::geometry::Rect;
use koru_window::{AccessibilityPending as Pending, AccessibilityProvider as Provider};
use koru_workspace::Workspace;
use koru_workspace::durability::{DocumentDurability, Durability, LinkHealth};
use koru_workspace_host::View;
use koru_workspace_host::accessibility::{
    Occurrence, PaneAccessibility, Scope, SemanticNode, Slots, ViewSlot, WindowSemantics,
    WindowSlot,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::Shell;
use super::document::{body_rows_of, paragraph_occurrence, put_caret_in};
use crate::mainview::MainView;
use crate::mainview::harness::*;

const VIEWPORT: (f32, f32) = (1200.0, 700.0);

// ----------------------------------------------------------------- the platform

#[derive(Default)]
struct Mailbox {
    attached: bool,
    pending: Pending,
    published: Vec<TreeUpdate>,
}

/// The platform, as far as [`PaneAccessibility`] can tell: a `WM_GETOBJECT` and
/// `update_if_active`'s refusal to build before one has arrived.
#[derive(Clone, Default)]
struct Adapter(Arc<Mutex<Mailbox>>);

impl Adapter {
    fn attach(&self) {
        let mut mailbox = self.0.lock().expect("the mailbox");
        mailbox.attached = true;
        mailbox.pending.initial_tree = true;
    }

    fn queue(&self, target: NodeId, action: Action) {
        self.0
            .lock()
            .expect("the mailbox")
            .pending
            .actions
            .push(ActionRequest {
                action,
                target_tree: TreeId::ROOT,
                target_node: target,
                data: None,
            });
    }

    fn take_published(&self) -> Vec<TreeUpdate> {
        std::mem::take(&mut self.0.lock().expect("the mailbox").published)
    }
}

impl Provider for Adapter {
    fn process_event(
        &mut self,
        _window: &winit::window::Window,
        _event: &winit::event::WindowEvent,
    ) {
    }

    fn take_pending(&mut self) -> Pending {
        std::mem::take(&mut self.0.lock().expect("the mailbox").pending)
    }

    fn publish(&mut self, build: &mut dyn FnMut() -> TreeUpdate) -> bool {
        if !self.0.lock().expect("the mailbox").attached {
            return false;
        }
        let update = build();
        self.0.lock().expect("the mailbox").published.push(update);
        true
    }
}

// ------------------------------------------------------------------- the client

/// A tree kept the way a consumer keeps one: a node is replaced whole, and
/// everything the update did not carry is retained.
#[derive(Default)]
struct Client {
    nodes: std::collections::HashMap<NodeId, Node>,
    root: Option<NodeId>,
    sizes: Vec<usize>,
    /// The author ids the most recent update carried — what this publish
    /// *sent*, as distinct from what the client ends up holding. The two
    /// questions are different and only the first one answers "did a save
    /// acknowledgement cost a paragraph".
    last: Vec<String>,
}

impl Client {
    fn apply(&mut self, update: &TreeUpdate) {
        let known: std::collections::HashSet<_> = self.nodes.keys().copied().collect();
        for (id, node) in &update.nodes {
            self.nodes.insert(*id, node.clone());
        }
        if let Some(tree) = &update.tree {
            self.root = Some(tree.root);
        }
        let reachable: std::collections::HashSet<_> = self.reachable().into_iter().collect();
        for (id, _) in &update.nodes {
            assert!(
                known.contains(id) || reachable.contains(id),
                "new accessibility node {id:?} has no parent in the current tree"
            );
        }
        // AccessKit removes disconnected subtrees. Keeping them in this mock
        // hid updates that tried to reuse a body after a modal removed it.
        self.nodes.retain(|id, _| reachable.contains(id));
        self.sizes.push(update.nodes.len());
        self.last = update
            .nodes
            .iter()
            .filter_map(|(_, node)| node.author_id().map(str::to_owned))
            .collect();
    }

    /// Every node reachable from the root — what the platform's own walk sees.
    /// A child a parent names but nothing published panics here, and aborts the
    /// process inside `accesskit_consumer`.
    fn reachable(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.expect("a root")];
        while let Some(id) = stack.pop() {
            let node = self
                .nodes
                .get(&id)
                .unwrap_or_else(|| panic!("a parent names {id:?}, which was never published"));
            out.push(id);
            stack.extend(node.children().iter().copied());
        }
        out
    }

    fn locators(&self) -> Vec<String> {
        self.reachable()
            .into_iter()
            .filter_map(|id| {
                self.nodes
                    .get(&id)
                    .and_then(Node::author_id)
                    .map(str::to_owned)
            })
            .collect()
    }

    fn holds(&self, locator: &str) -> bool {
        self.locators().iter().any(|held| held == locator)
    }

    fn last_size(&self) -> usize {
        *self.sizes.last().expect("a publish")
    }
}

// -------------------------------------------------------------------- the window

/// One window: a container, its host-side accessibility state, and the client
/// reading it.
struct Window {
    v: MainView,
    pane: PaneAccessibility,
    adapter: Adapter,
    client: Client,
    slot: WindowSlot,
    view: ViewSlot,
}

impl Window {
    /// A window over `w`, seated out of `slots` the way `App::seat_accessibility`
    /// seats one, with a client attached and the first whole tree published.
    fn open(w: &mut Workspace, slots: &mut Slots, document: KoruNodeId) -> Self {
        let (mut v, _) = container();
        let slot = slots.window();
        let view = slots.view();
        v.seat_accessibility(slot);
        v.open_node(&w.ws, document);
        v.recents.clear();
        let adapter = Adapter::default();
        let pane = PaneAccessibility::new(slot, view, Some(Box::new(adapter.clone())));
        let mut window = Self {
            v,
            pane,
            adapter,
            client: Client::default(),
            slot,
            view,
        };
        window.pane.revealed();
        window.adapter.attach();
        assert_eq!(window.service(), Some(Scope::Whole));
        // One idle pass: `observe` does not treat the first revision it is
        // shown as a change, so a fixture that published once and then acted
        // would find nothing owed.
        assert_eq!(window.service(), None, "an idle pass published something");
        window
    }

    /// One pass of the loop's accessibility service, written the way
    /// `App::service_accessibility` writes it.
    fn service(&mut self) -> Option<Scope> {
        if self.pane.listening() {
            let revision = View::semantics_revision(&self.v);
            self.pane.observe(revision);
        }
        let wants = self.pane.poll();
        if !wants.snapshot {
            self.drain();
            return None;
        }
        let scope = if wants.full {
            Scope::Whole
        } else {
            Scope::Delta
        };
        let mut children = if scope == Scope::Whole {
            koru_workspace_host::accessibility::window_commands(
                self.slot,
                false,
                koru_workspace_host::accessibility::ui_lang(),
                View::window_command_bounds(&self.v),
            )
        } else {
            Vec::new()
        };
        children.extend(View::semantics(&self.v, self.view, scope));
        let semantics = WindowSemantics {
            window: self.slot,
            label: "Koru".to_string(),
            scale: 1.0,
            client: Rect::new(0.0, 0.0, VIEWPORT.0, VIEWPORT.1),
            focus: View::accessibility_focus(&self.v, self.view),
            children,
            scope,
            retired: match scope {
                Scope::Whole => Vec::new(),
                Scope::Delta => View::accessibility_retired(&self.v),
            },
        };
        self.pane.publish(&semantics);
        self.drain();
        Some(scope)
    }

    fn drain(&mut self) {
        for update in self.adapter.take_published() {
            self.client.apply(&update);
        }
    }

    /// Perform whatever a client queued, the way the loop does, and say how
    /// many the *view* performed.
    fn perform(&mut self, ws: &mut WorkspaceDoc) -> usize {
        let resolved = self.pane.resolve();
        let mut done = 0;
        for request in resolved {
            if View::accessibility_action(
                &mut self.v,
                ws,
                request.occurrence,
                request.action,
                request.data.as_ref(),
            ) {
                done += 1;
            }
        }
        done
    }

    fn shell(&self) -> Shell {
        Shell {
            window: self.slot,
            view: self.view,
            viewport: VIEWPORT,
            lang: koru_workspace_host::accessibility::ui_lang(),
        }
    }

    fn paragraph(&self, row: KoruNodeId) -> Occurrence {
        paragraph_occurrence(&self.shell(), row)
    }

    fn body_rows(&self) -> Vec<KoruNodeId> {
        body_rows_of(&self.v)
    }

    fn press(&mut self, ws: &mut WorkspaceDoc, key: Key, text: Option<&str>) {
        View::key(&mut self.v, ws, &key, text, ModifiersState::empty());
    }

    /// The window's own semantic tree right now, flattened. For a question
    /// about *what would be published* rather than about what the client holds.
    fn nodes(&self, scope: Scope) -> Vec<SemanticNode> {
        let mut out = Vec::new();
        for node in View::semantics(&self.v, self.view, scope) {
            flatten(&node, &mut out);
        }
        out
    }
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

/// A workspace holding one document of `texts`, built without the stray empty
/// paragraph `koru_workspace::document` leaves behind.
fn workspace_with(texts: &[&str]) -> (Workspace, KoruNodeId) {
    let (_, mut w) = container();
    let doc =
        w.ws.insert(Parent::Root, CoreNode::document("Notes").unwrap())
            .expect("the document node");
    for text in texts {
        w.ws.insert(Parent::Node(doc), CoreNode::paragraph(*text).unwrap())
            .expect("a paragraph");
    }
    w.ws.enable_undo();
    (w, doc)
}

/// Right-click the first tab, which is how a person opens this menu.
fn open_a_tab_menu(v: &mut MainView, ws: &mut WorkspaceDoc) -> u64 {
    v.ensure_tab_widths();
    let rect = koru_shell_chrome::tabs::layout(v.strip(), v.tab_widths(), v.tab_scroll())
        .first()
        .expect("the strip has a tab")
        .body;
    let (x, y) = rect.centre();
    assert!(View::context_menu(v, ws, x, y), "the tab menu did not open");
    v.menu.as_ref().expect("a menu is up").instance
}

fn dismiss(v: &mut MainView, ws: &mut WorkspaceDoc) -> bool {
    View::key(
        v,
        ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    )
}

fn menu_rows(window: &Window) -> Vec<Occurrence> {
    window
        .nodes(Scope::Whole)
        .into_iter()
        .filter(|node| node.role == Role::MenuItem)
        .map(|node| node.occurrence)
        .collect()
}

// ============================================== 1a.1's identity, in 1b.3's shell

/// **Two windows over one workspace, both showing the same document.**
///
/// 1a.1 promised that the same block shown in two windows is two independently
/// focusable occurrences. It proved that of the identity type. This proves it
/// of the shipping shell now that a document is really published: the two
/// windows' paragraphs are two locators, two node ids, and neither window's
/// tree contains the other's.
#[test]
fn one_document_in_two_windows_is_two_sets_of_paragraphs() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let left = Window::open(&mut w, &mut slots, doc);
    let right = Window::open(&mut w, &mut slots, doc);
    assert_ne!(left.slot, right.slot, "two windows shared a slot");
    assert_ne!(left.view, right.view, "two windows shared a view slot");

    let rows = left.body_rows();
    assert!(!rows.is_empty(), "the fixture published no paragraphs");
    assert_eq!(
        rows,
        right.body_rows(),
        "the two windows disagree about the document"
    );

    for row in rows {
        let here = left.paragraph(row);
        let there = right.paragraph(row);
        assert_ne!(
            here.locator(),
            there.locator(),
            "one paragraph, one locator, two windows"
        );
        assert_ne!(here.node_id(), there.node_id());
        assert!(left.client.holds(&here.locator()));
        assert!(right.client.holds(&there.locator()));
        assert!(
            !left.client.holds(&there.locator()),
            "a window's tree carried the other window's paragraph"
        );
    }
    assert_eq!(left.pane.counters().duplicate_ids, 0);
    assert_eq!(right.pane.counters().duplicate_ids, 0);
    assert_eq!(left.pane.counters().unresolved_focus, 0);
    assert_eq!(right.pane.counters().unresolved_focus, 0);
}

/// **The same document open in two tabs of one window.**
///
/// `Ctrl+Enter` on a row already showing opens a second tab on the same
/// document, and the two tabs are two editors with two carets. Only the active
/// one is published, so the tree holds one occurrence per paragraph at a time —
/// but the identity is scoped by the *container's* view slot, so both tabs mint
/// the same locator for the same paragraph.
///
/// What that has to mean, and what this pins: switching between them leaves the
/// client holding a coherent tree of the tab that is showing, with the caret the
/// tab that is showing has, and nothing published twice.
#[test]
fn one_document_in_two_tabs_publishes_the_tab_that_is_showing() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);
    let rows = window.body_rows();
    let first = window.paragraph(rows[0]);

    let tabs_before = window.v.tabs().len();
    assert!(
        window.v.open_node_in_new_tab(&w.ws, doc),
        "a second tab on the same document would not open"
    );
    assert_eq!(window.v.tabs().len(), tabs_before + 1);
    window.service();

    // The paragraph is still there, published by the tab that is showing, and
    // published exactly once.
    assert!(window.client.holds(&first.locator()));
    let held = window.client.locators();
    let mut seen = held.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), held.len(), "a control was published twice");
    assert_eq!(window.pane.counters().duplicate_ids, 0);
    assert_eq!(window.pane.counters().unresolved_focus, 0);

    // Back to the first tab, and the tree is coherent again.
    let first_tab = window
        .v
        .tabs()
        .iter()
        .position(|tab| matches!(tab.view, crate::mainview::tab::TabView::Leaf(_)))
        .expect("a document tab");
    assert!(window.v.select(first_tab));
    window.service();
    let _ = window.client.reachable();
    assert!(window.client.holds(&first.locator()));
    assert_eq!(window.pane.counters().unresolved_focus, 0);
}

// ================================ 1a.2's blockers over 1b.3's published document

/// **A menu that closed is no longer routable — from the host, not only from
/// the view.**
///
/// 1a.2 built the refusal as a property of the *host*: a request naming an id
/// that is not in the tree currently on screen is refused, and
/// `Counters::actions_refused` is what says so. 1b.2 then made an update a
/// difference and the occurrence map accumulate, so the map only forgets what a
/// view *names* as retired. The document names its paragraphs; nothing named a
/// menu's rows, so the host went on routing them.
///
/// The view refuses them a second time — `menus::action` looks the row up among
/// the live menus — so nothing was ever performed. What was wrong is the
/// mechanism: the refusal an acceptance check reads off the counters, and the
/// map that grows for every menu a person ever opens.
#[test]
fn a_menu_that_closed_is_refused_by_the_host() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);

    open_a_tab_menu(&mut window.v, &mut w.ws);
    window.service();
    let row = *menu_rows(&window).first().expect("the tab menu has rows");
    assert!(
        window.client.holds(&row.locator()),
        "an open menu published no rows"
    );

    assert!(
        dismiss(&mut window.v, &mut w.ws),
        "Escape did not close the menu"
    );
    window.service();
    assert!(
        !window.client.holds(&row.locator()),
        "a closed menu's row is still reachable in the published tree"
    );

    let refused = window.pane.counters().actions_refused;
    window.adapter.queue(row.node_id(), Action::Click);
    window.service();
    assert_eq!(
        window.perform(&mut w.ws),
        0,
        "a closed menu's row was performed"
    );
    assert!(
        window.pane.counters().actions_refused > refused,
        "the host routed a request naming a control that has left the tree"
    );
}

/// **The occurrence map settles.**
///
/// Open and close the same menu ten times. Every opening mints a new instance
/// number, so every opening is a new set of ids; if nothing retires them the map
/// grows by a menu's worth each time, for the life of the session — and every
/// one of those stale ids is one the host will still route an action to.
#[test]
fn opening_and_closing_a_menu_does_not_grow_the_occurrence_map() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);

    let mut settled = None;
    for round in 0..10 {
        open_a_tab_menu(&mut window.v, &mut w.ws);
        window.service();
        assert!(dismiss(&mut window.v, &mut w.ws));
        window.service();
        // The first round is allowed to differ: it is the one that publishes
        // the surfaces a menu's opening pushes out of the way for the first
        // time.
        if round == 0 {
            settled = Some(window.pane.known_ids());
            continue;
        }
        assert_eq!(
            Some(window.pane.known_ids()),
            settled,
            "round {round}: the occurrence map grew by a menu's worth of ids"
        );
    }
}

/// **Switching tabs does not grow it either**, which is the same question asked
/// of the surface 1b.2 built the accumulation for.
#[test]
fn switching_tabs_repeatedly_does_not_grow_the_occurrence_map() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let second =
        w.ws.insert(Parent::Root, CoreNode::document("Brief").unwrap())
            .expect("a second document");
    w.ws.insert(
        Parent::Node(second),
        CoreNode::paragraph("elsewhere").unwrap(),
    )
    .expect("a paragraph");
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);
    window.v.open_node(&w.ws, second);
    window.service();

    let tabs: Vec<usize> = window
        .v
        .tabs()
        .iter()
        .enumerate()
        .filter(|(_, tab)| matches!(tab.view, crate::mainview::tab::TabView::Leaf(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(tabs.len(), 2, "the fixture wanted two document tabs");

    // **Compared with the same tab, not with the other one.** The two documents
    // are different lengths, so the map legitimately breathes by the difference
    // between them; what must not happen is a ratchet. Each landing on a given
    // tab is compared with the previous landing on that same tab.
    let mut settled = [None, None];
    for round in 0..12 {
        let which = round % 2;
        assert!(window.v.select(tabs[which]));
        window.service();
        let _ = window.client.reachable();
        let known = window.pane.known_ids();
        match settled[which] {
            None => settled[which] = Some(known),
            Some(before) => assert_eq!(
                known,
                before,
                "round {round}: coming back to the same tab left {} id(s) behind",
                known as i64 - before as i64
            ),
        }
    }
    assert_eq!(window.pane.counters().unresolved_focus, 0);
    assert_eq!(window.pane.counters().duplicate_ids, 0);
}

// ================================= 1b.4's save status beside 1b.3's document

fn durability(document: koru_core::DocumentId, durable: bool) -> Durability {
    Durability {
        documents: vec![(
            document,
            DocumentDurability {
                durable,
                in_flight: 0,
                buffered: false,
                buffer_failure: None,
                stopped: None,
                link: LinkHealth::Live,
                read_only: None,
            },
        )],
        document_list: None,
    }
}

/// **The save status and the document are in one tree and do not rebuild each
/// other.**
///
/// The status is folded into the *chrome's* fingerprint and the document into
/// its own, so a Store acknowledgement re-sends the chrome and no paragraph, and
/// a keystroke re-sends one paragraph and no chrome. Getting this the other way
/// round is what makes typing in a long document cost a whole window per
/// acknowledgement.
#[test]
fn a_save_acknowledgement_and_a_keystroke_do_not_publish_each_other() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);
    let rows = window.body_rows();
    let paragraph = window.paragraph(rows[0]).locator();
    // The per-document status label, not the group around it: the group is in
    // every whole chrome publish, so matching on it would pass whether or not
    // anything about saving had moved.
    let status = Occurrence::surface(
        window.slot,
        window.view,
        "save",
        koru_workspace_host::accessibility::ControlPart::new("document"),
    )
    .locator();
    put_caret_in(&mut window.v, rows[0]);
    window.service();

    // An in-memory tab carries no `DocumentId` of its own — only a
    // Store-backed open stamps one — and without it there is no document whose
    // save status to report. Stamp one, the way `harness::pin_active_document`
    // does, so the surface under test exists at all.
    let id = koru_core::DocumentId::new();
    let active = window.v.tabs().active();
    window
        .v
        .tabs_mut()
        .get_mut(active)
        .expect("the active tab")
        .document = Some(id);
    window.service();

    // A Store acknowledgement.
    View::set_durability(&mut window.v, &durability(id, false));
    window.service();
    View::set_durability(&mut window.v, &durability(id, true));
    let published = published_locators(&mut window);
    assert!(
        published.contains(&status),
        "a change of save status did not republish the status"
    );
    assert!(
        !published.contains(&paragraph),
        "a save acknowledgement republished a paragraph of the document"
    );

    // …and a keystroke.
    window.press(&mut w.ws, Key::Character("x".into()), Some("x"));
    let published = published_locators(&mut window);
    assert!(
        published.contains(&paragraph),
        "a keystroke did not republish the paragraph it landed in"
    );
    assert!(
        !published.contains(&status),
        "a keystroke republished the save status"
    );
}

/// Service one pass and say what that publish actually carried.
fn published_locators(window: &mut Window) -> Vec<String> {
    window.client.last.clear();
    window.service();
    window.client.last.clone()
}

// ======================================= a whole document under a whole window

/// **A thousand paragraphs, a menu, and the cost of the menu closing.**
///
/// A blocker publishes alone, so the document's paragraphs go with the container
/// they hang from and the next body publish has to be a whole one. That is
/// correct and it is *expensive*: this records the number rather than asserting
/// a budget, and pins the two things that must be true whatever the number is —
/// nothing is reshaped for the trip, and an idle pass afterwards publishes
/// nothing.
#[test]
fn a_menu_over_a_long_document_costs_one_whole_body_and_no_reshaping() {
    let texts: Vec<String> = (0..1_000).map(|i| format!("paragraph {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let (mut w, doc) = workspace_with(&refs);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);
    let activation = window.client.last_size();
    assert!(
        activation > 1_000,
        "the activation published {activation} nodes for a thousand paragraphs"
    );

    let shaped = window.v.document_counters().shapings;

    open_a_tab_menu(&mut window.v, &mut w.ws);
    window.service();
    let withheld = window.client.last_size();

    assert!(dismiss(&mut window.v, &mut w.ws));
    window.service();
    let restored = window.client.last_size();
    let _ = window.client.reachable();

    eprintln!(
        "a menu over a thousand paragraphs: opening published {withheld} node(s), \
         closing published {restored}, activation was {activation}"
    );

    // **The cost is ids, not layout.** The client dropped the paragraphs with
    // the container they hung from, so they have to go out again — but from the
    // cache the view never threw away, so parley shaped nothing for the trip.
    assert_eq!(
        window.v.document_counters().shapings,
        shaped,
        "a menu opening and closing reshaped the document"
    );
    assert_eq!(
        window.service(),
        None,
        "an idle pass after the menu closed published something"
    );
    assert_eq!(window.pane.counters().unresolved_focus, 0);
    assert_eq!(window.pane.counters().duplicate_ids, 0);
}

/// **A document vanishes beneath a modal and comes back.**
///
/// §5 asks for it in as many words, and the two halves are separately easy to
/// get wrong: while a blocker stands, a paragraph must be neither reachable in
/// the tree nor routable by the host; when it goes, the paragraph must be
/// readable again and the caret still in it.
#[test]
fn a_document_vanishes_beneath_a_modal_and_comes_back() {
    let (mut w, doc) = workspace_with(&["first", "second"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);
    let rows = window.body_rows();
    put_caret_in(&mut window.v, rows[0]);
    window.service();
    let paragraph = window.paragraph(rows[0]);
    assert!(window.client.holds(&paragraph.locator()));

    open_a_tab_menu(&mut window.v, &mut w.ws);
    window.service();
    assert!(
        !window.client.holds(&paragraph.locator()),
        "a paragraph of the document was still reachable beneath an open menu"
    );
    window.pane.invalidate();
    assert_eq!(window.service(), Some(Scope::Delta));
    assert_eq!(
        window.client.last_size(),
        0,
        "a standing menu leaked body nodes"
    );
    let refused = window.pane.counters().actions_refused;
    window.adapter.queue(paragraph.node_id(), Action::Focus);
    window.service();
    assert_eq!(
        window.perform(&mut w.ws),
        0,
        "a paragraph under a menu was acted on"
    );
    assert!(
        window.pane.counters().actions_refused > refused,
        "the host routed a request to a paragraph a menu was standing over"
    );

    assert!(dismiss(&mut window.v, &mut w.ws));
    window.service();
    let _ = window.client.reachable();
    assert!(
        window.client.holds(&paragraph.locator()),
        "the document did not come back when the menu closed"
    );
    assert_eq!(
        View::accessibility_focus(&window.v, window.view),
        Some(paragraph),
        "the caret is not where it was left"
    );
    assert_eq!(window.pane.counters().unresolved_focus, 0);
}

/// The reported Move to crash was a second publish while the same modal was
/// standing: its first publish had already removed the document body.
#[test]
fn move_to_withholds_the_body_on_repeated_publishes_until_cancel_or_confirm() {
    use koru_core::explore::{ExploreEntry, KIND_DOCUMENT};
    use koru_core::{ProfileId, WorkspaceEntry, WorkspaceId};

    for confirm in [false, true] {
        let (mut w, doc) = workspace_with(&["Move this document"]);
        let mut slots = Slots::default();
        let mut window = Window::open(&mut w, &mut slots, doc);
        let document = w.ws.document_id(doc);
        let current = w.ws.id().expect("workspace identity");
        let target = WorkspaceId::new();
        View::set_pane_context(
            &mut window.v,
            Vec::new(),
            ProfileId::new(),
            vec![
                WorkspaceEntry::new(current, "Source", 0),
                WorkspaceEntry::new(target, "Target", 1),
            ],
            Some(current),
        );
        View::set_entries(
            &mut window.v,
            vec![ExploreEntry {
                id: document,
                parent: None,
                ordinal: 0,
                title: "Move this document".into(),
                kind: KIND_DOCUMENT.into(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            }],
            Some(document),
        );
        window.service();
        assert!(window.v.open_move_to(vec![document]));
        assert_eq!(window.service(), Some(Scope::Delta));
        let body = window.shell().surface("body", super::parts::ROOT);
        let modal = window.shell().surface(
            "shell",
            koru_workspace_host::accessibility::ControlPart::new("move-to"),
        );
        assert!(window.client.holds(&modal.locator()));
        assert!(!window.client.nodes.contains_key(&body.node_id()));

        for _ in 0..3 {
            window.pane.invalidate();
            assert_eq!(window.service(), Some(Scope::Delta));
            assert_eq!(window.client.last_size(), 0);
            assert!(window.client.holds(&modal.locator()));
            assert!(!window.client.nodes.contains_key(&body.node_id()));
        }

        if confirm {
            assert!(
                window
                    .v
                    .confirm_move_to(crate::mainview::move_to::Destination {
                        workspace: target,
                        parent: None,
                        index: usize::MAX,
                        path_label: "Target".into(),
                        follow: false,
                    })
            );
            assert!(matches!(View::take_pane_request(&mut window.v),
                Some(koru_workspace_host::PaneRequest::TransferDocument { id, source, target: to, .. })
                    if id == document && source == current && to == target));
        } else {
            assert!(dismiss(&mut window.v, &mut w.ws));
        }
        assert_eq!(window.service(), Some(Scope::Delta));
        assert!(!window.client.holds(&modal.locator()));
        assert_eq!(window.pane.counters().unresolved_focus, 0);
        assert_eq!(window.pane.counters().duplicate_ids, 0);
        if !confirm {
            assert!(
                window.client.holds(&body.locator()),
                "cancel must restore the body"
            );
        }
    }
}

// ================================ 2.1's Tasks page inside 1a.2's container ====

/// Open a Tasks tab on Today and put `titles` in it, as the store's answer
/// would.
fn open_tasks_page(window: &mut Window, titles: &[&str]) -> Vec<KoruNodeId> {
    use crate::panes::explore::rail::{Bucket, RailTarget};
    assert!(
        window
            .v
            .open_tasks_tab(RailTarget::Bucket(Bucket::Today), false),
        "the Tasks tab did not open"
    );
    let today = chrono::Local::now().date_naive();
    let rows: Vec<crate::tasks_view::TaskEntry> = titles
        .iter()
        .map(|title| {
            let workspace = koru_store_core::ids::WorkspaceId::new();
            let document = koru_store_core::ids::DocumentId::new();
            let node = KoruNodeId::new();
            crate::tasks_view::TaskEntry::of(
                &koru_store_wire::tasks::TaskRow {
                    workspace,
                    document,
                    node: node.to_storage(),
                    task: format!(
                        "task:{}:{}:{}",
                        workspace.to_storage(),
                        document.to_storage(),
                        node.to_storage()
                    ),
                    node_kind: "paragraph".to_owned(),
                    lifecycle: "open".to_owned(),
                    document_trashed: false,
                    in_trash: false,
                    breadcrumb: String::new(),
                    title: (*title).to_owned(),
                    attributes: Vec::new(),
                },
                today,
                0,
            )
            .expect("a parseable row")
        })
        .collect();
    let ids: Vec<KoruNodeId> = rows.iter().map(|row| row.node).collect();
    let index = window.v.tabs().active();
    match window.v.tabs_mut().get_mut(index).map(|tab| &mut tab.view) {
        Some(crate::mainview::tab::TabView::Tasks(page)) => page.seed_for_test(rows, today),
        _ => panic!("the active tab is not the Tasks page"),
    }
    ids
}

fn tasks_control(window: &Window, part: &'static str) -> Occurrence {
    Occurrence::surface(
        window.slot,
        window.view,
        "tasks",
        koru_workspace_host::accessibility::ControlPart::new(part),
    )
}

/// **The Tasks page reaches a client at all.** Before this plan a Tasks tab was
/// a named `TabPanel` with nothing inside it, and the acceptance record said so.
#[test]
fn the_tasks_page_is_published_under_the_content_region() {
    let (mut w, doc) = workspace_with(&["first"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);

    let rows = open_tasks_page(&mut window, &["Draft", "Draft", "Draft"]);
    window.service();
    let _ = window.client.reachable();

    assert!(
        window
            .client
            .holds(&tasks_control(&window, "page").locator())
    );
    assert!(
        window
            .client
            .holds(&tasks_control(&window, "list").locator())
    );
    for row in &rows {
        assert!(
            window.client.holds(
                &Occurrence::model(
                    window.slot,
                    window.view,
                    row.as_uuid(),
                    koru_workspace_host::accessibility::ControlPart::new("task-row"),
                )
                .locator()
            ),
            "a task row is not in the published tree"
        );
    }
    assert_eq!(window.pane.counters().unresolved_focus, 0);
}

/// **A tick from a client reaches the store's queue.** The container drains an
/// accessibility request the way it drains a keystroke — which it did not do
/// until this plan gave an AT action something to ask the container for.
#[test]
fn a_tick_from_a_client_is_drained_by_the_container() {
    let (mut w, doc) = workspace_with(&["first"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);

    let rows = open_tasks_page(&mut window, &["Draft", "Draft", "Draft"]);
    window.service();
    let middle = Occurrence::model(
        window.slot,
        window.view,
        rows[1].as_uuid(),
        koru_workspace_host::accessibility::ControlPart::new("task-check"),
    );

    window.adapter.queue(middle.node_id(), Action::Click);
    window.service();
    assert_eq!(window.perform(&mut w.ws), 1, "the tick was not performed");
    // The request left the page rather than sitting in it: `drain_tasks_request`
    // ran, which is the tail an accessibility action used to skip entirely.
    let index = window.v.tabs().active();
    match window.v.tabs_mut().get_mut(index).map(|tab| &mut tab.view) {
        Some(crate::mainview::tab::TabView::Tasks(page)) => assert!(
            page.take_request().is_none(),
            "the container never drained what the client asked for"
        ),
        _ => panic!("the active tab is not the Tasks page"),
    }
}

/// **Leaving the Tasks page retires it.** The page is part of the chrome, which
/// is re-sent whole, so `Surfaces` names what has gone by comparing one publish
/// with the last (ADR-343-retiredsurfaces) — and the host then refuses a request
/// naming one of its controls.
#[test]
fn leaving_the_tasks_page_stops_its_controls_being_routable() {
    let (mut w, doc) = workspace_with(&["first"]);
    let mut slots = Slots::default();
    let mut window = Window::open(&mut w, &mut slots, doc);

    let rows = open_tasks_page(&mut window, &["Draft"]);
    window.service();
    let tick = Occurrence::model(
        window.slot,
        window.view,
        rows[0].as_uuid(),
        koru_workspace_host::accessibility::ControlPart::new("task-check"),
    );
    let _ = window.client.reachable();
    assert!(window.client.holds(&tick.locator()));

    let document_tab = window
        .v
        .tabs()
        .iter()
        .position(|tab| matches!(tab.view, crate::mainview::tab::TabView::Leaf(_)))
        .expect("a document tab");
    assert!(window.v.select(document_tab));
    window.service();
    let _ = window.client.reachable();
    assert!(
        !window.client.holds(&tick.locator()),
        "a Tasks page that is no longer showing is still in the tree"
    );

    let refused = window.pane.counters().actions_refused;
    window.adapter.queue(tick.node_id(), Action::Click);
    window.service();
    assert_eq!(
        window.perform(&mut w.ws),
        0,
        "a control on a page nobody is looking at was performed"
    );
    assert!(
        window.pane.counters().actions_refused > refused,
        "the host routed a request naming a control that has left the window"
    );
    assert_eq!(window.pane.counters().unresolved_focus, 0);
}
