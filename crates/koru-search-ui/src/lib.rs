// SPDX-License-Identifier: MPL-2.0

//! **One search surface, hostable by anything that can open a window.**
//!
//! A dedicated window, an in-app overlay, and — next — the agent's global
//! overlay, all the same type. What makes that possible is the dependency list
//! in `Cargo.toml`: no node model, no workspace, no shell, no editor. Check it
//! rather than believing this sentence.
//!
//! ## It speaks the store's ids, and nothing converts here
//!
//! Every id in and out is `koru_store_core::ids`. The surface used to hold both
//! vocabularies at once and convert between them ten times inside
//! `request_open` — a `NodeId` for the node, a `NodeId` built from a
//! *document*'s uuid for the trash root, a `ProfileId` in each of two crates.
//! All of that is app knowledge: how a host addresses a document, and what its
//! Trash screen keys its rows by. It moved out to the host's adapter, and what
//! is left is [`Target`] — what the *index* said, with the ids the store minted.
//!
//! ## A hit's node is a string here
//!
//! Deliberately. The store's index stores it as one, this crate has no type for
//! it, and inventing a newtype to hold a value it never dereferences would be
//! this crate learning a model it exists not to have. The host parses it into
//! whatever a node is to *it*, and refuses the navigation if it cannot.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod app_menu;
mod catalog;
mod due_picker;
mod entity_search;
mod filters;
mod reader;
mod scope;
mod state;
mod store;

use catalog::Catalog;
pub use entity_search::{EntityDocument, EntitySearch, EntitySearchUpdate, EntityWorkspace};
#[cfg(test)]
use reader::SearchReadResponse;
use reader::{SearchReadKind, SearchReadResult, SearchReader};
use scope::SearchScope;
pub use state::persist_choices_in;
use state::{
    DEFAULT_SORT, FilterState, SearchMenu, SearchView, drop_hidden_kinds, hit_key,
    keep_top_hit_per_document,
};

use koru_ai_core::{ChatId, MessageBlockId, MessageId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, split_leading_emoji, theme};
use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};
// **The store's vocabulary, not the app's.** These used to come from
// the app's own search crate, since deleted: an index it owned became an
// index the Koru Store owns, and these are what crosses the wire from it.
use koru_api_client::{RecentQuery, SearchHit, SearchPage, SearchQuery};
use koru_store_wire::search::{
    Lifecycle, LifecycleFilter, SearchFacet, SearchFacets, SearchSort, field,
};
use koru_ui::highlighted_text::{Span, draw_highlighted_line};
use koru_ui::profile_picker;
use koru_ui::selectable_list::{ListScrollbar, SelectableList};
use koru_ui::text_input::{InputStyle, TextInput};
use koru_ui::{ExitList, PopoverAlign, PopoverShell, Tooltip, TooltipContent};
use koru_window::WindowRequest;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// Write search routing diagnostics only when `KORU_SEARCH_DEBUG` is present.
///
/// **This crate's own**, because the routing decisions it prints are this
/// crate's subject: which hit was accepted, which was refused and why, and what
/// the host was asked to open. It was `crate::search_debug!`, which meant
/// a surface could not explain its own decisions without linking a workspace.
#[macro_export]
macro_rules! search_debug {
    ($($arg:tt)*) => {
        $crate::debug::write(format_args!($($arg)*))
    };
}

pub mod debug;

/// A result the person chose, in the ids the store minted.
///
/// **What the index said, and nothing about how a host opens it.** Turning this
/// into a document, a node and a tab is the host's, and the two hosts do it
/// differently — see the module note.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Store {
        profile: ProfileId,
        workspace: WorkspaceId,
        /// Which document the hit is in. `None` is a workspace-level match — a hit
        /// on the workspace's own name — which has nothing more to open.
        document: Option<DocumentId>,
        /// The node within that document, as the index stores it. `None` for a hit
        /// on a document's **title**, which is mirrored from the manifest listing
        /// rather than read from a node in anybody's tree.
        node: Option<String>,
        /// Whether the document is in the bin. A host that shows a Trash screen
        /// opens that instead; one that does not may ignore it.
        ///
        /// **Not a "trash root".** The store's model has no such node — trash is a
        /// flag on the manifest entry, so it is a property of the document — and
        /// what a host keys its bin rows by is the host's business.
        trashed: bool,
        /// Authored evidence from the selected result, for placing a caret or a
        /// transient flash. Never used to find anything.
        match_text: Option<String>,
        disposition: Disposition,
    },
    Chat {
        chat: ChatId,
        message: MessageId,
        block: MessageBlockId,
        /// The profile the chat belongs to (AI schema 13), so a host that
        /// searched every profile opens it in that profile's window. `None`
        /// from an older service, or for a chat it has not placed yet.
        profile: Option<ProfileId>,
        disposition: Disposition,
    },
}

/// Where a chosen result should open.
///
/// Carried on the target rather than inferred by the host from keyboard state:
/// the mouse and the keyboard entry points produce the same object, and a host
/// stays independent of this surface's key bindings.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Disposition {
    CurrentWindow,
    NewWindow,
}

/// **A row the host offers above the results: what the typed text could
/// become.** The surface knows no document and no task; the host names the
/// thing (*New task*, *New document*), where it would land (the subtitle)
/// and the glyph, and the surface draws them as the first group, CREATE,
/// while there is a query. A host that supplies none sees no group — the
/// Agent's overlay is that host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateRow {
    pub icon: char,
    pub title: String,
    pub subtitle: String,
}

/// One chip of a host's footer line — a key and what it does. When a host
/// supplies any, they stand **in place of** the surface's own hint line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FooterChip {
    pub key: String,
    pub label: String,
}

/// What `↵` on a CREATE row asks for: which row, the typed text it is made
/// from, and where it opens. `Ctrl+↵` keeps meaning *in a window* here as
/// it does for a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Create {
    pub row: usize,
    pub query: String,
    pub disposition: Disposition,
}

const CREATE_HEADING_H: f32 = 22.0;
const CREATE_GAP: f32 = 6.0;

const HEADER_H: f32 = 44.0;
const FOOTER_H: f32 = 34.0;
const ROW_H: f32 = 70.0;
const PAD: f32 = 18.0;
const QUERY_PAD_X: f32 = 12.0;
const QUERY_OUTLINE_INSET: f32 = 4.0;
const QUERY_ICON_SIZE: f32 = 16.0;
const HEADER_CONTROL_SIZE: f32 = 26.0;
/// Wide enough for the icon and the longest sort label, and constant so the
/// header does not reshuffle when the sort changes. See `sort_rect`.
const SORT_CHIP_W: f32 = 116.0;

/// The sort menu, in the order it is shown: the variant, the row's label, and
/// the shorter one the chip has room for.
///
/// **One table.** The rows were built in one `match`, the click on row *n* was
/// resolved in another, and the chip's label in a third — three places agreeing
/// by hand about an order none of them named. The same idiom as `cli.rs`'s
/// `FLAGS` and `koru-i18n`'s `ROWS`.
const SORTS: [(SearchSort, &str, &str); 3] = [
    (SearchSort::Relevance, "Relevance", "Relevance"),
    (
        SearchSort::ObservedModified,
        "Last modified",
        "Last modified",
    ),
    (
        SearchSort::LastUsedWorkspace,
        "Last used workspace",
        "Last used",
    ),
];

/// What the header chip calls a sort. See [`SORTS`].
/// Same split tabs and Welcome use: a leading emoji is the icon, not extra text.
fn pin_parts(title: &str) -> (Option<&str>, &str) {
    match split_leading_emoji(title) {
        (Some(emoji), rest) => (Some(emoji), rest),
        (None, _) => (None, title),
    }
}

fn sort_label(sort: SearchSort) -> &'static str {
    SORTS
        .iter()
        .find(|(candidate, ..)| *candidate == sort)
        .map(|(_, _, chip)| *chip)
        .unwrap_or_default()
}
const CARD_INSET_X: f32 = 8.0;
const CARD_INSET_Y: f32 = 3.0;
pub const SURFACE_RADIUS: f32 = 8.0;
const INPUT_RADIUS: f32 = SURFACE_RADIUS - QUERY_OUTLINE_INSET;
const CARD_RADIUS: f32 = SURFACE_RADIUS;
const READ_POLL: Duration = Duration::from_millis(16);
const WARNING_H: f32 = 34.0;
const LANDING_OPENED_LIMIT: usize = 3;
const RECENT_QUERY_LIMIT: usize = 5;
const RECENT_QUERY_ROW_H: f32 = 32.0;
const RECENT_QUERY_STRIDE: f32 = 34.0;
const RECENT_HEADING_TO_ROWS: f32 = 24.0;
const LANDING_HEADING_H: f32 = 22.0;
const LANDING_SECTION_GAP: f32 = 12.0;
/// Gap under the query field before landing content. Headings live inside the
/// results clip, so this is only air, not a reserved label band.
const LANDING_INSET: f32 = 8.0;
const PIN_CHIP_H: f32 = 32.0;
const PIN_CHIP_GAP: f32 = 6.0;
const PIN_CHIP_PAD_X: f32 = 8.0;
const PIN_ICON_SIZE: f32 = 16.0;
const PIN_TITLE_MAX_W: f32 = 140.0;

fn initial_session_state() -> state::SearchSessionState {
    // Unit tests construct many independent surfaces on reused worker threads;
    // keep those fixtures isolated while the production app restores the
    // process-local session state.
    #[cfg(test)]
    {
        state::SearchSessionState::default()
    }
    #[cfg(not(test))]
    {
        state::search_session()
    }
}

fn remember_session_query(query: &str) {
    #[cfg(not(test))]
    state::remember_query(query);
    #[cfg(test)]
    let _ = query;
}

fn remember_session_sort(sort: SearchSort) {
    #[cfg(not(test))]
    state::remember_sort(sort);
    #[cfg(test)]
    let _ = sort;
}

fn remember_session_profile(profile: Option<ProfileId>) {
    #[cfg(not(test))]
    state::remember_profile(profile.map(|id| id.to_storage()).as_deref());
    #[cfg(test)]
    let _ = profile;
}

fn remember_session_selection(selection: Option<state::SearchSessionSelection>) {
    #[cfg(not(test))]
    state::remember_selection(selection);
    #[cfg(test)]
    let _ = selection;
}

fn remember_session_include_ai(include_ai: bool) {
    state::remember_include_ai(include_ai);
}

/// One row of the query box's `#tag` completion.
///
/// **Folded once, when the table arrives.** A keystroke then compares
/// precomputed segments instead of folding every name again, which is what
/// keeps a popover that reopens on every character off the list of things that
/// make typing feel slow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TagCompletion {
    /// The display name, whole path included — `fun/home`.
    pub(crate) name: String,
    /// The accent token for the row's dot, or none for the default chip.
    pub(crate) color: Option<String>,
    /// Distinct nodes carrying it, binned ones already subtracted.
    pub(crate) uses: usize,
    /// [`name`](Self::name)'s folded segments, the unit the matcher compares.
    pub(crate) folded: Vec<String>,
}

/// How many completion rows are shown at once. Beyond this the list is
/// narrowed by typing another character, which is faster than scrolling one.
const TAG_ROWS: usize = 8;

/// The same ceiling for the `/key` chooser, and the same reason.
const FILTER_ROWS: usize = 8;

pub struct SearchSurface {
    query: TextInput,
    page: SearchPage,
    next_cursor: Option<String>,
    include_ai: bool,
    ai_targets: HashMap<String, (ChatId, MessageId, MessageBlockId, Option<ProfileId>)>,
    list: SelectableList,
    viewport: (f32, f32),
    engine: TextEngine,
    pointer: (f32, f32),
    hover: Option<usize>,
    scope: SearchScope,
    lifecycle: LifecycleFilter,
    /// Global search normally leaves archived workspaces out. The no-result
    /// action flips this to search exactly those live archived workspaces.
    archived_only: bool,
    facets: SearchFacets,
    /// Which workspaces exist and whose they are.
    ///
    /// **What replaced the `profile_id` on every hit.** The store's search has
    /// no profile — it is an authorization and query scope over *workspaces*,
    /// resolved by the caller — so the resolution happens here.
    catalog: Catalog,
    /// A global query cannot be built safely until the complete lifecycle
    /// catalog has arrived. Workspace-scoped search does not need it.
    catalog_ready: bool,
    /// The Store profile whose access scope this global search uses.
    ///
    /// **Held here rather than in `FilterState`** because a profile is the
    /// authorization boundary, not an optional result filter. The catalog
    /// resolves it to the active workspace ids sent to the Store.
    selected_profile: Option<ProfileId>,
    /// Why the last read failed, when the reason was the store rather than the
    /// query.
    ///
    /// **A store that is down must not read as "no results"**, which is a fact
    /// about what somebody wrote rather than about us.
    unavailable: Option<String>,
    /// A warning supplied by the host for a failure outside Search itself,
    /// such as being unable to launch the Koru window for a selected result.
    /// Store recovery must not clear this independently owned message.
    host_warning: Option<String>,
    recent_queries: Vec<RecentQuery>,
    selected_recent_query: Option<usize>,
    pinned: Vec<(String, DocumentId, WorkspaceId, bool)>,
    selected_pin: Option<usize>,
    pin_cursor: usize,
    filters: FilterState,
    filters_visible: bool,
    sort: SearchSort,
    menu: SearchMenu,
    menu_scroll: f32,
    /// Every live tag in scope, most-used first, for the `#tag` completion.
    /// Fetched once per scope on the background channel, never per keystroke.
    tags: Vec<TagCompletion>,
    /// Every filterable attribute in scope, in registry order, for the `/key`
    /// chooser. **Fetched in the same answer as the tags** and cached for the
    /// same reason: nothing per keystroke may touch the store.
    definitions: Vec<filters::FilterDefinition>,
    /// Whether the scope's tables have been asked for since it last changed, so
    /// a scope with genuinely no tags is not re-requested forever.
    scope_tables_requested: bool,
    /// Indices into [`tags`](Self::tags) matching the token being typed, and
    /// the byte range that token occupies in the query. Recomputed on every
    /// keystroke; `None` when the caret is not in a `#tag`.
    tag_hits: Vec<usize>,
    tag_token: Option<std::ops::Range<usize>>,
    /// What the caret is in the middle of, as far as the `/key` chooser is
    /// concerned: a key being chosen, a value being typed, or neither.
    /// Recomputed on every keystroke; `None` closes the chooser.
    filter_context: Option<filters::Context>,
    /// The rows on offer for [`filter_context`](Self::filter_context) — indices
    /// into [`definitions`](Self::definitions) while a key is being chosen, and
    /// into that definition's `options` while a value is.
    filter_hits: Vec<usize>,
    /// The calendar a temporal `/key` opens, and the before/after/24h step
    /// behind it ([ADR-220](../../../docs/decisions/ADR-201-225.md#adr-220-duefilter--due-opens-the-calendar-then-asks-which-side-and-a-bound-that-names-two-kinds-is-written-twice)).
    ///
    /// **Present only while `menu == SearchMenu::Filters`**, and derived from
    /// the caret like everything else about that popover: it is built when the
    /// caret enters a temporal value and dropped when it leaves, so a calendar
    /// cannot outlive the key it belonged to. While it is here it **owns the
    /// keyboard**, which is what makes `2w` and `+3d` typable into its phrase
    /// box rather than into the query.
    temporal: Option<due_picker::DueFilterPicker>,
    /// Keyboard/pointer highlight for the simple popovers. Kept separate from
    /// the results list so an open profile picker cannot move the result that
    /// Enter would otherwise open.
    menu_selected: Option<usize>,
    /// The `…` menu's widget, built by `open_menu` when that menu opens and
    /// read only while `menu == SearchMenu::App` — a stale value left behind
    /// by a close is replaced on the next open, never consulted before it.
    app_menu: Option<koru_ui::Menu<app_menu::AppMenuRow>>,
    view: SearchView,
    show_shortcuts: bool,
    /// The host's CREATE rows, drawn first while there is a query.
    create_rows: Vec<CreateRow>,
    /// The host's footer chips, drawn instead of the hint line when any.
    footer_chips: Vec<FooterChip>,
    /// The CREATE row the keyboard is on, if it is on one rather than a hit.
    selected_create: Option<usize>,
    hover_create: Option<usize>,
    /// What `↵` on a CREATE row asked for, until the host takes it.
    create: Option<Create>,
    /// The host's words in the empty box, over the surface's own.
    placeholder: Option<String>,
    tooltip: Tooltip,
    exiting: ExitList<String>,
    exiting_hits: Vec<(usize, SearchHit)>,
    scroll_drag: Option<f32>,
    request_seq: u64,
    pending_read: Option<u64>,
    pending_replace: Option<u64>,
    /// The lifecycle catalog is read on the background channel. While it is
    /// pending the standalone agent window still needs a maintenance wake,
    /// even though no search page can safely be requested yet.
    pending_catalog: bool,
    selection_restore: Option<state::SearchSessionSelection>,
    reader: Option<SearchReader>,
    target: Option<Target>,
    window_request: Option<WindowRequest>,
}

impl SearchSurface {
    /// **The query box and the engine that lays it out**, for a host routing a
    /// dictation into it.
    ///
    /// Two `&mut` borrows of disjoint fields, which is why this answers a pair
    /// rather than `&mut self` — the field cannot lay itself out, and this
    /// surface owns its own [`TextEngine`] rather than borrowing the
    /// container's.
    pub fn dictation_field(&mut self) -> (&mut TextInput, &mut TextEngine) {
        (&mut self.query, &mut self.engine)
    }

    /// Open a provisional dictation in the query box.
    pub fn begin_dictation(&mut self) -> bool {
        self.query.begin_composition(&mut self.engine)
    }

    /// Replace the provisional query without searching every intermediate
    /// recognition hypothesis.
    pub fn update_dictation(&mut self, text: &str) {
        self.query.update_composition(&mut self.engine, text);
    }

    /// Commit the final query and run the same refresh as a keyboard edit.
    pub fn commit_dictation(&mut self, text: &str) {
        let before = self.query.raw_text().to_owned();
        self.query.commit_composition(&mut self.engine, text);
        if self.query.raw_text() != before {
            self.query_changed();
        }
        self.refresh_completions();
    }

    /// Discard the provisional query without changing the result set.
    pub fn cancel_dictation(&mut self) {
        self.query.cancel_composition(&mut self.engine);
    }

    pub fn new() -> Self {
        Self::new_at(None)
    }

    /// Show or clear a host-detected Store availability warning.
    ///
    /// `true` means the visible surface changed. Results are cleared while the
    /// Store is unavailable so an old page cannot look current beneath the
    /// warning.
    pub fn set_availability_warning(&mut self, warning: Option<String>) -> bool {
        if self.unavailable == warning {
            return false;
        }
        self.unavailable = warning;
        if self.unavailable.is_some() {
            self.page.hits.clear();
            self.pending_replace = None;
        }
        true
    }

    pub fn availability_warning(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Show or clear a warning owned by the embedding host.
    pub fn set_host_warning(&mut self, warning: Option<String>) -> bool {
        if self.host_warning == warning {
            return false;
        }
        self.host_warning = warning;
        true
    }

    pub fn host_warning(&self) -> Option<&str> {
        self.host_warning.as_deref()
    }

    fn warning_message(&self) -> Option<&str> {
        self.host_warning
            .as_deref()
            .or_else(|| self.unavailable.as_deref())
    }

    /// A surface that queries the store at `root`, which is what a host launched
    /// with `--root` passes.
    ///
    /// **Separate from [`new`](Self::new) rather than a parameter on it**, so
    /// the thirty-odd tests that build a surface say nothing about roots and
    /// only the two real hosts — the app's adapter and the agent's overlay —
    /// carry one. `new()` is `new_at(None)`, which is the shared store: the
    /// answer every caller had before this existed.
    pub fn new_at(root: Option<std::path::PathBuf>) -> Self {
        Self::with_scope_at(SearchScope::Global, root)
    }

    /// **No reader under test, and the reason is not tidiness.**
    ///
    /// `SearchReader::start` spawns two threads that dial whatever store is
    /// listening on this channel — and on a developer's machine that is their
    /// *real* store, with their real documents in it. Thirty-odd unit tests
    /// build a surface, so a `cargo test` run was issuing a landing-page query
    /// against it and could record a navigation in its history. The tests that
    /// need a reader install one with `SearchReader::test_pair`, which is what
    /// makes this safe as well as correct.
    ///
    /// ## `cfg(test)` alone stopped being enough when this became a crate
    ///
    /// **A dependency is never compiled with `cfg(test)`.** It is set for the
    /// crate cargo is testing and for nothing it links, so the moment the
    /// surface moved out of `apps/koru` this guard silently stopped covering
    /// that app's tests — which are most of the thirty-odd. The behaviour would
    /// have come back exactly as it was, and only on a machine with a store
    /// running, which is every developer's.
    ///
    /// Hence the feature. `apps/koru` turns `no-reader` on as a
    /// **dev-dependency**, and resolver 2+ keeps a dev-only feature out of the
    /// normal build — so its tests get no reader and its binary is unchanged.
    /// A host that forgets is not silently unsafe: its own tests are, which is
    /// what `a_surface_built_under_test_dials_nothing` exists to catch.
    /// **`cfg!`, not `#[cfg]`.** Two bodies would delete the only caller of
    /// `SearchReader::start` under the feature, and every function it reaches
    /// would warn as dead — so the guard would arrive with a page of noise for
    /// somebody to silence with `allow(dead_code)`, which is how a real one
    /// gets hidden later. This compiles both paths and folds the constant.
    fn start_reader(root: Option<std::path::PathBuf>) -> Option<SearchReader> {
        if cfg!(any(test, feature = "no-reader")) {
            return None;
        }
        SearchReader::start(root)
            .map_err(|error| {
                eprintln!("warning: search reader could not start: {error}");
                error
            })
            .ok()
    }

    fn with_scope_at(scope: SearchScope, root: Option<std::path::PathBuf>) -> Self {
        let mut engine = TextEngine::new();
        let session = initial_session_state();
        let query = TextInput::with_text(&mut engine, theme::role::row(), &session.query);
        let reader = Self::start_reader(root);
        let catalog_ready = reader.is_none();
        let selected_profile = match &scope {
            SearchScope::Workspace { profile, .. } => Some(*profile),
            SearchScope::Global => session.profile.as_deref().and_then(ProfileId::parse),
        };
        let mut surface = Self {
            query,
            page: SearchPage {
                hits: Vec::new(),
                next_offset: None,
                updating_workspaces: Vec::new(),
                corrected_query: None,
                request_seq: 0,
            },
            next_cursor: None,
            include_ai: session.include_ai,
            ai_targets: HashMap::new(),
            list: SelectableList::new(ROW_H),
            viewport: (860.0, 620.0),
            engine,
            pointer: (0.0, 0.0),
            hover: None,
            scope,
            lifecycle: LifecycleFilter::Live,
            archived_only: false,
            facets: SearchFacets::default(),
            catalog: Catalog::default(),
            catalog_ready,
            selected_profile,
            unavailable: None,
            host_warning: None,
            recent_queries: Vec::new(),
            selected_recent_query: None,
            pinned: Vec::new(),
            selected_pin: None,
            pin_cursor: 0,
            filters: FilterState::default(),
            filters_visible: false,
            sort: session.sort,
            menu: SearchMenu::None,
            tags: Vec::new(),
            definitions: Vec::new(),
            scope_tables_requested: false,
            tag_hits: Vec::new(),
            tag_token: None,
            filter_context: None,
            filter_hits: Vec::new(),
            menu_scroll: 0.0,
            temporal: None,
            menu_selected: None,
            app_menu: None,
            view: SearchView::Results,
            show_shortcuts: false,
            create_rows: Vec::new(),
            footer_chips: Vec::new(),
            selected_create: None,
            hover_create: None,
            create: None,
            placeholder: None,
            tooltip: Tooltip::default(),
            exiting: ExitList::new(),
            exiting_hits: Vec::new(),
            scroll_drag: None,
            request_seq: 0,
            pending_read: None,
            pending_replace: None,
            pending_catalog: reader.is_some(),
            selection_restore: session.selection,
            reader,
            target: None,
            window_request: None,
        };
        // Asked once, up front: the filter menus need it and nothing else does.
        if let Some(reader) = &surface.reader {
            reader.request_catalog();
        }
        surface.selection_restore = surface
            .selection_restore
            .take()
            .filter(|selection| selection.context == surface.selection_context());
        surface.refresh();
        surface
    }

    pub fn for_workspace(profile: ProfileId, workspace: WorkspaceId, name: String) -> Self {
        Self::for_workspace_at(profile, workspace, name, None)
    }

    /// [`for_workspace`](Self::for_workspace) against the store at `root`. The
    /// scoped twin of [`new_at`](Self::new_at); see it for why the root is a
    /// second constructor rather than a parameter on the first.
    pub fn for_workspace_at(
        profile: ProfileId,
        workspace: WorkspaceId,
        name: String,
        root: Option<std::path::PathBuf>,
    ) -> Self {
        Self::with_scope_at(
            SearchScope::Workspace {
                profile,
                workspace,
                name,
            },
            root,
        )
    }

    fn results_width(&self) -> f32 {
        self.viewport.0
    }

    fn density_row_height() -> f32 {
        match theme::density() {
            theme::Density::Compact => 60.0,
            theme::Density::Normal => ROW_H,
            theme::Density::Comfortable => 78.0,
        }
    }

    fn filter_rect(&self) -> Rect {
        Rect::new(
            self.sort_rect().x - 32.0,
            (HEADER_H - HEADER_CONTROL_SIZE) * 0.5,
            HEADER_CONTROL_SIZE,
            HEADER_CONTROL_SIZE,
        )
    }

    fn sort_rect(&self) -> Rect {
        // **One width, whatever the sort.** It used to collapse to the icon
        // alone on relevance, so choosing a sort moved the filter icon and
        // narrowed the query field under the caret — a control that resizes
        // the thing you are typing in as a side effect of an unrelated menu.
        Rect::new(
            self.app_menu_rect().x - SORT_CHIP_W - 6.0,
            (HEADER_H - HEADER_CONTROL_SIZE) * 0.5,
            SORT_CHIP_W,
            HEADER_CONTROL_SIZE,
        )
    }

    fn app_menu_rect(&self) -> Rect {
        let icon = self.app_menu_icon_rect();
        Rect::new(
            icon.x - (HEADER_CONTROL_SIZE - icon.w) * 0.5,
            (HEADER_H - HEADER_CONTROL_SIZE) * 0.5,
            HEADER_CONTROL_SIZE,
            HEADER_CONTROL_SIZE,
        )
    }

    fn app_menu_icon_rect(&self) -> Rect {
        let outline = self.query_outline_rect();
        Rect::new(
            outline.x + outline.w - QUERY_PAD_X - QUERY_ICON_SIZE,
            outline.y + (outline.h - QUERY_ICON_SIZE) * 0.5,
            QUERY_ICON_SIZE,
            QUERY_ICON_SIZE,
        )
    }

    /// The header buttons' chips.
    ///
    /// **`Literal`, and all three are this surface's own bindings** — resolved
    /// by hand in [`SearchSurface::key`] and written out by hand here, which is
    /// what the registry exists to stop. They are unregistered because this
    /// surface is a layer the table does not have yet: it is a window (and an
    /// overlay) beside the container rather than under it, and its `Ctrl+F`
    /// would have to be declared as sharing a chord with the document's find
    /// bar. Written up in `docs/contributing/techdebt/README.md`; they still go through the seam, so
    /// they turn quiet with every other hint in the app.
    fn tooltip_at(&self, x: f32, y: f32) -> Option<TooltipContent> {
        use koru_keys::{Hinted, Section, show_hint};
        let hint = |text| show_hint(Hinted::Literal(text), Section::Tooltip);
        if self.filter_rect().contains(x, y) {
            return Some(
                TooltipContent::new(
                    1,
                    self.filter_rect(),
                    if self.filters_visible {
                        "Hide filters"
                    } else {
                        "Show filters"
                    },
                )
                .with_optional_shortcut(hint("Ctrl+F")),
            );
        }
        if self.sort_rect().contains(x, y) {
            return Some(
                TooltipContent::new(2, self.sort_rect(), "Sort")
                    .with_optional_shortcut(hint("Ctrl+O")),
            );
        }
        self.app_menu_rect().contains(x, y).then(|| {
            TooltipContent::new(3, self.app_menu_rect(), "More")
                .with_optional_shortcut(hint("Ctrl+,"))
        })
    }

    fn filter_line_visible(&self) -> bool {
        self.view == SearchView::Results
            && (self.filters_visible
                || self.filters.active()
                || self.archived_only
                || self.lifecycle != LifecycleFilter::Live)
    }

    fn filter_line_h(&self) -> f32 {
        if self.filter_line_visible() {
            38.0
        } else {
            0.0
        }
    }

    fn correction_h(&self) -> f32 {
        if self.view == SearchView::Results && self.page.corrected_query.is_some() {
            24.0
        } else {
            0.0
        }
    }

    fn warning_h(&self) -> f32 {
        if self.view == SearchView::Results && self.warning_message().is_some() {
            WARNING_H
        } else {
            0.0
        }
    }

    fn query_rect(&self) -> Rect {
        let outline = self.query_outline_rect();
        let icon = self.search_icon_rect();
        let text_x = icon.x + icon.w + QUERY_PAD_X;
        let right = self.sort_rect().x - QUERY_PAD_X;
        Rect::new(
            text_x - koru_ui::text_field::PAD,
            outline.y,
            (right - text_x + koru_ui::text_field::PAD).max(1.0),
            outline.h,
        )
    }

    fn search_icon_rect(&self) -> Rect {
        let outline = self.query_outline_rect();
        Rect::new(
            outline.x + QUERY_PAD_X,
            outline.y + (outline.h - QUERY_ICON_SIZE) * 0.5,
            QUERY_ICON_SIZE,
            QUERY_ICON_SIZE,
        )
    }

    fn query_outline_rect(&self) -> Rect {
        Rect::new(
            QUERY_OUTLINE_INSET,
            QUERY_OUTLINE_INSET,
            (self.results_width() - QUERY_OUTLINE_INSET * 2.0).max(1.0),
            HEADER_H - QUERY_OUTLINE_INSET * 2.0,
        )
    }

    fn profiles_chip_rect(&self) -> Rect {
        Rect::new(PAD, self.viewport.1 - FOOTER_H + 3.0, 180.0, 28.0)
    }

    fn workspaces_chip_rect(&self) -> Rect {
        Rect::new(15.0, HEADER_H + 4.0, 126.0, 26.0)
    }

    fn reset_filters_rect(&self) -> Rect {
        Rect::new(self.results_width() - 68.0, HEADER_H + 4.0, 54.0, 26.0)
    }

    fn popover(&self) -> Option<PopoverShell> {
        let viewport = Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1 - FOOTER_H);
        // **The calendar's shell is the calendar's size, and the step does not
        // change it.** That is what "content-only" means: the rect is settled
        // when the picker opens, so the slide reflows nothing and the panel
        // does not move under the pointer halfway through the gesture.
        if self.menu == SearchMenu::Filters
            && let Some(picker) = &self.temporal
        {
            let (w, h) = picker.size();
            return Some(PopoverShell::anchored(
                self.query_rect(),
                viewport,
                w,
                h.min(viewport.h - 16.0),
                PopoverAlign::Start,
            ));
        }
        let (anchor, height, align) = match self.menu {
            SearchMenu::None => return None,
            SearchMenu::Sort => (
                self.sort_rect(),
                16.0 + SORTS.len() as f32 * 32.0,
                PopoverAlign::End,
            ),
            SearchMenu::Profiles => (
                self.profiles_chip_rect(),
                profile_picker::content_height(self.catalog.profiles().len())
                    .min(profile_picker::MAX_H),
                PopoverAlign::Start,
            ),
            SearchMenu::Workspaces => (
                self.workspaces_chip_rect(),
                16.0 + (self.workspace_facets().count() + 1) as f32 * 32.0,
                PopoverAlign::Start,
            ),
            SearchMenu::App => (self.app_menu_rect(), 132.0, PopoverAlign::End),
            SearchMenu::Tags => (
                self.query_rect(),
                16.0 + self.tag_hits.len().min(TAG_ROWS) as f32 * 32.0,
                PopoverAlign::Start,
            ),
            SearchMenu::Filters => (
                self.query_rect(),
                16.0 + self.menu_row_count() as f32 * 32.0,
                PopoverAlign::Start,
            ),
        };
        Some(PopoverShell::anchored(
            anchor,
            viewport,
            if self.menu == SearchMenu::Profiles {
                theme::PANEL_W
            } else {
                230.0
            },
            height.min(viewport.h - 16.0),
            align,
        ))
    }

    /// The `#tag` completion's rows: a colour dot, the path in two weights,
    /// and how many blocks carry it.
    ///
    /// **Its own painter rather than the generic `(label, checked)` list**, for
    /// the same reason the profile picker has one: these rows say three things
    /// and that list draws one. The parent path is drawn dim and the leaf
    /// normal — the treatment the editor's chooser already uses — because two
    /// `home`s under different parents are two tags and a leaf-only row would
    /// make them look like one.
    fn paint_tag_popover(&mut self, canvas: &mut Scene, root: Affine, shell: PopoverShell) {
        let p = theme::p();
        let rows: Vec<(Option<String>, String, String, usize)> = self
            .tag_hits
            .iter()
            .take(TAG_ROWS)
            .filter_map(|&i| self.tags.get(i))
            .map(|tag| {
                let leaf = koru_tagname::leaf(&tag.name).to_owned();
                let parent = koru_tagname::parent(&tag.name)
                    .map(|path| format!("{path}/"))
                    .unwrap_or_default();
                (tag.color.clone(), parent, leaf, tag.uses)
            })
            .collect();

        for (index, (color, parent, leaf, uses)) in rows.iter().enumerate() {
            let row = Rect::new(
                shell.rect.x + 6.0,
                shell.rect.y + 6.0 + index as f32 * 32.0,
                shell.rect.w - 12.0,
                30.0,
            );
            let hovered = row.contains(self.pointer.0, self.pointer.1);
            if hovered || self.menu_selected == Some(index) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
            }
            // The dot, in the tag's own accent or the neutral one.
            let tone = color
                .as_deref()
                .and_then(theme::accent_tone)
                .map(|tone| tone.swatch)
                .unwrap_or(p.text_dim);
            scene::fill(
                canvas,
                root,
                tone,
                &scene::rrect(row.x + 8.0, row.y + 12.0, 6.0, 6.0, 3.0),
            );
            // The count first, so the name knows how much room is left.
            let count = self
                .engine
                .simple_layout(&uses.to_string(), theme::role::meta());
            let count_w = count.width();
            scene::draw_layout_first_line(
                canvas,
                root,
                &count,
                row.x + row.w - 8.0 - count_w,
                row.y + 8.0,
                Some(p.text_dim),
            );
            let mut x = row.x + 22.0;
            let room = row.w - 30.0 - count_w - 8.0;
            if !parent.is_empty() {
                let text = self.engine.elided(parent, theme::role::row(), room);
                let layout = self.engine.simple_layout(&text, theme::role::row());
                let w = layout.width();
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &layout,
                    x,
                    row.y + 6.0,
                    Some(p.text_dim),
                );
                x += w;
            }
            let text = self.engine.elided(
                leaf,
                theme::role::row(),
                (row.x + row.w - 8.0 - count_w - x).max(1.0),
            );
            let layout = self.engine.simple_layout(&text, theme::role::row());
            scene::draw_layout_first_line(canvas, root, &layout, x, row.y + 6.0, Some(p.text));
        }
    }

    /// The `/key` chooser's rows: an attribute and its key while a key is being
    /// chosen, one of its tokens while a value is, and a single line of
    /// guidance where the type implies a field rather than a list.
    ///
    /// **Its own painter rather than the generic `(label, checked)` list**, for
    /// the same reason the tag completion has one: a key row says two things —
    /// what it is called and what it is stored as — and nothing here is ever
    /// checked.
    fn paint_filter_popover(&mut self, canvas: &mut Scene, root: Affine, shell: PopoverShell) {
        let p = theme::p();
        // `(label, trailing)` — the trailing half is drawn dim and elided
        // first, because the name is what somebody is reading for.
        let rows: Vec<(String, String)> = match (&self.filter_context, self.filter_hint()) {
            (_, Some(hint)) => vec![(hint, String::new())],
            (Some(filters::Context::Key { .. }), None) => self
                .filter_hits
                .iter()
                .take(FILTER_ROWS)
                .filter_map(|&index| self.definitions.get(index))
                .map(|def| (def.label.clone(), def.key.clone()))
                .collect(),
            (Some(filters::Context::Value { def, .. }), None) => {
                let Some(def) = self.definitions.get(*def) else {
                    return;
                };
                self.filter_hits
                    .iter()
                    .take(FILTER_ROWS)
                    .filter_map(|&index| def.options.get(index))
                    .map(|option| (option.clone(), String::new()))
                    .collect()
            }
            (None, None) => return,
        };
        let hint_only = self.filter_is_hint();

        for (index, (label, trailing)) in rows.iter().enumerate() {
            let row = Rect::new(
                shell.rect.x + 6.0,
                shell.rect.y + 6.0 + index as f32 * 32.0,
                shell.rect.w - 12.0,
                30.0,
            );
            let hovered = !hint_only && row.contains(self.pointer.0, self.pointer.1);
            if hovered || (!hint_only && self.menu_selected == Some(index)) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
            }
            // The stored key first, so the label knows how much room is left.
            let mut room = row.w - 20.0;
            if !trailing.is_empty() {
                let text = self
                    .engine
                    .elided(trailing, theme::role::meta(), row.w * 0.5);
                let layout = self.engine.simple_layout(&text, theme::role::meta());
                let width = layout.width();
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &layout,
                    row.x + row.w - 10.0 - width,
                    row.y + 8.0,
                    Some(p.text_dim),
                );
                room -= width + 8.0;
            }
            let text = self.engine.elided(label, theme::role::row(), room.max(1.0));
            let layout = self.engine.simple_layout(&text, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &layout,
                row.x + 10.0,
                row.y + 6.0,
                Some(if hint_only { p.text_dim } else { p.text }),
            );
        }
    }

    fn menu_row_count(&self) -> usize {
        match self.menu {
            SearchMenu::None => 0,
            SearchMenu::Sort => SORTS.len(),
            SearchMenu::App => 3,
            SearchMenu::Profiles => self.catalog.profiles().len(),
            SearchMenu::Workspaces => self.workspace_facets().count() + 1,
            SearchMenu::Tags => self.tag_hits.len().min(TAG_ROWS),
            // A calendar has no 32px rows; its own hit test answers.
            SearchMenu::Filters if self.temporal.is_some() => 0,
            SearchMenu::Filters if self.filter_is_hint() => 1,
            SearchMenu::Filters => self.filter_hits.len().min(FILTER_ROWS),
        }
    }

    /// Whether the open chooser is showing a line of guidance rather than a
    /// list of choices.
    ///
    /// **One predicate, asked in four places** — the row count, the painter,
    /// what the arrow keys do, and whether anything is highlighted. Spelling it
    /// out at each of them is how a `select`'s value list would quietly become
    /// unpickable the first time somebody wrote `filter_editor().is_some()` and
    /// meant this.
    fn filter_is_hint(&self) -> bool {
        match self.filter_editor() {
            Some(filters::FilterEditor::Field) => true,
            // A temporal key gets the calendar while its value is empty and
            // the one line of guidance once a date is being spelled by hand —
            // the panel must not take the keyboard away from somebody halfway
            // through typing `2026-09-10`.
            Some(filters::FilterEditor::Temporal) => self.temporal.is_none(),
            _ => false,
        }
    }

    /// Which editor the open chooser is showing, or `None` while it is still
    /// choosing a key.
    ///
    /// **Derived from the caret, never stored.** A second field would let a
    /// stale editor outlive the key it belonged to — the popover is one
    /// gesture, and the text under the caret is the only record of where in it
    /// somebody is.
    fn filter_editor(&self) -> Option<filters::FilterEditor> {
        match &self.filter_context {
            Some(filters::Context::Value { def, .. }) => {
                self.definitions.get(*def).map(|def| def.editor)
            }
            _ => None,
        }
    }

    /// The one line a [`filters::FilterEditor::Field`] shows instead of a list.
    fn filter_hint(&self) -> Option<String> {
        let filters::Context::Value { def, .. } = self.filter_context.as_ref()? else {
            return None;
        };
        let def = self.definitions.get(*def)?;
        Some(match def.editor {
            filters::FilterEditor::Field if def.kinds.iter().any(|kind| kind == "number") => {
                format!("{} — a number; > and < compare", def.label)
            }
            filters::FilterEditor::Field => format!("{} — type a word, or \"a phrase\"", def.label),
            // Only reached once a date is being typed by hand: while the value
            // is empty this key shows the calendar instead.
            filters::FilterEditor::Temporal => format!("{} — a date, as 2026-09-10", def.label),
            // Neither shows a hint line: one has a list under it, and the
            // other is complete the moment the key is typed.
            filters::FilterEditor::Options | filters::FilterEditor::Presence => return None,
        })
    }

    /// Recompute the `#tag` completion from the query box, and open or close
    /// the popover to match. Called after anything that can move the caret or
    /// change the text.
    ///
    /// **The token rule is `koru-tagname`'s**, the same function the store's
    /// query parser uses to decide what is a filter — so the box never offers
    /// a completion that would not become one.
    fn refresh_tag_completion(&mut self) {
        // **`raw_text`, never `value`.** `value` trims, and the caret is an
        // offset into the untrimmed buffer — so on `#fu ` the trimmed text is
        // shorter than the caret, which clamps back inside the token and holds
        // the panel open over a token the person has already typed past.
        let text = self.query.raw_text().to_owned();
        let token = koru_tagname::hash_token_at(&text, self.query.caret());
        let Some(range) = token else {
            self.close_tag_completion();
            return;
        };
        // Ask for the table the first time one is wanted, not on open: a
        // surface nobody types a `#` into never pays for it.
        if !self.scope_tables_requested {
            self.request_scope_tables();
        }
        let typed = &text[range.start + 1..range.end];
        let query = koru_tagname::fold_segments(typed);
        self.tag_hits = self
            .tags
            .iter()
            .enumerate()
            .filter(|(_, tag)| koru_tagname::matches_query(&query, &tag.folded))
            .map(|(i, _)| i)
            .collect();
        if self.tag_hits.is_empty() {
            self.close_tag_completion();
            return;
        }
        self.tag_token = Some(range);
        // Only this popover may be open, and opening it must not disturb a
        // selection the results list is holding.
        if self.menu != SearchMenu::Tags {
            self.menu = SearchMenu::Tags;
            self.menu_scroll = 0.0;
        }
        self.menu_selected = Some(0);
    }

    fn close_tag_completion(&mut self) {
        self.tag_hits.clear();
        self.tag_token = None;
        if self.menu == SearchMenu::Tags {
            self.menu = SearchMenu::None;
            self.menu_selected = None;
        }
    }

    /// Replace the token under the caret with the chosen tag and close.
    ///
    /// A trailing space is written with it, so the next word starts cleanly and
    /// a second `#` reopens the list for another facet. Choosing commits and
    /// closes for the same reason the editor's `#tag` grammar does: the panel
    /// sits over the results, and one token is one answer.
    fn apply_tag_completion(&mut self, row: usize) {
        let (Some(range), Some(&index)) = (self.tag_token.clone(), self.tag_hits.get(row)) else {
            return;
        };
        let Some(tag) = self.tags.get(index) else {
            return;
        };
        // Raw, for the same reason `refresh_tag_completion` reads raw: the
        // range came from the untrimmed buffer, and the trailing space this
        // writes is exactly what `value` would take away.
        let mut text = self.query.raw_text().to_owned();
        text.replace_range(range.clone(), &format!("#{} ", tag.name));
        let caret = range.start + tag.name.len() + 2;
        self.query.set_text(&mut self.engine, &text);
        self.query.select_range(&mut self.engine, caret..caret);
        remember_session_query(self.query.raw_text());
        self.close_tag_completion();
        // **The text changed, so the answer has to** — the same rule
        // `apply_filter_completion` states, and for the same reason: a chosen
        // tag is not a typing aid that waits for the next keystroke to take
        // effect. Without this the page under the panel is the *previous*
        // query's, which reads as a filter that found the wrong rows rather
        // than as one that never ran.
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.clear_selection_memory();
        self.refresh();
    }

    /// Ask the reader for the scoped workspaces' tag table and attribute
    /// definitions.
    fn request_scope_tables(&mut self) {
        self.scope_tables_requested = true;
        let workspaces = self.scoped_workspaces();
        if let Some(reader) = self.reader.as_ref() {
            reader.request_scope_tables(workspaces);
        }
    }

    /// Recompute both of the query box's completions, in the order that keeps
    /// them exclusive.
    ///
    /// **Only one popover is ever open**, and a caret can only be in one token.
    /// The tag rule runs first because `#` is the older grammar and the one the
    /// *store* also reads; the filter chooser then asks about a caret the tag
    /// completion has already declined.
    fn refresh_completions(&mut self) {
        self.refresh_tag_completion();
        if self.menu == SearchMenu::Tags {
            self.close_filter_completion();
            return;
        }
        self.refresh_filter_completion();
    }

    /// Recompute the `/key` chooser from the query box, and open or close the
    /// popover to match.
    ///
    /// **Nothing here touches the store.** The definitions were fetched once
    /// per scope beside the tag table; narrowing them is a fold and a
    /// `contains` over a handful of rows.
    fn refresh_filter_completion(&mut self) {
        // `raw_text`, for the reason `refresh_tag_completion` gives: the caret
        // indexes the untrimmed buffer, and trimming it moves every offset.
        let text = self.query.raw_text().to_owned();
        let Some(context) = filters::context_at(&text, self.query.caret(), &self.definitions)
        else {
            self.close_filter_completion();
            return;
        };
        // Asked for the first time one is wanted, not on open: a surface
        // nobody types a `/` into never pays for it.
        if !self.scope_tables_requested {
            self.request_scope_tables();
        }
        self.filter_hits = match &context {
            filters::Context::Key { typed, .. } => {
                let folded = koru_tagname::fold_segment(typed);
                self.definitions
                    .iter()
                    .enumerate()
                    .filter(|(_, def)| def.matches(&folded))
                    .map(|(index, _)| index)
                    .collect()
            }
            filters::Context::Value { def, typed, .. } => {
                let folded = koru_tagname::fold_segment(typed);
                self.definitions
                    .get(*def)
                    .map(|def| {
                        def.options
                            .iter()
                            .enumerate()
                            .filter(|(_, option)| {
                                koru_tagname::fold_segment(option).contains(&folded)
                            })
                            .map(|(index, _)| index)
                            .collect()
                    })
                    .unwrap_or_default()
            }
        };
        self.filter_context = Some(context);
        // **An empty temporal value is the calendar**, not a list and not a
        // line of guidance. It is opened from the caret like everything else
        // here, so `/due ` typed by hand and *Due* chosen from the chooser
        // reach it by one path rather than two.
        //
        // **Empty, and only empty.** The picker takes every key while it is up,
        // so opening it over a value somebody is already spelling — or has
        // clicked back into to correct — would take the keyboard away
        // mid-word. A date being typed by hand keeps the one line of guidance
        // it had, and `Escape` on the calendar is what hands the box back.
        let empty_value = matches!(
            &self.filter_context,
            Some(filters::Context::Value { typed, .. }) if typed.trim().is_empty()
        );
        if empty_value && matches!(self.filter_editor(), Some(filters::FilterEditor::Temporal)) {
            self.open_temporal_picker(&text);
            return;
        }
        self.temporal = None;
        // A field says one line whatever the value is; a list with nothing left
        // in it says nothing at all, and closes rather than hanging empty.
        if self.filter_hits.is_empty() && !self.filter_is_hint() {
            self.close_filter_completion();
            return;
        }
        if self.menu != SearchMenu::Filters {
            self.menu = SearchMenu::Filters;
            self.menu_scroll = 0.0;
        }
        // A hint row is not a choice, so nothing is highlighted over it.
        self.menu_selected = (!self.filter_is_hint()).then_some(0);
    }

    /// Open the calendar over the `/key` span the caret is in, or leave the one
    /// that is already open alone.
    ///
    /// **Kept rather than rebuilt** while the caret stays in the same span: the
    /// picker holds a phrase halfway typed and a day the arrows have moved to,
    /// and rebuilding it on a refresh would throw both away. The span is
    /// re-read every time, because the text before it can have moved.
    fn open_temporal_picker(&mut self, text: &str) {
        let Some(edit) = filters::span_edit_at(text, self.query.caret(), &self.definitions) else {
            self.close_filter_completion();
            return;
        };
        let Some(def) = self.definitions.get(edit.def) else {
            self.close_filter_completion();
            return;
        };
        match &mut self.temporal {
            Some(picker) if picker.def == edit.def => picker.span = edit.range,
            _ => {
                let now = chrono::Local::now().naive_local();
                self.temporal = Some(due_picker::DueFilterPicker::open(
                    &mut self.engine,
                    edit.def,
                    &def.segment,
                    edit.range,
                    edit.op.and_then(due_picker::Side::of_op),
                    now,
                ));
            }
        }
        if self.menu != SearchMenu::Filters {
            self.menu = SearchMenu::Filters;
            self.menu_scroll = 0.0;
        }
        // The picker keeps its own highlight. `menu_selected` stays `None` so
        // the hit `Enter` would open in the results list cannot move while a
        // calendar is on screen — the rule every popover here follows.
        self.menu_selected = None;
    }

    fn close_filter_completion(&mut self) {
        self.filter_hits.clear();
        self.filter_context = None;
        self.temporal = None;
        if self.menu == SearchMenu::Filters {
            self.menu = SearchMenu::None;
            self.menu_selected = None;
        }
    }

    /// Write what the picker committed over the whole `/key…` span, and run it.
    ///
    /// **The text is the commit.** Nothing is held beside the box — the filter
    /// is re-read out of it by `filters::split` on every request, so it can be
    /// edited by hand, replayed from the recents list, and read back by the
    /// person who wrote it ([ADR-195] §5).
    ///
    /// [ADR-195]: ../../../docs/decisions/ADR-201-225.md#adr-218-filterchooser--the-search-box-parses-its-own-key-filters-and-the-chooser-is-built-from-the-definitions
    fn commit_temporal(&mut self, written: String) {
        let Some(picker) = self.temporal.take() else {
            return;
        };
        let mut text = self.query.raw_text().to_owned();
        let span = picker.span.clone();
        if span.end > text.len() {
            self.close_filter_completion();
            return;
        }
        let replacement = format!("{written} ");
        let caret = span.start + replacement.len();
        text.replace_range(span, &replacement);
        self.query.set_text(&mut self.engine, &text);
        self.query.select_range(&mut self.engine, caret..caret);
        remember_session_query(self.query.raw_text());
        self.close_filter_completion();
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.clear_selection_memory();
        self.refresh();
        self.refresh_completions();
    }

    /// Choose a row of the `/key` chooser: an attribute, or one of its values.
    ///
    /// The text is written back into the box both times, because the box is the
    /// model — a filter you can see is one you can edit and re-run, and
    /// [`filters::split`] reads it again on every request.
    fn apply_filter_completion(&mut self, row: usize) {
        let Some(context) = self.filter_context.clone() else {
            return;
        };
        let (token, replacement) = match context {
            filters::Context::Key { token, .. } => {
                let Some(def) = self
                    .filter_hits
                    .get(row)
                    .and_then(|&index| self.definitions.get(index))
                else {
                    return;
                };
                let written = match def.editor {
                    // The value follows in the same popover: a space moves the
                    // caret into it, and the chooser reopens on the values.
                    filters::FilterEditor::Options | filters::FilterEditor::Field => {
                        format!("{}{} ", filters::SIGIL, def.segment)
                    }
                    // **The calendar follows in the same popover**, by the
                    // same route `/due ` typed by hand takes: the space moves
                    // the caret into the value, and a temporal value *is* the
                    // picker. Nothing here opens it — `refresh_completions`
                    // below does, from the caret.
                    filters::FilterEditor::Temporal => {
                        format!("{}{} ", filters::SIGIL, def.segment)
                    }
                    // **No trailing space, because nothing follows.** The key
                    // is the whole filter, and a space would put the caret in
                    // a value the chooser has nothing to offer for — which is
                    // exactly the dead end `/task` used to open.
                    filters::FilterEditor::Presence => {
                        format!("{}{}", filters::SIGIL, def.segment)
                    }
                };
                (token, written)
            }
            filters::Context::Value { def, token, .. } => {
                let Some(option) = self
                    .definitions
                    .get(def)
                    .filter(|def| def.editor == filters::FilterEditor::Options)
                    .and_then(|def| {
                        self.filter_hits
                            .get(row)
                            .and_then(|&index| def.options.get(index))
                    })
                else {
                    // A field has nothing to choose; Enter simply accepts what
                    // is typed and gets out of the way.
                    self.close_filter_completion();
                    return;
                };
                let option = if option.contains(char::is_whitespace) {
                    format!("\"{option}\" ")
                } else {
                    format!("{option} ")
                };
                (token, option)
            }
        };
        let mut text = self.query.raw_text().to_owned();
        let caret = token.start + replacement.len();
        text.replace_range(token, &replacement);
        self.query.set_text(&mut self.engine, &text);
        self.query.select_range(&mut self.engine, caret..caret);
        remember_session_query(self.query.raw_text());
        self.close_filter_completion();
        // **The text changed, so the answer has to.** A chosen filter is not a
        // typing aid that waits for the next keystroke to take effect.
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.clear_selection_memory();
        self.refresh();
        // And the value list opens on the key that was just chosen, in the
        // same gesture.
        self.refresh_completions();
    }

    fn recent_query_rect(&self, index: usize) -> Rect {
        let top = self.recent_searches_heading_y() + RECENT_HEADING_TO_ROWS;
        Rect::new(
            CARD_INSET_X,
            top + index as f32 * RECENT_QUERY_STRIDE,
            self.results_width() - 16.0,
            RECENT_QUERY_ROW_H,
        )
    }

    pub fn set_pinned(&mut self, rows: &[(String, DocumentId, WorkspaceId, bool)]) {
        self.pinned = rows.to_vec();
        if self.pinned.is_empty() {
            self.selected_pin = None;
            self.pin_cursor = 0;
        } else {
            self.pin_cursor = self.pin_cursor.min(self.pinned.len() - 1);
            if let Some(i) = self.selected_pin {
                if i >= self.pinned.len() {
                    self.selected_pin = Some(self.pin_cursor);
                }
            }
        }
    }

    fn landing_pin_count(&self) -> usize {
        if self.query.is_empty() {
            self.pinned.len()
        } else {
            0
        }
    }

    fn pin_block_h(&self) -> f32 {
        if self.landing_pin_count() == 0 {
            0.0
        } else {
            LANDING_HEADING_H + PIN_CHIP_H + LANDING_SECTION_GAP
        }
    }

    fn opened_rows_offset(&self) -> f32 {
        self.pin_block_h() + LANDING_HEADING_H
    }

    fn pin_heading_y(&self) -> f32 {
        self.results_top() - self.list.scroll
    }

    fn pin_row_y(&self) -> f32 {
        self.results_top() + LANDING_HEADING_H - self.list.scroll
    }

    fn pin_chip_width_for(&mut self, title: &str, show_title: bool) -> f32 {
        let (_, label) = pin_parts(title);
        if !show_title || label.is_empty() {
            return theme::TAB_PINNED_W;
        }
        let text = self
            .engine
            .elided(label, theme::role::row(), PIN_TITLE_MAX_W);
        let layout = self.engine.simple_layout(&text, theme::role::row());
        (PIN_CHIP_PAD_X + PIN_ICON_SIZE + 6.0 + layout.width() + PIN_CHIP_PAD_X)
            .max(theme::TAB_PINNED_W)
    }

    fn pin_chip_rect(&mut self, index: usize) -> Rect {
        let y = self.pin_row_y();
        let pins: Vec<(String, bool)> = self
            .pinned
            .iter()
            .map(|(title, _, _, show)| (title.clone(), *show))
            .collect();
        let mut x = PAD;
        for (i, (title, show_title)) in pins.iter().enumerate() {
            let w = self.pin_chip_width_for(title, *show_title);
            if i == index {
                return Rect::new(x, y, w, PIN_CHIP_H);
            }
            x += w + PIN_CHIP_GAP;
        }
        Rect::new(x, y, 0.0, PIN_CHIP_H)
    }

    fn paint_pinned_rows(&mut self, canvas: &mut Scene, root: Affine) {
        if self.landing_pin_count() == 0 {
            return;
        }
        let pointer = self.pointer;
        let selected = self.selected_pin;
        let pins: Vec<(String, bool)> = self
            .pinned
            .iter()
            .map(|(title, _, _, show)| (title.clone(), *show))
            .collect();
        let y = self.pin_row_y();
        let mut x = PAD;
        for (index, (title, show_title)) in pins.iter().enumerate() {
            let w = self.pin_chip_width_for(title, *show_title);
            let rect = Rect::new(x, y, w, PIN_CHIP_H);
            let hovered = rect.contains(pointer.0, pointer.1);
            self.paint_pin_chip(
                canvas,
                root,
                rect,
                selected == Some(index),
                hovered,
                title,
                *show_title,
            );
            x += w + PIN_CHIP_GAP;
        }
    }

    fn paint_pin_chip(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        rect: Rect,
        selected: bool,
        hovered: bool,
        title: &str,
        show_title: bool,
    ) {
        let p = theme::p();
        if selected {
            scene::fill(
                canvas,
                root,
                p.row_selected,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
            );
            scene::stroke(
                canvas,
                root,
                p.accent,
                theme::BORDER_W,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
            );
        } else if hovered {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
            );
        }
        let (emoji, label) = pin_parts(title);
        let show_label = show_title && !label.is_empty();
        let icon_x = if show_label {
            rect.x + PIN_CHIP_PAD_X
        } else {
            rect.x + (rect.w - PIN_ICON_SIZE) * 0.5
        };
        if let Some(emoji) = emoji {
            let style = theme::TextStyle::new(
                theme::TextMetrics::new(10.0, 12.5),
                theme::FontWeight::Medium,
            );
            let glyph = self.engine.simple_layout(emoji, style);
            scene::draw_layout(
                canvas,
                root,
                &glyph,
                icon_x + (PIN_ICON_SIZE - glyph.width()) * 0.5,
                rect.y + (rect.h - glyph.height()) * 0.5,
                None,
            );
        } else {
            let icon = self.engine.icon_layout(icons::DOCUMENT, PIN_ICON_SIZE);
            scene::draw_layout_first_line(
                canvas,
                root,
                &icon,
                icon_x,
                rect.y + (PIN_CHIP_H - PIN_ICON_SIZE) * 0.5,
                Some(p.text_dim),
            );
        }
        if !show_label {
            return;
        }
        let text_x = icon_x + PIN_ICON_SIZE + 6.0;
        let text_w = (rect.x + rect.w - PIN_CHIP_PAD_X - text_x).max(0.0);
        let text = self.engine.elided(label, theme::role::row(), text_w);
        let text = self.engine.simple_layout(&text, theme::role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &text,
            text_x,
            rect.y + (PIN_CHIP_H - theme::role::row().metrics.line_height) * 0.5,
            Some(p.text),
        );
    }

    fn landing_opened_count(&self) -> usize {
        self.page.hits.len().min(LANDING_OPENED_LIMIT)
    }

    fn visible_hit_count(&self) -> usize {
        if self.query.is_empty() {
            self.landing_opened_count()
        } else {
            self.page.hits.len()
        }
    }

    /// Where the recent-search heading begins in landing content coordinates,
    /// before the shared pixel scroll is applied.
    fn recent_searches_offset(&self) -> f32 {
        let opened = self.landing_opened_count();
        let after_opened = self.opened_rows_offset()
            + if opened == 0 {
                44.0
            } else {
                opened as f32 * self.list.row_height
            };
        after_opened + if opened == 0 { 0.0 } else { 10.0 }
    }

    fn recent_searches_heading_y(&self) -> f32 {
        self.results_top() + self.recent_searches_offset() - self.list.scroll
    }

    fn shows_recent_queries(&self) -> bool {
        matches!(self.scope, SearchScope::Global)
    }

    fn landing_content_height(&self) -> f32 {
        let before_searches = self.recent_searches_offset();
        if self.shows_recent_queries() {
            before_searches
                + RECENT_HEADING_TO_ROWS
                + self.recent_query_count() as f32 * RECENT_QUERY_STRIDE
        } else {
            before_searches
        }
    }

    fn scroll_content_height(&self) -> f32 {
        if self.query.is_empty() {
            self.landing_content_height()
        } else {
            self.page.hits.len() as f32 * self.list.row_height
        }
    }

    fn clamp_scroll(&mut self) {
        let content_height = self.scroll_content_height();
        let viewport_height = self.results_height();
        self.list
            .clamp_scroll_for_content(content_height, viewport_height);
    }

    fn is_title_match(hit: &SearchHit) -> bool {
        // Compared against the constants, not literals: a token renamed in the
        // wire crate is then a compile error here rather than a row that
        // silently stops rendering as a title.
        matches!(hit.field.as_str(), field::TITLE | field::WORKSPACE)
    }

    fn edit_query(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) {
        let before = self.query.raw_text().to_owned();
        let debug_input = koru_ui::keys::debug::enabled();
        if debug_input {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input][pid={}] search input begin key={} text={} mods={} bytes_before={} view={:?}",
                std::process::id(),
                koru_ui::keys::debug::key_of(key),
                koru_ui::keys::debug::text_of(text),
                koru_ui::keys::debug::mods_of(mods),
                before.len(),
                self.view,
            ));
        }
        self.query.key(&mut self.engine, key, text, mods);
        let changed = self.query.raw_text() != before;
        if debug_input {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input][pid={}] search input complete changed={} bytes_after={} byte_delta={}",
                std::process::id(),
                changed,
                self.query.raw_text().len(),
                self.query.raw_text().len() as isize - before.len() as isize,
            ));
        }
        if changed {
            self.query_changed();
        }
        // **After the close above, not before it.** A keystroke closes any
        // open menu; the completion then decides for itself whether the caret
        // is still in a `#tag`. It also runs when the text did *not* change,
        // because an arrow key moves the caret out of one token and into
        // another without touching a character.
        self.refresh_completions();
    }

    /// Everything an authored query edit invalidates, whether it came from a
    /// key, paste, completion, or a final dictation transcript.
    fn query_changed(&mut self) {
        remember_session_query(self.query.raw_text());
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.selected_recent_query = None;
        self.selected_create = None;
        self.clear_selection_memory();
        self.menu = SearchMenu::None;
        self.view = SearchView::Results;
        self.refresh();
    }

    fn result_context(hit: &SearchHit) -> String {
        if hit.lifecycle == Lifecycle::Trashed {
            format!("{} · In Trash", hit.breadcrumb)
        } else {
            hit.breadcrumb.clone()
        }
    }

    fn result_card_rect(&self, y: f32) -> Rect {
        // The visible gap between the card and scrollbar thumb is exactly the
        // same as the card's left inset. The scrollbar's own inset and width
        // are part of the reserved lane, not extra whitespace.
        let reserved = theme::SCROLLBAR_W + theme::SCROLLBAR_EDGE_CLEAR + CARD_INSET_X;
        Rect::new(
            CARD_INSET_X,
            y + CARD_INSET_Y,
            (self.results_width() - CARD_INSET_X - reserved).max(1.0),
            self.list.row_height - CARD_INSET_Y * 2.0,
        )
    }

    fn profile_workspaces_rect(&self) -> Option<Rect> {
        matches!(&self.scope, SearchScope::Workspace { .. }).then_some(Rect::new(
            PAD,
            self.results_top() + 85.0,
            170.0,
            28.0,
        ))
    }

    fn archived_workspaces_rect(&self) -> Option<Rect> {
        (matches!(&self.scope, SearchScope::Global)
            && !self.archived_only
            && self
                .selected_profile
                .is_some_and(|profile| !self.catalog.archived_workspaces_of(profile).is_empty()))
        .then_some(Rect::new(PAD, self.results_top() + 85.0, 220.0, 28.0))
    }

    fn deleted_pages_rect(&self) -> Rect {
        let y = if self.profile_workspaces_rect().is_some()
            || self.archived_workspaces_rect().is_some()
        {
            self.results_top() + 113.0
        } else {
            self.results_top() + 85.0
        };
        Rect::new(PAD, y, 190.0, 28.0)
    }

    fn results_height(&self) -> f32 {
        (self.viewport.1 - self.results_top() - FOOTER_H).max(0.0)
    }

    /// The host's CREATE rows. Replacing them drops the keyboard off whatever
    /// row it was on; the hits keep their selection.
    pub fn set_create_rows(&mut self, rows: Vec<CreateRow>) {
        self.create_rows = rows;
        self.selected_create = None;
        self.hover_create = None;
    }

    /// The host's footer chips, drawn in place of the hint line.
    pub fn set_footer_chips(&mut self, chips: Vec<FooterChip>) {
        self.footer_chips = chips;
    }

    /// The host's words in the empty box (*Search or type a command*), in
    /// place of the surface's scope-shaped ones.
    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    /// What `↵` on a CREATE row asked for, once. The host answers it the way
    /// it answers a `Target`: by making the thing and closing the surface.
    pub fn take_create(&mut self) -> Option<Create> {
        self.create.take()
    }

    /// Whether the footer is the host's chips rather than the hint line.
    pub fn footer_shows_chips(&self) -> bool {
        !self.footer_chips.is_empty()
            && self.view == SearchView::Results
            && !self.replacing_results()
    }

    /// CREATE is drawn while the host gave rows and there is something typed
    /// to make them from — never over the landing page.
    fn create_shown(&self) -> bool {
        !self.create_rows.is_empty() && !self.query.is_empty() && self.view == SearchView::Results
    }

    /// The group's height: heading, rows, and the gap before the results.
    fn create_block_h(&self) -> f32 {
        if self.create_shown() {
            CREATE_HEADING_H + self.create_rows.len() as f32 * self.list.row_height + CREATE_GAP
        } else {
            0.0
        }
    }

    /// Where the group sits: between the header band and the results.
    fn create_rect(&self) -> Rect {
        let h = self.create_block_h();
        Rect::new(0.0, self.results_top() - h, self.results_width(), h)
    }

    fn create_row_rect(&self, row: usize) -> Rect {
        let band = self.create_rect();
        self.result_card_rect(band.y + CREATE_HEADING_H + row as f32 * self.list.row_height)
    }

    fn create_row_at(&self, y: f32) -> Option<usize> {
        if !self.create_shown() {
            return None;
        }
        let band = self.create_rect();
        let top = band.y + CREATE_HEADING_H;
        if y < top || y >= top + self.create_rows.len() as f32 * self.list.row_height {
            return None;
        }
        Some(((y - top) / self.list.row_height) as usize)
    }

    fn results_top(&self) -> f32 {
        let base = HEADER_H + self.filter_line_h() + self.correction_h() + self.warning_h();
        if self.query.is_empty() {
            base + LANDING_INSET
        } else {
            // The CREATE group leads the results, so the results begin under it.
            base + self.create_block_h()
        }
    }

    fn landing_heading_y(&self) -> f32 {
        self.results_top() + self.pin_block_h()
    }

    fn results_rect(&self) -> Rect {
        Rect::new(
            0.0,
            self.results_top(),
            self.results_width(),
            self.results_height(),
        )
    }

    fn scrollbar(&self) -> Option<ListScrollbar> {
        self.list.scrollbar_for_content(
            self.scroll_content_height(),
            self.results_height(),
            self.results_top(),
            theme::SCROLLBAR_MIN_THUMB,
        )
    }

    fn scrollbar_rect(&self) -> Option<Rect> {
        self.scrollbar().map(|bar| {
            bar.thumb_rect(
                self.results_width() - theme::SCROLLBAR_EDGE_CLEAR - theme::SCROLLBAR_W,
                theme::SCROLLBAR_W,
            )
        })
    }

    fn in_scrollbar_gutter(&self, x: f32, y: f32) -> bool {
        self.scrollbar().is_some()
            && self.results_rect().contains(x, y)
            && x >= self.results_width() - theme::SCROLLBAR_EDGE_CLEAR - theme::SCROLLBAR_GUTTER_W
    }

    /// What to send the store.
    ///
    /// **Scope is a set of workspace ids, not a profile and a workspace.** That
    /// is the shape the store's search takes, and the resolution it asks its
    /// caller to do: a workspace-scoped search names one, while a global search
    /// names every eligible workspace owned by its selected profile. Archived
    /// search names only that profile's live archived workspaces.
    /// The workspaces a query — or a tag fetch — is scoped to.
    ///
    /// Shared so the completion list cannot offer tags from workspaces the
    /// search itself would not look in.
    fn scoped_workspaces(&self) -> Vec<WorkspaceId> {
        match &self.scope {
            SearchScope::Global if self.archived_only => self
                .selected_profile
                .map(|profile| self.catalog.archived_workspaces_of(profile))
                .unwrap_or_default(),
            SearchScope::Global => self
                .selected_profile
                .map(|profile| self.catalog.active_workspaces_of(profile))
                .unwrap_or_default(),
            SearchScope::Workspace { workspace, .. } => vec![*workspace],
        }
    }

    fn search_query(&self, offset: usize) -> SearchQuery {
        let workspaces = match &self.scope {
            SearchScope::Global if self.archived_only => self
                .selected_profile
                .map(|profile| self.catalog.archived_workspaces_of(profile))
                .unwrap_or_default(),
            SearchScope::Global => self
                .selected_profile
                .map(|profile| self.catalog.active_workspaces_of(profile))
                .unwrap_or_default(),
            SearchScope::Workspace { workspace, .. } => {
                vec![*workspace]
            }
        };
        let excluded_workspaces = self.excluded_workspace_ids();
        // **The surface parses `/key` and the store does not.** `attributes`
        // already carries a typed filter, so a second text grammar there would
        // make one filter expressible two ways on the wire
        // ([ADR-218](../../../docs/decisions/ADR-201-225.md#adr-218-filterchooser--the-search-box-parses-its-own-key-filters-and-the-chooser-is-built-from-the-definitions)). `#tag`
        // is the exception and still travels inside `text`, because the store
        // has lifted it out since ADR-184.
        let (text, attributes) = filters::split(&self.query.value(), &self.definitions);
        SearchQuery {
            text,
            workspaces,
            excluded_workspaces,
            lifecycle: self.lifecycle,
            kinds: Vec::new(),
            // Empty on purpose: a `#tag` the person typed travels inside
            // `text` and is split out into this filter by the store
            // (`koru_search::query`), so the surface stays a text box and
            // the grammar has one home. A chip UI that filters without the
            // person typing `#` is what would fill this in.
            tags: Vec::new(),
            attributes,
            sort: self.sort,
            page_size: if self.query.is_empty() { 3 } else { 50 },
            offset,
            request_seq: self.request_seq,
        }
    }

    /// Facets eligible for the ordinary workspace filter.
    ///
    /// The store may calculate facets across every indexed partition even
    /// when the result query excludes some partitions. The lifecycle catalog
    /// is authoritative here too: one projection feeds geometry, input and
    /// painting so a hidden row cannot remain keyboard- or click-actionable.
    fn workspace_facets(&self) -> impl Iterator<Item = &SearchFacet> {
        self.facets.workspaces.iter().filter(|facet| {
            WorkspaceId::parse(&facet.workspace).is_some_and(|id| {
                self.catalog.is_active(id)
                    && self
                        .selected_profile
                        .is_none_or(|profile| self.catalog.owner_of(id) == Some(profile))
            })
        })
    }

    /// The scope decides which tag tables and which attribute definitions are
    /// on offer, so a change to it throws the cached lists away rather than
    /// completing against workspaces the search itself would no longer look in.
    fn invalidate_scope_tables(&mut self) {
        self.tags.clear();
        self.definitions.clear();
        self.scope_tables_requested = false;
        self.close_tag_completion();
        self.close_filter_completion();
    }

    fn select_profile(&mut self, profile: ProfileId) {
        self.selected_profile = Some(profile);
        self.scope = SearchScope::Global;
        self.archived_only = false;
        self.filters.clear();
        self.invalidate_scope_tables();
        remember_session_profile(Some(profile));
    }

    fn profile_selected(&self, profile: ProfileId) -> bool {
        self.selected_profile == Some(profile)
    }

    /// Every workspace this session has been told to leave out directly.
    fn excluded_workspace_ids(&self) -> Vec<WorkspaceId> {
        let mut excluded: Vec<WorkspaceId> = self
            .filters
            .excluded_workspaces()
            .iter()
            .filter_map(|id| WorkspaceId::parse(id))
            .collect();
        excluded.sort_by_key(|id| id.to_storage());
        excluded.dedup();
        excluded
    }

    fn refresh(&mut self) {
        self.request_seq = self.request_seq.wrapping_add(1);
        // Empty workspace scope means *every partition the index knows*. Do
        // not issue that query before the catalog can name archived and softly
        // deleted workspaces for exclusion.
        if matches!(self.scope, SearchScope::Global) && !self.catalog_ready && self.reader.is_some()
        {
            self.pending_read = None;
            self.pending_replace = Some(self.request_seq);
            return;
        }
        if matches!(self.scope, SearchScope::Global) {
            let searchable = self.selected_profile.map(|profile| {
                if self.archived_only {
                    self.catalog.archived_workspaces_of(profile)
                } else {
                    self.catalog.active_workspaces_of(profile)
                }
            });
            // Empty workspace scope means every indexed partition. Until a
            // profile is selected, or when it owns no eligible workspace, the
            // safe query is no query at all.
            if searchable.as_ref().is_none_or(Vec::is_empty) {
                self.pending_read = None;
                self.pending_replace = None;
                self.page.hits.clear();
                self.page.next_offset = None;
                self.next_cursor = None;
                self.ai_targets.clear();
                return;
            }
        }
        let query = self.search_query(0);
        // Keep the last complete page mounted while the newest query runs. A
        // replacement read disables activation until its matching response is
        // applied, so stable painting never makes an old result actionable for
        // new query text.
        self.page.next_offset = None;
        self.next_cursor = None;
        self.page.updating_workspaces.clear();
        self.page.corrected_query = None;
        self.request_read(SearchReadKind::FederatedPage {
            query,
            append: false,
            include_ai: self.include_ai,
            cursor: None,
            profile: self.selected_profile,
        });
    }

    fn request_read(&mut self, kind: SearchReadKind) {
        let seq = self.request_seq;
        let replaces = matches!(
            &kind,
            SearchReadKind::Page { append: false, .. }
                | SearchReadKind::FederatedPage { append: false, .. }
        );
        let sent = self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.request(seq, kind));
        self.pending_read = sent.then_some(seq);
        if replaces {
            self.pending_replace = sent.then_some(seq);
            if !sent {
                self.page.hits.clear();
            }
        }
    }

    fn replacing_results(&self) -> bool {
        self.pending_replace.is_some()
    }

    fn selection_context(&self) -> state::SearchSelectionContext {
        let scope = match &self.scope {
            SearchScope::Global => "global".to_owned(),
            SearchScope::Workspace {
                profile, workspace, ..
            } => format!(
                "workspace/{}/{}",
                profile.to_storage(),
                workspace.to_storage()
            ),
        };
        // **The resolved set, not the two things it was asked for.** A profile
        // exclusion and the workspace exclusions it expands to are the same
        // filter, so comparing on the expansion is what makes "is this the same
        // search I had a selection in" answer correctly either way.
        let mut excluded_workspaces: Vec<String> = self
            .excluded_workspace_ids()
            .iter()
            .map(|id| id.to_storage())
            .collect();
        excluded_workspaces.sort();
        state::SearchSelectionContext {
            scope,
            profile: self.selected_profile.map(|profile| profile.to_storage()),
            query: self.query.value(),
            sort: self.sort,
            lifecycle: self.lifecycle,
            archived_only: self.archived_only,
            include_ai: self.include_ai,
            excluded_workspaces,
        }
    }

    fn selection_snapshot(&self) -> Option<state::SearchSessionSelection> {
        if self.selected_recent_query.is_some() {
            return None;
        }
        let hit = self.page.hits.get(self.list.selected)?;
        Some(state::SearchSessionSelection {
            context: self.selection_context(),
            hit_key: hit_key(hit),
            index: self.list.selected,
        })
    }

    fn remember_current_selection(&self) {
        remember_session_selection(self.selection_snapshot());
    }

    fn clear_selection_memory(&mut self) {
        self.selection_restore = None;
        remember_session_selection(None);
    }

    fn ensure_current_selection_visible(&mut self) {
        if self.query.is_empty() {
            let opened = self.landing_opened_count();
            if let Some(_recent) = self.selected_recent_query {
                self.ensure_landing_selection_visible();
            } else if opened > 0 {
                self.list.selected = self.list.selected.min(opened - 1);
                self.ensure_landing_selection_visible();
            } else {
                self.list.selected = 0;
                self.clamp_scroll();
            }
        } else {
            self.list.selected = self
                .list
                .selected
                .min(self.page.hits.len().saturating_sub(1));
            self.list
                .ensure_visible(self.page.hits.len(), self.results_height());
        }
    }

    fn prepare_selection_restore(&mut self) {
        self.selection_restore = self.selection_snapshot();
    }

    /// Restore against the newly returned page, never against retained stale
    /// rows. Returns true when the saved position lies on a later page.
    fn apply_selection_restore(&mut self) -> bool {
        let Some(selection) = self.selection_restore.take() else {
            return false;
        };
        if selection.context != self.selection_context() {
            return false;
        }
        if let Some(index) = self
            .page
            .hits
            .iter()
            .position(|hit| hit_key(hit) == selection.hit_key)
        {
            self.list.selected = index;
        } else if selection.index >= self.page.hits.len()
            && (self.next_cursor.is_some() || self.page.next_offset.is_some())
        {
            self.selection_restore = Some(selection);
            return true;
        } else {
            self.list.selected = selection.index.min(self.page.hits.len().saturating_sub(1));
        }
        self.ensure_current_selection_visible();
        self.remember_current_selection();
        false
    }

    fn apply_ready_reads(&mut self) -> bool {
        let mut changed = false;
        let mut continue_selection_restore = false;
        let mut refresh_after_catalog = false;
        let Some(reader) = &self.reader else {
            return false;
        };
        let responses = reader.drain();
        for response in responses {
            // The catalog answers no query, so it is not matched against the
            // one in flight — it simply arrives.
            if let SearchReadResult::ScopeTables { tags, definitions } = response.result {
                let filters_changed = self.definitions != definitions;
                self.tags = tags;
                self.definitions = definitions;
                // The token under the caret may already be waiting on this.
                self.refresh_completions();
                // **And so may the query already in flight.** Until the
                // definitions arrived, `/priority high` was ordinary text and
                // was searched for as such; now it is a filter, and nothing
                // else will ask again.
                if filters_changed && !self.query.is_empty() {
                    changed = true;
                    self.refresh();
                }
                continue;
            }
            if let SearchReadResult::Catalog(catalog) = response.result {
                self.pending_catalog = false;
                let was_ready = self.catalog_ready;
                self.catalog_ready = catalog.ready();
                self.catalog = catalog;
                if self.catalog_ready {
                    let selected = match &self.scope {
                        SearchScope::Workspace { profile, .. } => Some(*profile),
                        SearchScope::Global => self
                            .selected_profile
                            .filter(|profile| self.catalog.has_profile(*profile))
                            .or_else(|| self.catalog.profiles().first().map(|(id, _)| *id)),
                    };
                    if self.selected_profile != selected {
                        self.selected_profile = selected;
                        remember_session_profile(selected);
                    }
                    self.unavailable = None;
                    refresh_after_catalog = !was_ready && matches!(self.scope, SearchScope::Global);
                } else if matches!(self.scope, SearchScope::Global) {
                    self.unavailable = Some(
                        "Search is unavailable — the workspace catalog is not answering."
                            .to_owned(),
                    );
                    self.page.hits.clear();
                    self.pending_replace = None;
                }
                changed = true;
                continue;
            }
            if self.pending_read != Some(response.seq) {
                continue;
            }
            self.pending_read = None;
            match response.result {
                SearchReadResult::Page {
                    append,
                    result: Ok(answer),
                } if answer.page.request_seq == response.seq => {
                    let mut page = answer.page;
                    // The store answered, so whatever it was refusing before is
                    // over.
                    self.unavailable = None;
                    if let Some(facets) = answer.facets {
                        self.facets = facets;
                    }
                    if let Some(recent_queries) = answer.recent_queries {
                        self.recent_queries = recent_queries;
                        self.selected_recent_query = self
                            .selected_recent_query
                            .and_then(|index| (index < self.recent_query_count()).then_some(index));
                    }
                    if append {
                        self.page.hits.append(&mut page.hits);
                        self.page.next_offset = page.next_offset;
                        self.page.updating_workspaces = page.updating_workspaces;
                        self.page.request_seq = response.seq;
                    } else {
                        self.page = page;
                        self.pending_replace = None;
                    }
                    keep_top_hit_per_document(&mut self.page.hits);
                    drop_hidden_kinds(&mut self.page.hits);
                    if self.selection_restore.is_some() {
                        continue_selection_restore = self.apply_selection_restore();
                    } else {
                        self.ensure_current_selection_visible();
                        self.remember_current_selection();
                    }
                    changed = true;
                }
                SearchReadResult::Page {
                    append: false,
                    result: Err(error),
                    ..
                } => {
                    // **A store that is down is not an empty page.** Clearing
                    // the hits and saying nothing paints "No results", which is
                    // a claim about somebody's writing; this is a fact about
                    // ours, and the surface says which.
                    if store::unavailable(&error) {
                        self.unavailable = Some(
                            "Search is unavailable — the Koru Store is not answering.".to_owned(),
                        );
                    } else {
                        crate::search_debug!(
                            "[Koru][search-read][pid={}] query failed: {error}",
                            std::process::id(),
                        );
                        self.unavailable = None;
                    }
                    self.page.hits.clear();
                    self.pending_replace = None;
                    changed = true;
                }
                SearchReadResult::Page { .. } => {}
                SearchReadResult::FederatedPage {
                    append,
                    result: Ok(answer),
                } if answer.page.request_seq == response.seq => {
                    let mut converted = Vec::with_capacity(answer.page.hits.len());
                    let mut targets = HashMap::new();
                    for hit in answer.page.hits {
                        match hit {
                            koru_api_client::FederatedSearchHit::Store { hit } => {
                                converted.push(hit);
                            }
                            koru_api_client::FederatedSearchHit::Ai { hit } => {
                                let display = ai_display_hit(&hit);
                                targets.insert(
                                    hit_key(&display),
                                    (hit.chat_id, hit.message_id, hit.block_id, hit.profile_id),
                                );
                                converted.push(display);
                            }
                        }
                    }
                    self.unavailable = None;
                    if let Some(facets) = answer.facets {
                        self.facets = facets;
                    }
                    if let Some(recent_queries) = answer.recent_queries {
                        self.recent_queries = recent_queries;
                        self.selected_recent_query = self
                            .selected_recent_query
                            .and_then(|index| (index < self.recent_query_count()).then_some(index));
                    }
                    if append {
                        self.page.hits.append(&mut converted);
                        self.ai_targets.extend(targets);
                    } else {
                        self.page.hits = converted;
                        self.ai_targets = targets;
                        self.pending_replace = None;
                    }
                    self.next_cursor = answer.page.next_cursor;
                    self.page.next_offset = None;
                    self.page.updating_workspaces = answer.page.updating_workspaces;
                    self.page.corrected_query = answer.page.corrected_query;
                    self.page.request_seq = response.seq;
                    keep_top_hit_per_document(&mut self.page.hits);
                    drop_hidden_kinds(&mut self.page.hits);
                    if self.selection_restore.is_some() {
                        continue_selection_restore = self.apply_selection_restore();
                    } else {
                        self.ensure_current_selection_visible();
                        self.remember_current_selection();
                    }
                    changed = true;
                }
                SearchReadResult::FederatedPage {
                    append: false,
                    result: Err(error),
                } => {
                    if store::unavailable(&error) {
                        self.unavailable = Some(
                            "Search is unavailable — the Koru API is not answering.".to_owned(),
                        );
                    } else {
                        crate::search_debug!(
                            "[Koru][search-read][pid={}] federated query failed: {error}",
                            std::process::id(),
                        );
                        self.unavailable = None;
                    }
                    self.page.hits.clear();
                    self.ai_targets.clear();
                    self.next_cursor = None;
                    self.pending_replace = None;
                    changed = true;
                }
                SearchReadResult::FederatedPage { .. } => {}
                // Handled above, before the `seq` match.
                SearchReadResult::Catalog(_) => {}
                // This surface asks for no outline. An entity picker does, and
                // it drains its own reader.
                SearchReadResult::Outline { .. } => {}
                SearchReadResult::DocumentFaces(_) => {}
                // Handled above, before the `seq` match, for the same reason
                // the catalog is: it answers no query.
                SearchReadResult::ScopeTables { .. } => {}
            }
        }
        if continue_selection_restore && self.pending_read.is_none() {
            self.load_more();
        }
        if refresh_after_catalog {
            self.refresh();
        }
        changed
    }

    /// Refresh a retained in-app search before showing it again. The query,
    /// selection, and lifecycle stay local to this reusable surface, while the
    /// result page is rebuilt from the latest committed index generation.
    pub fn resume(&mut self) {
        self.prepare_selection_restore();
        if let Some(reader) = &self.reader {
            reader.request_catalog();
            self.pending_catalog = true;
            if matches!(self.scope, SearchScope::Global) {
                self.catalog_ready = false;
            }
        }
        self.refresh();
    }

    fn search_archived_workspaces(&mut self) {
        self.scope = SearchScope::Global;
        self.archived_only = true;
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.selected_recent_query = None;
        self.clear_selection_memory();
        self.filters.clear();
        self.filters_visible = false;
        self.menu = SearchMenu::None;
        self.refresh();
    }

    /// Whether this surface will talk to a store at all.
    ///
    /// Exists so a **host's** tests can assert they are not about to query a
    /// developer's real index — see [`start_reader`](Self::start_reader) for why
    /// the host cannot check that any other way.
    pub fn has_reader(&self) -> bool {
        self.reader.is_some()
    }

    /// Which workspace this surface is scoped to, if it is scoped to one.
    ///
    /// **Public, and it used to be `#[cfg(test)]`.** A host's tests ask it —
    /// `mainview/tests.rs` checks that reopening a search window keeps its
    /// scope — and that worked only while the surface and those tests were one
    /// crate. A `#[cfg(test)]` item does not exist for anybody else, which is
    /// the kind of thing an extraction finds and nothing else does.
    fn load_more(&mut self) {
        let Some(cursor) = self.next_cursor.clone() else {
            return;
        };
        self.request_seq = self.request_seq.wrapping_add(1);
        let query = self.search_query(0);
        self.request_read(SearchReadKind::FederatedPage {
            query,
            append: true,
            include_ai: self.include_ai,
            cursor: Some(cursor),
            profile: self.selected_profile,
        });
    }

    fn move_selection(&mut self, delta: isize) -> bool {
        if self.query.is_empty() {
            return self.move_landing_selection(delta);
        }
        // The CREATE rows are above the hits and the arrows walk both as one
        // column: Up from the first hit lands on the last CREATE row, Down
        // from the last CREATE row on the first hit.
        if self.create_shown() {
            let last = self.create_rows.len() - 1;
            match self.selected_create {
                Some(row) if delta > 0 => {
                    if row < last {
                        self.selected_create = Some(row + 1);
                    } else if !self.page.hits.is_empty() {
                        self.selected_create = None;
                        self.list.selected = 0;
                    }
                    return true;
                }
                Some(row) => {
                    self.selected_create = Some(row.saturating_sub(1));
                    return true;
                }
                None if delta < 0 && self.list.selected == 0 => {
                    self.selected_create = Some(last);
                    return true;
                }
                None if delta > 0 && self.page.hits.is_empty() => {
                    self.selected_create = Some(0);
                    return true;
                }
                None => {}
            }
        }
        let changed = self
            .list
            .step(delta, self.page.hits.len(), self.results_height());
        if delta > 0 && self.list.selected.saturating_add(5) >= self.page.hits.len() {
            self.load_more();
        }
        if changed {
            self.remember_current_selection();
        }
        changed
    }

    fn recent_query_count(&self) -> usize {
        if self.query.is_empty() && self.shows_recent_queries() {
            self.recent_queries.len().min(RECENT_QUERY_LIMIT)
        } else {
            0
        }
    }

    fn move_landing_selection(&mut self, delta: isize) -> bool {
        if self.replacing_results() {
            return false;
        }
        let pin_slots = usize::from(self.landing_pin_count() > 0);
        let opened = self.landing_opened_count();
        let searches = self.recent_query_count();
        let len = pin_slots + opened + searches;
        if len == 0 {
            self.list.selected = 0;
            self.selected_recent_query = None;
            self.selected_pin = None;
            return false;
        }
        let current = if self.selected_pin.is_some() {
            Some(0)
        } else if let Some(index) = self.selected_recent_query {
            Some(pin_slots + opened + index)
        } else if opened > 0 {
            Some(pin_slots + self.list.selected.min(opened - 1))
        } else {
            None
        };
        let next = match current {
            Some(current) => (current as isize + delta).clamp(0, len as isize - 1) as usize,
            None if delta < 0 => len - 1,
            None => 0,
        };
        if pin_slots > 0 && next == 0 {
            self.selected_pin = Some(self.pin_cursor.min(self.landing_pin_count() - 1));
            self.selected_recent_query = None;
        } else if next < pin_slots + opened {
            self.list.selected = next - pin_slots;
            self.selected_pin = None;
            self.selected_recent_query = None;
        } else {
            self.selected_recent_query = Some(next - pin_slots - opened);
            self.selected_pin = None;
        }
        self.ensure_landing_selection_visible();
        let changed = current != Some(next);
        if changed {
            self.remember_current_selection();
        }
        changed
    }

    fn move_pin_horizontal(&mut self, delta: isize) -> bool {
        let pins = self.landing_pin_count();
        if pins == 0 || !self.query.is_empty() {
            return false;
        }
        let next = match self.selected_pin {
            Some(i) => (i as isize + delta).clamp(0, pins as isize - 1) as usize,
            None if delta < 0 => pins - 1,
            None => 0,
        };
        let changed = self.selected_pin != Some(next);
        self.pin_cursor = next;
        self.selected_pin = Some(next);
        self.selected_recent_query = None;
        self.ensure_landing_selection_visible();
        if changed {
            self.remember_current_selection();
        }
        true
    }

    fn ensure_landing_selection_visible(&mut self) {
        let (top, bottom) = if self.selected_pin.is_some() {
            (0.0, self.pin_block_h())
        } else if let Some(index) = self.selected_recent_query {
            let top = self.recent_searches_offset()
                + RECENT_HEADING_TO_ROWS
                + index as f32 * RECENT_QUERY_STRIDE;
            (top, top + RECENT_QUERY_ROW_H)
        } else {
            let top = self.opened_rows_offset() + self.list.selected as f32 * self.list.row_height;
            (top, top + self.list.row_height)
        };
        let viewport_height = self.results_height();
        if top < self.list.scroll {
            self.list.scroll = top;
        } else if bottom > self.list.scroll + viewport_height {
            self.list.scroll = (bottom - viewport_height).max(0.0);
        }
        self.clamp_scroll();
    }

    fn toggle_deleted_pages(&mut self) {
        self.lifecycle = match self.lifecycle {
            LifecycleFilter::Live => LifecycleFilter::Trash,
            LifecycleFilter::Trash | LifecycleFilter::All => LifecycleFilter::Live,
        };
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.clear_selection_memory();
        self.menu = SearchMenu::None;
        self.refresh();
    }

    fn refresh_after_filter_change(&mut self) {
        let before = std::mem::take(&mut self.page.hits);
        let remaining: Vec<_> = before
            .iter()
            .filter(|hit| self.filters.allows(hit))
            .cloned()
            .collect();
        let remaining_keys: Vec<_> = remaining.iter().map(hit_key).collect();
        self.exiting
            .begin(before.iter().map(hit_key), &remaining_keys, Instant::now());
        self.exiting_hits = before
            .into_iter()
            .enumerate()
            .filter(|(_, hit)| !self.filters.allows(hit))
            .collect();
        self.page.hits = remaining;
        self.request_seq = self.request_seq.wrapping_add(1);
        let query = self.search_query(0);
        self.page.next_offset = None;
        self.next_cursor = None;
        self.request_read(SearchReadKind::FederatedPage {
            query,
            append: false,
            include_ai: self.include_ai,
            cursor: None,
            profile: self.selected_profile,
        });
    }

    fn reset_filters(&mut self) {
        self.filters.clear();
        self.archived_only = false;
        self.lifecycle = LifecycleFilter::Live;
        self.menu = SearchMenu::None;
        self.refresh();
    }

    fn popover_row_at(&self, x: f32, y: f32) -> Option<usize> {
        // The `…` menu is the shared widget; its own hit test answers.
        if self.menu == SearchMenu::App {
            return self
                .app_menu
                .as_ref()
                .and_then(|menu| menu.item_at(self.menu_viewport(), x, y));
        }
        // The calendar hit-tests itself, in panel-local coordinates.
        if self.temporal.is_some() {
            return None;
        }
        let shell = self.popover()?;
        if self.menu == SearchMenu::Profiles {
            return profile_picker::row_at(
                shell.rect,
                x,
                y,
                self.menu_row_count(),
                self.menu_scroll,
            );
        }
        if !shell.contains(x, y) || y < shell.rect.y + 6.0 {
            return None;
        }
        let row = ((y - shell.rect.y - 6.0 + self.menu_scroll) / 32.0).floor() as usize;
        (row < self.menu_row_count()).then_some(row)
    }

    /// The area a floating menu may occupy: the window above the footer.
    fn menu_viewport(&self) -> (f32, f32) {
        (self.viewport.0, self.viewport.1 - FOOTER_H)
    }

    fn open_menu(&mut self, menu: SearchMenu) {
        self.menu_scroll = 0.0;
        self.menu_selected = if menu == SearchMenu::Profiles {
            let selected = self.selected_profile;
            let profiles = self.catalog.profiles();
            profiles
                .iter()
                .position(|(id, _)| Some(*id) == selected)
                .or_else(|| (!profiles.is_empty()).then_some(0))
        } else {
            None
        };
        if menu == SearchMenu::App {
            // The shared widget, hanging back under the `…` button the way
            // every `…` menu in the container does. Its rows are built from
            // the surface's state now, and fixed while the menu is open.
            let button = self.app_menu_rect();
            let rows = app_menu::AppMenuRow::list(self.show_shortcuts, self.view);
            let mut widget = koru_ui::Menu::aligned(
                &mut self.engine,
                (button.x + button.w, button.y + button.h),
                rows,
                koru_ui::Anchor::ButtonRight,
            );
            // The first row arrives highlighted, so Enter does something and
            // the arrows have somewhere to start from.
            widget.step(&[true, true, true], 1);
            self.app_menu = Some(widget);
        }
        self.menu = menu;
    }

    fn step_profile_menu(&mut self, delta: isize) {
        let rows = self.catalog.profiles().len();
        if rows == 0 {
            self.menu_selected = None;
            return;
        }
        let current = self
            .menu_selected
            .unwrap_or_else(|| if delta < 0 { 0 } else { rows - 1 });
        let next = (current as isize + delta).rem_euclid(rows as isize) as usize;
        self.menu_selected = Some(next);

        let Some(shell) = self.popover() else {
            return;
        };
        let top = shell.rect.y + profile_picker::PAD + profile_picker::HEADING_H;
        let bottom = shell.rect.y + shell.rect.h - profile_picker::PAD;
        let row = profile_picker::row_rect(shell.rect, next, self.menu_scroll);
        if row.y < top {
            self.menu_scroll -= top - row.y;
        } else if row.y + row.h > bottom {
            self.menu_scroll += row.y + row.h - bottom;
        }
        let content = profile_picker::content_height(rows);
        self.menu_scroll = self
            .menu_scroll
            .clamp(0.0, (content - shell.rect.h).max(0.0));
    }

    fn set_query_text(&mut self, text: &str) {
        self.query.set_text(&mut self.engine, text);
        self.selected_recent_query = None;
        self.clear_selection_memory();
        remember_session_query(self.query.raw_text());
    }

    fn activate_recent_query(&mut self, index: usize) -> bool {
        let Some(text) = self
            .recent_queries
            .get(index)
            .map(|recent| recent.text.clone())
        else {
            return false;
        };
        self.set_query_text(&text);
        self.list.selected = 0;
        self.list.scroll = 0.0;
        self.refresh();
        true
    }

    fn activate_popover_row(&mut self, row: usize) {
        match self.menu {
            SearchMenu::Tags => self.apply_tag_completion(row),
            SearchMenu::Filters => self.apply_filter_completion(row),
            SearchMenu::Sort => {
                if let Some(&(sort, ..)) = SORTS.get(row) {
                    self.sort = sort;
                    remember_session_sort(sort);
                    self.menu = SearchMenu::None;
                    self.refresh();
                }
            }
            SearchMenu::Profiles => {
                if let Some((profile, _)) = self.catalog.profiles().into_iter().nth(row) {
                    self.select_profile(profile);
                    self.menu = SearchMenu::None;
                    self.page.hits.clear();
                    self.list.selected = 0;
                    self.list.scroll = 0.0;
                    self.clear_selection_memory();
                    self.refresh();
                }
            }
            SearchMenu::Workspaces => {
                if row == 0 {
                    self.include_ai = !self.include_ai;
                    remember_session_include_ai(self.include_ai);
                    if !self.include_ai {
                        self.page
                            .hits
                            .retain(|hit| !self.ai_targets.contains_key(&hit_key(hit)));
                        self.ai_targets.clear();
                    }
                    self.next_cursor = None;
                    self.refresh();
                    return;
                }
                let facet = { self.workspace_facets().nth(row - 1).cloned() };
                if let Some(facet) = facet {
                    self.filters.toggle_workspace(&facet);
                    self.refresh_after_filter_change();
                }
            }
            SearchMenu::App => {
                let action = self
                    .app_menu
                    .as_ref()
                    .and_then(|menu| menu.items().get(row))
                    .map(|row| row.action);
                match action {
                    // A toggle: the menu stays open so the check is seen to
                    // move, and the row's state follows the surface's.
                    Some(app_menu::AppMenuAction::KeyboardHints) => {
                        self.show_shortcuts = !self.show_shortcuts;
                        if let Some(menu) = &mut self.app_menu {
                            for row in menu.items_mut() {
                                if row.action == app_menu::AppMenuAction::KeyboardHints {
                                    row.checked = self.show_shortcuts;
                                }
                            }
                        }
                    }
                    Some(app_menu::AppMenuAction::HelpPreview) => {
                        self.view = SearchView::HelpPreview;
                        self.menu = SearchMenu::None;
                    }
                    Some(app_menu::AppMenuAction::FeedbackPreview) => {
                        self.view = SearchView::FeedbackPreview;
                        self.menu = SearchMenu::None;
                    }
                    None => {}
                }
            }
            SearchMenu::None => {}
        }
    }

    fn activate_pin(&mut self, index: usize, disposition: Disposition) {
        let Some((_, document, workspace, _)) = self.pinned.get(index) else {
            return;
        };
        let document = *document;
        let workspace = *workspace;
        let profile = self.catalog.owner_of(workspace).or(match &self.scope {
            SearchScope::Global => self.selected_profile,
            SearchScope::Workspace { profile, .. } => Some(*profile),
        });
        let Some(profile) = profile else {
            return;
        };
        self.target = Some(Target::Store {
            profile,
            workspace,
            document: Some(document),
            node: None,
            trashed: false,
            match_text: None,
            disposition,
        });
    }

    fn request_open(&mut self, disposition: Disposition) {
        // Rows retained during a replacement read belong to the last complete
        // query. They remain visible to avoid a blank-frame flicker but cannot
        // be activated as though they matched the new input.
        if self.replacing_results() {
            return;
        }
        if self.create_shown() {
            // The row the keyboard is on; with nothing found, `↵` on the typed
            // text means the first thing it could become.
            let row = match self.selected_create {
                Some(row) => Some(row),
                None if self.page.hits.is_empty() => Some(0),
                None => None,
            };
            if let Some(row) = row {
                self.create = Some(Create {
                    row,
                    query: self.query.raw_text().trim().to_owned(),
                    disposition,
                });
                return;
            }
        }
        if self.query.is_empty()
            && let Some(index) = self.selected_pin
        {
            self.activate_pin(index, disposition);
            return;
        }
        if self.query.is_empty()
            && let Some(index) = self.selected_recent_query
        {
            self.activate_recent_query(index);
            return;
        }
        let Some(hit) = self.page.hits.get(self.list.selected) else {
            return;
        };
        if let Some(&(chat, message, block, profile)) = self.ai_targets.get(&hit_key(hit)) {
            self.target = Some(Target::Chat {
                chat,
                message,
                block,
                profile,
                disposition,
            });
            return;
        }
        let match_text = hit
            .spans
            .first()
            .and_then(|span| hit.snippet.get(span.start..span.end))
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        let Some(workspace) = WorkspaceId::parse(&hit.workspace) else {
            crate::search_debug!(
                "[Koru][search-open][pid={}] selection rejected: invalid workspace id",
                std::process::id()
            );
            return;
        };
        // **The profile comes from the workspace, not from the hit.** The
        // store's search carries no profile, and a host still opens a workspace
        // *within* one — so the catalog answers what the index will not. A
        // workspace the catalog has not heard of refuses the navigation rather
        // than guessing, which is the same shape the invalid-id rejection has.
        let Some(profile) = self.catalog.owner_of(workspace) else {
            crate::search_debug!(
                "[Koru][search-open][pid={}] selection rejected: no profile owns workspace={}",
                std::process::id(),
                hit.workspace,
            );
            return;
        };
        let allowed_profile = match &self.scope {
            SearchScope::Global => self.selected_profile,
            SearchScope::Workspace { profile, .. } => Some(*profile),
        };
        if allowed_profile != Some(profile) {
            crate::search_debug!(
                "[Koru][search-open][pid={}] selection rejected: profile access mismatch selected={} owner={} workspace={}",
                std::process::id(),
                allowed_profile
                    .map(|id| id.to_storage())
                    .as_deref()
                    .unwrap_or("-"),
                profile.to_storage(),
                workspace.to_storage(),
            );
            return;
        }
        let document = hit.document.as_deref().and_then(DocumentId::parse);
        // **Whether it is binned, not where its bin is.** The store's model has
        // no trash *root node*: trash is a flag on the manifest entry, so it is
        // a property of the document, and what a host keys its bin rows by is
        // the host's business. Carrying the flag rather than a derived id is
        // what let the last `NodeId` leave this crate — but the *decision* it
        // drives is unchanged and still load-bearing: without it a binned hit
        // takes the live branch and opens a document that is in the bin as
        // though it were not, because a binned node still resolves.
        let trashed = hit.lifecycle == Lifecycle::Trashed;
        crate::search_debug!(
            "[Koru][search-open][pid={}] selection accepted: profile={} workspace={} document={} node={} trashed={trashed} disposition={disposition:?}",
            std::process::id(),
            profile.to_storage(),
            workspace.to_storage(),
            document.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
            hit.node.as_deref().unwrap_or("-"),
        );
        if let Some(reader) = &self.reader {
            reader.record_navigation(
                self.request_seq,
                workspace,
                hit.document.clone(),
                hit.node.clone(),
                self.query.value(),
            );
        }
        self.target = Some(Target::Store {
            profile,
            workspace,
            // **Which document the node is in.** Under one `LoroDoc` per
            // document a host holds the *active* one, so a hit anywhere else
            // has to say where it lives or the node resolves to nothing and the
            // result opens nothing at all.
            document,
            node: hit.node.clone(),
            trashed,
            match_text,
            disposition,
        });
    }

    fn paint(&mut self, canvas: &mut Scene, root: Affine, caret_on: bool) {
        self.list.row_height = Self::density_row_height();
        self.clamp_scroll();
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        scene::fill(
            canvas,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, self.results_width(), HEADER_H),
        );
        let outline = self.query_outline_rect();
        scene::stroke(
            canvas,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(outline.x, outline.y, outline.w, outline.h, INPUT_RADIUS),
        );
        let icon_rect = self.search_icon_rect();
        let icon = self.engine.icon_layout(icons::SEARCH, QUERY_ICON_SIZE);
        scene::draw_layout_first_line(
            canvas,
            root,
            &icon,
            icon_rect.x,
            icon_rect.y,
            Some(p.text_dim),
        );
        let query_rect = self.query_rect();
        let placeholder = match (&self.placeholder, &self.scope) {
            (Some(words), _) => words.as_str(),
            (None, SearchScope::Global) => "Search pages, tasks, comments, and workspaces",
            (None, SearchScope::Workspace { .. }) => "Search this workspace",
        };
        let placeholder = placeholder.to_owned();
        self.query.draw(
            canvas,
            root,
            &mut self.engine,
            query_rect,
            // The document editor and Search must use the same caret quad.
            // Baseline-clamping this field silently replaced the configured
            // caret height, so one preference produced two different carets.
            InputStyle::bare(theme::role::row()),
            Some(placeholder.as_str()),
            caret_on,
        );
        self.paint_header_controls(canvas, root);
        if self.filter_line_visible() {
            self.paint_filter_line(canvas, root);
        }
        if let Some(corrected) = self.page.corrected_query.as_deref() {
            let text = format!("Showing results for {corrected}");
            let layout = self.engine.simple_layout(&text, theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &layout,
                PAD,
                HEADER_H + self.filter_line_h() + 4.0,
                Some(p.text_dim),
            );
        }

        if self.view == SearchView::Results && self.warning_message().is_some() {
            self.paint_availability_warning(canvas, root);
        }

        if self.create_shown() {
            self.paint_create(canvas, root);
        }
        if self.view != SearchView::Results {
            self.paint_secondary_view(canvas, root);
        } else if self.unavailable.is_some() {
            // The warning is the result state. Painting an empty page below it
            // would falsely say that the query completed with no matches.
        } else if self.query.is_empty() {
            self.paint_landing(canvas, root);
        } else if self.page.hits.is_empty() {
            if let Some(heading) = self.indexing_heading() {
                self.paint_indexing(canvas, root, &heading);
            } else {
                self.paint_empty_results(canvas, root);
            }
        } else {
            self.paint_results(canvas, root);
        }
        if self.menu != SearchMenu::None {
            self.paint_popover(canvas, root);
        }
        self.paint_footer(canvas, root);
        self.tooltip.draw(
            canvas,
            root,
            &mut self.engine,
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            Instant::now(),
        );
    }

    fn paint_availability_warning(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(message) = self.warning_message().map(str::to_owned) else {
            return;
        };
        let p = theme::p();
        let y = HEADER_H + self.filter_line_h() + self.correction_h();
        scene::fill(
            canvas,
            root,
            p.notice_bg,
            &scene::rect(0.0, y, self.results_width(), WARNING_H),
        );
        scene::fill(
            canvas,
            root,
            p.notice_fg.multiply_alpha(0.25),
            &scene::rect(0.0, y + WARNING_H - 1.0, self.results_width(), 1.0),
        );
        let icon = self.engine.icon_layout(icons::LINK_SIMPLE_BREAK, 15.0);
        scene::draw_layout_first_line(canvas, root, &icon, PAD, y + 8.0, Some(p.notice_fg));
        let available = (self.results_width() - PAD * 2.0 - icon.width() - 9.0).max(1.0);
        let message = self.engine.elided(&message, theme::role::meta(), available);
        let message = self.engine.simple_layout(&message, theme::role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &message,
            PAD + icon.width() + 9.0,
            y + 7.0,
            Some(p.notice_fg),
        );
    }

    fn paint_header_controls(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let filter = self.filter_rect();
        let filter_active = self.filters_visible || self.filters.active();
        if filter_active || filter.contains(self.pointer.0, self.pointer.1) {
            scene::fill(
                canvas,
                root,
                if filter_active {
                    p.chip_bg
                } else {
                    p.hover_tint
                },
                &scene::rrect(filter.x, filter.y, filter.w, filter.h, 7.0),
            );
        }
        let filter_icon = self.engine.icon_layout(icons::LIST_CHECKS, 15.0);
        scene::draw_layout_first_line(
            canvas,
            root,
            &filter_icon,
            filter.x + 5.5,
            filter.y + 5.5,
            Some(if filter_active { p.text } else { p.text_dim }),
        );

        let sort = self.sort_rect();
        // **Named always, tinted only when it was chosen.** The chip says what
        // the order is even at the default, because "last modified" is a claim
        // about the list a reader would otherwise have to infer from the ages
        // in it; the tint keeps its old meaning — you changed something — and
        // so still matches the filter chip beside it.
        let sort_chosen = self.sort != DEFAULT_SORT;
        if sort_chosen || sort.contains(self.pointer.0, self.pointer.1) {
            scene::fill(
                canvas,
                root,
                if sort_chosen { p.chip_bg } else { p.hover_tint },
                &scene::rrect(sort.x, sort.y, sort.w, sort.h, 7.0),
            );
        }
        let sort_ink = if sort_chosen { p.text } else { p.text_dim };
        let sort_icon = self.engine.icon_layout(icons::UNFOLD_ALL, 14.0);
        scene::draw_layout_first_line(
            canvas,
            root,
            &sort_icon,
            sort.x + 6.0,
            sort.y + 6.0,
            Some(sort_ink),
        );
        let label = self
            .engine
            .simple_layout(sort_label(self.sort), theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            sort.x + 27.0,
            sort.y + 6.0,
            Some(sort_ink),
        );

        let app = self.app_menu_rect();
        if self.menu == SearchMenu::App || app.contains(self.pointer.0, self.pointer.1) {
            scene::fill(
                canvas,
                root,
                if self.menu == SearchMenu::App {
                    p.chip_bg
                } else {
                    p.hover_tint
                },
                &scene::rrect(app.x, app.y, app.w, app.h, 7.0),
            );
        }
        let dots_rect = self.app_menu_icon_rect();
        let dots = self.engine.icon_layout(icons::DOTS_THREE, QUERY_ICON_SIZE);
        scene::draw_layout_first_line(
            canvas,
            root,
            &dots,
            dots_rect.x,
            dots_rect.y,
            Some(p.text_dim),
        );
    }

    fn paint_filter_line(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let workspaces = self.workspaces_chip_rect();
        scene::fill(
            canvas,
            root,
            if self.filters.active() {
                p.row_selected
            } else {
                p.chip_bg
            },
            &scene::rrect(workspaces.x, workspaces.y, workspaces.w, workspaces.h, 7.0),
        );
        let workspace_total = self.workspace_facets().count();
        // A workspace is on unless it was explicitly excluded within the
        // selected profile scope.
        let excluded = self.excluded_workspace_ids();
        let workspace_on = self
            .workspace_facets()
            .filter(|facet| {
                WorkspaceId::parse(&facet.workspace).is_none_or(|id| !excluded.contains(&id))
            })
            .count();
        let workspaces_label = if workspace_on == workspace_total {
            "Workspaces".to_owned()
        } else {
            format!("Workspaces {workspace_on}/{workspace_total}")
        };
        let dot = scene::rrect(workspaces.x + 8.0, workspaces.y + 10.0, 6.0, 6.0, 3.0);
        scene::fill(canvas, root, p.accent, &dot);
        let label = self.engine.elided(
            &workspaces_label,
            theme::role::hint(),
            (workspaces.w - 28.0).max(1.0),
        );
        let layout = self.engine.simple_layout(&label, theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &layout,
            workspaces.x + 20.0,
            workspaces.y + 6.0,
            Some(p.text),
        );
        if self.filters.active() || self.archived_only || self.lifecycle != LifecycleFilter::Live {
            let reset = self.reset_filters_rect();
            let layout = self.engine.simple_layout("Reset", theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &layout,
                reset.x + 9.0,
                reset.y + 6.0,
                Some(p.accent),
            );
        }
    }

    fn paint_landing(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let viewport = self.results_rect();
        if viewport.h <= 0.0 {
            return;
        }
        canvas.push_clip_layer(
            Fill::NonZero,
            root,
            &scene::rect(viewport.x, viewport.y, viewport.w, viewport.h),
        );
        scene::fill(
            canvas,
            root,
            p.panel_bg,
            &scene::rrect(
                CARD_INSET_X,
                viewport.y + CARD_INSET_Y,
                (viewport.w - CARD_INSET_X * 2.0).max(1.0),
                (viewport.h - CARD_INSET_Y * 2.0).max(1.0),
                10.0,
            ),
        );
        if self.landing_pin_count() > 0 {
            let heading = self.engine.simple_layout("PINNED", theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &heading,
                PAD,
                self.pin_heading_y(),
                Some(p.text_dim),
            );
        }
        self.paint_pinned_rows(canvas, root);
        let heading = self
            .engine
            .simple_layout("RECENTLY OPENED", theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &heading,
            PAD,
            self.landing_heading_y() - self.list.scroll,
            Some(p.text_dim),
        );
        if self.page.hits.is_empty() {
            let message = match &self.scope {
                SearchScope::Global => self
                    .selected_profile
                    .and_then(|profile| self.catalog.profile_name(profile))
                    .map(|name| format!("Start typing to search {name}'s workspaces."))
                    .unwrap_or_else(|| "Choose a profile to search its workspaces.".into()),
                SearchScope::Workspace { name, .. } => format!("Start typing to search {name}."),
            };
            let layout = self.engine.simple_layout(&message, theme::role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &layout,
                PAD,
                self.results_top() + self.opened_rows_offset() + 10.0 - self.list.scroll,
                Some(p.text_dim),
            );
        } else {
            self.paint_results(canvas, root);
        }
        if !self.shows_recent_queries() {
            canvas.pop_layer();
            self.paint_scrollbar(canvas, root);
            return;
        }
        let heading_y = self.recent_searches_heading_y();
        let heading = self
            .engine
            .simple_layout("RECENT SEARCHES", theme::role::hint());
        scene::draw_layout_first_line(canvas, root, &heading, PAD, heading_y, Some(p.text_dim));
        for (index, recent) in self
            .recent_queries
            .iter()
            .take(RECENT_QUERY_LIMIT)
            .enumerate()
        {
            let rect = self.recent_query_rect(index);
            if self.selected_recent_query == Some(index) {
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
                );
                scene::stroke(
                    canvas,
                    root,
                    p.accent,
                    theme::BORDER_W,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
                );
            } else if rect.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(rect.x, rect.y, rect.w, rect.h, CARD_RADIUS),
                );
            }
            let icon = self.engine.icon_layout(icons::SEARCH, 14.0);
            scene::draw_layout_first_line(
                canvas,
                root,
                &icon,
                rect.x + 10.0,
                rect.y + 8.0,
                Some(p.text_dim),
            );
            let text = self
                .engine
                .elided(&recent.text, theme::role::row(), rect.w - 44.0);
            let text = self.engine.simple_layout(&text, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &text,
                rect.x + 34.0,
                rect.y + 6.0,
                Some(p.text),
            );
        }
        canvas.pop_layer();
        self.paint_scrollbar(canvas, root);
    }

    fn paint_secondary_view(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let card = Rect::new(
            8.0,
            HEADER_H + 4.0,
            self.viewport.0 - 16.0,
            self.viewport.1 - HEADER_H - FOOTER_H - 12.0,
        );
        scene::fill(
            canvas,
            root,
            p.panel_bg,
            &scene::rrect(card.x, card.y, card.w, card.h, 10.0),
        );
        let title_text = match self.view {
            SearchView::HelpPreview => "Help preview",
            SearchView::FeedbackPreview => "Feedback preview",
            SearchView::Results => return,
        };
        let back = self.engine.icon_layout(icons::CARET_LEFT, 15.0);
        scene::draw_layout_first_line(
            canvas,
            root,
            &back,
            card.x + 14.0,
            card.y + 15.0,
            Some(p.text_dim),
        );
        let title = self.engine.simple_layout(
            title_text,
            theme::role::row().with_weight(theme::FontWeight::Bold),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            card.x + 38.0,
            card.y + 12.0,
            Some(p.text),
        );
        match self.view {
            SearchView::HelpPreview => self.paint_help_preview(canvas, root, card),
            SearchView::FeedbackPreview => self.paint_feedback_preview(canvas, root, card),
            SearchView::Results => {}
        }
    }

    fn paint_help_preview(&mut self, canvas: &mut Scene, root: Affine, card: Rect) {
        let p = theme::p();
        let mut y = card.y + 48.0;
        for (section, articles) in [
            (
                "FINDING THINGS",
                [
                    (
                        "Search every workspace",
                        "Titles, headings, tasks, properties, and body text.",
                    ),
                    (
                        "Filter and sort",
                        "Exclude a profile for this session or change result order.",
                    ),
                    (
                        "Use only the keyboard",
                        "Move with arrows; Enter opens; Ctrl+Enter opens a window.",
                    ),
                ],
            ),
            (
                "OPENING RESULTS",
                [
                    (
                        "Switch workspaces safely",
                        "Koru routes the exact profile, workspace, and document.",
                    ),
                    (
                        "Find the matched text",
                        "The authored match flashes briefly with the caret before it.",
                    ),
                    (
                        "Look in deleted pages",
                        "The no-results action searches recoverable Trash content.",
                    ),
                ],
            ),
        ] {
            let heading = self.engine.simple_layout(section, theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &heading,
                card.x + 20.0,
                y,
                Some(p.text_dim),
            );
            y += 22.0;
            for (title, description) in articles {
                if y + 34.0 > card.y + card.h - 20.0 {
                    break;
                }
                let row = Rect::new(card.x + 12.0, y - 4.0, card.w - 24.0, 38.0);
                if row.contains(self.pointer.0, self.pointer.1) {
                    scene::fill(
                        canvas,
                        root,
                        p.hover_tint,
                        &scene::rrect(row.x, row.y, row.w, row.h, 8.0),
                    );
                }
                let title = self.engine.simple_layout(title, theme::role::row());
                scene::draw_layout_first_line(canvas, root, &title, card.x + 20.0, y, Some(p.text));
                let description =
                    self.engine
                        .elided(description, theme::role::hint(), card.w - 48.0);
                let description = self.engine.simple_layout(&description, theme::role::hint());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &description,
                    card.x + 20.0,
                    y + 18.0,
                    Some(p.text_dim),
                );
                y += 42.0;
            }
            y += 8.0;
        }
    }

    fn paint_feedback_preview(&mut self, canvas: &mut Scene, root: Affine, card: Rect) {
        let p = theme::p();
        let notice = self.engine.simple_layout(
            "Preview only — no telemetry, persistence, or submission",
            theme::role::hint(),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &notice,
            card.x + 20.0,
            card.y + 48.0,
            Some(p.text_dim),
        );
        let question = self.engine.simple_layout(
            "Did you find what you were looking for?",
            theme::role::row(),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &question,
            card.x + 20.0,
            card.y + 80.0,
            Some(p.text),
        );
        for index in 0..5 {
            let rect = Rect::new(
                card.x + 20.0 + index as f32 * 34.0,
                card.y + 106.0,
                26.0,
                26.0,
            );
            scene::fill(
                canvas,
                root,
                p.chip_bg,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 8.0),
            );
            let number = self
                .engine
                .simple_layout(&(index + 1).to_string(), theme::role::hint());
            scene::draw_layout_first_line(
                canvas,
                root,
                &number,
                rect.x + 9.0,
                rect.y + 6.0,
                Some(p.text_dim),
            );
        }
        let obstacle = self
            .engine
            .simple_layout("What got in the way?", theme::role::row());
        scene::draw_layout_first_line(
            canvas,
            root,
            &obstacle,
            card.x + 20.0,
            card.y + 150.0,
            Some(p.text),
        );
        let mut chip_x = card.x + 20.0;
        for label in ["Wrong results", "Too slow", "Wrong workspace"] {
            let layout = self.engine.simple_layout(label, theme::role::hint());
            let width = layout.width() + 18.0;
            scene::fill(
                canvas,
                root,
                p.chip_bg,
                &scene::rrect(chip_x, card.y + 176.0, width, 26.0, 7.0),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &layout,
                chip_x + 9.0,
                card.y + 182.0,
                Some(p.text_dim),
            );
            chip_x += width + 7.0;
        }
        let field = Rect::new(card.x + 20.0, card.y + 218.0, card.w - 40.0, 54.0);
        scene::fill(
            canvas,
            root,
            p.hover_tint,
            &scene::rrect(field.x, field.y, field.w, field.h, 8.0),
        );
        let placeholder = self.engine.simple_layout(
            "Tell us what you expected to see — optional",
            theme::role::hint(),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &placeholder,
            field.x + 10.0,
            field.y + 9.0,
            Some(p.text_dim),
        );
    }

    fn paint_popover(&mut self, canvas: &mut Scene, root: Affine) {
        // The `…` menu is the shared widget: standard rows, standard chord
        // column, standard hover. Every row here is enabled.
        if self.menu == SearchMenu::App {
            let viewport = self.menu_viewport();
            if let Some(menu) = &self.app_menu {
                menu.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    viewport,
                    &[true, true, true],
                );
            }
            return;
        }
        let Some(shell) = self.popover() else { return };
        shell.draw(canvas, root);
        if self.menu == SearchMenu::Profiles {
            self.paint_profile_popover(canvas, root, shell);
            return;
        }
        if self.menu == SearchMenu::Tags {
            self.paint_tag_popover(canvas, root, shell);
            return;
        }
        if self.menu == SearchMenu::Filters {
            // Two disjoint fields, taken as fields so the borrow checker can
            // see they are disjoint — a method on `self` could not draw the
            // grid with `self.engine`.
            if let Some(picker) = &mut self.temporal {
                picker.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    shell.rect,
                    shell.radius,
                    Instant::now(),
                );
                return;
            }
            self.paint_filter_popover(canvas, root, shell);
            return;
        }
        let p = theme::p();
        let mut rows: Vec<(String, bool)> = Vec::new();
        match self.menu {
            SearchMenu::Sort => {
                rows.extend(
                    SORTS
                        .iter()
                        .map(|&(sort, label, _)| (label.to_owned(), self.sort == sort)),
                );
            }
            SearchMenu::Profiles => {
                rows.extend(
                    self.catalog
                        .profiles()
                        .into_iter()
                        .map(|(id, name)| (name, self.profile_selected(id))),
                );
            }
            SearchMenu::Workspaces => {
                let excluded = self.excluded_workspace_ids();
                rows.push(("Include AI chats".into(), self.include_ai));
                rows.extend(self.workspace_facets().map(|facet| {
                    let id = WorkspaceId::parse(&facet.workspace);
                    // **The workspace's name alone.** It used to read
                    // "Roadmap · Steven — Work", which named the profile on
                    // every row of a list the profile menu already groups.
                    (
                        facet.workspace_name.clone(),
                        id.is_none_or(|id| !excluded.contains(&id)),
                    )
                }));
            }
            // Each painted by its own function, above.
            SearchMenu::Tags | SearchMenu::Filters | SearchMenu::App | SearchMenu::None => return,
        }
        canvas.push_clip_layer(
            Fill::NonZero,
            root,
            &scene::rrect(
                shell.rect.x,
                shell.rect.y,
                shell.rect.w,
                shell.rect.h,
                shell.radius,
            ),
        );
        for (index, (label, checked)) in rows.iter().enumerate() {
            let row = Rect::new(
                shell.rect.x + 6.0,
                shell.rect.y + 6.0 + index as f32 * 32.0 - self.menu_scroll,
                shell.rect.w - 12.0,
                30.0,
            );
            if row.contains(self.pointer.0, self.pointer.1) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(row.x, row.y, row.w, row.h, 7.0),
                );
            }
            if *checked {
                let check = self.engine.icon_layout(icons::CHECK, 13.0);
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &check,
                    row.x + 7.0,
                    row.y + 8.0,
                    Some(p.text),
                );
            }
            let text = self.engine.elided(label, theme::role::row(), row.w - 38.0);
            let text = self.engine.simple_layout(&text, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &text,
                row.x + 28.0,
                row.y + 6.0,
                Some(p.text),
            );
        }
        let content_h = 12.0 + rows.len() as f32 * 32.0;
        if content_h > shell.rect.h {
            let track_h = shell.rect.h - 12.0;
            let thumb_h = (track_h * track_h / content_h).max(24.0);
            let max_scroll = content_h - shell.rect.h;
            let travel = track_h - thumb_h;
            let thumb_y =
                shell.rect.y + 6.0 + travel * (self.menu_scroll / max_scroll).clamp(0.0, 1.0);
            scene::fill(
                canvas,
                root,
                p.scrollbar_thumb,
                &scene::rrect(
                    shell.rect.x + shell.rect.w - 6.0,
                    thumb_y,
                    3.0,
                    thumb_h,
                    1.5,
                ),
            );
        }
        canvas.pop_layer();
    }

    fn paint_profile_popover(&mut self, canvas: &mut Scene, root: Affine, shell: PopoverShell) {
        let profiles = self.catalog.profiles();
        let rows_top = shell.rect.y + profile_picker::PAD + profile_picker::HEADING_H;
        canvas.push_clip_layer(
            Fill::NonZero,
            root,
            &scene::rect(
                shell.rect.x,
                rows_top,
                shell.rect.w,
                (shell.rect.y + shell.rect.h - rows_top).max(0.0),
            ),
        );
        for (index, (id, name)) in profiles.iter().enumerate() {
            let rect = profile_picker::row_rect(shell.rect, index, self.menu_scroll);
            if rect.y + rect.h <= rows_top || rect.y >= shell.rect.y + shell.rect.h {
                continue;
            }
            let detail = self.catalog.profile_detail(*id);
            let palette_key = if detail.is_empty() {
                id.to_storage()
            } else {
                detail.to_owned()
            };
            let selected = self.selected_profile == Some(*id);
            profile_picker::draw_row(
                canvas,
                root,
                &mut self.engine,
                rect,
                profile_picker::ProfileRow {
                    name,
                    detail,
                    palette_key: &palette_key,
                    selected,
                    removable: false,
                    menu: false,
                    avatar: None,
                    icon: self.catalog.profile_icon(*id),
                    color: self.catalog.profile_color(*id),
                },
                rect.contains(self.pointer.0, self.pointer.1) || self.menu_selected == Some(index),
            );
        }
        canvas.pop_layer();

        profile_picker::draw_heading(canvas, root, &mut self.engine, shell.rect);
        let content_h = profile_picker::content_height(profiles.len());
        if content_h > shell.rect.h {
            let p = theme::p();
            let track_h = shell.rect.h - 12.0;
            let thumb_h = (track_h * track_h / content_h).max(24.0);
            let max_scroll = content_h - shell.rect.h;
            let travel = track_h - thumb_h;
            let thumb_y =
                shell.rect.y + 6.0 + travel * (self.menu_scroll / max_scroll).clamp(0.0, 1.0);
            scene::fill(
                canvas,
                root,
                p.scrollbar_thumb,
                &scene::rrect(
                    shell.rect.x + shell.rect.w - 6.0,
                    thumb_y,
                    3.0,
                    thumb_h,
                    1.5,
                ),
            );
        }
    }

    /// **The one empty page that is not an answer.**
    ///
    /// While a workspace this query covers is still being indexed, no hits is a
    /// progress report rather than a verdict. It used to be drawn as *No
    /// current matches. Updating 15 workspace(s)…* — one line, and the half a
    /// person reads first is the half that says there is nothing. Reported as
    /// "searching by hashtag is not working" against a store that had restarted
    /// onto a schema bump: the whole index was being rebuilt, and the document
    /// carrying the tag was not reached for another two minutes.
    ///
    /// `None` once every workspace in scope is current — which is when
    /// [`paint_empty_results`](Self::paint_empty_results), the real verdict with
    /// the places left to look, takes over.
    fn indexing_heading(&self) -> Option<String> {
        match self.page.updating_workspaces.len() {
            0 => None,
            1 => Some("Still indexing one workspace…".to_owned()),
            many => Some(format!("Still indexing {many} workspaces…")),
        }
    }

    fn paint_indexing(&mut self, canvas: &mut Scene, root: Affine, heading: &str) {
        let p = theme::p();
        let title = self.engine.simple_layout(
            heading,
            theme::role::row().with_weight(theme::FontWeight::Bold),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            PAD,
            self.results_top() + 34.0,
            Some(p.text),
        );
        // Says the two things the single line did not: that nothing has matched
        // *yet*, and that the page will change on its own.
        let detail = self.engine.simple_layout(
            "No matches yet — results appear as the index catches up.",
            theme::role::meta(),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &detail,
            PAD,
            self.results_top() + 60.0,
            Some(p.text_dim),
        );
    }

    fn paint_empty_results(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let title = if self.filters.active() {
            "Nothing here — every workspace with a match is filtered out."
        } else if self.lifecycle == LifecycleFilter::Live && self.archived_only {
            "No results in archived workspaces"
        } else if self.lifecycle == LifecycleFilter::Live {
            if matches!(&self.scope, SearchScope::Workspace { .. }) {
                "No results in this workspace"
            } else {
                "No results"
            }
        } else {
            "No results in deleted pages"
        };
        let title = self.engine.simple_layout(
            title,
            theme::role::row().with_weight(theme::FontWeight::Bold),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &title,
            PAD,
            self.results_top() + 34.0,
            Some(p.text),
        );

        if self.filters.active() {
            let context = self.engine.simple_layout(
                "Turn one back on, or reset the filters.",
                theme::role::meta(),
            );
            scene::draw_layout_first_line(
                canvas,
                root,
                &context,
                PAD,
                self.results_top() + 60.0,
                Some(p.text_dim),
            );
        } else if self.lifecycle == LifecycleFilter::Live {
            let context_copy = if self.archived_only {
                "Some results may be in your deleted pages."
            } else if matches!(&self.scope, SearchScope::Workspace { .. }) {
                "Look in this profile, or include deleted pages."
            } else if self.archived_workspaces_rect().is_some() {
                "Some results may be in archived workspaces or deleted pages."
            } else {
                "Some results may be in your deleted pages."
            };
            let context = self.engine.simple_layout(context_copy, theme::role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &context,
                PAD,
                self.results_top() + 60.0,
                Some(p.text_dim),
            );
            if let Some(rect) = self.profile_workspaces_rect() {
                let action = self
                    .engine
                    .simple_layout("Search this profile", theme::role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &action,
                    rect.x,
                    rect.y + 4.0,
                    Some(p.accent),
                );
            }
            if let Some(rect) = self.archived_workspaces_rect() {
                let action = self
                    .engine
                    .simple_layout("Search archived workspaces", theme::role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &action,
                    rect.x,
                    rect.y + 4.0,
                    Some(p.accent),
                );
            }
            let action = self
                .engine
                .simple_layout("Search deleted pages", theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &action,
                self.deleted_pages_rect().x,
                self.deleted_pages_rect().y + 4.0,
                Some(p.accent),
            );
        }
    }

    /// The CREATE group: its heading and the host's rows, each a card like a
    /// hit's — the glyph, *{title} "{query}"* in the row role, the subtitle
    /// under it in the meta role.
    fn paint_create(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let band = self.create_rect();
        let heading = self.engine.simple_layout("CREATE", theme::role::hint());
        scene::draw_layout_first_line(canvas, root, &heading, PAD, band.y + 6.0, Some(p.text_dim));
        let query = self.query.raw_text().trim().to_owned();
        let rows = self.create_rows.clone();
        for (index, row) in rows.iter().enumerate() {
            let card = self.create_row_rect(index);
            if self.selected_create == Some(index) {
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
                scene::stroke(
                    canvas,
                    root,
                    p.accent,
                    theme::BORDER_W,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
            } else if self.hover_create == Some(index) {
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
            }
            let y = card.y - CARD_INSET_Y;
            let icon = self.engine.icon_layout(row.icon, 16.0);
            scene::draw_layout_first_line(canvas, root, &icon, PAD, y + 10.0, Some(p.text_dim));
            let content_x = PAD + 26.0;
            let content_w = (card.x + card.w - content_x - 8.0).max(1.0);
            let title = format!("{} \u{201c}{query}\u{201d}", row.title);
            let title = self.engine.elided(&title, theme::role::row(), content_w);
            let title = self.engine.simple_layout(&title, theme::role::row());
            scene::draw_layout_first_line(canvas, root, &title, content_x, y + 9.0, Some(p.text));
            let subtitle = self
                .engine
                .elided(&row.subtitle, theme::role::meta(), content_w);
            let subtitle = self.engine.simple_layout(&subtitle, theme::role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &subtitle,
                content_x,
                y + 34.0,
                Some(p.text_dim),
            );
        }
    }

    fn paint_results(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let width = self.results_width();
        let viewport = self.results_rect();
        if viewport.h <= 0.0 {
            return;
        }
        let text_width =
            (width - PAD * 2.0 - theme::SCROLLBAR_W - theme::SCROLLBAR_EDGE_CLEAR).max(0.0);
        let nested = self.query.is_empty();
        if !nested {
            scene::fill(
                canvas,
                root,
                p.panel_bg,
                &scene::rrect(
                    CARD_INSET_X,
                    viewport.y + CARD_INSET_Y,
                    (width - CARD_INSET_X * 2.0).max(1.0),
                    (viewport.h - CARD_INSET_Y * 2.0).max(1.0),
                    10.0,
                ),
            );
            canvas.push_clip_layer(
                Fill::NonZero,
                root,
                &scene::rect(viewport.x, viewport.y, viewport.w, viewport.h),
            );
        }
        for index in 0..self.visible_hit_count() {
            let hit = self.page.hits[index].clone();
            let top = self.results_top()
                + if self.query.is_empty() {
                    self.opened_rows_offset()
                } else {
                    0.0
                };
            let y = self.list.row_y(top, index);
            if y + self.list.row_height <= viewport.y || y >= viewport.y + viewport.h {
                continue;
            }
            if index == self.list.selected
                && self.selected_pin.is_none()
                && self.selected_recent_query.is_none()
                && self.selected_create.is_none()
            {
                let card = self.result_card_rect(y);
                scene::fill(
                    canvas,
                    root,
                    p.row_selected,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
                scene::stroke(
                    canvas,
                    root,
                    p.accent,
                    theme::BORDER_W,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
            } else if self.hover == Some(index) {
                let card = self.result_card_rect(y);
                scene::fill(
                    canvas,
                    root,
                    p.hover_tint,
                    &scene::rrect(card.x, card.y, card.w, card.h, CARD_RADIUS),
                );
            }
            let title_match = Self::is_title_match(&hit);
            let glyph = if self.ai_targets.contains_key(&hit_key(&hit)) {
                icons::SPARKLE
            } else if hit.kind == "workspace" {
                icons::FOLDER
            } else if hit.document_kind == "file" {
                koru_ui::file_icon::FileIcon::new(hit.mime.as_deref(), Some(&hit.title)).glyph()
            } else {
                icons::DOCUMENT
            };
            let icon = self.engine.icon_layout(glyph, 16.0);
            scene::draw_layout_first_line(canvas, root, &icon, PAD, y + 10.0, Some(p.text_dim));
            let content_x = PAD + 26.0;
            let content_width = (text_width - 26.0).max(0.0);
            if title_match {
                draw_highlighted_line(
                    canvas,
                    root,
                    &mut self.engine,
                    &hit.snippet,
                    hit.spans.iter().map(|span| Span {
                        start: span.start,
                        end: span.end,
                    }),
                    content_x,
                    y + 17.0,
                    content_width,
                    theme::role::row(),
                    p.text,
                    p.text,
                    p.highlights[0].1,
                );
            } else {
                let title = self
                    .engine
                    .elided(&hit.title, theme::role::row(), content_width);
                let title = self.engine.simple_layout(&title, theme::role::row());
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &title,
                    content_x,
                    y + 9.0,
                    Some(p.text),
                );
            }
            let mut context = Self::result_context(&hit);
            let mut context_offset = 0;
            if self.sort == SearchSort::ObservedModified
                && let Some(modified) = hit.observed_modified_at_ms
            {
                let prefix = format!("{} · ", relative_age(modified));
                context_offset = prefix.len();
                context = format!("{prefix}{context}");
            }
            let context_y = if title_match { y + 41.0 } else { y + 31.0 };
            draw_highlighted_line(
                canvas,
                root,
                &mut self.engine,
                &context,
                hit.breadcrumb_spans.iter().map(|span| Span {
                    start: span.start + context_offset,
                    end: span.end + context_offset,
                }),
                content_x,
                context_y,
                content_width,
                theme::role::meta(),
                p.text_dim,
                p.text_dim,
                p.highlights[0].1,
            );
            if !title_match {
                draw_highlighted_line(
                    canvas,
                    root,
                    &mut self.engine,
                    &hit.snippet,
                    hit.spans.iter().map(|span| Span {
                        start: span.start,
                        end: span.end,
                    }),
                    content_x,
                    y + 51.0,
                    content_width,
                    theme::role::meta(),
                    p.text_dim,
                    p.text,
                    p.highlights[0].1,
                );
            }
        }
        let now = Instant::now();
        for (original_index, hit) in &self.exiting_hits {
            if self.query.is_empty() && *original_index >= LANDING_OPENED_LIMIT {
                continue;
            }
            let key = hit_key(hit);
            let Some(progress) = self.exiting.progress(&key, now) else {
                continue;
            };
            let progress = progress.clamp(0.0, 1.0);
            let collapse = ((progress - 0.38) / 0.62).clamp(0.0, 1.0);
            let y = self.list.row_y(viewport.y, *original_index) - self.list.row_height * collapse;
            let alpha = (1.0 - collapse).max(0.0);
            let text = self
                .engine
                .elided(&hit.title, theme::role::row(), text_width - 26.0);
            let text = self.engine.simple_layout(&text, theme::role::row());
            scene::draw_layout_first_line(
                canvas,
                root,
                &text,
                PAD + 26.0,
                y + 9.0,
                Some(p.text_dim.with_alpha(alpha)),
            );
            scene::fill(
                canvas,
                root,
                p.text_dim.with_alpha(alpha * 0.6),
                &scene::rect(PAD + 26.0, y + 18.0, text.width(), 1.0),
            );
        }
        if !nested {
            canvas.pop_layer();
        }

        if !self.query.is_empty() {
            self.paint_scrollbar(canvas, root);
        }
    }

    fn paint_scrollbar(&self, canvas: &mut Scene, root: Affine) {
        if let Some(thumb) = self.scrollbar_rect() {
            scene::fill(
                canvas,
                root,
                theme::p().scrollbar_thumb,
                &scene::rrect(thumb.x, thumb.y, thumb.w, thumb.h, thumb.w * 0.5),
            );
        }
    }

    fn paint_footer(&mut self, canvas: &mut Scene, root: Affine) {
        let p = theme::p();
        let y = self.viewport.1 - FOOTER_H;
        scene::fill(
            canvas,
            root,
            p.panel_bg,
            &scene::rect(0.0, y, self.viewport.0, FOOTER_H),
        );
        scene::fill(
            canvas,
            root,
            p.panel_border,
            &scene::rect(0.0, y, self.viewport.0, 1.0),
        );
        canvas.push_clip_layer(
            Fill::NonZero,
            root,
            &scene::rect(PAD, y, (self.viewport.0 - PAD * 2.0).max(0.0), FOOTER_H),
        );
        let mut x = PAD;
        if matches!(self.scope, SearchScope::Global) {
            self.paint_footer_profile(canvas, root);
            let profile = self.profiles_chip_rect();
            x = profile.x + profile.w + 12.0;
        }
        if self.footer_shows_chips() {
            // The host's chips stand in for the hint line: a key in a small
            // pill, what it does beside it.
            let chips = self.footer_chips.clone();
            for chip in &chips {
                let key = self.engine.simple_layout(&chip.key, theme::role::hint());
                let pill_w = key.width() + 10.0;
                let pill = Rect::new(x, y + 7.0, pill_w, 20.0);
                scene::stroke(
                    canvas,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &scene::rrect(pill.x, pill.y, pill.w, pill.h, 4.0),
                );
                scene::draw_layout_first_line(
                    canvas,
                    root,
                    &key,
                    pill.x + 5.0,
                    scene::centre_y_in(&key, pill),
                    Some(p.text_dim),
                );
                x += pill_w + 5.0;
                let label = self.engine.simple_layout(&chip.label, theme::role::hint());
                scene::draw_layout_first_line(canvas, root, &label, x, y + 9.0, Some(p.text_dim));
                x += label.width() + 14.0;
            }
            canvas.pop_layer();
            return;
        }
        let hints: &[(char, &str)] = if self.view != SearchView::Results {
            &[(icons::CARET_LEFT, " Esc back")]
        } else if self.replacing_results() {
            &[(icons::SEARCH, " Searching…")]
        } else if self.show_shortcuts {
            &[
                (icons::CARET_DOWN, " Move  ·  "),
                (icons::CHECK, " Enter open  ·  "),
                (icons::POP_OUT, " Ctrl+Enter/click window  ·  "),
                (icons::WINDOW_CLOSE, " Esc close"),
            ]
        } else {
            &[]
        };
        for (icon, label) in hints {
            let glyph = self.engine.icon_layout(*icon, 13.0);
            scene::draw_layout_first_line(canvas, root, &glyph, x, y + 7.0, Some(p.text_dim));
            x += glyph.width();
            let text = self.engine.simple_layout(label, theme::role::hint());
            scene::draw_layout_first_line(canvas, root, &text, x, y + 7.0, Some(p.text_dim));
            x += text.width();
        }
        if hints.is_empty() && matches!(self.scope, SearchScope::Workspace { .. }) {
            let label = "Ctrl+Space  Switch to global scope";
            let text = self.engine.simple_layout(label, theme::role::hint());
            scene::draw_layout_first_line(canvas, root, &text, x, y + 9.0, Some(p.text_dim));
        }
        canvas.pop_layer();
    }

    fn paint_footer_profile(&mut self, canvas: &mut Scene, root: Affine) {
        let rect = self.profiles_chip_rect();
        let hot = rect.contains(self.pointer.0, self.pointer.1);
        let p = theme::p();
        if hot || self.menu == SearchMenu::Profiles {
            scene::fill(
                canvas,
                root,
                p.hover_tint,
                &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
            );
        }
        let profile = self.selected_profile;
        let name = profile
            .and_then(|id| self.catalog.profile_name(id))
            .unwrap_or("Choose profile")
            .to_owned();
        let detail = profile
            .map(|id| self.catalog.profile_detail(id))
            .unwrap_or_default()
            .to_owned();
        let palette_key = if detail.is_empty() {
            profile.map(|id| id.to_storage()).unwrap_or_default()
        } else {
            detail
        };
        let avatar = Rect::new(rect.x + 3.0, rect.y + 3.0, 22.0, 22.0);
        profile_picker::draw_avatar(
            canvas,
            root,
            &mut self.engine,
            avatar,
            None,
            &name,
            &palette_key,
            theme::role::hint(),
        );
        let caret_w = 16.0;
        let label_x = avatar.x + avatar.w + 7.0;
        let label = self.engine.elided(
            &name,
            theme::role::hint(),
            (rect.x + rect.w - caret_w - 8.0 - label_x).max(1.0),
        );
        let label = self.engine.simple_layout(&label, theme::role::hint());
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            label_x,
            scene::centre_y_in(&label, rect),
            Some(p.text),
        );
        let caret = self.engine.icon_layout(icons::CARET_DOWN, 12.0);
        scene::draw_layout_first_line(
            canvas,
            root,
            &caret,
            rect.x + rect.w - caret_w,
            scene::centre_y_in(&caret, rect),
            Some(p.text_dim),
        );
    }
}

fn ai_display_hit(hit: &koru_ai_core::AiSearchHit) -> SearchHit {
    SearchHit {
        workspace: format!("ai:chat:{}", hit.chat_id),
        workspace_name: "Koru AI".into(),
        document: Some(hit.chat_id.to_string()),
        node: Some(hit.block_id.to_string()),
        document_kind: "ai_chat".into(),
        mime: None,
        kind: "ai_chat".into(),
        field: field::BODY.into(),
        title: hit.chat_title.clone(),
        breadcrumb: "Koru AI".into(),
        breadcrumb_spans: Vec::new(),
        snippet: hit.snippet.clone(),
        spans: hit
            .spans
            .iter()
            .map(|span| koru_store_wire::search::MatchSpan {
                start: span.start,
                end: span.end,
            })
            .collect(),
        lifecycle: Lifecycle::Live,
        trashed_at: None,
        trashed_by: None,
        score: f64::from(hit.score),
        observed_modified_at_ms: None,
        workspace_last_used_at_ms: None,
    }
}

fn relative_age(timestamp_ms: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let seconds = now.saturating_sub(timestamp_ms).max(0) / 1_000;
    match seconds {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

impl Default for SearchSurface {
    fn default() -> Self {
        Self::new()
    }
}

/// What a host drives, and what it gets back.
///
/// **Inherent, not a trait.** These were `koru_workspace_host::View`'s until stage 5,
/// and seven of them took a `&mut WorkspaceDoc` or a `&Workspace` that this
/// surface never read — it is a list of results over an HTTP query, and the
/// document under the window is nothing to do with it. Each host writes the
/// adapter its own loop wants: `apps/koru` an `impl View`, the agent a
/// `WindowApp`. Every method still answers **whether anything changed**, so a
/// host redraws only when it must.
impl SearchSurface {
    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let command_modifier = mods.control_key() || mods.super_key() || mods.alt_key();
        // Recover modified editing keys before interpreting Ctrl+letter
        // shortcuts: ASCII Backspace is U+0008, which otherwise looks exactly
        // like Ctrl+H and opens Help instead of deleting a word.
        let recovered_editing_key = koru_ui::keys::recover_modified_editing_key(key, mods);
        let key = recovered_editing_key.as_ref().unwrap_or(key);
        // A recovered U+0008/U+007F belongs to Backspace, not shortcut text.
        // Passing that original text to `ctrl_letter` maps it to Ctrl+H and
        // opens Help before the shared input ever sees the edit.
        let named_modified_editing_key = (mods.control_key() || mods.super_key())
            && matches!(key, Key::Named(NamedKey::Backspace | NamedKey::Delete));
        let shortcut_text = if recovered_editing_key.is_some() || named_modified_editing_key {
            None
        } else {
            text
        };
        let control_letter = if mods.control_key() || mods.super_key() {
            koru_ui::keys::ctrl_letter(key, shortcut_text)
        } else {
            None
        };
        if koru_ui::keys::debug::enabled() {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input][pid={}] search receive key={} text={} mods={} recovered_editing={} shortcut_text={} control_letter={:?} view={:?} bytes={}",
                std::process::id(),
                koru_ui::keys::debug::key_of(key),
                koru_ui::keys::debug::text_of(text),
                koru_ui::keys::debug::mods_of(mods),
                recovered_editing_key.is_some(),
                koru_ui::keys::debug::text_of(shortcut_text),
                control_letter,
                self.view,
                self.query.raw_text().len(),
            ));
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if self.menu == SearchMenu::Tags {
                // Its own close, so the token and the hits go with the panel.
                self.close_tag_completion();
            } else if self.menu == SearchMenu::Filters {
                self.close_filter_completion();
            } else if self.menu != SearchMenu::None {
                self.menu = SearchMenu::None;
            } else if self.view != SearchView::Results {
                self.view = SearchView::Results;
            } else if !self.query.is_empty() {
                self.set_query_text("");
                self.refresh();
            } else {
                self.window_request = Some(WindowRequest::Close);
            }
            return true;
        }
        // **The calendar owns the keyboard while it is open**, every key of
        // it. That is what makes `2w`, `+3d` and `sept 10` typable into its
        // phrase box — a chord left to the surface would be a character the
        // box never receives, and the field's own Ctrl+A and Ctrl+V belong to
        // whoever has the caret. Escape, above, is the way out.
        if self.temporal.is_some() {
            let now = Instant::now();
            let outcome = self.temporal.as_mut().expect("just checked").key(
                &mut self.engine,
                key,
                text,
                mods,
                now,
            );
            match outcome {
                due_picker::Picked::Consumed => {}
                due_picker::Picked::Commit(written) => self.commit_temporal(written),
                // Backspacing out of `/due ` is editing the query, not driving
                // a calendar: the picker closes and the box gets the key.
                due_picker::Picked::Released => {
                    self.close_filter_completion();
                    self.edit_query(key, text, mods);
                }
            }
            return true;
        }
        if (mods.control_key() || mods.super_key())
            && !mods.shift_key()
            && !mods.alt_key()
            && matches!(key, Key::Named(NamedKey::Space))
            && self.switch_to_profile_scope()
        {
            return true;
        }
        if (mods.control_key() || mods.super_key())
            && matches!(key, Key::Character(value) if value.as_str() == ",")
        {
            if self.menu == SearchMenu::App {
                self.menu = SearchMenu::None;
            } else {
                self.open_menu(SearchMenu::App);
            }
            return true;
        }
        // The `…` menu is keyboard-driven, the way every menu in the
        // container is: arrows step the highlight, Enter chooses, Escape
        // (above) closes. Anything else falls through — typing still edits
        // the query, as it does with this surface's other menus open.
        if self.menu == SearchMenu::App {
            match key {
                Key::Named(NamedKey::ArrowDown) => {
                    if let Some(menu) = &mut self.app_menu {
                        menu.step(&[true, true, true], 1);
                    }
                    return true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    if let Some(menu) = &mut self.app_menu {
                        menu.step(&[true, true, true], -1);
                    }
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    let row = self.app_menu.as_ref().and_then(|menu| menu.hover);
                    if let Some(row) = row {
                        self.activate_popover_row(row);
                    }
                    return true;
                }
                _ => {}
            }
        }
        // **Before the results list sees any of these.** While the completion
        // is up it owns Up, Down, Enter, Tab and Escape; the hit the results
        // list has selected must not move underneath it.
        if matches!(self.menu, SearchMenu::Tags | SearchMenu::Filters) {
            let rows = self.menu_row_count();
            // A hint row is not a choice, so the arrows have nothing to move
            // and Enter accepts what is already typed.
            let selectable = self.menu != SearchMenu::Filters || !self.filter_is_hint();
            match key {
                Key::Named(NamedKey::ArrowDown) if rows > 0 && selectable => {
                    let current = self.menu_selected.unwrap_or(rows - 1);
                    self.menu_selected = Some((current + 1) % rows);
                    return true;
                }
                Key::Named(NamedKey::ArrowUp) if rows > 0 && selectable => {
                    let current = self.menu_selected.unwrap_or(0);
                    self.menu_selected = Some((current + rows - 1) % rows);
                    return true;
                }
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Tab) => {
                    match self.menu_selected {
                        Some(row) => self.activate_popover_row(row),
                        // Nothing highlighted means a field: accepting is
                        // simply getting the panel out of the way.
                        None => self.close_filter_completion(),
                    }
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    // Closes the list and changes **no text** — there is
                    // nothing to undo, because nothing was written.
                    self.close_tag_completion();
                    self.close_filter_completion();
                    return true;
                }
                _ => {}
            }
        }
        if self.menu == SearchMenu::Profiles {
            match key {
                Key::Named(NamedKey::ArrowDown) => {
                    self.step_profile_menu(1);
                    return true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.step_profile_menu(-1);
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(row) = self.menu_selected {
                        self.activate_popover_row(row);
                    }
                    return true;
                }
                _ => {}
            }
        }
        if self.view != SearchView::Results {
            if control_letter == Some('h') {
                koru_ui::keys::debug::write(format_args!(
                    "[Koru][input][pid={}] search claimed=help-shortcut",
                    std::process::id()
                ));
                self.view = SearchView::HelpPreview;
            } else if control_letter == Some('f') && mods.shift_key() {
                koru_ui::keys::debug::write(format_args!(
                    "[Koru][input][pid={}] search claimed=feedback-shortcut",
                    std::process::id()
                ));
                self.view = SearchView::FeedbackPreview;
            } else {
                // The query remains a real focused input on read-only Help and
                // Feedback screens. Editing it returns to results; navigation
                // keys that do not edit remain safely swallowed here.
                self.edit_query(key, text, mods);
            }
            return true;
        }
        if control_letter == Some('o') {
            if self.menu == SearchMenu::Sort {
                self.menu = SearchMenu::None;
            } else {
                self.open_menu(SearchMenu::Sort);
            }
            return true;
        }
        if control_letter == Some('f') && mods.shift_key() {
            self.view = SearchView::FeedbackPreview;
            self.menu = SearchMenu::None;
            return true;
        }
        if control_letter == Some('f') {
            self.filters_visible = !self.filters_visible;
            if !self.filters_visible {
                self.reset_filters();
            }
            return true;
        }
        if control_letter == Some('p') && mods.alt_key() && !mods.shift_key() {
            self.open_menu(SearchMenu::Profiles);
            return true;
        }
        if control_letter == Some('h') {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input][pid={}] search claimed=help-shortcut",
                std::process::id()
            ));
            self.view = SearchView::HelpPreview;
            self.menu = SearchMenu::None;
            return true;
        }
        if (mods.control_key() || mods.super_key())
            && matches!(key, Key::Character(value) if value.as_str() == "/" || value.as_str() == "?")
        {
            self.show_shortcuts = !self.show_shortcuts;
            return true;
        }
        match key {
            Key::Named(NamedKey::ArrowDown) => self.move_selection(1),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(-1),
            Key::Named(NamedKey::ArrowLeft) if self.query.is_empty() => {
                let _ = self.move_pin_horizontal(-1);
                true
            }
            Key::Named(NamedKey::ArrowRight) if self.query.is_empty() => {
                let _ = self.move_pin_horizontal(1);
                true
            }
            Key::Named(NamedKey::PageDown) => self.move_selection(8),
            Key::Named(NamedKey::PageUp) => self.move_selection(-8),
            Key::Named(NamedKey::Enter) => {
                let disposition = if mods.control_key() || mods.super_key() {
                    Disposition::NewWindow
                } else {
                    Disposition::CurrentWindow
                };
                self.request_open(disposition);
                true
            }
            Key::Named(NamedKey::Tab) if !command_modifier => true,
            _ => {
                self.edit_query(key, text, mods);
                true
            }
        }
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, ctrl: bool) -> bool {
        self.pointer = (x, y);
        self.tooltip.clear();
        if self.menu != SearchMenu::None {
            // The calendar first: its rows are cells, not a 32px list, and its
            // own hit test is the only one that knows where a day is drawn.
            if self.temporal.is_some() {
                let shell = self.popover().map(|shell| shell.rect);
                match shell.filter(|rect| rect.contains(x, y)) {
                    Some(rect) => {
                        let now = Instant::now();
                        let outcome = self.temporal.as_mut().expect("just checked").click(
                            &mut self.engine,
                            x - rect.x,
                            y - rect.y,
                            now,
                        );
                        if let due_picker::Picked::Commit(written) = outcome {
                            self.commit_temporal(written);
                        }
                    }
                    // A press outside closes it and writes nothing, the same
                    // dismissal every other popover here has.
                    None => self.close_filter_completion(),
                }
                return true;
            }
            if let Some(row) = self.popover_row_at(x, y) {
                self.activate_popover_row(row);
            } else if self.menu == SearchMenu::Tags {
                // A press outside closes it and writes nothing — the same
                // dismissal every other popover here has, and the same one the
                // editor's tag chooser has.
                self.close_tag_completion();
            } else if self.menu == SearchMenu::Filters {
                self.close_filter_completion();
            } else {
                self.menu = SearchMenu::None;
            }
            return true;
        }
        if self.app_menu_rect().contains(x, y) {
            self.open_menu(SearchMenu::App);
            return true;
        }
        if self.view != SearchView::Results {
            if y >= HEADER_H && y <= HEADER_H + 52.0 {
                self.view = SearchView::Results;
            }
            return true;
        }
        if self.filter_rect().contains(x, y) {
            self.filters_visible = !self.filters_visible;
            if !self.filters_visible {
                self.reset_filters();
            }
            return true;
        }
        if self.sort_rect().contains(x, y) {
            self.open_menu(SearchMenu::Sort);
            return true;
        }
        if matches!(self.scope, SearchScope::Global) && self.profiles_chip_rect().contains(x, y) {
            self.open_menu(SearchMenu::Profiles);
            return true;
        }
        if self.filter_line_visible() && self.workspaces_chip_rect().contains(x, y) {
            self.open_menu(SearchMenu::Workspaces);
            return true;
        }
        if self.filter_line_visible()
            && (self.filters.active()
                || self.archived_only
                || self.lifecycle != LifecycleFilter::Live)
            && self.reset_filters_rect().contains(x, y)
        {
            self.reset_filters();
            return true;
        }
        let query_rect = self.query_rect();
        if self.query.click(&mut self.engine, query_rect, x, y) {
            return true;
        }
        if self.lifecycle == LifecycleFilter::Live
            && !self.filters.active()
            && self.page.hits.is_empty()
            && !self.query.is_empty()
            && self
                .profile_workspaces_rect()
                .is_some_and(|rect| rect.contains(x, y))
        {
            self.switch_to_profile_scope();
            return true;
        }
        // The scrollbar is above every row, including compact recent-query
        // rows whose wide hit rectangles reach into its gutter.
        if self.in_scrollbar_gutter(x, y) {
            if let Some(bar) = self.scrollbar() {
                let thumb = self.scrollbar_rect().unwrap();
                let grab = if thumb.contains(x, y) {
                    y - thumb.y
                } else {
                    bar.thumb_height * 0.5
                };
                self.list.scroll = bar.scroll_for_pointer(y, grab);
                self.scroll_drag = Some(grab);
            }
            return true;
        }
        if self.query.is_empty() {
            for index in 0..self.landing_pin_count() {
                if self.pin_chip_rect(index).contains(x, y) {
                    self.selected_pin = Some(index);
                    self.selected_recent_query = None;
                    self.request_open(if ctrl {
                        Disposition::NewWindow
                    } else {
                        Disposition::CurrentWindow
                    });
                    return true;
                }
            }
        }
        if self.query.is_empty() && self.shows_recent_queries() {
            for index in 0..self.recent_query_count() {
                if self.recent_query_rect(index).contains(x, y) {
                    self.activate_recent_query(index);
                    return true;
                }
            }
        }
        if self.lifecycle == LifecycleFilter::Live
            && !self.filters.active()
            && self.page.hits.is_empty()
            && !self.query.is_empty()
            && self
                .archived_workspaces_rect()
                .is_some_and(|rect| rect.contains(x, y))
        {
            self.search_archived_workspaces();
            return true;
        }
        if self.lifecycle == LifecycleFilter::Live
            && !self.filters.active()
            && self.page.hits.is_empty()
            && !self.query.is_empty()
            && self.deleted_pages_rect().contains(x, y)
        {
            self.toggle_deleted_pages();
            return true;
        }
        if let Some(row) = self.create_row_at(y)
            && self.create_row_rect(row).contains(x, y)
        {
            self.selected_create = Some(row);
            self.request_open(if ctrl {
                Disposition::NewWindow
            } else {
                Disposition::CurrentWindow
            });
            return true;
        }
        if self.results_rect().contains(x, y)
            && let Some(row) = self.list.row_at(
                self.results_top()
                    + if self.query.is_empty() {
                        self.opened_rows_offset()
                    } else {
                        0.0
                    },
                y,
                self.visible_hit_count(),
            )
        {
            self.list.selected = row;
            self.selected_recent_query = None;
            self.selected_pin = None;
            self.remember_current_selection();
            if !self.query.is_empty() && row.saturating_add(5) >= self.page.hits.len() {
                self.load_more();
            }
            self.request_open(if ctrl {
                Disposition::NewWindow
            } else {
                Disposition::CurrentWindow
            });
            return true;
        }
        false
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        // A popover is modal to this surface. Its rows do their own hit test;
        // the result list underneath must not react merely because the picker
        // happens to cover it.
        if self.menu != SearchMenu::None {
            // The `…` menu's widget tracks which row the pointer is over;
            // the older popovers answer hover at draw time instead.
            if self.menu == SearchMenu::App {
                let viewport = self.menu_viewport();
                if let Some(menu) = &mut self.app_menu {
                    menu.hover = menu.item_at(viewport, x, y);
                }
            } else if self.menu == SearchMenu::Profiles {
                let next = self.popover_row_at(x, y);
                let changed = next != self.menu_selected;
                self.menu_selected = next;
                return changed | self.hover.take().is_some() | self.tooltip.clear();
            } else if self.temporal.is_some() {
                let rect = self.popover().map(|shell| shell.rect);
                let changed = match (rect, &mut self.temporal) {
                    (Some(rect), Some(picker)) => picker.hover(x - rect.x, y - rect.y),
                    _ => false,
                };
                return changed | self.hover.take().is_some() | self.tooltip.clear();
            }
            let changed = self.hover.take().is_some();
            return changed | self.tooltip.clear();
        }
        let tooltip_changed = self
            .tooltip
            .set_hover(self.tooltip_at(x, y), Instant::now());
        let query_rect = self.query_rect();
        if self.query.drag(&mut self.engine, query_rect, x) {
            return true;
        }
        if let Some(grab) = self.scroll_drag
            && let Some(bar) = self.scrollbar()
        {
            let next = bar.scroll_for_pointer(y, grab);
            let changed = (next - self.list.scroll).abs() > f32::EPSILON;
            self.list.scroll = next;
            return changed || tooltip_changed;
        }
        let next = if self.results_rect().contains(x, y) && !self.in_scrollbar_gutter(x, y) {
            self.list
                .row_at(self.results_top(), y, self.visible_hit_count())
        } else {
            None
        };
        let changed = next != self.hover;
        self.hover = next;
        let next_create = self
            .create_row_at(y)
            .filter(|row| self.create_row_rect(*row).contains(x, y));
        let create_changed = next_create != self.hover_create;
        self.hover_create = next_create;
        changed || create_changed || tooltip_changed
    }

    pub fn mouse_up(&mut self) -> bool {
        self.query.release();
        self.scroll_drag = None;
        // View's mouse-up boolean tells the shell that customer state changed.
        // Scrolling is local presentation state and must never touch Loro.
        false
    }

    pub fn mouse_double_click(&mut self, x: f32, y: f32) -> bool {
        // **The second press of a double click is not a second answer.** The
        // first one names a day and raises the step *under the pointer*, so
        // replaying it would commit whichever side happened to be drawn there.
        if self.temporal.is_some() {
            return true;
        }
        if self.menu != SearchMenu::None {
            return self.mouse_down(x, y, false);
        }
        let query_rect = self.query_rect();
        if self.query.double_click(&mut self.engine, query_rect, x, y) {
            return true;
        }
        self.request_open(Disposition::CurrentWindow);
        true
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * self.list.row_height * 0.7,
            MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
        };
        if self.menu != SearchMenu::None {
            // A calendar is a fixed panel: there is nothing under it to reach.
            if self.temporal.is_some() {
                return;
            }
            if let Some(shell) = self.popover() {
                let content = if self.menu == SearchMenu::Profiles {
                    profile_picker::content_height(self.menu_row_count())
                } else {
                    12.0 + self.menu_row_count() as f32 * 32.0
                };
                self.menu_scroll =
                    (self.menu_scroll + amount).clamp(0.0, (content - shell.rect.h).max(0.0));
            }
            return;
        }
        let content_height = self.scroll_content_height();
        self.list
            .scroll_by_content(amount, content_height, self.results_height());
        if self.query.is_empty() {
            return;
        }
        let visible_bottom = self.list.scroll + self.results_height();
        if visible_bottom + self.list.row_height * 5.0
            >= self.page.hits.len() as f32 * self.list.row_height
        {
            self.load_more();
        }
    }

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.clamp_scroll();
    }
    pub fn invalidate(&mut self) {
        self.engine.clear_pill_cache();
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let read = (self.pending_read.is_some() || self.pending_catalog).then(|| now + READ_POLL);
        // **The slide needs a wake as well as an `animating`.** One host reads
        // `animating`, the other only asks for a deadline — so a step that
        // announced itself in one place and not the other would slide in
        // `apps/koru` and jump in the agent's overlay.
        let sliding = self
            .temporal
            .as_ref()
            .is_some_and(|picker| picker.animating(now))
            .then(|| now + READ_POLL);
        [
            read,
            sliding,
            self.exiting.deadline(),
            self.tooltip.maintenance_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn maintain(&mut self, now: Instant) -> bool {
        let sliding = self
            .temporal
            .as_ref()
            .is_some_and(|picker| picker.animating(now));
        let changed = self.apply_ready_reads() || self.tooltip.maintain(now) || sliding;
        if self.exiting.maintain(now) {
            self.exiting_hits.clear();
            return true;
        }
        changed
    }

    pub fn animating(&self) -> bool {
        let now = Instant::now();
        self.exiting.animating()
            || self.tooltip.animating(now)
            // **Returns to `false` when the step lands.** ADR-26 makes this the
            // one exception to drawing only on change, and a surface that never
            // stops asking burns a core.
            || self
                .temporal
                .as_ref()
                .is_some_and(|picker| picker.animating(now))
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine, caret_on: bool, pointer: (f32, f32)) {
        self.pointer = pointer;
        self.paint(canvas, root, caret_on);
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        let deleted = self.lifecycle == LifecycleFilter::Live
            && !self.filters.active()
            && self.page.hits.is_empty()
            && !self.query.is_empty()
            && self
                .deleted_pages_rect()
                .contains(self.pointer.0, self.pointer.1);
        let profile_workspaces = self.lifecycle == LifecycleFilter::Live
            && !self.filters.active()
            && self.page.hits.is_empty()
            && !self.query.is_empty()
            && self
                .profile_workspaces_rect()
                .is_some_and(|rect| rect.contains(self.pointer.0, self.pointer.1));
        let control = self.filter_rect().contains(self.pointer.0, self.pointer.1)
            || self.sort_rect().contains(self.pointer.0, self.pointer.1)
            || self
                .app_menu_rect()
                .contains(self.pointer.0, self.pointer.1)
            || (matches!(self.scope, SearchScope::Global)
                && self
                    .profiles_chip_rect()
                    .contains(self.pointer.0, self.pointer.1))
            || (self.filter_line_visible()
                && (self
                    .workspaces_chip_rect()
                    .contains(self.pointer.0, self.pointer.1)
                    || self
                        .reset_filters_rect()
                        .contains(self.pointer.0, self.pointer.1)))
            || self
                .popover()
                .is_some_and(|popover| popover.contains(self.pointer.0, self.pointer.1))
            || self
                .recent_queries
                .iter()
                .take(RECENT_QUERY_LIMIT)
                .enumerate()
                .any(|(index, _)| {
                    self.query.is_empty()
                        && self.shows_recent_queries()
                        && self
                            .recent_query_rect(index)
                            .contains(self.pointer.0, self.pointer.1)
                });
        // Whole hit lane (wider than the drawn pill), and while a drag is held
        // even if the pointer leaves it.
        if self.scroll_drag.is_some() || self.in_scrollbar_gutter(self.pointer.0, self.pointer.1) {
            CursorIcon::NsResize
        } else if (self.hover.is_some() && !self.replacing_results())
            || deleted
            || profile_workspaces
            || control
        {
            koru_ui::cursor::PRESS
        } else if self.query_rect().contains(self.pointer.0, self.pointer.1) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    pub fn is_global_search(&self) -> bool {
        true
    }
    /// A result the person chose, taken by the asking. The host opens it.
    pub fn take_target(&mut self) -> Option<Target> {
        self.target.take()
    }
    pub fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.window_request.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_api_client::SearchResponse;

    fn set_query(surface: &mut SearchSurface, text: &str) {
        surface.query.set_text(&mut surface.engine, text);
        // Tests that install their own page are modelling a completed read,
        // not the asynchronous landing-page request started by `new`.
        surface.pending_read = None;
        surface.pending_replace = None;
    }

    /// Tell a surface who owns the workspaces its fixture hits are in.
    ///
    /// **Opening a result needs this.** The profile no longer rides on a hit,
    /// so the surface asks the catalog whose workspace it is — and refuses the
    /// navigation when nothing answers, which is the correct behaviour and
    /// exactly what an empty test catalog would produce.
    fn with_catalog(surface: &mut SearchSurface, profile: ProfileId, workspace: WorkspaceId) {
        surface.catalog = Catalog::for_test(
            vec![(workspace, "Roadmap".into(), profile)],
            vec![(profile, "Profile".into())],
        );
        surface.selected_profile = Some(profile);
    }

    /// A node id, which is a **string** here.
    ///
    /// The index stores one as text and this crate has no type for it — see the
    /// module note. Minting a `DocumentId` is simply the shortest way to a
    /// fresh uuid in a crate that holds no other id generator.
    fn a_node_id() -> String {
        DocumentId::new().to_storage()
    }

    /// A hit as the store would answer with one.
    ///
    /// **The profile is no longer on it**, so the parameter stays only to keep
    /// every caller's shape — a hit belongs to a workspace, and whose workspace
    /// that is comes from the catalog.
    fn hit(_profile: ProfileId, workspace: WorkspaceId, node: &str) -> SearchHit {
        SearchHit {
            workspace: workspace.to_string(),
            workspace_name: "Roadmap".into(),
            document: Some(node.to_string()),
            node: Some(node.to_string()),
            document_kind: "page".into(),
            mime: None,
            kind: "document".into(),
            title: "Shortcuts".into(),
            breadcrumb: "Roadmap / Shortcuts".into(),
            breadcrumb_spans: Vec::new(),
            field: field::BODY.into(),
            snippet: "Shortcuts".into(),
            spans: vec![],
            lifecycle: Lifecycle::Live,
            trashed_at: None,
            trashed_by: None,
            score: 1.0,
            observed_modified_at_ms: None,
            workspace_last_used_at_ms: None,
        }
    }

    #[test]
    fn only_workspace_and_document_titles_use_the_compact_result() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut result = hit(profile, workspace, &node);

        // **Tokens, not English labels.** The store ranks and stores these, and
        // a surface resolves whatever words it wants to show.
        result.field = field::TITLE.into();
        assert!(SearchSurface::is_title_match(&result));
        result.field = field::WORKSPACE.into();
        assert!(SearchSurface::is_title_match(&result));

        result.field = field::heading(1);
        assert!(!SearchSurface::is_title_match(&result));
        result.field = field::BODY.into();
        assert!(!SearchSurface::is_title_match(&result));
    }

    #[test]
    fn result_context_does_not_repeat_the_matching_field() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut result = hit(profile, workspace, &node);

        assert_eq!(
            SearchSurface::result_context(&result),
            "Roadmap / Shortcuts"
        );
        result.lifecycle = Lifecycle::Trashed;
        assert_eq!(
            SearchSurface::result_context(&result),
            "Roadmap / Shortcuts · In Trash"
        );
    }

    /// A surface with a tag table, and nothing else set up.
    fn surface_with_tags() -> SearchSurface {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);
        surface.tags = ["fun", "fun/home", "fun/outside", "later"]
            .iter()
            .enumerate()
            .map(|(i, name)| TagCompletion {
                name: (*name).to_owned(),
                color: None,
                uses: 10 - i,
                folded: koru_tagname::fold_segments(name),
            })
            .collect();
        // The reader is `None` under test, so nothing dials a store; say the
        // tables have already arrived so the completion does not ask for one.
        surface.scope_tables_requested = true;
        surface
    }

    /// The scope's attribute definitions as the store would answer them: the
    /// four system ones a workspace always has, plus **one a workspace
    /// invented**. That last row is what every filter test below is really
    /// about — it must work with no code that knows its name.
    fn a_scope_of_definitions() -> Vec<koru_store_wire::attributes::AttributeDefinition> {
        fn def(
            key: &str,
            label: &str,
            value_type: &str,
            options: &[&str],
        ) -> koru_store_wire::attributes::AttributeDefinition {
            koru_store_wire::attributes::AttributeDefinition {
                id: key.to_owned(),
                key: key.to_owned(),
                label: label.to_owned(),
                value_type: value_type.to_owned(),
                cardinality: "one".to_owned(),
                scope: "shared".to_owned(),
                system: key.starts_with("koru."),
                options: options.iter().map(|o| (*o).to_owned()).collect(),
                filterable: true,
                sortable: true,
                groupable: true,
            }
        }
        vec![
            def("koru.tag", "Tags", "multi_select", &[]),
            def("koru.color", "Color", "select", &["red", "teal"]),
            def("koru.due", "Due", "datetime_zoned", &[]),
            def(
                "koru.priority",
                "Priority",
                "select",
                &["low", "medium", "high", "urgent"],
            ),
            def("user.effort", "Effort", "number", &[]),
        ]
    }

    /// A surface whose scope tables have arrived, and nothing else set up.
    fn surface_with_filters() -> SearchSurface {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);
        surface.definitions = filters::digest(&a_scope_of_definitions());
        surface.scope_tables_requested = true;
        surface
    }

    /// Which rows the chooser is offering, as text.
    fn offered(surface: &SearchSurface) -> Vec<String> {
        match &surface.filter_context {
            Some(filters::Context::Key { .. }) => surface
                .filter_hits
                .iter()
                .map(|&i| surface.definitions[i].label.clone())
                .collect(),
            Some(filters::Context::Value { def, .. }) => {
                let def = &surface.definitions[*def];
                surface
                    .filter_hits
                    .iter()
                    .map(|&i| def.options[i].clone())
                    .collect()
            }
            None => Vec::new(),
        }
    }

    fn type_str(surface: &mut SearchSurface, text: &str) {
        for ch in text.chars() {
            let s = ch.to_string();
            surface.edit_query(
                &Key::Character(s.clone().into()),
                Some(&s),
                ModifiersState::empty(),
            );
        }
    }

    /// **The popover opens on a `#` token and only there.** The rule is
    /// `koru-tagname`'s, the same one the store's query parser reads a filter
    /// with, so the box cannot offer a completion that would not become one.
    #[test]
    fn the_completion_opens_on_a_hash_token() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "fun");
        assert_eq!(surface.menu, SearchMenu::None, "a bare word is not a tag");

        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fu");
        assert_eq!(surface.menu, SearchMenu::Tags);
        assert_eq!(surface.tag_hits.len(), 3, "fun and its two children");

        let mut surface = surface_with_tags();
        type_str(&mut surface, "a#b");
        assert_eq!(surface.menu, SearchMenu::None, "mid-word is ordinary text");
    }

    /// A token naming nothing offers nothing, rather than an empty panel.
    #[test]
    fn a_token_that_matches_no_tag_opens_nothing() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#zzz");
        assert_eq!(surface.menu, SearchMenu::None);
    }

    /// **A `/` in the token narrows to a branch**, the way it does in the
    /// editor's chooser — which is the whole reason the matcher is shared.
    #[test]
    fn a_path_token_narrows_to_the_branch() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fun/h");
        assert_eq!(surface.tag_hits.len(), 1);
        assert_eq!(surface.tags[surface.tag_hits[0]].name, "fun/home");
    }

    /// Choosing replaces **only the token**, leaves the rest of the query
    /// alone, and closes.
    #[test]
    fn choosing_replaces_the_token_and_closes() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "hello #fu");
        assert_eq!(surface.menu, SearchMenu::Tags);
        let row = surface
            .tag_hits
            .iter()
            .position(|&i| surface.tags[i].name == "fun/home")
            .expect("fun/home is on offer");
        surface.activate_popover_row(row);

        assert_eq!(surface.query.raw_text(), "hello #fun/home ");
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.tag_token.is_none());
        assert_eq!(
            surface.query.caret(),
            "hello #fun/home ".len(),
            "and the caret is past the space, ready for the next word"
        );
    }

    /// **Escape closes and writes nothing.** There is no undo to offer because
    /// nothing was changed.
    #[test]
    fn escape_closes_the_completion_and_changes_no_text() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fu");
        let before = surface.query.raw_text().to_owned();
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(),));
        assert_eq!(surface.menu, SearchMenu::None);
        assert_eq!(surface.query.raw_text(), before);
    }

    /// **The results list must not move while the completion owns the
    /// keyboard.** This is the rule `menu_selected` already exists to keep for
    /// the profile picker, applied to one more popover.
    #[test]
    fn arrows_move_the_completion_and_not_the_results() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fu");
        // **After typing, not before**: a keystroke resets the results
        // selection to the top, which is its own correct behaviour and would
        // otherwise be mistaken here for the popover moving it.
        surface.list.selected = 2;
        assert_eq!(surface.menu_selected, Some(0));

        for _ in 0..2 {
            assert!(surface.key(
                &Key::Named(NamedKey::ArrowDown),
                None,
                ModifiersState::empty(),
            ));
        }
        assert_eq!(surface.menu_selected, Some(2));
        assert_eq!(surface.list.selected, 2, "the hit under Enter did not move");
    }

    /// Moving the caret out of the token closes the list even though no
    /// character changed — which is why the refresh does not sit behind the
    /// `changed` guard.
    #[test]
    fn leaving_the_token_closes_the_completion() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fu ");
        assert_eq!(surface.menu, SearchMenu::None, "the space ended the token");
    }

    /// **The caret has to be touching the tag**, and a *complete* name is not a
    /// special case: `#fun` with the caret against it still offers, and one
    /// space later it does not.
    ///
    /// Reported from a build that predated the fix for it, which is why the
    /// boundary is pinned from both sides here rather than only the closing
    /// one — a test that says "closed" and never says "open" would also pass
    /// on a popover that had stopped working altogether.
    #[test]
    fn a_completed_name_offers_while_touched_and_closes_after_a_space() {
        let mut surface = surface_with_tags();
        type_str(&mut surface, "#fun");
        assert_eq!(
            surface.menu,
            SearchMenu::Tags,
            "the caret is against the name, so the branch is still on offer"
        );

        type_str(&mut surface, " ");
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.tag_token.is_none(), "and the token went with it");
        assert!(surface.tag_hits.is_empty());
    }

    /// A scope change throws both tables away: completing against a workspace
    /// the search would not look in offers a filter that finds nothing.
    #[test]
    fn changing_the_scope_invalidates_the_tables() {
        let mut surface = surface_with_tags();
        surface.definitions = filters::digest(&a_scope_of_definitions());
        type_str(&mut surface, "#fu");
        assert_eq!(surface.menu, SearchMenu::Tags);

        surface.invalidate_scope_tables();
        assert!(surface.tags.is_empty());
        assert!(surface.definitions.is_empty());
        assert!(
            !surface.scope_tables_requested,
            "and they will be asked for again"
        );
        assert_eq!(surface.menu, SearchMenu::None);
    }

    // ------------------------------------------------------ the `/key` chooser

    /// **The chooser opens on a `/` token and only there.** The rule is
    /// `koru_tagname::token_at`'s — the same one `#` uses — so a date and a
    /// path stay the text they have always been.
    #[test]
    fn the_chooser_opens_on_a_slash_token() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/pri");
        assert_eq!(surface.menu, SearchMenu::Filters);
        assert_eq!(offered(&surface), ["Priority"]);

        for text in ["10/28", "and/or", "/nonesuch"] {
            let mut surface = surface_with_filters();
            type_str(&mut surface, text);
            assert_eq!(surface.menu, SearchMenu::None, "{text} is ordinary text");
        }
    }

    /// A bare `/` offers everything the scope can be filtered on — and
    /// **`koru.tag` is not among them**: its options are the tag table, and `#`
    /// is already its grammar.
    #[test]
    fn a_bare_slash_offers_the_filterable_attributes() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/");
        assert_eq!(offered(&surface), ["Color", "Due", "Priority", "Effort"]);
    }

    /// **The row a workspace invented, with no code that knows its name.**
    /// `user.effort` is offered, is spelled by its last segment, and its type
    /// decides that it opens a field rather than a list.
    #[test]
    fn a_custom_definition_is_offered_and_opens_the_editor_its_type_implies() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/eff");
        assert_eq!(offered(&surface), ["Effort"]);

        surface.activate_popover_row(0);
        assert_eq!(surface.query.raw_text(), "/effort ");
        assert_eq!(
            surface.filter_editor(),
            Some(filters::FilterEditor::Field),
            "a number is typed, not picked"
        );
        assert_eq!(surface.menu_row_count(), 1, "one line of guidance");
        assert_eq!(
            surface.menu_selected, None,
            "and a hint is not something Enter chooses"
        );
    }

    /// **The whole contract, end to end.** Choosing a key opens its values;
    /// choosing a value writes the filter into the box, sends it as
    /// `SearchQuery.attributes` with the right kinds, and leaves nothing of it
    /// in the text the store searches for.
    #[test]
    fn choosing_a_key_then_a_value_sends_a_filter_and_strips_it_from_the_text() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "quarterly /pri");
        surface.activate_popover_row(0);
        assert_eq!(surface.query.raw_text(), "quarterly /priority ");
        assert_eq!(
            offered(&surface),
            ["low", "medium", "high", "urgent"],
            "the value list opened in the same gesture"
        );
        assert_eq!(
            surface.menu_selected,
            Some(0),
            "and a list of values is pickable — only a field's hint is not"
        );

        type_str(&mut surface, "hi");
        assert_eq!(offered(&surface), ["high"]);
        surface.activate_popover_row(0);
        assert_eq!(surface.query.raw_text(), "quarterly /priority high ");
        assert_eq!(surface.menu, SearchMenu::None);

        let query = surface.search_query(0);
        assert_eq!(query.text, "quarterly");
        assert_eq!(query.attributes.len(), 1);
        assert_eq!(query.attributes[0].key, "koru.priority");
        assert_eq!(query.attributes[0].values, ["high"]);
        assert_eq!(
            query.attributes[0].kinds,
            ["text"],
            "a select's token is stored as text, and a filter naming no kind matches nothing"
        );
    }

    /// The custom number attribute filters with a comparison and no new code —
    /// the test that says the design is general rather than four special cases.
    #[test]
    fn a_custom_number_filters_with_a_comparison() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/effort >3");
        let query = surface.search_query(0);
        assert!(query.text.is_empty());
        assert_eq!(query.attributes.len(), 1);
        assert_eq!(query.attributes[0].key, "user.effort");
        assert_eq!(
            query.attributes[0].op,
            koru_store_wire::search::AttrOp::After
        );
        assert_eq!(query.attributes[0].values, ["3"]);
        assert_eq!(query.attributes[0].kinds, ["number"]);
    }

    // ------------------------------------------------- the `/due` calendar

    /// Midnight of `day` in this machine's zone, as epoch milliseconds — the
    /// bound the `datetime_zoned` half of a due window is compared against.
    ///
    /// Written out here rather than read back from the surface, so the test
    /// says *which* instant a calendar day means: the local midnight, not the
    /// UTC one and not noon.
    fn local_midnight_ms(day: chrono::NaiveDate) -> i64 {
        use chrono::TimeZone;
        chrono::Local
            .from_local_datetime(&day.and_time(chrono::NaiveTime::MIN))
            .single()
            .expect("no DST jump at midnight on the days used here")
            .timestamp_millis()
    }

    /// Put the open picker on a known day, so nothing here depends on when it
    /// is run.
    fn aim_at(surface: &mut SearchSurface, day: chrono::NaiveDate) {
        surface
            .temporal
            .as_mut()
            .expect("the calendar is open")
            .grid
            .selected = day;
    }

    fn press(surface: &mut SearchSurface, key: NamedKey) -> bool {
        surface.key(&Key::Named(key), None, ModifiersState::empty())
    }

    fn command_bar() -> SearchSurface {
        let mut surface = SearchSurface::new();
        surface.viewport = (640.0, 400.0);
        surface.set_create_rows(vec![
            CreateRow {
                icon: 'a',
                title: "New task".into(),
                subtitle: "Tasks · Later".into(),
            },
            CreateRow {
                icon: 'b',
                title: "New document".into(),
                subtitle: "Documents".into(),
            },
        ]);
        surface
    }

    /// **A host that supplies no rows sees no group** — the Agent's overlay
    /// is that host — and no chips means the surface's own hint line.
    #[test]
    fn a_host_with_no_rows_draws_no_create_group() {
        let mut surface = SearchSurface::new();
        surface.viewport = (640.0, 400.0);
        set_query(&mut surface, "fix login");
        assert!(!surface.create_shown());
        assert_eq!(surface.create_block_h(), 0.0);
        assert_eq!(surface.results_top(), HEADER_H);
        assert!(!surface.footer_shows_chips());
        // Nothing above the (empty) hits to climb onto.
        assert!(!press(&mut surface, NamedKey::ArrowUp));
        assert_eq!(surface.selected_create, None);
        press(&mut surface, NamedKey::Enter);
        assert_eq!(surface.take_create(), None);
    }

    /// **CREATE leads the results while there is a query**, the arrows walk
    /// the rows and the hits as one column, and `↵` on a row asks for the
    /// thing — `Ctrl+↵` for it in a window, as for a hit.
    #[test]
    fn create_rows_lead_the_results_and_enter_asks_for_the_thing() {
        let mut surface = command_bar();
        assert!(
            !surface.create_shown(),
            "nothing typed: the landing page, no group"
        );
        set_query(&mut surface, "fix login");
        assert!(surface.create_shown());
        let block = surface.create_block_h();
        assert!(block > 0.0);
        assert_eq!(
            surface.results_top(),
            HEADER_H + block,
            "the hits begin under it"
        );
        assert_eq!(surface.create_rect().y, HEADER_H);

        // Up from the first hit lands on the last row; Up again climbs.
        assert!(press(&mut surface, NamedKey::ArrowUp));
        assert_eq!(surface.selected_create, Some(1));
        assert!(press(&mut surface, NamedKey::ArrowUp));
        assert_eq!(surface.selected_create, Some(0));
        assert!(press(&mut surface, NamedKey::ArrowUp));
        assert_eq!(surface.selected_create, Some(0), "the top stays the top");
        assert!(press(&mut surface, NamedKey::ArrowDown));
        assert_eq!(surface.selected_create, Some(1));
        // With no hits, Down from the last row has nowhere to go.
        assert!(press(&mut surface, NamedKey::ArrowDown));
        assert_eq!(surface.selected_create, Some(1));

        assert!(press(&mut surface, NamedKey::Enter));
        assert_eq!(
            surface.take_create(),
            Some(Create {
                row: 1,
                query: "fix login".into(),
                disposition: Disposition::CurrentWindow,
            })
        );
        assert_eq!(surface.take_create(), None, "taken once");

        surface.selected_create = Some(0);
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::CONTROL));
        assert_eq!(
            surface.take_create().map(|c| (c.row, c.disposition)),
            Some((0, Disposition::NewWindow))
        );

        // Typing again drops the keyboard back onto the hits.
        type_str(&mut surface, "!");
        assert_eq!(surface.selected_create, None);
        assert_eq!(surface.query.raw_text(), "fix login!");
    }

    /// A click lands on the row under it, and the pointer over one hovers it.
    #[test]
    fn a_click_on_a_create_row_asks_for_it() {
        let mut surface = command_bar();
        set_query(&mut surface, "fix");
        let card = surface.create_row_rect(1);
        let (x, y) = (card.x + 20.0, card.y + card.h * 0.5);
        assert!(surface.mouse_move(x, y));
        assert_eq!(surface.hover_create, Some(1));
        assert!(surface.mouse_down(x, y, false));
        assert_eq!(surface.take_create().map(|c| c.row), Some(1));
        // Above the rows is the heading, which is not a choice.
        let band = surface.create_rect();
        assert!(!surface.mouse_down(PAD, band.y + 4.0, false));
    }

    /// **The host's chips replace the hint line**, and only while results are
    /// showing — a secondary view keeps its own *Esc back*.
    #[test]
    fn footer_chips_replace_the_hint_line() {
        let mut surface = command_bar();
        set_query(&mut surface, "fix");
        assert!(
            !surface.footer_shows_chips(),
            "no chips supplied: the hint line"
        );
        surface.set_footer_chips(vec![FooterChip {
            key: "↵".into(),
            label: "open".into(),
        }]);
        assert!(surface.footer_shows_chips());
        surface.view = SearchView::HelpPreview;
        assert!(!surface.footer_shows_chips());
    }

    /// **Choosing a temporal key opens the calendar.** A temporal key is
    /// offered — hiding Due would be a lie about what the box can filter on —
    /// and choosing it moves the caret into the value, which is what a temporal
    /// value *is*: the picker.
    #[test]
    fn choosing_a_temporal_key_opens_the_calendar() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due");
        assert_eq!(offered(&surface), ["Due"]);
        surface.activate_popover_row(0);
        assert_eq!(surface.query.raw_text(), "/due ");
        assert_eq!(surface.menu, SearchMenu::Filters);
        assert!(surface.temporal.is_some(), "the calendar is showing");
        assert_eq!(
            surface.menu_selected, None,
            "the picker keeps its own highlight, so the results list keeps its own"
        );

        // And typing it by hand reaches the same picker: one path, from the
        // caret, rather than one for the chooser and one for the keyboard.
        let mut typed = surface_with_filters();
        type_str(&mut typed, "/due ");
        assert!(typed.temporal.is_some());
    }

    /// **The whole gesture.** A day names itself, the step asks which side, and
    /// the answer lands in the box as text that parses back into the filter it
    /// came from — over both kinds a due may be stored as, with a bound written
    /// for each column.
    #[test]
    fn a_chosen_date_raises_the_step_and_the_filter_reaches_the_query() {
        use koru_store_wire::search::AttrOp;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        let mut surface = surface_with_filters();
        type_str(&mut surface, "quarterly /due ");
        aim_at(&mut surface, day);
        surface.list.selected = 2;

        assert!(press(&mut surface, NamedKey::Enter));
        assert!(
            surface.temporal.is_some(),
            "a date alone is not a filter: the step is asking which side"
        );
        assert_eq!(
            surface.query.raw_text(),
            "quarterly /due ",
            "and nothing is written until it is answered"
        );

        assert!(press(&mut surface, NamedKey::ArrowDown));
        assert_eq!(
            surface.list.selected, 2,
            "the hit under Enter did not move while the picker owned the keyboard"
        );
        assert!(press(&mut surface, NamedKey::Enter));
        assert_eq!(surface.query.raw_text(), "quarterly /due:after 2026-09-10 ");
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.temporal.is_none());

        let query = surface.search_query(0);
        assert_eq!(query.text, "quarterly", "the filter is not a search term");
        assert_eq!(query.attributes.len(), 1);
        let filter = &query.attributes[0];
        assert_eq!(filter.key, "koru.due");
        assert_eq!(filter.op, AttrOp::After);
        assert_eq!(filter.values, ["2026-09-10"]);
        assert_eq!(filter.kinds, ["date", "datetime_zoned"]);
        assert_eq!(
            filter.numeric_values,
            [local_midnight_ms(day).to_string()],
            "the instant half of the window has its own bound, or it drops out"
        );
    }

    /// **`/due:after` skips the step**, because the side is already named and
    /// asking again would be asking a question that has an answer.
    #[test]
    fn a_named_qualifier_commits_on_the_date() {
        use koru_store_wire::search::AttrOp;
        for (qualifier, op) in [("before", AttrOp::Before), ("after", AttrOp::After)] {
            let mut surface = surface_with_filters();
            type_str(&mut surface, &format!("/due:{qualifier} "));
            assert!(
                surface.temporal.is_some(),
                "the calendar opened on {qualifier}"
            );
            aim_at(
                &mut surface,
                chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
            );

            assert!(press(&mut surface, NamedKey::Enter));
            assert_eq!(
                surface.query.raw_text(),
                format!("/due:{qualifier} 2026-09-10 ")
            );
            assert!(surface.temporal.is_none(), "no step was raised");
            assert_eq!(surface.search_query(0).attributes[0].op, op);
        }
    }

    /// **The half-open boundary, from the surface's side**: the same day
    /// answers two different questions, and the two ops are the two halves of a
    /// partition — `After` is `>=` and takes the edge, `Before` is `<` and does
    /// not. What the store then does with them is pinned in `koru-search`.
    #[test]
    fn the_two_sides_of_one_day_are_a_partition() {
        use koru_store_wire::search::AttrOp;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        let mut bounds = Vec::new();
        for row in [0usize, 1] {
            let mut surface = surface_with_filters();
            type_str(&mut surface, "/due ");
            aim_at(&mut surface, day);
            press(&mut surface, NamedKey::Enter);
            for _ in 0..row {
                press(&mut surface, NamedKey::ArrowDown);
            }
            press(&mut surface, NamedKey::Enter);
            let filter = surface.search_query(0).attributes.remove(0);
            bounds.push((filter.op, filter.values.clone(), filter.numeric_values));
        }
        assert_eq!(
            bounds[0].0,
            AttrOp::Before,
            "the first row excludes the edge"
        );
        assert_eq!(bounds[1].0, AttrOp::After, "the second includes it");
        assert_eq!(
            bounds[0].1, bounds[1].1,
            "one day, two questions — the bound is the same edge"
        );
        assert_eq!(bounds[0].2, bounds[1].2);
    }

    /// **Within 24 hours is a half-open `Between`** whose two bounds are a day
    /// apart in both columns.
    #[test]
    fn within_24_hours_is_a_half_open_pair() {
        use koru_store_wire::search::AttrOp;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due ");
        aim_at(&mut surface, day);
        press(&mut surface, NamedKey::Enter);
        press(&mut surface, NamedKey::ArrowDown);
        press(&mut surface, NamedKey::ArrowDown);
        press(&mut surface, NamedKey::Enter);
        assert_eq!(
            surface.query.raw_text(),
            "/due:between 2026-09-10..2026-09-11 "
        );

        let filter = surface.search_query(0).attributes.remove(0);
        assert_eq!(filter.op, AttrOp::Between);
        assert_eq!(filter.values, ["2026-09-10", "2026-09-11"]);
        let ms: Vec<i64> = filter
            .numeric_values
            .iter()
            .map(|value| value.parse().expect("epoch milliseconds"))
            .collect();
        assert_eq!(ms.len(), 2);
        assert_eq!(
            ms[1] - ms[0],
            24 * 60 * 60 * 1000,
            "twenty-four hours, and September carries no transition anywhere"
        );
    }

    /// **A date being spelled by hand keeps the keyboard.** The calendar takes
    /// every key while it is up, so it opens over an *empty* value and only
    /// there — otherwise typing `2026-09-10` after `/due:before ` would land in
    /// the picker's phrase box instead of the query, and clicking back into a
    /// filter to correct it would cover the thing being corrected.
    #[test]
    fn a_hand_spelled_date_is_not_interrupted_by_the_calendar() {
        use koru_store_wire::search::AttrOp;
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due:before ");
        assert!(surface.temporal.is_some(), "an empty value is the calendar");

        // Escape hands the box back, and what is typed then stays in it.
        assert!(press(&mut surface, NamedKey::Escape));
        type_str(&mut surface, "2026-09-10");
        assert_eq!(surface.query.raw_text(), "/due:before 2026-09-10");
        assert!(
            surface.temporal.is_none(),
            "and the calendar does not reopen over what is being typed"
        );
        assert!(
            surface.filter_is_hint(),
            "it says what it wants instead, the way a number's field does"
        );

        let query = surface.search_query(0);
        assert_eq!(query.attributes[0].op, AttrOp::Before);
        assert_eq!(query.attributes[0].values, ["2026-09-10"]);
        assert_eq!(
            query.attributes[0].numeric_values,
            [
                local_midnight_ms(chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap())
                    .to_string()
            ],
            "a hand-spelled bound gets its instant copy too — the box is the model"
        );
    }

    /// **Escape closes the calendar and writes nothing**, and so does a press
    /// outside it — the same dismissal the rest of this surface has.
    #[test]
    fn escape_closes_the_calendar_and_changes_no_text() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due ");
        assert!(press(&mut surface, NamedKey::Escape));
        assert_eq!(surface.query.raw_text(), "/due ");
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.temporal.is_none());

        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due ");
        surface.mouse_down(20.0, 380.0, false);
        assert_eq!(surface.query.raw_text(), "/due ");
        assert!(surface.temporal.is_none());
    }

    /// The panel's rect is settled when the calendar opens and **does not
    /// change when the step slides in** — that is what makes the slide
    /// content-only, and what stops the popover jumping under the pointer
    /// halfway through the gesture.
    #[test]
    fn the_step_does_not_resize_the_panel() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/due ");
        let before = surface.popover().expect("the calendar").rect;
        press(&mut surface, NamedKey::Enter);
        let after = surface.popover().expect("the step").rect;
        assert_eq!(before.x, after.x);
        assert_eq!(before.y, after.y);
        assert_eq!(before.w, after.w);
        assert_eq!(before.h, after.h);
    }

    /// **Escape closes and writes nothing**, and a press outside does the same.
    #[test]
    fn escape_and_a_press_outside_close_the_chooser_and_change_no_text() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/pri");
        let before = surface.query.raw_text().to_owned();
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
        assert_eq!(surface.menu, SearchMenu::None);
        assert_eq!(surface.query.raw_text(), before);

        let mut surface = surface_with_filters();
        type_str(&mut surface, "/pri");
        assert_eq!(surface.menu, SearchMenu::Filters);
        // Well below the popover, which hangs off the query box at the top.
        surface.mouse_down(20.0, 380.0, false);
        assert_eq!(surface.menu, SearchMenu::None);
        assert_eq!(surface.query.raw_text(), before);
    }

    /// **The results list must not move while the chooser owns the keyboard.**
    /// The same rule `menu_selected` keeps for the tag completion and the
    /// profile picker, applied to one more popover.
    #[test]
    fn arrows_move_the_chooser_and_not_the_results() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/");
        // After typing, not before: a keystroke resets the results selection to
        // the top, which would otherwise be mistaken here for the popover
        // moving it.
        surface.list.selected = 2;
        assert_eq!(surface.menu_selected, Some(0));

        for _ in 0..2 {
            assert!(surface.key(
                &Key::Named(NamedKey::ArrowDown),
                None,
                ModifiersState::empty(),
            ));
        }
        assert_eq!(surface.menu_selected, Some(2));
        assert_eq!(surface.list.selected, 2, "the hit under Enter did not move");
    }

    /// Moving the caret out of the filter closes the chooser even though no
    /// character changed.
    #[test]
    fn leaving_the_filter_closes_the_chooser() {
        let mut surface = surface_with_filters();
        type_str(&mut surface, "/priority high ");
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.filter_context.is_none());
    }

    /// **Nothing per keystroke touches the store.** The tables are asked for
    /// once, on the first sigil typed, and the narrowing is local from then on.
    #[test]
    fn the_tables_are_asked_for_once_and_narrowing_is_local() {
        let (reader, _responses, _requests, background) =
            reader::SearchReader::test_pair_with_background();
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);
        surface.reader = Some(reader);
        surface.definitions = filters::digest(&a_scope_of_definitions());

        type_str(&mut surface, "/priority");
        let asked = background
            .try_iter()
            .filter(|work| matches!(work, reader::BackgroundWork::ScopeTables(_)))
            .count();
        assert_eq!(asked, 1, "one request for nine keystrokes");
    }

    #[test]
    fn input_outline_radius_is_concentric_with_the_search_surface() {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);

        assert_eq!(SURFACE_RADIUS, 8.0);
        assert_eq!(INPUT_RADIUS, 4.0);
        let outline = surface.query_outline_rect();
        assert_eq!(outline, Rect::new(4.0, 4.0, 592.0, 36.0));

        let icon = surface.search_icon_rect();
        let input = surface.query_rect();
        let menu = surface.app_menu_rect();
        let menu_icon = surface.app_menu_icon_rect();
        assert_eq!(icon.x - outline.x, 12.0);
        assert_eq!(input.x + koru_ui::text_field::PAD, icon.x + icon.w + 12.0);
        assert_eq!(outline.x + outline.w - (menu_icon.x + menu_icon.w), 12.0);
        assert_eq!(input.y, outline.y);
        assert_eq!(input.h, outline.h);
        assert_eq!(icon.y + icon.h * 0.5, outline.y + outline.h * 0.5);
        assert_eq!(menu.y + menu.h * 0.5, outline.y + outline.h * 0.5);
        assert_eq!(menu_icon.y + menu_icon.h * 0.5, outline.y + outline.h * 0.5);
        assert_eq!(
            (outline.h - theme::BORDER_W as f32 * 2.0 - theme::role::row().metrics.line_height)
                * 0.5,
            8.0
        );
    }

    #[test]
    fn active_filters_push_the_recents_heading_below_the_filter_row() {
        let mut surface = SearchSurface::new();
        surface.pending_read = None;
        surface.pending_replace = None;
        let without_filters = surface.landing_heading_y();

        surface.filters_visible = true;
        let with_filters = surface.landing_heading_y();

        assert_eq!(with_filters - without_filters, surface.filter_line_h());
        assert!(with_filters > surface.workspaces_chip_rect().y + surface.workspaces_chip_rect().h);
    }

    #[test]
    fn global_profile_control_is_anchored_in_the_footer() {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);

        let profile = surface.profiles_chip_rect();
        assert_eq!(profile.x, PAD);
        assert!(profile.y >= surface.viewport.1 - FOOTER_H);
        assert!(profile.y + profile.h <= surface.viewport.1);
        assert_eq!(surface.filter_line_h(), 0.0);
    }

    #[test]
    fn footer_profile_control_opens_the_shared_picker_above_results() {
        let profile = ProfileId::new();
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);
        surface.catalog = Catalog::for_test(Vec::new(), vec![(profile, "Steven".into())]);
        surface.selected_profile = Some(profile);

        let control = surface.profiles_chip_rect();
        assert!(surface.mouse_down(
            control.x + control.w * 0.5,
            control.y + control.h * 0.5,
            false,
        ));
        assert_eq!(surface.menu, SearchMenu::Profiles);

        let picker = surface.popover().expect("profile picker");
        assert!(picker.rect.y + picker.rect.h <= control.y);
        assert_eq!(
            surface.popover_row_at(
                picker.rect.x + 10.0,
                picker.rect.y + profile_picker::PAD + profile_picker::HEADING_H * 0.5,
            ),
            None,
        );
        let first = profile_picker::row_rect(picker.rect, 0, 0.0);
        assert_eq!(
            surface.popover_row_at(first.x + 10.0, first.y + first.h * 0.5),
            Some(0),
        );
    }

    #[test]
    fn ctrl_alt_p_opens_the_footer_profile_picker() {
        let mut surface = SearchSurface::new();
        let mods = ModifiersState::CONTROL | ModifiersState::ALT;

        assert!(surface.key(&Key::Character("p".into()), Some("p"), mods));
        assert_eq!(surface.menu, SearchMenu::Profiles);
    }

    #[test]
    fn profile_picker_arrows_move_its_rows_instead_of_search_results() {
        let alpha = ProfileId::new();
        let beta = ProfileId::new();
        let gamma = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.catalog = Catalog::for_test(
            vec![(workspace, "Workspace".into(), alpha)],
            vec![
                (alpha, "Alpha".into()),
                (beta, "Beta".into()),
                (gamma, "Gamma".into()),
            ],
        );
        surface.selected_profile = Some(alpha);
        surface.page.hits = vec![hit(alpha, workspace, &a_node_id()); 3];
        surface.list.selected = 1;
        surface.open_menu(SearchMenu::Profiles);

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.menu_selected, Some(1));
        assert_eq!(
            surface.list.selected, 1,
            "the result selection did not move"
        );

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.menu_selected, Some(0));
        assert!(surface.key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.menu_selected, Some(2), "Up wraps to the last row");

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert_eq!(surface.selected_profile, Some(gamma));
        assert_eq!(surface.menu, SearchMenu::None);
    }

    #[test]
    fn a_long_profile_picker_is_capped_at_300() {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 600.0);
        surface.catalog = Catalog::for_test(
            Vec::new(),
            (0..20)
                .map(|index| (ProfileId::new(), format!("Profile {index}")))
                .collect(),
        );
        surface.open_menu(SearchMenu::Profiles);

        assert_eq!(
            surface.popover().expect("profile picker").rect.h,
            profile_picker::MAX_H
        );
    }

    #[test]
    fn ctrl_p_is_not_a_profile_picker_alias() {
        let mut surface = SearchSurface::new();
        assert!(surface.key(
            &Key::Character("p".into()),
            Some("p"),
            ModifiersState::CONTROL,
        ));
        assert_eq!(surface.menu, SearchMenu::None);
    }

    #[test]
    fn profile_picker_blocks_pointer_state_and_clicks_from_results_beneath_it() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);
        set_query(&mut surface, "result");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &a_node_id()); 10];
        surface.menu = SearchMenu::Profiles;

        let picker = surface.popover().expect("profile picker");
        let x = picker.rect.x + picker.rect.w * 0.5;
        let y = picker.rect.y + picker.rect.h * 0.5;
        assert!(surface.results_rect().contains(x, y));
        surface.hover = Some(0);

        assert!(surface.mouse_move(x, y));
        assert_eq!(
            surface.hover, None,
            "a result still reacted under the picker"
        );

        // Dismissing the picker over a result consumes that same press.
        let dismiss_x = surface.viewport.0 - 4.0;
        let dismiss_y = surface.results_top() + 4.0;
        assert!(!picker.rect.contains(dismiss_x, dismiss_y));
        assert!(surface.mouse_down(dismiss_x, dismiss_y, false));
        assert_eq!(surface.menu, SearchMenu::None);
        assert!(surface.take_target().is_none());
    }

    #[test]
    fn empty_opened_state_and_recent_searches_have_separate_lines() {
        let mut surface = SearchSurface::new();
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits.clear();

        let empty_message_y = surface.results_top() + LANDING_HEADING_H + 10.0;
        let searches_heading_y = surface.recent_searches_heading_y();
        let first_query = surface.recent_query_rect(0);

        assert!(searches_heading_y >= empty_message_y + 34.0);
        assert!(first_query.y >= searches_heading_y + 24.0);
    }

    #[test]
    fn arrows_navigate_recent_queries_when_no_opened_target_exists() {
        let mut surface = SearchSurface::new();
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits.clear();
        surface.recent_queries = vec![
            RecentQuery {
                text: "what".into(),
                used_at_ms: 2,
            },
            RecentQuery {
                text: "ideas".into(),
                used_at_ms: 1,
            },
        ];

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_recent_query, Some(0));

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_recent_query, Some(1));

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_recent_query, Some(0));
    }

    #[test]
    fn enter_runs_the_selected_recent_query() {
        let mut surface = SearchSurface::new();
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits.clear();
        surface.recent_queries = vec![RecentQuery {
            text: "shortcuts".into(),
            used_at_ms: 1,
        }];

        surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        );
        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));

        assert_eq!(surface.query.value(), "shortcuts");
        assert_eq!(surface.selected_recent_query, None);
        assert!(surface.take_target().is_none());
    }

    #[test]
    fn landing_arrows_cross_from_opened_targets_into_recent_queries() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits = vec![hit(profile, workspace, &a_node_id())];
        surface.recent_queries = vec![RecentQuery {
            text: "shortcuts".into(),
            used_at_ms: 1,
        }];

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_recent_query, Some(0));

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_recent_query, None);
        assert_eq!(surface.list.selected, 0);
    }

    #[test]
    fn landing_scroll_uses_three_opened_rows_and_recent_queries_as_one_extent() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 260.0);
        surface.page.hits = (0..8)
            .map(|_| hit(profile, workspace, &a_node_id()))
            .collect();
        surface.recent_queries = (0..RECENT_QUERY_LIMIT)
            .map(|index| RecentQuery {
                text: format!("query {index}"),
                used_at_ms: index as i64,
            })
            .collect();

        assert_eq!(surface.visible_hit_count(), LANDING_OPENED_LIMIT);
        let expected = LANDING_HEADING_H
            + LANDING_OPENED_LIMIT as f32 * surface.list.row_height
            + 10.0
            + RECENT_HEADING_TO_ROWS
            + RECENT_QUERY_LIMIT as f32 * RECENT_QUERY_STRIDE;
        assert_eq!(surface.landing_content_height(), expected);

        let before = surface.recent_searches_heading_y();
        surface.scroll_by(MouseScrollDelta::LineDelta(0.0, -1.0));
        assert!(surface.list.scroll > 0.0);
        assert_eq!(
            before - surface.recent_searches_heading_y(),
            surface.list.scroll
        );
        assert_eq!(
            surface.scrollbar().unwrap().max_scroll,
            expected - surface.results_height()
        );
    }

    #[test]
    fn landing_pins_occupy_the_reserved_band_above_recents() {
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.set_pinned(&[
            ("Bugs".into(), DocumentId::new(), workspace, true),
            ("Ideas".into(), DocumentId::new(), workspace, false),
        ]);
        assert_eq!(
            surface.pin_block_h(),
            LANDING_HEADING_H + PIN_CHIP_H + LANDING_SECTION_GAP
        );
        let first = surface.pin_chip_rect(0);
        let second = surface.pin_chip_rect(1);
        assert_eq!(first.y, surface.pin_row_y());
        assert_eq!(second.y, first.y);
        assert!(second.x > first.x);
        assert!(first.w > theme::TAB_PINNED_W);
        assert_eq!(second.w, theme::TAB_PINNED_W);
        assert!(surface.landing_heading_y() >= first.y + PIN_CHIP_H);
    }

    #[test]
    fn pin_parts_treat_a_leading_emoji_as_the_icon() {
        assert_eq!(pin_parts("🐛 Bugs"), (Some("🐛"), "Bugs"));
        assert_eq!(pin_parts("Bugs"), (None, "Bugs"));
        assert_eq!(pin_parts("🔥"), (Some("🔥"), ""));
    }

    #[test]
    fn landing_left_right_moves_among_pins_and_down_enters_recents() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        with_catalog(&mut surface, profile, workspace);
        surface.set_pinned(&[
            ("Bugs".into(), DocumentId::new(), workspace, false),
            ("Ideas".into(), DocumentId::new(), workspace, false),
        ]);
        surface.page.hits = vec![
            hit(profile, workspace, &a_node_id()),
            hit(profile, workspace, &a_node_id()),
        ];
        surface.pending_read = None;
        surface.pending_replace = None;

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_pin, Some(0));

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_pin, Some(1));

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_pin, None);
        assert_eq!(surface.list.selected, 0);

        assert!(surface.key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(surface.selected_pin, Some(1));
    }

    #[test]
    fn landing_keyboard_navigation_scrolls_the_last_recent_query_into_view() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 260.0);
        surface.page.hits = (0..LANDING_OPENED_LIMIT)
            .map(|_| hit(profile, workspace, &a_node_id()))
            .collect();
        surface.recent_queries = (0..RECENT_QUERY_LIMIT)
            .map(|index| RecentQuery {
                text: format!("query {index}"),
                used_at_ms: index as i64,
            })
            .collect();

        for _ in 1..(LANDING_OPENED_LIMIT + RECENT_QUERY_LIMIT) {
            assert!(surface.move_landing_selection(1));
        }

        assert_eq!(surface.selected_recent_query, Some(RECENT_QUERY_LIMIT - 1));
        let selected = surface.recent_query_rect(RECENT_QUERY_LIMIT - 1);
        let viewport = surface.results_rect();
        assert!(selected.y >= viewport.y);
        assert!(selected.y + selected.h <= viewport.y + viewport.h);
    }

    #[test]
    fn workspace_surface_scopes_queries_to_that_one_workspace() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let surface = SearchSurface::for_workspace(profile, workspace, "Roadmap".into());

        // **Scope is a set of workspace ids now**, not a profile and a
        // workspace: that is the shape the store takes, and a profile is
        // resolved to the workspaces it owns before the query is sent.
        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![workspace]);
        assert_eq!(surface.workspace_scope(), Some((profile, workspace)));
    }

    #[test]
    fn standalone_surface_without_a_catalog_has_no_profile_scope() {
        let surface = SearchSurface::new();
        let query = surface.search_query(0);

        // Production never sends this empty scope: `refresh` waits for the
        // catalog and a selected profile because empty means every partition.
        assert!(query.workspaces.is_empty());
        assert!(query.excluded_workspaces.is_empty());
        assert_eq!(surface.selected_profile, None);
        assert_eq!(surface.workspace_scope(), None);
    }

    #[test]
    fn global_search_excludes_archived_and_softly_deleted_workspaces() {
        let owner = ProfileId::new();
        let active = WorkspaceId::new();
        let archived = WorkspaceId::new();
        let deleted = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.catalog = Catalog::for_test_with_lifecycle(
            vec![
                (active, "Active".into(), owner, false, false),
                (archived, "Archived".into(), owner, true, false),
                (deleted, "Deleted".into(), owner, false, true),
            ],
            vec![],
        );
        surface.selected_profile = Some(owner);

        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![active]);
        assert!(query.excluded_workspaces.is_empty());
    }

    #[test]
    fn workspace_filter_rows_exclude_archived_deleted_and_unknown_facets() {
        let owner = ProfileId::new();
        let active = WorkspaceId::new();
        let archived = WorkspaceId::new();
        let deleted = WorkspaceId::new();
        let unknown = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.catalog = Catalog::for_test_with_lifecycle(
            vec![
                (active, "Active".into(), owner, false, false),
                (archived, "Archived".into(), owner, true, false),
                (deleted, "Deleted".into(), owner, false, true),
            ],
            vec![],
        );
        surface.catalog_ready = true;
        surface.selected_profile = Some(owner);
        surface.facets = SearchFacets {
            workspaces: vec![
                SearchFacet {
                    workspace: archived.to_storage(),
                    workspace_name: "Archived".into(),
                },
                SearchFacet {
                    workspace: active.to_storage(),
                    workspace_name: "Active".into(),
                },
                SearchFacet {
                    workspace: deleted.to_storage(),
                    workspace_name: "Deleted".into(),
                },
                SearchFacet {
                    workspace: unknown.to_storage(),
                    workspace_name: "Unknown".into(),
                },
            ],
        };

        surface.menu = SearchMenu::Workspaces;
        // The source toggle is independent of the workspace facets below it.
        assert_eq!(surface.menu_row_count(), 2);
        assert_eq!(
            surface
                .workspace_facets()
                .map(|facet| facet.workspace_name.as_str())
                .collect::<Vec<_>>(),
            ["Active"]
        );
        surface.activate_popover_row(1);
        assert_eq!(surface.filters.excluded_workspaces(), [active.to_storage()]);
    }

    #[test]
    fn archived_fallback_searches_only_live_archived_workspaces() {
        let owner = ProfileId::new();
        let other_owner = ProfileId::new();
        let active = WorkspaceId::new();
        let archived = WorkspaceId::new();
        let other_archived = WorkspaceId::new();
        let deleted_archive = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "missing");
        surface.page.hits.clear();
        surface.catalog = Catalog::for_test_with_lifecycle(
            vec![
                (active, "Active".into(), owner, false, false),
                (archived, "Archived".into(), owner, true, false),
                (
                    other_archived,
                    "Other archive".into(),
                    other_owner,
                    true,
                    false,
                ),
                (deleted_archive, "Deleted archive".into(), owner, true, true),
            ],
            vec![],
        );
        surface.selected_profile = Some(owner);
        let rect = surface
            .archived_workspaces_rect()
            .expect("archived fallback");

        assert!(surface.mouse_down(rect.x + 2.0, rect.y + 2.0, false));
        assert_eq!(surface.query.raw_text(), "missing");
        assert!(surface.archived_only);
        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![archived]);
        assert!(!query.workspaces.contains(&active));
        assert!(!query.workspaces.contains(&other_archived));
        assert!(!query.workspaces.contains(&deleted_archive));
    }

    #[test]
    fn active_result_card_is_inset_from_the_full_width_row() {
        let mut surface = SearchSurface::new();
        surface.viewport = (600.0, 400.0);

        let card = surface.result_card_rect(100.0);
        assert_eq!(
            card,
            Rect::new(
                8.0,
                103.0,
                600.0 - 8.0 - theme::SCROLLBAR_W - theme::SCROLLBAR_EDGE_CLEAR - CARD_INSET_X,
                64.0,
            )
        );
        let scrollbar_left = 600.0 - theme::SCROLLBAR_EDGE_CLEAR - theme::SCROLLBAR_W;
        assert_eq!(scrollbar_left - (card.x + card.w), CARD_INSET_X);
    }

    #[test]
    fn escape_closes_a_menu_before_clearing_the_query() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "launch");
        surface.menu = SearchMenu::Sort;
        assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty(),));
        assert_eq!(surface.menu, SearchMenu::None);
        assert_eq!(surface.query.raw_text(), "launch");
    }

    #[test]
    fn one_click_requests_the_selected_result() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        let mut result = hit(profile, workspace, &node);
        result.spans = vec![koru_store_wire::search::MatchSpan { start: 0, end: 9 }];
        surface.page.hits = vec![result];

        assert!(surface.mouse_down(PAD, HEADER_H + 2.0, false));
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                profile: requested_profile,
                workspace: requested_workspace,
                document: Some(_),
                node: Some(requested_node),
                trashed: false,
                match_text: Some(requested_match),
                disposition: Disposition::CurrentWindow,
            }) if requested_profile == profile
                && requested_workspace == workspace
                && requested_node == node
                && requested_match == "Shortcuts"
        ));
    }

    #[test]
    fn control_click_requests_a_new_window() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &node)];

        assert!(surface.mouse_down(PAD, HEADER_H + 2.0, true));
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                disposition: Disposition::NewWindow,
                ..
            })
        ));
    }

    #[test]
    fn enter_preserves_the_query_and_requests_the_current_window() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &node)];

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert_eq!(surface.query.raw_text(), "shortcuts");
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                disposition: Disposition::CurrentWindow,
                ..
            })
        ));
    }

    #[test]
    fn control_enter_preserves_the_query_and_requests_a_new_window() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &node)];

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::CONTROL,));
        assert_eq!(surface.query.raw_text(), "shortcuts");
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                disposition: Disposition::NewWindow,
                ..
            })
        ));
    }

    #[test]
    fn shift_enter_opens_in_the_current_window() {
        // New-window is Ctrl/Cmd only. Shift+Enter used to mean the same as
        // Ctrl+Enter and made ordinary Enter-with-shift look like a pop-out.
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &node)];

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT,));
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                disposition: Disposition::CurrentWindow,
                ..
            })
        ));
    }

    /// **A surface nobody has touched asks for the most recently modified
    /// first**, and asks the *store* for it — the ordering is an `ORDER BY`,
    /// not a shuffle of one page, so it holds across the whole result set and
    /// not merely the fifty rows already fetched.
    #[test]
    fn a_fresh_surface_sorts_by_last_modified() {
        let surface = SearchSurface::new();
        assert_eq!(surface.sort, SearchSort::ObservedModified);
        assert_eq!(surface.search_query(0).sort, SearchSort::ObservedModified);
    }

    /// The chip names the ordering even at the default, and does not resize
    /// the query field beside it when the ordering changes.
    #[test]
    fn the_sort_chip_names_every_sort_at_one_width() {
        let mut surface = SearchSurface::new();
        let width = surface.sort_rect().w;
        let query_right = surface.query_rect().x + surface.query_rect().w;
        for (sort, _, chip) in SORTS {
            surface.sort = sort;
            assert!(!chip.is_empty());
            assert_eq!(sort_label(sort), chip);
            assert_eq!(surface.sort_rect().w, width);
            assert_eq!(surface.query_rect().x + surface.query_rect().w, query_right);
        }
    }

    /// Clicking row *n* chooses the sort row *n* shows. They used to be two
    /// hand-written lists in the same file.
    #[test]
    fn every_sort_row_chooses_the_sort_it_shows() {
        // A fourth variant stops this compiling, which is what stops a sort
        // that has no row and no label from being added silently.
        for sort in [
            SearchSort::Relevance,
            SearchSort::ObservedModified,
            SearchSort::LastUsedWorkspace,
        ] {
            match sort {
                SearchSort::Relevance
                | SearchSort::ObservedModified
                | SearchSort::LastUsedWorkspace => {}
            }
            assert!(SORTS.iter().any(|(listed, ..)| *listed == sort));
        }

        let mut surface = SearchSurface::new();
        for (row, (sort, label, _)) in SORTS.into_iter().enumerate() {
            surface.open_menu(SearchMenu::Sort);
            assert_eq!(surface.menu_row_count(), SORTS.len());
            surface.activate_popover_row(row);
            assert_eq!(surface.sort, sort, "row {row} is labelled {label}");
        }
    }

    /// The selected profile reaches the Store as exactly its active workspaces.
    #[test]
    fn a_selected_profile_and_workspace_filter_both_reach_the_query() {
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();

        // A profile owning one workspace, as the catalog would have it.
        let owned = WorkspaceId::new();
        let owner = ProfileId::new();
        surface.catalog = Catalog::for_test(vec![(owned, "Owned".into(), owner)], vec![]);
        surface.select_profile(owner);

        surface
            .filters
            .toggle_workspace(&koru_store_wire::search::SearchFacet {
                workspace: workspace.to_storage(),
                workspace_name: "Roadmap".into(),
            });
        surface.sort = SearchSort::LastUsedWorkspace;

        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![owned]);
        assert!(query.excluded_workspaces.contains(&workspace));
        assert_eq!(query.sort, SearchSort::LastUsedWorkspace);
    }

    #[test]
    fn choosing_a_profile_replaces_results_and_queries_only_its_workspaces() {
        let alpha = ProfileId::new();
        let beta = ProfileId::new();
        let alpha_workspace = WorkspaceId::new();
        let beta_workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.catalog = Catalog::for_test(
            vec![
                (alpha_workspace, "Alpha work".into(), alpha),
                (beta_workspace, "Beta work".into(), beta),
            ],
            vec![(alpha, "Alpha".into()), (beta, "Beta".into())],
        );
        surface.selected_profile = Some(alpha);
        surface.page.hits = vec![hit(alpha, alpha_workspace, &a_node_id())];
        surface.menu = SearchMenu::Profiles;

        surface.activate_popover_row(1);

        assert_eq!(surface.selected_profile, Some(beta));
        assert!(surface.page.hits.is_empty());
        assert_eq!(surface.search_query(0).workspaces, vec![beta_workspace]);
    }

    #[test]
    fn a_result_outside_the_selected_profile_cannot_be_opened() {
        let selected = ProfileId::new();
        let owner = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "private");
        surface.catalog = Catalog::for_test(
            vec![(workspace, "Private".into(), owner)],
            vec![(selected, "Selected".into()), (owner, "Owner".into())],
        );
        surface.selected_profile = Some(selected);
        surface.page.hits = vec![hit(owner, workspace, &a_node_id())];

        assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty(),));
        assert!(surface.take_target().is_none());
    }

    #[test]
    fn reopening_restores_the_same_result_by_stable_identity() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let first = hit(profile, workspace, &a_node_id());
        let selected = hit(profile, workspace, &a_node_id());
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![selected.clone(), first];
        surface.selection_restore = Some(state::SearchSessionSelection {
            context: surface.selection_context(),
            hit_key: hit_key(&selected),
            // The item used to rank second; identity wins after it moves.
            index: 1,
        });

        assert!(!surface.apply_selection_restore());
        assert_eq!(surface.list.selected, 0);
        assert!(surface.selection_restore.is_none());
    }

    #[test]
    fn reopening_falls_back_to_the_saved_position_if_the_result_disappeared() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = (0..6)
            .map(|_| hit(profile, workspace, &a_node_id()))
            .collect();
        surface.selection_restore = Some(state::SearchSessionSelection {
            context: surface.selection_context(),
            hit_key: "result-that-no-longer-exists".into(),
            index: 4,
        });

        assert!(!surface.apply_selection_restore());
        assert_eq!(surface.list.selected, 4);
    }

    #[test]
    fn reopening_loads_later_pages_before_restoring_a_deep_selection() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = (0..50)
            .map(|_| hit(profile, workspace, &a_node_id()))
            .collect();
        surface.page.next_offset = Some(50);
        let selection = state::SearchSessionSelection {
            context: surface.selection_context(),
            hit_key: "result-on-a-later-page".into(),
            index: 73,
        };
        surface.selection_restore = Some(selection.clone());

        assert!(surface.apply_selection_restore());
        assert_eq!(surface.selection_restore, Some(selection));
        assert_eq!(surface.list.selected, 0);
    }

    #[test]
    fn workspace_landing_never_surfaces_global_query_history() {
        let surface =
            SearchSurface::for_workspace(ProfileId::new(), WorkspaceId::new(), "Roadmap".into());
        assert!(!surface.shows_recent_queries());
        assert!(SearchSurface::new().shows_recent_queries());
    }

    #[test]
    fn no_result_action_switches_to_deleted_pages() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "missing");
        surface.page.hits.clear();
        let rect = surface.deleted_pages_rect();

        assert!(surface.mouse_down(rect.x + 2.0, rect.y + 2.0, false));
        assert_eq!(surface.lifecycle, LifecycleFilter::Trash);
    }

    #[test]
    fn workspace_no_result_action_expands_to_its_profile_without_clearing_query() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let other_profile = ProfileId::new();
        let other_workspace = WorkspaceId::new();
        let mut surface = SearchSurface::for_workspace(profile, workspace, "Roadmap".into());
        surface.catalog = Catalog::for_test(
            vec![
                (workspace, "Roadmap".into(), profile),
                (other_workspace, "Private".into(), other_profile),
            ],
            vec![(profile, "Work".into()), (other_profile, "Personal".into())],
        );
        set_query(&mut surface, "emoji");
        surface.page.hits.clear();
        let rect = surface.profile_workspaces_rect().expect("workspace action");

        assert!(surface.mouse_down(rect.x + 2.0, rect.y + 2.0, false));
        assert_eq!(surface.query.raw_text(), "emoji");
        assert_eq!(surface.workspace_scope(), None);
        assert_eq!(surface.selected_profile, Some(profile));
        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![workspace]);
        assert!(!query.workspaces.contains(&other_workspace));
    }

    #[test]
    fn ctrl_space_expands_workspace_search_to_its_profile_without_clearing_query() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let sibling = WorkspaceId::new();
        let other_profile = ProfileId::new();
        let other_workspace = WorkspaceId::new();
        let mut surface = SearchSurface::for_workspace(profile, workspace, "Roadmap".into());
        surface.catalog = Catalog::for_test(
            vec![
                (workspace, "Roadmap".into(), profile),
                (sibling, "Notes".into(), profile),
                (other_workspace, "Private".into(), other_profile),
            ],
            vec![(profile, "Work".into()), (other_profile, "Personal".into())],
        );
        set_query(&mut surface, "emoji");

        assert!(surface.key(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::CONTROL,
        ));
        assert_eq!(surface.query.raw_text(), "emoji");
        assert_eq!(surface.workspace_scope(), None);
        let query = surface.search_query(0);
        assert_eq!(query.workspaces, vec![workspace, sibling]);
        assert!(!query.workspaces.contains(&other_workspace));

        assert!(surface.key(
            &Key::Named(NamedKey::Space),
            Some(" "),
            ModifiersState::CONTROL,
        ));
        assert_eq!(surface.workspace_scope(), None);
    }

    #[test]
    fn tab_no_longer_opens_a_preview_panel_or_edits_the_query() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "launch");

        assert!(surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(),));
        assert_eq!(surface.view, SearchView::Results);
        assert_eq!(surface.menu, SearchMenu::None);
        assert_eq!(surface.query.raw_text(), "launch");
    }

    #[test]
    fn shift_left_and_backspace_replace_the_selected_query_text() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "launch");

        surface.key(
            &Key::Named(NamedKey::ArrowLeft),
            None,
            ModifiersState::SHIFT,
        );
        surface.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(surface.query.raw_text(), "launc");
    }

    #[test]
    fn control_backspace_deletes_a_word_from_the_search_input() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "hello someone");

        surface.key(
            &Key::Named(NamedKey::Backspace),
            None,
            ModifiersState::CONTROL,
        );

        assert_eq!(surface.query.value(), "hello");
    }

    #[test]
    fn control_character_backspace_is_recovered_by_the_search_surface() {
        for raw in ["\u{8}", "\u{7f}"] {
            let mut surface = SearchSurface::new();
            set_query(&mut surface, "hello someone");

            surface.key(
                &Key::Character(raw.into()),
                Some(raw),
                ModifiersState::CONTROL,
            );

            assert_eq!(surface.query.value(), "hello", "raw key {raw:?}");
            assert_eq!(surface.view, SearchView::Results, "raw key {raw:?}");
        }
    }

    #[test]
    fn shell_normalized_backspace_does_not_decode_its_retained_text_as_ctrl_h() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "hello world");

        // This is the exact Windows shape observed with KORU_INPUT_DEBUG: the
        // shell has repaired the logical key, while winit retains U+0008 in
        // `KeyEvent::text`.
        surface.key(
            &Key::Named(NamedKey::Backspace),
            Some("\u{8}"),
            ModifiersState::CONTROL,
        );

        assert_eq!(surface.query.value(), "hello");
        assert_eq!(surface.view, SearchView::Results);
    }

    #[test]
    fn editing_the_visible_query_from_help_returns_to_results() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "hello someone");
        surface.view = SearchView::HelpPreview;

        surface.key(
            &Key::Character("\u{8}".into()),
            Some("\u{8}"),
            ModifiersState::CONTROL,
        );

        assert_eq!(surface.query.value(), "hello");
        assert_eq!(surface.view, SearchView::Results);
    }

    #[test]
    fn app_menu_opens_above_the_help_preview() {
        let mut surface = SearchSurface::new();
        surface.view = SearchView::HelpPreview;
        let menu = surface.app_menu_rect();

        surface.mouse_down(menu.x + menu.w * 0.5, menu.y + menu.h * 0.5, false);

        assert_eq!(surface.view, SearchView::HelpPreview);
        assert_eq!(surface.menu, SearchMenu::App);
        assert!(surface.popover().is_some());
    }

    #[test]
    fn app_menu_shortcut_works_while_help_is_visible() {
        let mut surface = SearchSurface::new();
        surface.view = SearchView::HelpPreview;

        surface.key(
            &Key::Character(",".into()),
            Some(","),
            ModifiersState::CONTROL,
        );

        assert_eq!(surface.view, SearchView::HelpPreview);
        assert_eq!(surface.menu, SearchMenu::App);
    }

    /// The `…` menu is the shared menu widget: arrows step the highlight and
    /// Enter chooses, the way every menu in the container works.
    #[test]
    fn the_app_menu_steps_and_chooses_by_keyboard() {
        let mut surface = SearchSurface::new();
        surface.key(
            &Key::Character(",".into()),
            Some(","),
            ModifiersState::CONTROL,
        );
        assert_eq!(surface.menu, SearchMenu::App);
        let first = surface.app_menu.as_ref().and_then(|menu| menu.hover);
        assert_eq!(first, Some(0), "the first row arrives highlighted");

        surface.key(
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        );
        let second = surface.app_menu.as_ref().and_then(|menu| menu.hover);
        assert_eq!(second, Some(1));

        surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
        assert_eq!(surface.view, SearchView::HelpPreview, "row 1 chosen");
        assert_eq!(surface.menu, SearchMenu::None, "choosing closes the menu");
    }

    /// The toggle row is the exception: choosing it flips the state and the
    /// menu stays open, so the check is seen to move.
    #[test]
    fn the_keyboard_hints_row_toggles_without_closing_the_menu() {
        let mut surface = SearchSurface::new();
        surface.key(
            &Key::Character(",".into()),
            Some(","),
            ModifiersState::CONTROL,
        );

        surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());

        assert!(surface.show_shortcuts, "the toggle flipped");
        assert_eq!(surface.menu, SearchMenu::App, "the menu stayed open");
        let checked = surface
            .app_menu
            .as_ref()
            .and_then(|menu| menu.items().first())
            .map(|row| row.checked);
        assert_eq!(checked, Some(true), "the row shows the new state");
    }

    #[test]
    fn pressing_filter_clears_its_tooltip_before_layout_changes() {
        let mut surface = SearchSurface::new();
        let filter = surface.filter_rect();
        let now = Instant::now();
        surface
            .tooltip
            .set_focus(surface.tooltip_at(filter.x, filter.y), now);
        assert!(surface.tooltip.is_present());

        surface.mouse_down(filter.x + filter.w * 0.5, filter.y + filter.h * 0.5, false);

        assert!(!surface.tooltip.is_present());
        assert!(surface.filters_visible);
    }

    #[test]
    fn wheel_scrolls_only_the_results_between_fixed_header_and_footer() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.viewport = (600.0, 400.0);
        surface.page.hits = vec![hit(profile, workspace, &node); 10];

        assert_eq!(surface.results_rect(), Rect::new(0.0, 44.0, 600.0, 322.0));
        surface.scroll_by(MouseScrollDelta::LineDelta(0.0, -1.0));
        assert!(surface.list.scroll > 0.0);
        assert_eq!(surface.results_top(), HEADER_H);
        assert_eq!(surface.viewport.1 - FOOTER_H, 366.0);
    }

    #[test]
    fn footer_click_cannot_hit_a_clipped_result_row() {
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let node = a_node_id();
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "shortcuts");
        with_catalog(&mut surface, profile, workspace);
        surface.viewport = (600.0, 400.0);
        surface.page.hits = vec![hit(profile, workspace, &node); 10];

        assert!(!surface.mouse_down(350.0, 390.0, false));
        assert!(surface.take_target().is_none());
    }

    #[test]
    fn a_replacement_request_keeps_the_last_complete_page_mounted() {
        let (reader, _response_tx, request_rx) = SearchReader::test_pair();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let retained = hit(profile, workspace, &a_node_id());
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.page.hits = vec![retained.clone()];
        surface.query.set_text(&mut surface.engine, "new query");
        // Global Search cannot issue a query until Store's lifecycle catalog
        // has established profile access and workspace exclusions. This case
        // is about page replacement, so give it that prerequisite before
        // blocking on the request it is meant to assert.
        with_catalog(&mut surface, profile, workspace);

        surface.refresh();

        request_rx.recv().unwrap();
        assert_eq!(surface.page.hits, vec![retained]);
        assert!(surface.replacing_results());
    }

    #[test]
    fn global_search_waits_for_lifecycle_catalog_before_querying() {
        let (reader, response_tx, request_rx) = SearchReader::test_pair();
        let owner = ProfileId::new();
        let archived = WorkspaceId::new();
        let active = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        surface.catalog_ready = false;
        surface.pending_catalog = true;
        surface.pending_read = None;
        surface.pending_replace = None;
        surface.query.set_text(&mut surface.engine, "roadmap");

        surface.refresh();

        assert!(request_rx.try_recv().is_err());
        assert!(surface.replacing_results());
        assert!(surface.maintenance_deadline().is_some());

        response_tx
            .send(SearchReadResponse {
                seq: 0,
                result: SearchReadResult::Catalog(Catalog::for_test_with_lifecycle(
                    vec![
                        (archived, "Archive".into(), owner, true, false),
                        (active, "Active".into(), owner, false, false),
                    ],
                    vec![],
                )),
            })
            .unwrap();
        assert!(surface.apply_ready_reads());
        assert!(!surface.pending_catalog);

        let request = request_rx.recv().unwrap();
        let SearchReadKind::FederatedPage { query, .. } = request.into_kind() else {
            panic!("the shared surface must use federated search")
        };
        assert_eq!(query.workspaces, vec![active]);
        assert!(!query.workspaces.contains(&archived));
    }

    #[test]
    fn an_old_in_flight_page_cannot_replace_the_newest_query() {
        let (reader, response_tx, _request_rx) = SearchReader::test_pair();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        let retained = hit(ProfileId::new(), WorkspaceId::new(), &a_node_id());
        surface.page.hits = vec![retained.clone()];
        surface.pending_read = Some(2);
        surface.pending_replace = Some(2);

        response_tx
            .send(SearchReadResponse {
                seq: 1,
                result: SearchReadResult::Page {
                    append: false,
                    result: Ok(SearchResponse {
                        page: SearchPage {
                            hits: vec![hit(ProfileId::new(), WorkspaceId::new(), &a_node_id())],
                            next_offset: None,
                            updating_workspaces: Vec::new(),
                            corrected_query: None,
                            request_seq: 1,
                        },
                        facets: None,
                        recent_queries: None,
                    }),
                },
            })
            .unwrap();

        assert!(!surface.apply_ready_reads());
        assert_eq!(surface.page.hits, vec![retained]);
        assert_eq!(surface.pending_read, Some(2));
        assert_eq!(surface.pending_replace, Some(2));
    }

    #[test]
    fn retained_rows_are_inert_until_the_matching_page_arrives() {
        let (reader, response_tx, _request_rx) = SearchReader::test_pair();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let old_node = a_node_id();
        let new_node = a_node_id();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        with_catalog(&mut surface, profile, workspace);
        surface.page.hits = vec![hit(profile, workspace, &old_node)];
        surface.pending_read = Some(2);
        surface.pending_replace = Some(2);

        surface.request_open(Disposition::CurrentWindow);
        assert!(surface.take_target().is_none());

        response_tx
            .send(SearchReadResponse {
                seq: 2,
                result: SearchReadResult::Page {
                    append: false,
                    result: Ok(SearchResponse {
                        page: SearchPage {
                            hits: vec![hit(profile, workspace, &new_node)],
                            next_offset: None,
                            updating_workspaces: Vec::new(),
                            corrected_query: None,
                            request_seq: 2,
                        },
                        facets: None,
                        recent_queries: None,
                    }),
                },
            })
            .unwrap();

        assert!(surface.apply_ready_reads());
        assert!(!surface.replacing_results());
        surface.request_open(Disposition::CurrentWindow);
        assert!(matches!(
            surface.take_target(),
            Some(Target::Store {
                node: Some(node),
                ..
            }) if node == new_node
        ));
    }

    /// QUESTIONS #7 (maintainer 2026-10-10): with `ENABLE_CANVAS` off, a canvas the
    /// store answered with never becomes a row the surface lists or opens.
    #[test]
    fn a_landing_page_drops_canvases_while_the_build_hides_them() {
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            let (reader, response_tx, _request_rx) = SearchReader::test_pair();
            let profile = ProfileId::new();
            let workspace = WorkspaceId::new();
            let mut surface = SearchSurface::new();
            surface.reader = Some(reader);
            with_catalog(&mut surface, profile, workspace);
            surface.pending_read = Some(3);
            let page_hit = hit(profile, workspace, &a_node_id());
            let mut canvas = hit(profile, workspace, &a_node_id());
            canvas.document_kind = "canvas".into();
            response_tx
                .send(SearchReadResponse {
                    seq: 3,
                    result: SearchReadResult::Page {
                        append: false,
                        result: Ok(SearchResponse {
                            page: SearchPage {
                                hits: vec![canvas, page_hit.clone()],
                                next_offset: None,
                                updating_workspaces: Vec::new(),
                                corrected_query: None,
                                request_seq: 3,
                            },
                            facets: None,
                            recent_queries: None,
                        }),
                    },
                })
                .unwrap();
            assert!(surface.apply_ready_reads());
            assert_eq!(surface.page.hits, vec![page_hit]);
        });
    }

    /// **Zero hits while the index is still building is not a verdict.** The
    /// line that said so began "No current matches", which is the half a person
    /// reads — and reads as final.
    #[test]
    fn an_empty_page_from_a_workspace_still_indexing_does_not_read_as_final() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "#newworld");
        assert_eq!(surface.indexing_heading(), None, "nothing is updating");

        surface.page.updating_workspaces = vec![WorkspaceId::new().to_storage()];
        assert_eq!(
            surface.indexing_heading().as_deref(),
            Some("Still indexing one workspace…")
        );

        surface.page.updating_workspaces =
            (0..15).map(|_| WorkspaceId::new().to_storage()).collect();
        assert_eq!(
            surface.indexing_heading().as_deref(),
            Some("Still indexing 15 workspaces…")
        );
    }

    #[test]
    fn ai_result_keeps_exact_block_identity_and_can_be_excluded_immediately() {
        let (reader, response_tx, request_rx) = SearchReader::test_pair();
        let chat = ChatId::new();
        let message = MessageId::new();
        let block = MessageBlockId::new();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        with_catalog(&mut surface, ProfileId::new(), WorkspaceId::new());
        surface.query.set_text(&mut surface.engine, "answer");
        surface.pending_read = Some(7);
        surface.pending_replace = Some(7);
        response_tx
            .send(SearchReadResponse {
                seq: 7,
                result: SearchReadResult::FederatedPage {
                    append: false,
                    result: Ok(koru_api_client::FederatedSearchResponse {
                        page: koru_api_client::FederatedSearchPage {
                            hits: vec![koru_api_client::FederatedSearchHit::Ai {
                                hit: koru_ai_core::AiSearchHit {
                                    chat_id: chat,
                                    message_id: message,
                                    block_id: block,
                                    chat_title: "Planning".into(),
                                    snippet: "An answer".into(),
                                    spans: vec![koru_ai_core::AiSearchSpan { start: 3, end: 9 }],
                                    score: 1.0,
                                    profile_id: None,
                                },
                            }],
                            next_cursor: None,
                            updating_workspaces: Vec::new(),
                            request_seq: 7,
                            corrected_query: None,
                        },
                        facets: None,
                        recent_queries: None,
                        sources: Vec::new(),
                    }),
                },
            })
            .unwrap();
        assert!(surface.apply_ready_reads());
        assert_eq!(surface.page.hits.len(), 1);
        assert!(
            surface
                .ai_targets
                .contains_key(&hit_key(&surface.page.hits[0]))
        );
        surface.request_open(Disposition::CurrentWindow);
        assert_eq!(
            surface.take_target(),
            Some(Target::Chat {
                chat,
                message,
                block,
                profile: None,
                disposition: Disposition::CurrentWindow,
            })
        );

        surface.menu = SearchMenu::Workspaces;
        surface.activate_popover_row(0);
        assert!(!surface.include_ai);
        assert!(surface.page.hits.is_empty());
        let request = request_rx.recv().unwrap();
        assert!(matches!(
            request.into_kind(),
            SearchReadKind::FederatedPage {
                include_ai: false,
                ..
            }
        ));
    }

    #[test]
    fn an_availability_warning_reserves_a_banner_and_clears_stale_results() {
        let mut surface = SearchSurface::new();
        set_query(&mut surface, "roadmap");
        surface.page.hits = vec![hit(ProfileId::new(), WorkspaceId::new(), &a_node_id())];
        let before = surface.results_top();

        assert!(surface.set_availability_warning(Some("Store unavailable".into())));
        assert_eq!(surface.availability_warning(), Some("Store unavailable"));
        assert_eq!(surface.results_top(), before + WARNING_H);
        assert!(surface.page.hits.is_empty());
        assert!(!surface.set_availability_warning(Some("Store unavailable".into())));

        assert!(surface.set_availability_warning(None));
        assert_eq!(surface.results_top(), before);
    }

    #[test]
    fn store_recovery_does_not_clear_a_hosts_launch_warning() {
        let mut surface = SearchSurface::new();
        assert!(surface.set_host_warning(Some("Koru app was not found".into())));
        assert!(surface.set_availability_warning(Some("Store unavailable".into())));

        assert!(surface.set_availability_warning(None));
        assert_eq!(surface.host_warning(), Some("Koru app was not found"));
        assert_eq!(surface.warning_message(), Some("Koru app was not found"));
    }

    #[test]
    fn a_dictated_query_stays_provisional_then_refreshes_on_commit() {
        let (reader, _responses, requests) = SearchReader::test_pair();
        let profile = ProfileId::new();
        let workspace = WorkspaceId::new();
        let mut surface = SearchSurface::new();
        surface.reader = Some(reader);
        surface.pending_read = None;
        surface.pending_replace = None;
        with_catalog(&mut surface, profile, workspace);

        assert!(surface.begin_dictation());
        surface.update_dictation("release");
        surface.update_dictation("release plan");
        assert_eq!(surface.query.raw_text(), "", "partials are not queries");
        assert!(requests.try_recv().is_err(), "partials did not search");

        surface.commit_dictation("release plan");
        assert_eq!(surface.query.raw_text(), "release plan");
        let request = requests.recv().expect("the final query refreshed results");
        let SearchReadKind::FederatedPage { query, .. } = request.into_kind() else {
            panic!("global search must issue a federated page")
        };
        assert_eq!(query.text, "release plan");
    }
}
