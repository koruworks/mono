// SPDX-License-Identifier: MPL-2.0

//! **Plan 1b.1's gate: can an external client read and edit Koru's real
//! editor?**
//!
//! Everything else about the text projection is asserted in
//! `cargo test --workspace` against pure values. What that cannot establish is
//! the thing the roadmap gates on: *does a Windows UI Automation client — the
//! same API Narrator and NVDA use — find a paragraph, read its exact text
//! through the Text pattern, see where the caret is, select text, have real
//! typing replace exactly that selection, and receive the native change
//! events?*
//!
//! So this stands outside the process boundary the way a screen reader does: it
//! creates a UIA **client** in the COM multi-threaded apartment, reaches a real
//! window through `ElementFromHandle`, and asks only questions a client can
//! ask.
//!
//! ## What it hosts, and why it is here
//!
//! **The editor under test is the shipping one.** `koru_editor::Editor` over a
//! real `WorkspaceDoc`, laid out by the real `rowlayout::reflow` through
//! parley, driven by the real `Editing::key`. There is no synthetic buffer, no
//! hidden native `EDIT` control and no flattened string — the roadmap names all
//! three as failures of this plan, and every text this test reads comes out of
//! `Body::rows`.
//!
//! It lives in `apps/koru-document` because this is the only crate that
//! already depends on all three sides — the editor, the host's projection and
//! the window's adapter — so hosting the proof here creates no dependency edge
//! that does not exist in the shipping app. Plan 1a.2 owns `apps/koru`, which
//! this deliberately does not touch; Plan 1b.3 is what wires the same
//! conversion into the shell for real.
//!
//! ## Why it is `#[ignore]`d
//!
//! It opens a real window on a real desktop and drives it. A separate lane,
//! invoked deliberately:
//!
//! ```text
//! cargo test -p koru-document --test accessibility_text_native -- --ignored --nocapture
//! ```
//!
//! ## What it proves, and what it deliberately does not
//!
//! Proven: T1–T6 and T8 of the plan's table, against the real editor, from
//! outside the process. Not proven, and not provable here: **T7**, which is
//! what Narrator and NVDA *say*, judged by a person. That is
//! `docs/features/accessibility/README.md`.

#![cfg(windows)]

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accesskit::{Action, ActionData, Role};
use koru_core::NodeId;
use koru_core::WorkspaceId;
use koru_core::node::Node as CoreNode;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::geometry::Rect;
use koru_editor::accessibility::{DocumentAccessibility, Publish, RowText, ViewGeometry};
use koru_editor::{Editing, Editor, Paragraph};
use koru_window::{AccessibilityProvider, WindowBridge};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, PaneAccessibility, Redaction, Runs, Scope, SemanticNode,
    Slots, TextBody, ViewSlot, WindowSemantics, WindowSlot,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::windows::EventLoopBuilderExtWindows;
use winit::window::{Window, WindowId};
// `#[implement]` expands to `windows_core::` paths, so the crate has to be in
// scope by its own name even though the macro arrives through `windows::core`.
use windows_core as _;

/// How long the client waits for the provider to answer before giving up.
/// Generous: the first `WM_GETOBJECT` starts UI Automation itself.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The part of a paragraph that carries its text. A code keyword, never a
/// label — see `koru_workspace_host::accessibility::identity`.
const TEXT: ControlPart = ControlPart::new("text");

/// An inline object inside a paragraph — a Koru pill. Scoped by the
/// **attribute's** own uuid, so two pills in one paragraph are two controls and
/// a pill keeps its identity when the text around it is edited.
const PILL: ControlPart = ControlPart::new("pill");

const WINDOW_W: f32 = 760.0;
const WINDOW_H: f32 = 620.0;

// ----------------------------------------------------------- the fixture
//
// Synthetic, and it stays that way: §7 of the roadmap forbids real document
// text in anything automated.

/// Long enough to wrap at this width — the T8 wrapped case.
const WRAPPED: &str = "This first paragraph is deliberately long enough that it \
                       has to wrap onto more than one visual line at the width \
                       this window uses, which is the case a caret has to keep \
                       straight.";
/// The T8 late-activation case: this is what the row says at startup, and the
/// harness edits it **before** any client attaches.
const SECOND_AT_STARTUP: &str = "Second paragraph.";
const APPENDED_BEFORE_ATTACH: &str = " Edited before anyone attached.";
/// The T2 empty case. No placeholder text may appear for it.
const EMPTY: &str = "";
/// The mixed-content case: prose with an inline object in the middle of it. The
/// pill is inserted at `MIXED_CONTENT_ANCHOR` once the row exists.
const MIXED_CONTENT: &str = "Due  and then some more text after it.";
/// Where the pill goes in `MIXED_CONTENT` — between the two spaces.
const MIXED_CONTENT_ANCHOR: usize = 4;
/// The T8 non-ASCII case: Hangul (multi-byte), a decomposed Latin letter, and
/// an emoji with a skin-tone modifier — the three shapes where bytes, scalars,
/// clusters and grapheme clusters all disagree.
const MIXED: &str = "보상 e\u{301} 👍🏻 ok";

fn second_paragraph_after_edit() -> String {
    format!("{SECOND_AT_STARTUP}{APPENDED_BEFORE_ATTACH}")
}

// ------------------------------------------------------- the two threads

/// What the client asks the window's loop to do. Everything here is a real
/// editor input; nothing reaches into the document behind the editor's back.
#[derive(Clone, Debug)]
enum Ask {
    /// A real key press, through `Editing::key`.
    Press {
        key: Press,
        mods: ModifiersState,
    },
    /// Just tell me what the editor thinks now.
    State,
    /// The document's own title changed — the T1 label edit.
    RenameDocument(String),
    /// Ask the window for one dump of the next tree it publishes, and send the
    /// text back. The plan's "compare a requested dump with the same update
    /// seen externally through UIA" starts here.
    Dump,
    Finish,
}

#[derive(Clone, Debug)]
enum Press {
    Named(NamedKey),
    Char(String),
}

#[derive(Debug)]
enum Wake {
    /// A screen reader left a note on the window's mailbox.
    Accessibility,
    Ask(Ask),
}

/// What the editor thinks, sent back after the tree has been republished — so a
/// client that has an answer is looking at a tree that already reflects it.
#[derive(Clone, Debug, Default)]
struct EditorState {
    focus_row: usize,
    caret: usize,
    selection: Option<(usize, usize)>,
    texts: Vec<String>,
    line_counts: Vec<usize>,
    rebuilds: u64,
    activations: u64,
    actions_routed: u64,
    actions_refused: u64,
    /// Locators of every action the host resolved. Says what the client's
    /// requests actually landed on.
    routed: Vec<String>,
    /// Actions that arrived for something this harness does not perform. The
    /// `Focus` that UIA sends before an invoke is the ordinary case; see
    /// `guide/contributing/deps/ACCESSKIT.md`.
    ignored: u64,
    /// Trees published as a difference rather than a whole window.
    deltas: u64,
    /// Node ids published, summed over every update — the delta's size.
    nodes_published: u64,
    /// Paragraph projections that ran parley's `build_nodes`. The number that
    /// must not grow with the document.
    shapings: u64,
    /// The text of the dump, when one was asked for and has since been taken.
    dump: Option<String>,
}

// ------------------------------------------------------------ the window

struct Harness {
    ws: WorkspaceDoc,
    editor: Editor,
    /// The document node whose text is the container's label.
    document: NodeId,
    rows: Vec<NodeId>,
    /// The fixture's one inline object, so the client can be told its locator.
    pill: koru_core::AttrId,
    window_slot: WindowSlot,
    /// The view a person is reading. Every gate but T1's second occurrence
    /// works in this one.
    main: ViewSlot,
    /// A *second* view of the same document, published at the same time. T1's
    /// "distinguish a second occurrence" is exactly this: one model node, two
    /// independently addressable controls.
    other: ViewSlot,
    access_main: DocumentAccessibility,
    access_other: DocumentAccessibility,
    pane: Option<PaneAccessibility>,
    window: Option<Arc<Window>>,
    /// How the adapter's handlers wake this loop. Held here because
    /// `ApplicationHandler` has nowhere else to put one.
    proxy: EventLoopProxy<Wake>,
    hwnd_tx: Option<Sender<Ready>>,
    state_tx: Sender<EditorState>,
    ignored_actions: u64,
    routed: Vec<String>,
    /// Occurrences that have left the tree and are owed a retirement in the
    /// next update. In a delta nothing else says they are gone.
    retired: Vec<Occurrence>,
    /// The container label each view was last published with. A rename is the
    /// one change a delta cannot see by looking at the rows.
    published_titles: std::collections::HashMap<ViewSlot, String>,
    /// The dump the last publish produced, waiting to be sent back.
    dump: Option<String>,
    /// The next publish must be a whole snapshot, because somebody asked to
    /// look at the tree rather than at a difference.
    force_whole: bool,
    /// A reply is owed once the next publish has happened.
    owes_reply: bool,
    finished: bool,
    /// How many trees had been built when the client first attached. The T8
    /// late-activation evidence, read on the loop side.
    rebuilds_before_attach: Option<u64>,
}

/// What the client needs to talk to this window: the handle, and the locators
/// it will search by.
#[derive(Clone, Debug)]
struct Ready {
    hwnd: isize,
    body: String,
    paragraphs: Vec<String>,
    other_first: String,
    /// The locator of the fixture's inline object.
    pill: String,
}

fn fixture() -> (WorkspaceDoc, NodeId, Vec<NodeId>, koru_core::AttrId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "accessibility fixture")
        .expect("an in-memory workspace");
    let document = ws
        .insert(
            Parent::Root,
            CoreNode::document("Fixture").expect("a document"),
        )
        .expect("the document node");
    let mut rows = Vec::new();
    for text in [WRAPPED, SECOND_AT_STARTUP, EMPTY, MIXED, MIXED_CONTENT] {
        rows.push(
            ws.insert(
                Parent::Node(document),
                CoreNode::paragraph(text).expect("a paragraph"),
            )
            .expect("a paragraph node"),
        );
    }
    // The inline object. A pill is an attribute plus a `U+2060` anchor in the
    // text, and it is the one thing in a paragraph a text run cannot carry —
    // `accesskit_windows` implements no embedded-object support at all, so
    // without a control of its own it reaches a client as one zero-width
    // character and nothing else.
    let pill = koru_core::node::Attr::new(
        "due",
        koru_core::node::AttrValue::Date(
            koru_core::time::DateOnly::parse("2026-08-02").expect("a date"),
        ),
    );
    ws.insert_pill(rows[4], MIXED_CONTENT_ANCHOR, &pill)
        .expect("the pill went in");
    // After the rows exist, the way `prepare` turns undo on when a document is
    // opened, so the only undo steps are the ones this test makes.
    ws.enable_undo();
    (ws, document, rows, pill.id)
}

impl Harness {
    fn new(
        proxy: EventLoopProxy<Wake>,
        state_tx: Sender<EditorState>,
        hwnd_tx: Sender<Ready>,
    ) -> Self {
        let (ws, document, rows, pill) = fixture();
        let mut editor = Editor::rooted_at(&ws, document, true);
        editor.set_viewport(WINDOW_W, WINDOW_H);
        editor.view.active = true;
        let mut slots = Slots::default();
        let window_slot = slots.window();
        let main = slots.view();
        let other = slots.view();
        Self {
            ws,
            editor,
            document,
            rows,
            pill,
            window_slot,
            main,
            other,
            access_main: DocumentAccessibility::default(),
            access_other: DocumentAccessibility::default(),
            pane: None,
            window: None,
            proxy,
            hwnd_tx: Some(hwnd_tx),
            state_tx,
            ignored_actions: 0,
            routed: Vec::new(),
            retired: Vec::new(),
            published_titles: std::collections::HashMap::new(),
            dump: None,
            force_whole: false,
            owes_reply: false,
            finished: false,
            rebuilds_before_attach: None,
        }
    }

    fn paragraph_occurrence(&self, view: ViewSlot, row: NodeId) -> Occurrence {
        Occurrence::model(self.window_slot, view, row.as_uuid(), TEXT)
    }

    fn body_occurrence(&self, view: ViewSlot) -> Occurrence {
        Occurrence::surface(self.window_slot, view, "body", ControlPart::ROOT)
    }

    /// The label the container carries: the document's own title.
    ///
    /// Read at publish time rather than cached, so the T1 rename is visible
    /// without disturbing a single row — which is the point of the gate.
    fn title(&self) -> String {
        self.ws
            .read(self.document)
            .ok()
            .and_then(|node| node.content.as_text().map(str::to_owned))
            .unwrap_or_default()
    }

    /// One view's contribution to this update.
    ///
    /// **A whole snapshot is one subtree; a difference is a handful of loose
    /// paragraphs.** In a delta the container is left out unless its child list
    /// changed, and a paragraph node published on its own keeps the parent the
    /// client already has — which is what makes an ordinary keystroke one node
    /// instead of a thousand.
    fn view_nodes(
        &mut self,
        view: ViewSlot,
        only_first: bool,
        publish: Publish,
    ) -> Vec<SemanticNode> {
        let window_slot = self.window_slot;
        let main = self.main;
        let focus_row = self.editor.body.focus;
        let geometry = ViewGeometry {
            origin: (0.0, 0.0),
            content_x: self.editor.view.content_x,
            scroll: self.editor.view.scroll,
            viewport_h: self.editor.view.viewport.1,
        };
        let rows: &[Paragraph] = if only_first {
            &self.editor.body.rows[..1.min(self.editor.body.rows.len())]
        } else {
            &self.editor.body.rows
        };
        let focus = (view == main).then_some(focus_row);
        // **The container has properties of its own.** Its accessible name is
        // the document's title, and a rename changes nothing about any row — so
        // a delta that only ever republished the container when its child list
        // moved would silently drop the T1 label edit. Read before `access` is
        // borrowed, because both want `self`.
        let title = self.title();
        let title_changed = self.published_titles.get(&view) != Some(&title);
        let order_now: Vec<NodeId> = rows.iter().map(|row| row.id).collect();
        let access = if view == main {
            &mut self.access_main
        } else {
            &mut self.access_other
        };
        let occurrence_of = |id: NodeId| Occurrence::model(window_slot, view, id.as_uuid(), TEXT);
        let update = access.body(rows, geometry, focus, publish, |id| {
            occurrence_of(id).node_id()
        });

        let mut published: Vec<SemanticNode> = Vec::with_capacity(update.rows.len() + 1);
        for row in &update.rows {
            let occurrence = occurrence_of(row.id);
            let mut node = SemanticNode::new(occurrence, Role::MultilineTextInput)
                // Focus, so an AT can move into a paragraph; SetTextSelection,
                // so it can select; ScrollIntoView, because a paragraph that is
                // not laid out yet is reached by the view's own scroll and by
                // nothing else. Nothing further: an advertised action that
                // performs nothing is worse than one that is absent.
                .with_action(Action::Focus)
                .with_action(Action::SetTextSelection)
                .with_action(Action::ScrollIntoView);
            if let Some(bounds) = row.bounds {
                node = node.with_bounds(bounds);
            }
            // **The inline objects, as sibling controls after the runs.** A
            // pill cannot be published inside a run, because the adapter
            // implements no embedded-object support at all
            // (`guide/contributing/deps/ACCESSKIT.md`): `RangeFromChild` is not implemented
            // and `GetChildren` on a range always answers empty. Its label is
            // the caption the document already carries — user content, never
            // translated — and no action is advertised, because activating one
            // is not wired to a behaviour owner yet.
            //
            // They come *after* the runs rather than between them, so a reader
            // meets a pill at the end of its paragraph rather than at the point
            // the anchor sits. `InlineObject::at` carries the anchor's byte
            // offset for a publisher that can do better; with this adapter
            // there is nowhere better to put it, and being announced late is
            // the smaller failure than not being announced at all. What a
            // screen reader makes of that is Q-B2.
            let objects: Vec<SemanticNode> = row
                .objects
                .iter()
                .map(|object| {
                    let mut pill = SemanticNode::new(
                        Occurrence::model(window_slot, view, object.attr.as_uuid(), PILL),
                        object.role(),
                    )
                    .with_label(object.label.clone());
                    if let Some(value) = object.value() {
                        pill = pill.with_value(value.to_string());
                    }
                    if let Some(bounds) = object.bounds {
                        pill = pill.with_bounds(bounds);
                    }
                    pill
                })
                .collect();
            if !objects.is_empty() {
                node = node.with_children(objects);
            }
            node = match &row.text {
                // A paragraph with no layout yet: discoverable, focusable and
                // revealable, and it publishes no text pattern rather than a
                // rectangle and a string that do not exist.
                RowText::Absent => node,
                RowText::Runs { runs, selection } => node.with_text(TextBody {
                    runs: Runs::Published(runs.clone()),
                    selection: *selection,
                    read_only: false,
                }),
                RowText::Retained { runs, selection } => node.with_text(TextBody {
                    runs: Runs::Retained(runs.clone()),
                    selection: *selection,
                    read_only: false,
                }),
            };
            published.push(node);
        }

        self.retired
            .extend(update.removed.iter().map(|id| occurrence_of(*id)));

        // **`Role::Group`, not `Role::Document`.** A `Document` would claim a
        // Text pattern of its own — `accesskit_consumer` gives one to any
        // container with text-run descendants, and text runs are found through
        // intermediate nodes — and Koru would then be publishing two carets for
        // one editor. The paragraphs own the text; this owns the order.
        if update.order.is_none() && !title_changed && publish == Publish::Delta {
            // Neither the child list nor the container's own label changed, so
            // the container is exactly what the client already holds and is
            // left out entirely. Its paragraphs go up on their own and keep the
            // parent the client has.
            return published;
        }
        // A node is republished whole, so the container has to carry its
        // complete child list even when only its label moved.
        let order: Vec<accesskit::NodeId> = update
            .order
            .unwrap_or(order_now)
            .iter()
            .map(|id| occurrence_of(*id).node_id())
            .collect();
        self.published_titles.insert(view, title.clone());
        let container = SemanticNode::new(self.body_occurrence(view), Role::Group)
            .with_label(title)
            .with_bounds(Rect::new(0.0, 0.0, WINDOW_W, WINDOW_H))
            .with_child_order(order)
            .with_children(published);
        vec![container]
    }

    fn semantics(&mut self, publish: Publish) -> WindowSemantics {
        let window = self.window.as_ref().expect("a window").clone();
        let scale = window.scale_factor();
        let size = window.inner_size();
        let focus = self
            .editor
            .body
            .rows
            .get(self.editor.body.focus)
            .map(|row| self.paragraph_occurrence(self.main, row.id));
        let mut children = self.view_nodes(self.main, false, publish);
        children.extend(self.view_nodes(self.other, true, publish));
        WindowSemantics {
            window: self.window_slot,
            label: "Koru text accessibility proof".to_string(),
            scale,
            client: Rect::new(
                0.0,
                0.0,
                size.width as f32 / scale as f32,
                size.height as f32 / scale as f32,
            ),
            focus,
            children,
            scope: match publish {
                Publish::Whole => Scope::Whole,
                Publish::Delta => Scope::Delta,
            },
            retired: std::mem::take(&mut self.retired),
        }
    }

    fn state(&self) -> EditorState {
        let counters = self
            .pane
            .as_ref()
            .map(PaneAccessibility::counters)
            .unwrap_or_default();
        let row = self.editor.body.rows.get(self.editor.body.focus);
        EditorState {
            focus_row: self.editor.body.focus,
            caret: row.map(|row| row.caret()).unwrap_or_default(),
            selection: row.and_then(Paragraph::directed_range),
            texts: self
                .editor
                .body
                .rows
                .iter()
                .map(|row| row.text.clone())
                .collect(),
            line_counts: self
                .editor
                .body
                .rows
                .iter()
                .map(Paragraph::line_count)
                .collect(),
            rebuilds: counters.rebuilds,
            activations: counters.activations,
            actions_routed: counters.actions_routed,
            actions_refused: counters.actions_refused,
            routed: self.routed.clone(),
            ignored: self.ignored_actions,
            deltas: counters.deltas,
            nodes_published: counters.nodes_published,
            shapings: self.access_main.counters().shapings,
            dump: self.dump.clone(),
        }
    }

    fn apply(&mut self, ask: Ask) {
        match ask {
            Ask::Press { key, mods } => {
                let (key, text) = match &key {
                    Press::Named(named) => (Key::Named(*named), None),
                    Press::Char(text) => (Key::Character(text.as_str().into()), Some(text.clone())),
                };
                self.editor.key(&mut self.ws, &key, text.as_deref(), mods);
                self.editor.reflow();
            }
            Ask::RenameDocument(title) => {
                // A *label* edit: the container's accessible name comes from
                // this, and no identity may move because of it.
                let _ = self.ws.set_text(self.document, &title);
            }
            Ask::Dump => {
                // A dump of a *difference* would describe whatever happened to
                // change, which is nothing a client can be compared against.
                // Asking for one forces the next publish to be a whole
                // snapshot, which is what "show me what you have" means.
                self.force_whole = true;
                if let Some(pane) = self.pane.as_mut() {
                    pane.request_dump(Redaction::Structure);
                }
            }
            Ask::State => {}
            Ask::Finish => self.finished = true,
        }
        self.owes_reply = true;
        if let Some(pane) = self.pane.as_mut() {
            pane.invalidate();
        }
    }

    /// Turn what a client asked for into what the editor already does.
    ///
    /// The only two actions this harness performs. Everything else is counted
    /// and dropped — `accesskit_windows` forwards a `Focus` before every invoke
    /// whether or not the node advertised it, so an action router that assumed
    /// it would only be asked for what it offered would be wrong on the first
    /// click.
    fn route(&mut self, resolved: Vec<koru_workspace_host::accessibility::Resolved>) -> bool {
        let mut acted = false;
        for request in resolved {
            self.routed.push(request.occurrence.locator());
            let ObjectKey::Model(uuid) = request.occurrence.object else {
                self.ignored_actions += 1;
                continue;
            };
            let Some(index) = self
                .editor
                .body
                .rows
                .iter()
                .position(|row| row.id.as_uuid() == uuid)
            else {
                self.ignored_actions += 1;
                continue;
            };
            match (request.action, request.data) {
                (Action::Focus, _) => {
                    self.editor.body.focus = index;
                    self.editor.view.active = true;
                    acted = true;
                }
                (Action::SetTextSelection, Some(ActionData::SetTextSelection(selection))) => {
                    let row = &self.editor.body.rows[index];
                    let Some((anchor, focus)) = self.access_main.byte_range(row, &selection) else {
                        // A range naming runs this paragraph never published.
                        // Refused, not clamped onto whatever is nearest.
                        self.ignored_actions += 1;
                        continue;
                    };
                    self.editor.body.focus = index;
                    self.editor.view.active = true;
                    let row = &mut self.editor.body.rows[index];
                    row.select_range_now(anchor, focus);
                    acted = true;
                }
                _ => self.ignored_actions += 1,
            }
        }
        if let (true, Some(pane)) = (acted, self.pane.as_mut()) {
            pane.invalidate();
        }
        acted
    }
}

impl ApplicationHandler<Wake> for Harness {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Koru text accessibility proof")
            .with_visible(false)
            .with_active(false)
            .with_decorations(false)
            .with_inner_size(winit::dpi::LogicalSize::new(
                f64::from(WINDOW_W),
                f64::from(WINDOW_H),
            ));
        let window = Arc::new(events.create_window(attributes).expect("create window"));

        let proxy: EventLoopProxy<Wake> = self.proxy.clone();
        let bridge = WindowBridge::before_first_reveal(
            events,
            &window,
            Arc::new(move || {
                let _ = proxy.send_event(Wake::Accessibility);
            }),
        );
        assert!(
            bridge.is_some(),
            "no AccessKit adapter was installed on a hidden window — nothing below can mean \
             anything"
        );
        self.pane = Some(PaneAccessibility::new(
            self.window_slot,
            self.main,
            bridge.map(|bridge| Box::new(bridge) as Box<dyn AccessibilityProvider>),
        ));

        window.set_visible(true);
        if let Some(pane) = self.pane.as_mut() {
            pane.revealed();
        }
        self.window = Some(window.clone());

        // **The T8 late-activation setup.** An edit made while nobody is
        // listening. No tree exists yet — `update_if_active` does not call its
        // closure before a client attaches — so the first tree the client ever
        // sees must contain this, not what the fixture started with.
        self.editor.body.focus = 1;
        if let Some(row) = self.editor.body.rows.get_mut(1) {
            let at = row.text.len();
            row.place_caret(at);
        }
        self.editor.reflow();
        self.editor
            .insert_text(&mut self.ws, APPENDED_BEFORE_ATTACH);
        self.editor.reflow();
        self.editor.body.focus = 0;
        if let Some(row) = self.editor.body.rows.get_mut(0) {
            row.place_caret(0);
        }
        self.editor.reflow();

        let ready = Ready {
            hwnd: hwnd_of(&window),
            body: self.body_occurrence(self.main).locator(),
            paragraphs: self
                .rows
                .iter()
                .map(|id| self.paragraph_occurrence(self.main, *id).locator())
                .collect(),
            other_first: self
                .paragraph_occurrence(self.other, self.rows[0])
                .locator(),
            pill: Occurrence::model(self.window_slot, self.main, self.pill.as_uuid(), PILL)
                .locator(),
        };
        if let Some(tx) = self.hwnd_tx.take() {
            let _ = tx.send(ready);
        }
    }

    fn window_event(&mut self, _events: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(pane), Some(window)) = (self.pane.as_mut(), self.window.as_ref()) {
            pane.process_event(window, &event);
        }
    }

    fn user_event(&mut self, _events: &ActiveEventLoop, wake: Wake) {
        match wake {
            Wake::Accessibility => {}
            Wake::Ask(ask) => self.apply(ask),
        }
    }

    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if self.pane.is_none() {
            return;
        }
        // One service pass, twice: an action a client sent may change what the
        // tree should say, and the client is entitled to see the result before
        // it is told anything.
        for _ in 0..2 {
            let poll = self.pane.as_mut().expect("a pane").poll();
            if poll.snapshot {
                if self.rebuilds_before_attach.is_none() {
                    let counters = self.pane.as_ref().expect("a pane").counters();
                    self.rebuilds_before_attach = Some(counters.rebuilds);
                }
                // **The kind of update is the pane's answer, not a guess.** A
                // client that has just attached is owed the whole window; every
                // other publish is a difference against the tree it holds.
                let publish = if poll.full || std::mem::take(&mut self.force_whole) {
                    Publish::Whole
                } else {
                    Publish::Delta
                };
                let semantics = self.semantics(publish);
                self.pane.as_mut().expect("a pane").publish(&semantics);
                if let Some(text) = self.pane.as_mut().expect("a pane").take_dump() {
                    self.dump = Some(text);
                }
            }
            let resolved = self.pane.as_mut().expect("a pane").resolve();
            if resolved.is_empty() {
                break;
            }
            self.route(resolved);
        }
        if self.owes_reply {
            self.owes_reply = false;
            let _ = self.state_tx.send(self.state());
        }
        if self.finished {
            if let Some(pane) = self.pane.as_mut() {
                pane.retire();
            }
            // `exit()` from `about_to_wait` is not honoured until the loop's next
            // message: winit blocks in `MsgWaitForMultipleObjectsEx` *before* it
            // reads the exit flag, so under `ControlFlow::Wait` this parks until
            // some unrelated message happens to arrive. See guide/contributing/deps/WINIT.md.
            events.set_control_flow(ControlFlow::Poll);
            events.exit();
        }
    }
}

fn hwnd_of(window: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window
        .window_handle()
        .expect("a live window has a handle")
        .as_raw()
    {
        RawWindowHandle::Win32(handle) => handle.hwnd.get(),
        other => panic!("not a Win32 window: {other:?}"),
    }
}

// ----------------------------------------------------------------- gates

/// Everything the client managed to observe, in the order it asked.
#[derive(Debug, Default)]
struct Findings {
    // T1
    paragraph_found_by_automation_id: bool,
    second_occurrence_is_a_different_element: bool,
    ids_survive_a_label_edit: bool,
    renamed_label: Option<String>,
    // T2
    first_paragraph_text: Option<String>,
    second_paragraph_text: Option<String>,
    empty_paragraph_text: Option<String>,
    mixed_paragraph_text: Option<String>,
    control_type_is_edit: bool,
    supported_text_selection: Option<i32>,
    // T3
    caret_agreements: Vec<(&'static str, usize, usize)>,
    // T4
    selection_before: Option<String>,
    selection_after_set: Option<String>,
    typed_replacement: Option<String>,
    // T5
    text_changed_events: usize,
    selection_changed_events: usize,
    // T6
    paragraph_hop_text: Option<String>,
    typed_into_intended_paragraph: Option<String>,
    // T8
    wrapped_first_line: Option<String>,
    wrapped_line_count: usize,
    late_activation_text: Option<String>,
    // T10 — the inline object
    pill_found_by_automation_id: bool,
    pill_name: Option<String>,
    pill_control_type: Option<i32>,
    mixed_paragraph_before_edit: Option<String>,
    mixed_paragraph_after_edit: Option<String>,
    pill_survived_the_edit: bool,
    // T9 — the dump against the same update seen from outside
    dump_text: Option<String>,
    dump_locator_matches: bool,
    dump_bounds: Option<(f64, f64)>,
    uia_bounds: Option<(f64, f64)>,
    dump_chars: Option<usize>,
    uia_chars: Option<usize>,
    dump_run_count: Option<usize>,
    dump_leaked_content: Vec<&'static str>,
    /// The window's own accounting at the end of the run: how many updates were
    /// differences rather than whole trees, and how much was in them.
    incremental: Option<EditorState>,
    /// The editor's own view of the world at the moment the AT's selection had
    /// just been typed over — counters included, so the report can say what the
    /// host actually routed rather than what the client hoped.
    counters: Option<EditorState>,
    error: Option<String>,
}

fn main_test_body(
    ready: &Receiver<Ready>,
    states: &Receiver<EditorState>,
    proxy: &EventLoopProxy<Wake>,
    findings: &Arc<Mutex<Findings>>,
) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::*;

    let ready = ready.recv_timeout(TIMEOUT).expect("the window was created");
    let hwnd = HWND(ready.hwnd as *mut core::ffi::c_void);

    // The client initialises COM; the provider deliberately does not.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .expect("COM initialised");
    let _com = scopeguard(|| unsafe { CoUninitialize() });

    let uia: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
            .expect("a UI Automation client");

    // Poll: the first `ElementFromHandle` is what sends `WM_GETOBJECT`, and the
    // adapter answers with its placeholder tree until the loop's next pass.
    let deadline = Instant::now() + TIMEOUT;
    let mut root = None;
    while Instant::now() < deadline {
        let element = unsafe { uia.ElementFromHandle(hwnd) }.expect("the window has an element");
        let id = unsafe { element.CurrentAutomationId() }
            .map(|id| id.to_string())
            .unwrap_or_default();
        if id.starts_with("koru:") {
            root = Some(element);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let root = root.expect("the window root never published a Koru locator");

    let find = |locator: &str| -> Option<IUIAutomationElement> {
        let condition = unsafe {
            uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(locator))
        }
        .expect("a property condition");
        unsafe { root.FindFirst(TreeScope_Descendants, &condition) }.ok()
    };
    // The tree lands on the loop's next pass; a paragraph may not be there for
    // the first few milliseconds after the root is.
    let find_waiting = |locator: &str| -> Option<IUIAutomationElement> {
        let deadline = Instant::now() + TIMEOUT;
        while Instant::now() < deadline {
            if let Some(found) = find(locator) {
                return Some(found);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    };

    let ask = |ask: Ask| -> EditorState {
        proxy.send_event(Wake::Ask(ask)).expect("the loop is alive");
        states.recv_timeout(TIMEOUT).expect("the loop answered")
    };
    let press = |named: NamedKey, mods: ModifiersState| -> EditorState {
        ask(Ask::Press {
            key: Press::Named(named),
            mods,
        })
    };
    let text_of = |element: &IUIAutomationElement| -> String {
        let pattern: IUIAutomationTextPattern =
            unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }
                .expect("the paragraph offers the Text pattern");
        let range = unsafe { pattern.DocumentRange() }.expect("a document range");
        unsafe { range.GetText(-1) }
            .map(|text| text.to_string())
            .unwrap_or_default()
    };

    // ------------------------------------------------------------ T1
    let first = find_waiting(&ready.paragraphs[0]).expect("the first paragraph by AutomationId");
    let other = find_waiting(&ready.other_first).expect("the second occurrence by AutomationId");
    {
        let mut found = findings.lock().expect("no test poisons this");
        found.paragraph_found_by_automation_id = true;
        let same = unsafe { uia.CompareElements(&first, &other) }
            .map(|same| same.as_bool())
            .unwrap_or(true);
        found.second_occurrence_is_a_different_element = !same;
        found.control_type_is_edit = unsafe { first.CurrentControlType() }
            .map(|control| control == UIA_EditControlTypeId)
            .unwrap_or(false);
    }

    // ------------------------------------------------------------ T8 (late activation)
    // The very first text this client has ever read out of this window.
    findings
        .lock()
        .expect("no test poisons this")
        .late_activation_text = find_waiting(&ready.paragraphs[1]).map(|element| text_of(&element));

    // ------------------------------------------------------------ T2
    {
        let mut found = findings.lock().expect("no test poisons this");
        found.first_paragraph_text = Some(text_of(&first));
        found.second_paragraph_text =
            find_waiting(&ready.paragraphs[1]).map(|element| text_of(&element));
        found.empty_paragraph_text =
            find_waiting(&ready.paragraphs[2]).map(|element| text_of(&element));
        found.mixed_paragraph_text =
            find_waiting(&ready.paragraphs[3]).map(|element| text_of(&element));
        let pattern: IUIAutomationTextPattern =
            unsafe { first.GetCurrentPatternAs(UIA_TextPatternId) }.expect("the Text pattern");
        found.supported_text_selection = unsafe { pattern.SupportedTextSelection() }
            .ok()
            .map(|supported| supported.0);
    }

    // ------------------------------------------------------------ T1 (label edit)
    {
        let state = ask(Ask::RenameDocument(
            "Renamed while a client was reading".into(),
        ));
        let _ = state;
        let again = find(&ready.paragraphs[0]);
        let mut found = findings.lock().expect("no test poisons this");
        found.ids_survive_a_label_edit = again.is_some();
        found.renamed_label = find(&ready.body)
            .and_then(|element| unsafe { element.CurrentName() }.ok())
            .map(|name| name.to_string());
    }

    // ------------------------------------------------------------ T5 (arm the events)
    let events = Arc::new(Mutex::new(Vec::<i32>::new()));
    let sink: IUIAutomationEventHandler = EventSink(Arc::clone(&events)).into();
    for id in [
        UIA_Text_TextChangedEventId,
        UIA_Text_TextSelectionChangedEventId,
    ] {
        unsafe { uia.AddAutomationEventHandler(id, &root, TreeScope_Subtree, None, &sink) }
            .expect("an automation event handler");
    }
    let _handlers = scopeguard({
        let uia = uia.clone();
        let root = root.clone();
        let sink = sink.clone();
        move || {
            for id in [
                UIA_Text_TextChangedEventId,
                UIA_Text_TextSelectionChangedEventId,
            ] {
                let _ = unsafe { uia.RemoveAutomationEventHandler(id, &root, &sink) };
            }
        }
    });

    // ------------------------------------------------------------ T3
    // Real keyboard movement, and the client's own reading of where the caret
    // is. `caret_offset` measures the caret by asking UIA for the text before
    // it — never by moving a TextRange, which is not a caret move.
    let caret_offset = |element: &IUIAutomationElement| -> Option<usize> {
        let pattern: IUIAutomationTextPattern =
            unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }.ok()?;
        let selection = unsafe { pattern.GetSelection() }.ok()?;
        if unsafe { selection.Length() }.unwrap_or(0) == 0 {
            return None;
        }
        let range = unsafe { selection.GetElement(0) }.ok()?;
        let prefix = unsafe { pattern.DocumentRange() }.ok()?;
        unsafe {
            prefix
                .MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &range,
                    TextPatternRangeEndpoint_Start,
                )
                .ok()?;
        }
        let text = unsafe { prefix.GetText(-1) }.ok()?.to_string();
        Some(text.len())
    };

    // Start of paragraph one, then step.
    press(NamedKey::Home, ModifiersState::empty());
    let mut agreements = Vec::new();
    for (label, key, mods) in [
        ("character", NamedKey::ArrowRight, ModifiersState::empty()),
        ("character", NamedKey::ArrowRight, ModifiersState::empty()),
        ("word", NamedKey::ArrowRight, koru_keys::PRIMARY),
        ("visual line", NamedKey::ArrowDown, ModifiersState::empty()),
    ] {
        let state = press(key, mods);
        let observed = caret_offset(&first).unwrap_or(usize::MAX);
        agreements.push((label, state.caret, observed));
    }
    findings
        .lock()
        .expect("no test poisons this")
        .caret_agreements = agreements;

    // ------------------------------------------------------------ T4
    {
        let pattern: IUIAutomationTextPattern =
            unsafe { first.GetCurrentPatternAs(UIA_TextPatternId) }.expect("the Text pattern");
        let selection = unsafe { pattern.GetSelection() }.expect("a selection");
        let current = unsafe { selection.GetElement(0) }.expect("one range");
        findings
            .lock()
            .expect("no test poisons this")
            .selection_before = Some(unsafe { current.GetText(-1) }.expect("text").to_string());

        // Select the first word of the paragraph, the way an AT does: take a
        // degenerate range at the document start, expand it to a word, and ask
        // for it.
        let word = unsafe { pattern.DocumentRange() }.expect("a document range");
        unsafe {
            word.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &word,
                TextPatternRangeEndpoint_Start,
            )
        }
        .expect("a degenerate range");
        unsafe { word.ExpandToEnclosingUnit(TextUnit_Word) }.expect("one word");
        let wanted = unsafe { word.GetText(-1) }.expect("the word").to_string();
        unsafe { word.Select() }.expect("the AT selected it");

        // Give the loop a pass to resolve the request, then read it back from
        // the editor itself.
        let state = await_state(proxy, states, |state| {
            state
                .selection
                .map(|(a, b)| {
                    state.texts[0]
                        .get(a.min(b)..a.max(b))
                        .map(|slice| slice == wanted)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });
        let mut found = findings.lock().expect("no test poisons this");
        found.selection_after_set = state
            .selection
            .and_then(|(a, b)| state.texts[0].get(a.min(b)..a.max(b)).map(str::to_owned));
        drop(found);

        // And real typing replaces exactly that.
        let typed = ask(Ask::Press {
            key: Press::Char("Z".into()),
            mods: ModifiersState::empty(),
        });
        let mut found = findings.lock().expect("no test poisons this");
        found.typed_replacement = typed.texts.first().cloned();
        found.counters = Some(typed.clone());
    }

    // ------------------------------------------------------------ T6
    {
        // Down until the caret is in the second paragraph, then read it through
        // its own element and type there.
        let mut state = press(NamedKey::ArrowDown, ModifiersState::empty());
        let deadline = Instant::now() + TIMEOUT;
        while state.focus_row == 0 && Instant::now() < deadline {
            state = press(NamedKey::ArrowDown, ModifiersState::empty());
        }
        let landed = state.focus_row;
        let element = find_waiting(&ready.paragraphs[landed]).expect("the paragraph moved to");
        findings
            .lock()
            .expect("no test poisons this")
            .paragraph_hop_text = Some(text_of(&element));
        let typed = ask(Ask::Press {
            key: Press::Char("Q".into()),
            mods: ModifiersState::empty(),
        });
        findings
            .lock()
            .expect("no test poisons this")
            .typed_into_intended_paragraph = typed.texts.get(landed).cloned();
        // Back again, so the gate is a round trip rather than a one-way move.
        let back = press(NamedKey::ArrowUp, ModifiersState::empty());
        let _ = back;
        // Undo the stray character so later reads are about the fixture.
        let _ = ask(Ask::Press {
            key: Press::Char("z".into()),
            mods: koru_keys::PRIMARY,
        });
    }

    // ------------------------------------------------------------ T5 (collect)
    {
        std::thread::sleep(Duration::from_millis(400));
        let seen = events.lock().expect("no test poisons this");
        let mut found = findings.lock().expect("no test poisons this");
        found.text_changed_events = seen
            .iter()
            .filter(|id| **id == UIA_Text_TextChangedEventId.0)
            .count();
        found.selection_changed_events = seen
            .iter()
            .filter(|id| **id == UIA_Text_TextSelectionChangedEventId.0)
            .count();
    }

    // ------------------------------------------------------------ T8 (wrapping)
    {
        let pattern: IUIAutomationTextPattern =
            unsafe { first.GetCurrentPatternAs(UIA_TextPatternId) }.expect("the Text pattern");
        let line = unsafe { pattern.DocumentRange() }.expect("a document range");
        unsafe {
            line.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &line,
                TextPatternRangeEndpoint_Start,
            )
        }
        .expect("a degenerate range");
        unsafe { line.ExpandToEnclosingUnit(TextUnit_Line) }.expect("one visual line");
        let first_line = unsafe { line.GetText(-1) }.expect("the line").to_string();
        let mut lines = 1usize;
        while unsafe { line.Move(TextUnit_Line, 1) }.unwrap_or(0) != 0 {
            lines += 1;
            if lines > 64 {
                break;
            }
        }
        let mut found = findings.lock().expect("no test poisons this");
        found.wrapped_first_line = Some(first_line);
        found.wrapped_line_count = lines;
    }

    // ------------------------------------------------------------ T10
    // **An inline object is neither swallowed nor spoken as a storage anchor.**
    // A Koru pill reaches the text as one default-ignorable `U+2060`, and the
    // adapter implements no embedded-object support at all — so if the pill is
    // not a control of its own, a client meets it as nothing. This is the check
    // that it is one, and that a simple text edit in the same paragraph does
    // not destroy it.
    {
        let mixed = find_waiting(&ready.paragraphs[4]).expect("the mixed-content paragraph");
        let before = text_of(&mixed);
        let pill = find_waiting(&ready.pill);
        let (name, control) = match &pill {
            Some(element) => (
                unsafe { element.CurrentName() }.ok().map(|n| n.to_string()),
                unsafe { element.CurrentControlType() }.ok().map(|c| c.0),
            ),
            None => (None, None),
        };

        // A real edit in that paragraph: focus it, go home, type.
        let mut state = ask(Ask::State);
        let deadline = Instant::now() + TIMEOUT;
        while state.focus_row != 4 && Instant::now() < deadline {
            state = press(NamedKey::ArrowDown, ModifiersState::empty());
        }
        press(NamedKey::Home, ModifiersState::empty());
        let typed = ask(Ask::Press {
            key: Press::Char("A".into()),
            mods: ModifiersState::empty(),
        });
        let after_edit = find_waiting(&ready.pill).is_some();

        let mut found = findings.lock().expect("no test poisons this");
        found.pill_found_by_automation_id = pill.is_some();
        found.pill_name = name;
        found.pill_control_type = control;
        found.mixed_paragraph_before_edit = Some(before);
        found.mixed_paragraph_after_edit = typed.texts.get(4).cloned();
        found.pill_survived_the_edit = after_edit;
        drop(found);

        // Put the fixture back, so the dump below describes the document the
        // rest of this file has been reading.
        let _ = ask(Ask::Press {
            key: Press::Char("z".into()),
            mods: koru_keys::PRIMARY,
        });
    }

    // ------------------------------------------------------------ T9
    // **Does the dump tell the truth?** Everything else in this file asks the
    // provider a question and checks the answer. This asks the provider to
    // *describe itself*, then asks UI Automation the same questions about the
    // same update, and checks that the two agree. A dump that quietly described
    // an earlier tree — or a tree the code meant to publish rather than the one
    // it did — would be worse than no dump at all, because somebody would
    // believe it.
    {
        let state = ask(Ask::Dump);
        let dump = state.dump.clone().unwrap_or_default();
        let locator = ready.paragraphs[0].clone();

        // What the dump says about the first paragraph.
        let line = dump
            .lines()
            .find(|line| line.contains(&locator))
            .unwrap_or_default()
            .to_string();
        let node_id = field(&line, "#").and_then(|id| id.parse::<u64>().ok());
        let dump_bounds = field(&line, "bounds=").and_then(parse_bounds);
        // Its runs are the lines that name it as their parent, and their
        // character counts are the only thing a structural dump says about
        // text.
        let (runs, chars) = match node_id {
            Some(id) => {
                let want = format!("parent=#{id}");
                let runs: Vec<&str> = dump.lines().filter(|line| line.contains(&want)).collect();
                let chars: usize = runs
                    .iter()
                    .filter_map(|line| field(line, "chars="))
                    .filter_map(|n| n.parse::<usize>().ok())
                    .sum();
                (Some(runs.len()), Some(chars))
            }
            None => (None, None),
        };

        // And what a client sees of the same element.
        let rect = unsafe { first.CurrentBoundingRectangle() }.ok();
        let uia_bounds = rect.map(|rect| {
            (
                f64::from(rect.right - rect.left),
                f64::from(rect.bottom - rect.top),
            )
        });
        // The paragraph is ASCII by this point (T4 replaced "This " with "Z"),
        // so one AccessKit character is one UTF-16 code unit and the two counts
        // are directly comparable.
        let uia_chars = Some(text_of(&first).encode_utf16().count());

        let mut found = findings.lock().expect("no test poisons this");
        found.dump_locator_matches = !line.is_empty();
        found.dump_bounds = dump_bounds;
        found.uia_bounds = uia_bounds;
        found.dump_chars = chars;
        found.uia_chars = uia_chars;
        found.dump_run_count = runs;
        // Nothing a person wrote may be in a default dump. The fixture's own
        // strings are the search terms, which is the only way to assert this
        // without trusting the redaction code to describe itself.
        for probe in [
            WRAPPED,
            SECOND_AT_STARTUP,
            MIXED,
            "Renamed while a client was reading",
        ] {
            if dump.contains(probe) {
                found.dump_leaked_content.push(match probe {
                    p if p == WRAPPED => "the first paragraph",
                    p if p == SECOND_AT_STARTUP => "the second paragraph",
                    p if p == MIXED => "the non-ASCII paragraph",
                    _ => "the document title",
                });
            }
        }
        found.dump_text = Some(dump);
    }

    findings.lock().expect("no test poisons this").incremental = Some(ask(Ask::State));

    let _ = ask(Ask::Finish);
}

/// The value after `key` on one dump line, up to the next space.
fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(key)? + key.len();
    Some(
        line[at..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect(),
    )
}

/// `x,y,WxH` as `(w, h)`.
fn parse_bounds(text: String) -> Option<(f64, f64)> {
    let (_, size) = text.rsplit_once(',')?;
    let (w, h) = size.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Ask the loop for its state until a predicate holds, or the timeout runs out.
fn await_state(
    proxy: &EventLoopProxy<Wake>,
    states: &Receiver<EditorState>,
    want: impl Fn(&EditorState) -> bool,
) -> EditorState {
    let deadline = Instant::now() + TIMEOUT;
    let mut last = EditorState::default();
    while Instant::now() < deadline {
        if proxy.send_event(Wake::Ask(Ask::State)).is_err() {
            break;
        }
        match states.recv_timeout(Duration::from_secs(2)) {
            Ok(state) => {
                if want(&state) {
                    return state;
                }
                last = state;
            }
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    last
}

/// A real UIA event client. T5 is about *receipt*: an event this process
/// created and never delivered is not an event a screen reader hears.
#[windows::core::implement(windows::Win32::UI::Accessibility::IUIAutomationEventHandler)]
struct EventSink(Arc<Mutex<Vec<i32>>>);

impl windows::Win32::UI::Accessibility::IUIAutomationEventHandler_Impl for EventSink_Impl {
    fn HandleAutomationEvent(
        &self,
        _sender: windows::core::Ref<windows::Win32::UI::Accessibility::IUIAutomationElement>,
        event: windows::Win32::UI::Accessibility::UIA_EVENT_ID,
    ) -> windows::core::Result<()> {
        if let Ok(mut seen) = self.0.lock() {
            seen.push(event.0);
        }
        Ok(())
    }
}

/// Run a closure on the way out, whichever way the client leaves.
fn scopeguard(f: impl FnOnce()) -> impl Drop {
    struct Guard<F: FnOnce()>(Option<F>);
    impl<F: FnOnce()> Drop for Guard<F> {
        fn drop(&mut self) {
            if let Some(f) = self.0.take() {
                f();
            }
        }
    }
    Guard(Some(f))
}

// ------------------------------------------------------------------ test

#[test]
#[ignore = "interactive Windows proof: opens a real window and drives the real editor through UI Automation"]
fn an_external_client_reads_and_edits_korus_real_editor() {
    let events = EventLoop::<Wake>::with_user_event()
        // The harness runs tests on a worker thread; winit refuses to build an
        // event loop off the main thread on Windows without this.
        .with_any_thread(true)
        .build()
        .expect("an event loop");
    let proxy = events.create_proxy();
    let (ready_tx, ready_rx) = channel::<Ready>();
    let (state_tx, state_rx) = channel::<EditorState>();
    let findings = Arc::new(Mutex::new(Findings::default()));

    let client = {
        let proxy = proxy.clone();
        let findings = Arc::clone(&findings);
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                main_test_body(&ready_rx, &state_rx, &proxy, &findings);
            }));
            if let Err(panic) = outcome {
                let why = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "the client thread panicked".to_owned());
                findings.lock().expect("no test poisons this").error = Some(why);
                let _ = proxy.send_event(Wake::Ask(Ask::Finish));
            }
        })
    };

    let mut harness = Harness::new(proxy, state_tx, ready_tx);
    events.run_app(&mut harness).expect("the loop ran");
    client.join().expect("the client thread joined");

    let found = findings.lock().expect("no test poisons this");
    println!("{found:#?}");
    println!(
        "rebuilds before the client attached: {:?}",
        harness.rebuilds_before_attach
    );
    println!("routed: {:?}", harness.routed);
    assert_eq!(found.error, None, "the client could not finish");

    // ---- T1
    assert!(
        found.paragraph_found_by_automation_id,
        "T1: a client could not find a paragraph by its AutomationId"
    );
    assert!(
        found.second_occurrence_is_a_different_element,
        "T1: two occurrences of one paragraph are the same element"
    );
    assert!(
        found.ids_survive_a_label_edit,
        "T1: a label edit changed a paragraph's identity"
    );
    assert_eq!(
        found.renamed_label.as_deref(),
        Some("Renamed while a client was reading"),
        "T1: the label edit did not reach the client"
    );

    // ---- T2
    assert!(found.control_type_is_edit, "T2: a paragraph is not an Edit");
    assert_eq!(found.first_paragraph_text.as_deref(), Some(WRAPPED));
    assert_eq!(
        found.second_paragraph_text.as_deref(),
        Some(second_paragraph_after_edit().as_str())
    );
    assert_eq!(
        found.empty_paragraph_text.as_deref(),
        Some(EMPTY),
        "T2: an empty paragraph must read as empty, never as placeholder text"
    );
    assert_eq!(found.mixed_paragraph_text.as_deref(), Some(MIXED));
    assert_eq!(
        found.supported_text_selection,
        Some(1),
        "T2/T4: the paragraph must report SupportedTextSelection_Single"
    );

    // ---- T3
    for (unit, editor, client) in &found.caret_agreements {
        assert_eq!(
            editor, client,
            "T3: after {unit} movement the editor's caret and the client's \
             disagree"
        );
    }
    assert!(
        found
            .caret_agreements
            .iter()
            .any(|(_, editor, _)| *editor > 0),
        "T3: the caret never moved at all"
    );

    // ---- T4
    assert!(
        found.selection_before.is_some(),
        "T4: a client could not read the current selection"
    );
    let selected = found
        .selection_after_set
        .clone()
        .expect("T4: a SetSelection request selected nothing");
    assert_eq!(
        selected, "This ",
        "T4: the AT's SetSelection selected the wrong text"
    );
    let replaced = found
        .typed_replacement
        .clone()
        .expect("T4: nothing was typed");
    assert_eq!(
        replaced,
        WRAPPED.replacen("This ", "Z", 1),
        "T4: typing did not replace exactly the selection"
    );

    // ---- T5
    assert!(
        found.text_changed_events > 0,
        "T5: no native text-changed event was received by a UIA event client"
    );
    assert!(
        found.selection_changed_events > 0,
        "T5: no native selection-changed event was received"
    );

    // ---- T6
    assert!(
        found.paragraph_hop_text.is_some(),
        "T6: focus never left the first paragraph"
    );
    let typed_there = found
        .typed_into_intended_paragraph
        .clone()
        .expect("T6: nothing was typed after the hop");
    assert!(
        typed_there.contains('Q'),
        "T6: the character landed in a different paragraph: {typed_there:?}"
    );

    // ---- T8
    assert!(
        found.wrapped_line_count > 1,
        "T8: the wrapped paragraph reports one visual line"
    );
    let line = found.wrapped_first_line.clone().unwrap_or_default();
    assert!(
        !line.is_empty() && WRAPPED.replacen("This ", "Z", 1).starts_with(&line),
        "T8: the first visual line is not a prefix of the paragraph: {line:?}"
    );
    assert_eq!(
        found.late_activation_text.as_deref(),
        Some(second_paragraph_after_edit().as_str()),
        "T8: a late client was given the state the window started in"
    );
    assert_eq!(
        harness.rebuilds_before_attach,
        Some(0),
        "T8: a tree was built before any client attached"
    );

    // ---- the host's own accounting, which is what says the client's requests
    // reached the behaviour that already owns them rather than a shortcut.
    let counters = found
        .counters
        .clone()
        .expect("the client recorded the editor's state");
    assert_eq!(
        counters.activations, 1,
        "exactly one client attached and was given exactly one full snapshot"
    );
    assert!(
        counters.actions_routed >= 1,
        "the AT's SetSelection never reached the host's routing"
    );
    assert_eq!(
        counters.actions_refused, 0,
        "an action was refused during a run in which nothing went stale"
    );
    assert!(
        counters.rebuilds >= 1,
        "no tree was ever built for the attached client"
    );
    assert!(
        counters.line_counts[0] > 1,
        "the wrapped fixture did not wrap in the editor either"
    );
    // UIA sends a `Focus` before it invokes anything, whether or not the node
    // advertised one; the harness performs `Focus` and `SetTextSelection` and
    // counts everything else. The gate is that nothing was *misrouted*, which
    // is what the locators say.
    println!(
        "ignored actions: {}, locators routed to: {:?}",
        counters.ignored, counters.routed
    );
    assert!(
        counters
            .routed
            .iter()
            .all(|locator| locator.starts_with("koru:")),
        "a request was routed to something that is not a Koru occurrence"
    );

    // ---- T10: the inline object
    assert!(
        found.pill_found_by_automation_id,
        "a pill is invisible to a client: it reaches one only as its `U+2060` \
         anchor, which is exactly what section 5 forbids"
    );
    assert_eq!(
        found.pill_name.as_deref(),
        Some("Aug 2"),
        "the pill did not carry its own label"
    );
    assert!(
        found.pill_control_type.is_some(),
        "the pill is not in the control view at all"
    );
    let before = found
        .mixed_paragraph_before_edit
        .clone()
        .unwrap_or_default();
    assert!(
        before.starts_with("Due "),
        "the mixed-content paragraph did not read as authored: {before:?}"
    );
    assert!(
        found.pill_survived_the_edit,
        "a plain keystroke in the same paragraph destroyed the inline object"
    );
    let after = found.mixed_paragraph_after_edit.clone().unwrap_or_default();
    assert!(
        after.starts_with('A') && after.contains("and then some more text after it."),
        "the edit did not land where it was typed: {after:?}"
    );

    // ---- T9: the dump, against the same update seen from outside
    let dump = found.dump_text.clone().unwrap_or_default();
    println!("---- the requested dump ----\n{dump}");
    assert!(
        found.dump_locator_matches,
        "the dump does not describe the paragraph the client is holding"
    );
    assert!(
        found.dump_leaked_content.is_empty(),
        "a default dump contained document text: {:?}",
        found.dump_leaked_content
    );
    assert!(
        dump.contains("scope=whole"),
        "the dump did not say which kind of update it describes"
    );
    let (dw, dh) = found
        .dump_bounds
        .expect("the dump gave the paragraph no bounds");
    let (uw, uh) = found.uia_bounds.expect("UIA gave the paragraph no bounds");
    assert!(
        (dw - uw).abs() <= 2.0 && (dh - uh).abs() <= 2.0,
        "the dump and UI Automation disagree about the paragraph's size: \
         dump {dw}x{dh}, client {uw}x{uh}"
    );
    assert_eq!(
        found.dump_chars, found.uia_chars,
        "the dump and UI Automation disagree about how many characters the \
         paragraph has"
    );
    assert!(
        found.dump_run_count.unwrap_or_default() > 1,
        "a paragraph that wraps onto four visual lines was dumped as one run"
    );

    // ---- and the incremental accounting, which is what this plan is for
    let incremental = found
        .incremental
        .clone()
        .expect("the client asked for the window's final state");
    println!(
        "updates: {} rebuilds, {} of them differences; {} node ids published; \
         {} paragraph projections shaped",
        incremental.rebuilds, incremental.deltas, incremental.nodes_published, incremental.shapings,
    );
    assert!(
        incremental.deltas > 0,
        "every update in the run was a whole tree; nothing incremental happened"
    );
    assert!(
        incremental.shapings < incremental.rebuilds * 4,
        "the projection reshaped every paragraph on every publish: {} shapings \
         over {} publishes of a four-paragraph fixture",
        incremental.shapings,
        incremental.rebuilds
    );
}
