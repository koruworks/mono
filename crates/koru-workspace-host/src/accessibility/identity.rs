// SPDX-License-Identifier: MPL-2.0

//! **What a screen reader is pointing at, said in a way that cannot drift.**
//!
//! Model identity stays in `koru-core`; this is *presentation* identity, and
//! the two answer different questions. A block has one node id however many
//! times it is on screen. A person navigating with a screen reader is in one
//! window, in one tab, on one control — and moving to the same block in a
//! second window has to be a move, not a no-op. So an [`Occurrence`] is scoped
//! by **window, view, object and control part**, which is exactly the four
//! things that can differ while the model id does not.
//!
//! ## Nothing here is a name
//!
//! Every component is a number or an ASCII keyword the code chose. A label is
//! translated, renamed and reordered; an identity is none of those things, and
//! a locator that quoted a label would change under a client the moment
//! somebody switched language. [`Occurrence::locator`] is asserted to be
//! byte-identical across all eight languages.
//!
//! ## Two representations, and why both exist
//!
//! - [`Occurrence::locator`] is the **string** — `koru:w1/v2/n:<uuid>#label` —
//!   and it is what an external UIA client reads, because
//!   `accesskit_windows` maps `Node::author_id` onto
//!   `UIA_AutomationIdPropertyId`. That is the supported native property the
//!   roadmap requires a locator to land on; see `guide/contributing/deps/ACCESSKIT.md`.
//! - [`Occurrence::node_id`] is the **number** AccessKit addresses nodes with.
//!   `accesskit::NodeId` is a `u64`, so it cannot hold the four components and
//!   is a hash of the locator instead. It is an in-process handle, not a
//!   contract: a UIA client sees it only inside `GetRuntimeId`, which UIA
//!   itself documents as valid for one session.

use std::fmt::{self, Write as _};

use uuid::Uuid;

/// One window instance, numbered once and never reused.
///
/// Not the winit `WindowId`: that is opaque, may be recycled by the platform,
/// and says nothing when the window it named is gone. A number that is only
/// ever handed out is what lets a request arriving for a closed window be
/// *refused* rather than land on whichever window happens to still be open.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowSlot(u64);

impl WindowSlot {
    /// For tests and for the one caller that mints them.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// One view (a tab, a pane, a transient menu) inside a window.
///
/// A *slot*, not a position: reordering tabs moves what is drawn where and
/// changes no identity. A menu that closes and reopens is a new instance and
/// takes a new slot, which is what stops a queued action for the old one from
/// being routed to the new one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewSlot(u64);

impl ViewSlot {
    /// The window itself — its root and its own window commands, which belong
    /// to no view.
    pub const WINDOW: Self = Self(0);

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Which control of an object, when one object shows as more than one.
///
/// A `&'static str` rather than a closed enum, because every later plan adds
/// parts and a shared enum would make each of them edit this file. It is
/// deliberately not a `String`: a part is a keyword the code chose, so it can
/// be a literal, and a literal cannot accidentally be a translated label.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControlPart(&'static str);

impl ControlPart {
    /// The whole of the object — the ordinary case.
    pub const ROOT: Self = Self("root");

    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for ControlPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// The window's own parts, which belong to no view and no model object.
pub mod part {
    use super::ControlPart;

    /// The window root — the node a UIA client reaches through
    /// `ElementFromHandle`.
    pub const WINDOW: ControlPart = ControlPart::new("window");
    pub const MINIMIZE: ControlPart = ControlPart::new("minimize");
    pub const MAXIMIZE: ControlPart = ControlPart::new("maximize");
    pub const CLOSE: ControlPart = ControlPart::new("close");
}

/// What an occurrence is an occurrence *of*.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKey {
    /// A part of the window or its chrome that no model object stands behind.
    /// The name is a code keyword, never a label.
    Surface(&'static str),
    /// A model object, by the id `koru-core` already gave it. A `Uuid`
    /// rather than a `NodeId` or a `DocumentId` so this module — and the host
    /// — stay ignorant of which of them a view is pointing at.
    Model(Uuid),
    /// **One live instance of something a view opens and closes** — a tab, an
    /// open menu, a popover. `kind` is a code keyword; `id` is a number its
    /// owner hands out once and never reuses.
    ///
    /// It is here because two of the roadmap's identity invariants cannot be
    /// met by either arm above. *Two tabs showing the same document are two
    /// occurrences* rules out [`Model`](Self::Model), which would give both the
    /// same locator. *A menu that closes and reopens must not reuse its
    /// predecessor's identity* rules out [`Surface`](Self::Surface), which is a
    /// constant. A position satisfies neither: reordering tabs must move no
    /// identity at all.
    ///
    /// **Why not a [`ViewSlot`] per instance**, which is what the slot
    /// allocator is shaped for: a view contributes through `&self` and is
    /// handed no allocator, so it cannot mint one at the moment an instance is
    /// created. Its owner already keeps a monotonic instance number for its own
    /// reasons — `Tab::id` is one — and a locator is already scoped by window
    /// and view, so an owner-local counter is unique here without a shared
    /// allocator threaded through the trait. See ADR-338.
    Instance { kind: &'static str, id: u64 },
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(name) => write!(f, "s:{name}"),
            Self::Model(id) => write!(f, "n:{}", id.simple()),
            Self::Instance { kind, id } => write!(f, "{kind}:{id}"),
        }
    }
}

/// One thing a screen reader can be on.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Occurrence {
    pub window: WindowSlot,
    pub view: ViewSlot,
    pub object: ObjectKey,
    pub part: ControlPart,
}

/// The prefix every Koru locator starts with, so a client — or a person
/// reading an inspector — can tell one at a glance.
pub const LOCATOR_PREFIX: &str = "koru:";

impl Occurrence {
    /// The window itself.
    pub fn window_root(window: WindowSlot) -> Self {
        Self {
            window,
            view: ViewSlot::WINDOW,
            object: ObjectKey::Surface("window"),
            part: part::WINDOW,
        }
    }

    /// A window command — minimise, maximise, close — which belongs to the
    /// window rather than to anything drawn inside it.
    pub fn window_command(window: WindowSlot, part: ControlPart) -> Self {
        Self {
            window,
            view: ViewSlot::WINDOW,
            object: ObjectKey::Surface("window"),
            part,
        }
    }

    /// A model object shown by one view.
    pub fn model(window: WindowSlot, view: ViewSlot, id: Uuid, part: ControlPart) -> Self {
        Self {
            window,
            view,
            object: ObjectKey::Model(id),
            part,
        }
    }

    /// One live instance a view owns — a tab, an open menu. See
    /// [`ObjectKey::Instance`] for why an instance number rather than a
    /// position or a model id.
    pub fn instance(
        window: WindowSlot,
        view: ViewSlot,
        kind: &'static str,
        id: u64,
        part: ControlPart,
    ) -> Self {
        Self {
            window,
            view,
            object: ObjectKey::Instance { kind, id },
            part,
        }
    }

    /// A surface a view owns that stands for no model object — an empty state,
    /// a strip, a group heading.
    pub fn surface(
        window: WindowSlot,
        view: ViewSlot,
        name: &'static str,
        part: ControlPart,
    ) -> Self {
        Self {
            window,
            view,
            object: ObjectKey::Surface(name),
            part,
        }
    }

    /// The stable string an external client reads.
    ///
    /// `koru:w<window>/v<view>/<object>#<part>`. Every component is a number
    /// or a code keyword; none of it is translated, positional, or derived
    /// from anything a person can rename.
    pub fn locator(&self) -> String {
        let mut out = String::with_capacity(48);
        // `write!` into a `String` cannot fail; the result is discarded rather
        // than unwrapped so a formatting change can never panic a draw path.
        let _ = write!(
            out,
            "{LOCATOR_PREFIX}w{}/v{}/{}#{}",
            self.window.raw(),
            self.view.raw(),
            self.object,
            self.part
        );
        out
    }

    /// The in-process handle AccessKit addresses this node with.
    ///
    /// A 64-bit FNV-1a of the locator. Written out rather than taken from
    /// `DefaultHasher` because that one is explicitly allowed to change
    /// between Rust releases, and an id that moves under a running client — or
    /// between a test and the binary it describes — is worse than one that is
    /// merely short. Zero is stepped over: `accesskit_windows` uses `NodeId(0)`
    /// for the placeholder root it serves before our first update lands.
    pub fn node_id(&self) -> accesskit::NodeId {
        accesskit::NodeId(node_id_of(&self.locator()))
    }
}

/// FNV-1a, 64 bit. See [`Occurrence::node_id`] for why it is written here.
pub(crate) fn node_id_of(locator: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in locator.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    if hash == 0 { 1 } else { hash }
}

/// Hands out window and view slots, in one place so nothing can reuse one.
///
/// Owned by the host rather than a process global: a global would make two
/// tests share a counter and make the numbers in a log depend on what else
/// had run.
#[derive(Debug, Default)]
pub struct Slots {
    next_window: u64,
    next_view: u64,
}

impl Slots {
    /// A window that has just been created.
    pub fn window(&mut self) -> WindowSlot {
        self.next_window += 1;
        WindowSlot(self.next_window)
    }

    /// A view instance that has just been created. Starts at 1 because
    /// [`ViewSlot::WINDOW`] is zero.
    pub fn view(&mut self) -> ViewSlot {
        self.next_view += 1;
        ViewSlot(self.next_view)
    }
}
