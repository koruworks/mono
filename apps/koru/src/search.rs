// SPDX-License-Identifier: MPL-2.0

//! The search surface, as this app's kind of view.
//!
//! The surface itself is [`koru_search_ui`] — 3,600 lines that know no node, no
//! workspace and no document, so the agent can host the same one. What is here
//! is the adapter: the `View` this app's window loop drives, and the two
//! translations only this app can do.
//!
//! ## Why a newtype rather than an `impl View` on the surface
//!
//! The orphan rule. `View` belongs to `koru-workspace-host` and `SearchSurface` to
//! `koru-search-ui`, and both are foreign here — so the impl needs a local
//! type to hang on. That turns out to be where the translations want to live
//! anyway.
//!
//! ## The two translations
//!
//! **Ids.** The surface speaks the store's; this app speaks `koru-core`'s.
//! Both are UUID newtypes, so each conversion is total — but *which* uuid goes
//! where is app knowledge, and it used to be ten conversions buried inside the
//! surface's `request_open`.
//!
//! **Where a result opens.** The result's profile/workspace are search-time
//! metadata, not a document route. A document result becomes stable
//! `NavigationTarget::Document` identity; the shell asks Store for its current
//! location and reads current Trash state there. A workspace-level result keeps
//! its profile/workspace because that workspace itself is the destination.

use koru_core::{NodeId, ProfileId, WorkspaceId};
use koru_window::WindowRequest;
use koru_workspace_host::{PaneRequest, SearchTargetDisposition, View};

pub(crate) use koru_search_ui::SURFACE_RADIUS;

/// The radius the app's own chrome rounds the overlay to. Named here so
/// `mainview` does not reach across a crate boundary for a number that is the
/// surface's own idea of its shape.
pub(crate) struct SearchSurface(koru_search_ui::SearchSurface);

impl SearchSurface {
    /// The query box and its engine — see
    /// [`koru_search_ui::SearchSurface::dictation_field`]. Forwarded rather
    /// than inherited, because this newtype inherits nothing.
    #[cfg(test)]
    pub(crate) fn dictation_field(
        &mut self,
    ) -> (&mut koru_ui::TextInput, &mut koru_draw::text::TextEngine) {
        self.0.dictation_field()
    }

    /// Route through the surface so committing a query also refreshes results.
    pub(crate) fn dictate(&mut self, action: &koru_dictation::DictationAction) -> bool {
        match action {
            koru_dictation::DictationAction::Begin { .. } => self.0.begin_dictation(),
            koru_dictation::DictationAction::Update { text, .. } => {
                self.0.update_dictation(text.as_str());
                true
            }
            koru_dictation::DictationAction::Commit { text, .. } => {
                self.0.commit_dictation(text.as_str());
                true
            }
            koru_dictation::DictationAction::Cancel { .. } => {
                self.0.cancel_dictation();
                true
            }
        }
    }

    pub(crate) fn new() -> Self {
        Self(koru_search_ui::SearchSurface::new_at(gateway_root()))
    }

    pub(crate) fn for_workspace(profile: ProfileId, workspace: WorkspaceId, name: String) -> Self {
        Self(koru_search_ui::SearchSurface::for_workspace_at(
            store_profile(profile),
            store_workspace(workspace),
            name,
            gateway_root(),
        ))
    }

    pub(crate) fn resume(&mut self) {
        self.0.resume();
    }

    /// **This host's command bar** (PLAN.final P13): the CREATE rows the
    /// typed text could become, the footer's chips and the box's words. The
    /// surface draws them; what a row makes is answered in `mainview` when
    /// the surface hands back a `Create`.
    pub(crate) fn make_command_bar(
        &mut self,
        rows: Vec<koru_search_ui::CreateRow>,
        chips: Vec<koru_search_ui::FooterChip>,
        placeholder: String,
    ) {
        self.0.set_create_rows(rows);
        self.0.set_footer_chips(chips);
        self.0.set_placeholder(Some(placeholder));
    }

    /// What `↵` on a CREATE row asked for, once.
    pub(crate) fn take_create(&mut self) -> Option<koru_search_ui::Create> {
        self.0.take_create()
    }

    pub(crate) fn resume_in_workspace(
        &mut self,
        profile: ProfileId,
        workspace: WorkspaceId,
        name: String,
    ) {
        self.0
            .resume_in_workspace(store_profile(profile), store_workspace(workspace), name);
    }

    /// Which workspace this surface is scoped to, in *this app's* ids.
    ///
    /// Asked when a search window is closed and reopened, so the scope survives
    /// — and converted back here, because the surface answers in the store's.
    ///
    /// **`cfg(test)` because only `mainview/tests.rs` asks.** The container
    /// keeps the scope by holding the surface itself (`retained_search`), so
    /// nothing in the shipping path needs to read it back; the test is what
    /// checks that reopening does not silently drop it. Note that the *surface*
    /// cannot do this — a `#[cfg(test)]` item does not exist for another crate,
    /// which is exactly why `koru_search_ui::workspace_scope` is unconditional.
    #[cfg(test)]
    pub(crate) fn workspace_scope(&self) -> Option<(ProfileId, WorkspaceId)> {
        self.0.workspace_scope().map(|(profile, workspace)| {
            (
                ProfileId::from_uuid(profile.as_uuid()),
                WorkspaceId::from_uuid(workspace.as_uuid()),
            )
        })
    }
}

// ------------------------------------------------------------- the two halves

fn store_profile(id: ProfileId) -> koru_store_core::ids::ProfileId {
    koru_store_core::ids::ProfileId::from_uuid(id.as_uuid())
}

fn store_workspace(id: WorkspaceId) -> koru_store_core::ids::WorkspaceId {
    koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid())
}

/// What the app should do about a result the person chose.
///
/// `None` when the target names a node this app cannot parse — the same shape
/// the surface's own refusals have: a navigation that cannot be performed is
/// declined rather than performed somewhere arbitrary.
pub(crate) fn pane_request(target: koru_search_ui::Target) -> Option<PaneRequest> {
    let (profile, workspace, document, node, match_text, disposition) = match target {
        koru_search_ui::Target::Store {
            profile,
            workspace,
            document,
            node,
            trashed: _,
            match_text,
            disposition,
        } => (profile, workspace, document, node, match_text, disposition),
        koru_search_ui::Target::Chat { chat, block, .. } => {
            return Some(PaneRequest::OpenChatLink {
                chat: chat.to_string(),
                block: Some(block.to_string()),
            });
        }
    };
    let document = document.map(|id| koru_core::DocumentId::from_uuid(id.as_uuid()));
    // The index stores a node id as a string, because it has no type for one.
    // A value this app cannot parse is a refusal, not a `None`: `None` already
    // means "a hit on the document's title", which lands somewhere real.
    let node = match node.as_deref() {
        None => None,
        Some(raw) => match NodeId::parse(raw) {
            Some(id) => Some(id),
            None => {
                koru_workspace_host::search_debug!(
                    "[Koru][search-open][pid={}] target rejected: unparseable node id",
                    std::process::id()
                );
                return None;
            }
        },
    };
    let target = match document {
        Some(document) => koru_workspace_host::NavigationTarget::Document { document, node },
        None => koru_workspace_host::NavigationTarget::Workspace {
            profile: ProfileId::from_uuid(profile.as_uuid()),
            workspace: WorkspaceId::from_uuid(workspace.as_uuid()),
        },
    };
    Some(PaneRequest::Navigate {
        target,
        match_text,
        disposition: match disposition {
            koru_search_ui::Disposition::CurrentWindow => SearchTargetDisposition::CurrentWindow,
            koru_search_ui::Disposition::NewWindow => SearchTargetDisposition::NewWindow,
        },
    })
}

// ------------------------------------------------------------------ the view

impl View for SearchSurface {
    fn key(
        &mut self,
        _ws: &mut koru_core::schema::WorkspaceDoc,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        self.0.key(key, text, mods)
    }

    fn mouse_down(
        &mut self,
        _ws: &mut koru_core::schema::WorkspaceDoc,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool {
        self.0.mouse_down(x, y, ctrl)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.0.mouse_move(x, y)
    }

    fn mouse_up(&mut self, _ws: &mut koru_core::schema::WorkspaceDoc) -> bool {
        self.0.mouse_up()
    }

    fn mouse_double_click(
        &mut self,
        _ws: &koru_core::schema::WorkspaceDoc,
        x: f32,
        y: f32,
    ) -> bool {
        self.0.mouse_double_click(x, y)
    }

    fn scroll_by(&mut self, delta: winit::event::MouseScrollDelta) {
        self.0.scroll_by(delta);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.0.set_viewport(w, h);
    }

    fn cursor_icon(&self) -> winit::window::CursorIcon {
        self.0.cursor_icon()
    }

    /// **Both no-ops, and that is the point.** A document changing underneath
    /// this pane, or being replaced entirely, is nothing to a list of results
    /// over an HTTP query. They were no-ops before the extraction too; what is
    /// new is that the surface no longer has to be handed a document to ignore.
    fn adopt(&mut self, _ws: &koru_core::schema::WorkspaceDoc) {}
    fn reseat(&mut self, _ws: &koru_core::schema::WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.0.invalidate();
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        self.0.maintenance_deadline()
    }

    fn maintain(
        &mut self,
        _ws: &mut koru_core::schema::WorkspaceDoc,
        now: std::time::Instant,
    ) -> bool {
        self.0.maintain(now)
    }

    fn animating(&self) -> bool {
        self.0.animating()
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        _workspace: &koru_workspace::Workspace,
        _images: &koru_draw::ImageCache,
        root: vello::kurbo::Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        self.0.draw(scene, root, caret_on, pointer);
    }

    fn is_global_search(&self) -> bool {
        self.0.is_global_search()
    }

    fn take_pane_request(&mut self) -> Option<PaneRequest> {
        self.0.take_target().and_then(pane_request)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.0.take_window_request()
    }
}

/// `--root`, when this launch was given one.
///
/// **Read here rather than handed down from `App`.** The surface is built by
/// `MainView`, which has no `App` above it and no store connection of its own —
/// and argv does not change, so one reading is as good as another. What matters
/// is that the overlay queries the same store the window's documents came from;
/// a shared-store overlay in a rooted window returns hits from workspaces that
/// window cannot open, which reads as a broken index rather than as the wrong
/// store. The @ mention picker's `EntitySearch` reads here too — it answers the
/// same question with the same stakes.
pub(crate) fn gateway_root() -> Option<std::path::PathBuf> {
    koru_workspace_host::cli::root_from_args()
}

#[cfg(test)]
mod tests;
