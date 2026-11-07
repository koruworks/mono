// SPDX-License-Identifier: MPL-2.0

//! **A prepared snapshot of one window, turned into one AccessKit update.**
//!
//! Two rules shape everything here.
//!
//! **The input is already in memory.** [`WindowSemantics`] is a plain value:
//! no `Workspace`, no store session, no editor. It cannot read the Store,
//! cannot fetch, and — because a view contributes through `&self` — cannot
//! reach `Editing::reflow`, which needs `&mut`. That is the seam the
//! roadmap's "semantic queries read prepared in-memory state" is asserted
//! through: the types make the other thing unwriteable rather than a rule
//! somebody has to remember.
//!
//! **AccessKit's schema is the vocabulary.** A role is an
//! [`accesskit::Role`], an action an [`accesskit::Action`]. Koru adds exactly
//! two things AccessKit has no opinion about: the [`Occurrence`] that says
//! *which* control this is, and a rectangle in the logical coordinates every
//! Koru draw call already uses. Cloning the rest of AccessKit's schema into a
//! second UI framework was considered and rejected — see ADR-336.
//!
//! ## Coordinates are converted exactly once
//!
//! Koru lays out in **logical** pixels. `accesskit_windows` adds
//! `ClientToScreen(hwnd)` — physical screen pixels — to whatever bounds it is
//! given, so the bounds it wants are **physical pixels relative to the client
//! area's top-left**. [`build`] is the one place that multiplies by the scale
//! factor, and no other part of this feature touches a scale.

use std::collections::HashMap;

use accesskit::{Action, Node, NodeId, Rect, Role, TextSelection, Tree, TreeId, TreeUpdate};
use koru_draw::geometry::Rect as LogicalRect;

use super::identity::{Occurrence, WindowSlot};

/// The runs of one text control, published or merely named.
///
/// **The delta's inner half.** An AccessKit node is replaced whole by an
/// update, so a paragraph whose caret moved has to be re-sent *with its
/// children* — but its runs did not change and re-sending them would copy every
/// character position in the paragraph to move a caret. [`Retained`](Self::Retained)
/// is the difference: the same ids, in the same order, and nothing else.
///
/// A run republished with identical properties raises no event
/// (`guide/contributing/deps/ACCESSKIT.md`), so this is a cost decision rather than a
/// correctness one — but at a thousand paragraphs the cost *is* the
/// correctness, which is what §5 says about not rebuilding a text tree on a
/// caret move.
#[derive(Clone, Debug, PartialEq)]
pub enum Runs {
    /// Publish these nodes. New runs, changed runs, or runs that moved.
    Published(Vec<(NodeId, Node)>),
    /// The client already has these runs. Name them so their parent keeps its
    /// children, and publish nothing.
    Retained(Vec<NodeId>),
}

impl Runs {
    /// The ids, whichever form this is.
    pub fn ids(&self) -> Vec<NodeId> {
        match self {
            Self::Published(runs) => runs.iter().map(|(id, _)| *id).collect(),
            Self::Retained(ids) => ids.clone(),
        }
    }

    /// How many runs this names. A counter a test can read.
    pub fn len(&self) -> usize {
        match self {
            Self::Published(runs) => runs.len(),
            Self::Retained(ids) => ids.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The text a control exposes, as AccessKit describes text.
///
/// **Built by whoever owns the text, not here.** What a paragraph is, where its
/// lines break and which byte a caret sits on are the editor's knowledge, and
/// this module is deliberately ignorant of all three (ADR-336). What is the
/// host's is what is the host's for every other node: the coordinate space, and
/// the fact that these runs are *children* of the control that carries them.
///
/// A run is `accesskit::Role::TextRun` and is never a control — the platform's
/// consumer filters that role out of the control view, and a client meets one
/// only inside a text position — which is why it has no [`Occurrence`]: there
/// is nothing for a screen reader to *be on*, so there is no occurrence to
/// name.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBody {
    /// The runs, in reading order — either the nodes to publish or, when the
    /// client already has them byte for byte, only their ids. See [`Runs`].
    pub runs: Runs,
    /// The editor's caret or selection, already expressed against
    /// [`runs`](Self::runs). `None` when this control does not hold the caret,
    /// which is what tells a client there is no selection here rather than an
    /// empty one.
    pub selection: Option<TextSelection>,
    /// Whether the text may be changed. Read-only is a fact a screen reader
    /// announces, so it is stated rather than left to be inferred.
    pub read_only: bool,
}

/// One control, as the window's owner already knows it.
///
/// Built fresh each time the tree is published, from state that is already
/// laid out. Nothing here is stored, and nothing here is a projection of the
/// document that outlives the update it goes into.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticNode {
    /// Which control this is. Never derived from the label.
    pub occurrence: Occurrence,
    pub role: Role,
    /// What a screen reader says. Localized, and deliberately not part of any
    /// identity.
    pub label: Option<String>,
    /// **What the control currently holds**, for a control whose name and
    /// contents are two different things — a field named *Workspace name*
    /// whose value is what somebody typed.
    ///
    /// `accesskit_windows` answers `UIA_ValueValuePropertyId` from it and the
    /// node acquires UIA's `ValuePattern`. Two things worth knowing before
    /// setting it. The pattern is offered whenever a value is *present*, not
    /// when an action is advertised — so a client may send
    /// [`Action::SetValue`] whatever this node offers, and an owner that
    /// leaves it unhandled is telling the client a lie it can act on. And
    /// [`Role::Label`] is the exception in both directions: it gets **no**
    /// `ValuePattern`, and its accessible *name* is read out of its value
    /// rather than its label — `accesskit_consumer::Node::label_comes_from_value`
    /// is `role() == Role::Label`. A caller does not have to know that
    /// ([`build`] fills a label's value in from its label), but a caller that
    /// sets both is choosing which one a Windows client hears. Established by
    /// experiment; see `guide/contributing/deps/ACCESSKIT.md`.
    pub value: Option<String>,
    /// Where it is, in this window's **logical** coordinates. `None` for a
    /// control whose owner has not published its geometry yet; AccessKit
    /// treats that as an empty rectangle, which is legal and is what the
    /// window commands carry until the view that draws them contributes their
    /// rects.
    pub bounds: Option<LogicalRect>,
    /// What may be asked of it. Advertise only what a behaviour owner will
    /// actually perform: an action a client can see and that does nothing is
    /// worse than one that is absent.
    pub actions: Vec<Action>,
    pub disabled: bool,
    /// The text this control exposes, when it is a text control. `None` for
    /// every control that is not one — a button, a tab, a container.
    pub text: Option<TextBody>,
    /// Selected or not — a tab in its strip, the open row in its tree. `None`
    /// means *the concept does not apply here*, which AccessKit asks for
    /// explicitly: a `false` makes a screen reader say "not selected", and
    /// saying that about a button is noise.
    ///
    /// **Setting it at all costs the node its Invoke pattern**, on any
    /// platform: `accesskit_consumer`'s `is_invocable` excludes a node whose
    /// `is_selected()` is `Some`, on the argument that selecting *is* the
    /// action. A client selects it through `SelectionItem` instead, which
    /// `accesskit_windows` turns straight back into [`Action::Click`] — but
    /// only when the value would actually change, so `Select` on the row that
    /// is already selected sends nothing. Found by the interactive probe; see
    /// `guide/contributing/deps/ACCESSKIT.md`.
    pub selected: Option<bool>,
    /// Ticked, not ticked, or partly — a checkbox's whole state. `None` means
    /// *this control is not a toggle*, which is the answer for everything but
    /// one.
    ///
    /// **It reaches a Windows client, unlike `expanded`.** `accesskit_windows`
    /// offers `UIA_TogglePatternId` whenever `toggled().is_some()` and the node
    /// is not a selection item, answers `UIA_ToggleToggleStatePropertyId` from
    /// it, and turns the pattern's own `Toggle()` straight back into
    /// [`Action::Click`] — so a client reads the state *and* has a gesture that
    /// arrives as the press the pointer already makes.
    ///
    /// **Setting it costs the node its Invoke pattern**, the same clause
    /// `selected` falls under: `is_invocable` excludes a node that
    /// `supports_toggle`, on the argument that a control which keeps state is a
    /// toggle rather than something invoked. That is an even trade here and not
    /// the loss `expanded` is, because Toggle replaces Invoke and performs the
    /// same click; it is a reason not to set it on a control that is not
    /// actually a checkbox. See `guide/contributing/deps/ACCESSKIT.md`.
    pub toggled: Option<accesskit::Toggled>,
    /// Depth in a tree, one-based, so a reader can say *level 2*.
    ///
    /// **Inert on Windows today.** `accesskit_windows` 0.32.1's property table
    /// has no `UIA_LevelPropertyId` row, so nothing carries this to a client.
    /// It is published anyway because it is right in the schema and costs a
    /// `usize`; the interactive probe records that it does not arrive.
    pub level: Option<usize>,
    /// **This one, counting from zero.** AccessKit's `position_in_set` is
    /// zero-based and `accesskit_windows` adds one on the way out, so a node
    /// that stored a one-based number would be read as *2 of …* for the first
    /// item. Found by the interactive probe.
    ///
    /// **Counts what exists, not what is on screen** — an item scrolled out of
    /// view is still item 7 of 12.
    pub position: Option<usize>,
    /// How many items the container holds. **On the container, not the item**:
    /// `accesskit_consumer` answers an item's `size_of_set` by walking up to
    /// its selection container and asking *that*, so a total set on the item
    /// is discarded. The container also has to be a role AccessKit counts as
    /// one — `Tree`, `TabList`, `Menu`, `ListBox` and their neighbours.
    pub size_of_set: Option<usize>,
    /// A surface that owns everything under it. A screen reader keeps the
    /// reader inside a modal, which is the other half of publishing nothing
    /// beneath one.
    pub modal: bool,
    /// **Still being made** — a picture loading into the place this node
    /// stands for. AccessKit's `busy`, which a screen reader reads as the
    /// content not being ready yet rather than as an empty control.
    pub busy: bool,
    /// The children to publish. In a whole snapshot this is every child; in a
    /// delta it is only the ones that changed, and
    /// [`child_order`](Self::child_order) then says what the full list is.
    /// **A node whose label a screen reader speaks when it changes**, without
    /// the person having navigated to it. `None` — the ordinary case, and the
    /// default at the root — is silence.
    ///
    /// Three facts, all read out of `accesskit_windows` 0.32.1 and
    /// `accesskit_consumer` 0.35.0 and recorded in `guide/contributing/deps/ACCESSKIT.md`,
    /// because none of them is in either crate's documentation:
    ///
    /// - **`UIA_LiveRegionChangedEventId` fires when the node's *name*
    ///   changes**, when a live node that has a name is added, or when the
    ///   setting itself changes. A node republished with an identical label
    ///   raises nothing. That is what makes *announce once* a property of the
    ///   adapter rather than a rule a caller has to remember — and it is why a
    ///   live node's label must carry news, never a status that moves with
    ///   every keystroke.
    /// - **It is inherited.** `accesskit_consumer::Node::live` falls through to
    ///   the parent, so setting it on a container makes every descendant live
    ///   as well. Set it on the leaf that speaks, never on a group.
    /// - **A live node with no label says nothing** — the adapter's guard is
    ///   `wrapper.name().is_some()` — so an announcement is withdrawn by
    ///   clearing its label just as well as by removing the node.
    pub live: Option<accesskit::Live>,
    pub children: Vec<SemanticNode>,
    /// This node's **complete** ordered child list, when it differs from what
    /// the client already has.
    ///
    /// `None` — the ordinary case — means the children in
    /// [`children`](Self::children) *are* the list, which is what a whole
    /// snapshot always means and what a delta means for a node whose children
    /// did not move. It is `Some` only when a delta re-publishes a container
    /// because rows were inserted, removed or reordered: an AccessKit node is
    /// replaced whole, so a container re-sent without its full child list would
    /// lose every child it did not carry.
    pub child_order: Option<Vec<NodeId>>,
}

impl SemanticNode {
    pub fn new(occurrence: Occurrence, role: Role) -> Self {
        Self {
            occurrence,
            role,
            label: None,
            value: None,
            bounds: None,
            actions: Vec::new(),
            disabled: false,
            text: None,
            selected: None,
            toggled: None,
            level: None,
            position: None,
            size_of_set: None,
            modal: false,
            busy: false,
            live: None,
            children: Vec::new(),
            child_order: None,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// What the control holds, as distinct from what it is called. See
    /// [`value`](SemanticNode::value): a `Role::Label` gets no `ValuePattern`,
    /// and `build` fills its value in from its label so a Windows client hears
    /// a name at all.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Announce a change here without being asked. See
    /// [`live`](SemanticNode::live): the announcement follows the **label**.
    pub fn with_live(mut self, live: accesskit::Live) -> Self {
        self.live = Some(live);
        self
    }

    /// Content still on its way. See [`busy`](SemanticNode::busy).
    pub fn with_busy(mut self) -> Self {
        self.busy = true;
        self
    }

    pub fn with_bounds(mut self, bounds: LogicalRect) -> Self {
        self.bounds = Some(bounds);
        self
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_actions(mut self, actions: impl IntoIterator<Item = Action>) -> Self {
        self.actions.extend(actions);
        self
    }

    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// A checkbox's state. See [`toggled`](SemanticNode::toggled): this is the
    /// one flag that trades Invoke for a pattern that does the same thing, so
    /// it belongs on a control that really is a checkbox and nowhere else.
    pub fn with_toggled(mut self, ticked: bool) -> Self {
        self.toggled = Some(if ticked {
            accesskit::Toggled::True
        } else {
            accesskit::Toggled::False
        });
        self
    }

    pub fn with_level(mut self, level: usize) -> Self {
        self.level = Some(level);
        self
    }

    /// Where this item sits among its siblings, **counting from zero**. See
    /// [`position`](SemanticNode::position) for why the caller does not add one.
    pub fn with_position(mut self, index: usize) -> Self {
        self.position = Some(index);
        self
    }

    /// How many items this **container** holds. See
    /// [`size_of_set`](SemanticNode::size_of_set) for why it is not on the item.
    pub fn with_size_of_set(mut self, total: usize) -> Self {
        self.size_of_set = Some(total);
        self
    }

    pub fn modal(mut self) -> Self {
        self.modal = true;
        self
    }

    /// Say this node's label out loud whenever it *changes*. See
    /// [`live`](SemanticNode::live) — including why it belongs on the leaf that
    /// speaks rather than on the group around it.
    pub fn announcing(mut self, live: accesskit::Live) -> Self {
        self.live = Some(live);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn with_children(mut self, children: Vec<SemanticNode>) -> Self {
        self.children = children;
        self
    }

    pub fn with_text(mut self, text: TextBody) -> Self {
        self.text = Some(text);
        self
    }

    /// State this node's complete child list while publishing only some of
    /// them. See [`child_order`](Self::child_order).
    pub fn with_child_order(mut self, order: Vec<NodeId>) -> Self {
        self.child_order = Some(order);
        self
    }
}

/// Whether a snapshot is the whole window or only what changed.
///
/// **`accesskit::TreeUpdate` is already a delta** — `nodes` means "the nodes
/// that changed", the consumer keeps the rest, and `accesskit_windows` raises
/// its text and selection events by diffing one update against the last. So
/// nothing in the platform layer moves to support this; what moves is who is
/// allowed to leave a node out.
///
/// The rule §5 states as "activation may build the initial tree in O(current
/// document); ordinary edits update affected semantic content" is exactly these
/// two variants, and which one is owed is [`Poll::full`](super::Poll::full)'s
/// answer, not a caller's guess.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every control this window offers, and the tree root with it. What a
    /// newly attached client is owed, and the only thing that may follow a
    /// client detaching.
    #[default]
    Whole,
    /// Only the subtrees that changed. The root and the `Tree` are left out —
    /// the client already has them — and [`WindowSemantics::retired`] names
    /// what has gone.
    Delta,
}

/// Everything one window offers a screen reader right now — or, in a
/// [`Scope::Delta`], everything about it that changed.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSemantics {
    pub window: WindowSlot,
    /// The window's accessible name. On Windows the adapter would fall back to
    /// the OS window title; it is set explicitly so every platform says the
    /// same thing.
    pub label: String,
    /// Logical pixels to device pixels. Applied here and nowhere else.
    pub scale: f64,
    /// The client area, in logical coordinates, so the root has real bounds.
    pub client: LogicalRect,
    /// Which control holds the keyboard. `None` means the window itself, which
    /// is the honest answer while no inner control has claimed focus.
    pub focus: Option<Occurrence>,
    pub children: Vec<SemanticNode>,
    /// Whether this is the whole window or a difference. See [`Scope`].
    pub scope: Scope,
    /// Controls this update removes, for a [`Scope::Delta`].
    ///
    /// The consumer drops a node the moment its parent stops naming it, so this
    /// changes nothing about the tree — what it changes is the *occurrence map*
    /// a later action is resolved against. A menu that closed has to stop being
    /// routable, and in a delta nothing else says so.
    pub retired: Vec<Occurrence>,
}

impl WindowSemantics {
    /// The same snapshot, declared as a difference rather than a whole window.
    pub fn as_delta(mut self, retired: Vec<Occurrence>) -> Self {
        self.scope = Scope::Delta;
        self.retired = retired;
        self
    }
}

/// One published tree, and what its node ids mean.
///
/// The map is the whole of stale-action refusal: an action naming an id that
/// is not in the tree currently on screen has nothing to be routed to, whether
/// because a menu closed, a tab went, or the client is replaying.
#[derive(Debug)]
pub struct Projection {
    pub update: TreeUpdate,
    pub occurrences: HashMap<NodeId, Occurrence>,
    /// Nodes published, root included. A number a counter test can read.
    pub nodes: usize,
    /// Two occurrences that wanted the same node id. Always a bug — either a
    /// caller published one control twice, or two locators collided — so it is
    /// counted rather than silently resolved.
    pub duplicate_ids: usize,
    /// The focus occurrence named a control that is not in the tree. Also a
    /// bug, and one a screen reader would otherwise show as a window with no
    /// focus at all.
    pub unresolved_focus: bool,
    /// Node ids this update takes out of the window, from
    /// [`WindowSemantics::retired`]. Nothing in the `TreeUpdate` carries them —
    /// the consumer drops an unreachable node by itself — so they are here for
    /// the occurrence map, which is what a later action is resolved against.
    pub retired: Vec<NodeId>,
}

/// An update with nothing in it, so [`build`] can fill a value in as it goes.
/// `TreeUpdate` has no `Default` of its own — an update with no root is not a
/// tree, and the type is right to refuse to produce one.
fn empty_update() -> TreeUpdate {
    TreeUpdate {
        nodes: Vec::new(),
        tree: None,
        tree_id: TreeId::ROOT,
        focus: NodeId(0),
    }
}

/// Turn a snapshot into the update the adapter publishes.
///
/// `known` is the occurrence map this window has *already* published — what the
/// client is holding. A [`Scope::Whole`] build ignores it, a full snapshot
/// being authoritative on its own; a [`Scope::Delta`] build needs it to answer
/// one question honestly, which is whether the focused control exists at all
/// when this update does not carry it.
pub fn build(semantics: &WindowSemantics, known: &HashMap<NodeId, Occurrence>) -> Projection {
    let root_occurrence = Occurrence::window_root(semantics.window);
    let root_id = root_occurrence.node_id();
    let whole = semantics.scope == Scope::Whole;

    let mut projection = Projection {
        update: empty_update(),
        occurrences: HashMap::new(),
        nodes: 0,
        duplicate_ids: 0,
        unresolved_focus: false,
        retired: semantics
            .retired
            .iter()
            .map(|occurrence| occurrence.node_id())
            .collect(),
    };
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();

    let children = semantics
        .children
        .iter()
        .map(|child| {
            append(child, &mut nodes, &mut projection, semantics.scale);
            child.occurrence.node_id()
        })
        .collect::<Vec<_>>();

    if whole {
        let mut root = Node::new(Role::Window);
        root.set_label(semantics.label.clone());
        root.set_author_id(root_occurrence.locator());
        // A second stable, untranslated native property to find a Koru window
        // by. `accesskit_windows` maps this onto `UIA_ClassNamePropertyId`.
        root.set_class_name("KoruWindow");
        root.set_bounds(physical(semantics.client, semantics.scale));
        root.set_children(children);
        record(root_id, root_occurrence, &mut projection);
        nodes.push((root_id, root));
    }

    // A delta's focus may name a control the delta does not carry — the
    // ordinary case, since a caret move inside a paragraph republishes that
    // paragraph and nothing else. The question is whether the client holds it
    // *anywhere*, which is what `known` answers and what a single update
    // cannot. **Less what this update retires**: `known` is the map *before*
    // it, and a control the delta takes away is gone once the client applies
    // it — naming it as the focus panics AccessKit (switching profile retired
    // the focused row and took the window down).
    let holds = |id: &NodeId| {
        projection.occurrences.contains_key(id)
            || (!whole && known.contains_key(id) && !projection.retired.contains(id))
    };
    let focus = match semantics.focus {
        None => root_id,
        Some(occurrence) => {
            let id = occurrence.node_id();
            if holds(&id) {
                id
            } else {
                projection.unresolved_focus = true;
                root_id
            }
        }
    };

    projection.nodes = nodes.len();
    projection.update = TreeUpdate {
        nodes,
        // **The root is the tree, and a delta does not re-declare it.** Sending
        // it again would be harmless and cheap; leaving it out is what makes
        // "this update is a difference" a fact the value carries rather than a
        // convention a caller has to remember.
        tree: whole.then(|| Tree {
            root: root_id,
            toolkit_name: Some("Koru".to_string()),
            toolkit_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        tree_id: TreeId::ROOT,
        focus,
    };
    projection
}

fn append(
    node: &SemanticNode,
    nodes: &mut Vec<(NodeId, Node)>,
    projection: &mut Projection,
    scale: f64,
) {
    let id = node.occurrence.node_id();
    let mut built = Node::new(node.role);
    built.set_author_id(node.occurrence.locator());
    if let Some(label) = &node.label {
        built.set_label(label.clone());
    }
    // **A `Role::Label` reads its accessible name out of its *value*.**
    //
    // `accesskit_windows`' `NodeWrapper::name` is
    // `if label_comes_from_value() { write_value() } else { write_label() }`,
    // and `label_comes_from_value` is `role() == Role::Label`. So a label
    // carrying only a `label` is **nameless to a Windows client** — the
    // property comes back as an empty string, which is what an inspector shows
    // and what a screen reader reads. Found by the first-run probe (Plan U0.1),
    // which asked a live status for its name and got `""` back;
    // `guide/contributing/deps/ACCESSKIT.md` has the reading.
    //
    // Filled in here rather than at every call site: a caller says
    // `.with_label(…)` about a label because that is what it is, and one place
    // that knows the adapter's rule is better than a rule every projection has
    // to remember. Other platforms read `label`, so both are set.
    let value = match (&node.value, node.role, &node.label) {
        (Some(value), _, _) => Some(value.clone()),
        (None, Role::Label, Some(label)) => Some(label.clone()),
        (None, _, _) => None,
    };
    if let Some(value) = value {
        built.set_value(value);
    }
    if let Some(bounds) = node.bounds {
        built.set_bounds(physical(bounds, scale));
    }
    for action in &node.actions {
        built.add_action(*action);
    }
    if node.disabled {
        built.set_disabled();
    }
    if let Some(selected) = node.selected {
        built.set_selected(selected);
    }
    if let Some(toggled) = node.toggled {
        built.set_toggled(toggled);
    }
    if let Some(level) = node.level {
        built.set_level(level);
    }
    if let Some(position) = node.position {
        built.set_position_in_set(position);
    }
    if let Some(total) = node.size_of_set {
        built.set_size_of_set(total);
    }
    if node.modal {
        built.set_modal();
    }
    if node.busy {
        built.set_busy();
    }
    if let Some(live) = node.live {
        built.set_live(live);
    }
    // Text runs come first, because reading order is child order and the text
    // of a control precedes anything nested after it. Built once: recursing
    // into `node.children` twice would publish every descendant twice and
    // count each of them as a duplicate id.
    let mut children = Vec::new();
    if let Some(text) = &node.text {
        match &text.runs {
            Runs::Published(runs) => {
                for (run_id, run) in runs {
                    let mut run = run.clone();
                    scale_text_run(&mut run, scale);
                    children.push(*run_id);
                    nodes.push((*run_id, run));
                }
            }
            // Named, not copied: the client already holds these nodes, and the
            // control is being republished because something *about it*
            // changed — its caret, usually.
            Runs::Retained(ids) => children.extend(ids.iter().copied()),
        }
        if let Some(selection) = text.selection {
            built.set_text_selection(selection);
        }
        if text.read_only {
            built.set_read_only();
        }
    }
    let published: Vec<NodeId> = node
        .children
        .iter()
        .map(|child| {
            append(child, nodes, projection, scale);
            child.occurrence.node_id()
        })
        .collect();
    match &node.child_order {
        // A delta republishing a container: the list is what the client must
        // end up holding, not what this update happens to carry.
        Some(order) => children.extend(order.iter().copied()),
        None => children.extend(published),
    }
    if !children.is_empty() {
        built.set_children(children);
    }
    record(id, node.occurrence, projection);
    nodes.push((id, built));
}

fn record(id: NodeId, occurrence: Occurrence, projection: &mut Projection) {
    // Any repeat is a bug, and the two kinds are worth telling apart in a
    // debugger but not in the count: either two locators hashed to one id, or
    // one control was published twice and would then have two parents.
    if projection.occurrences.insert(id, occurrence).is_some() {
        projection.duplicate_ids += 1;
    }
}

/// The same conversion, applied to a text run's three geometries.
///
/// A run carries its box like any other node, and *also* the offset and advance
/// of every character within that box — which `accesskit_consumer` adds to
/// `bounds.x0` to answer `GetBoundingRectangles`. All three are in the same
/// space, so all three scale, and forgetting the last two puts a screen
/// magnifier's highlight in the wrong place on any display that is not at 100%
/// — visibly wrong, and only on a machine nobody develops on.
fn scale_text_run(run: &mut Node, scale: f64) {
    if (scale - 1.0).abs() < f64::EPSILON {
        return;
    }
    let factor = scale as f32;
    if let Some(bounds) = run.bounds() {
        run.set_bounds(Rect::new(
            bounds.x0 * scale,
            bounds.y0 * scale,
            bounds.x1 * scale,
            bounds.y1 * scale,
        ));
    }
    if let Some(positions) = run.character_positions() {
        let scaled: Vec<f32> = positions.iter().map(|at| at * factor).collect();
        run.set_character_positions(scaled);
    }
    if let Some(widths) = run.character_widths() {
        let scaled: Vec<f32> = widths.iter().map(|w| w * factor).collect();
        run.set_character_widths(scaled);
    }
}

/// Logical window coordinates to the physical, client-relative rectangle
/// AccessKit hands the platform. The one conversion in this feature.
fn physical(rect: LogicalRect, scale: f64) -> Rect {
    let x0 = f64::from(rect.x) * scale;
    let y0 = f64::from(rect.y) * scale;
    Rect::new(
        x0,
        y0,
        x0 + f64::from(rect.w) * scale,
        y0 + f64::from(rect.h) * scale,
    )
}
