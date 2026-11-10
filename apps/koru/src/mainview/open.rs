// SPDX-License-Identifier: MPL-2.0

//! How a document arrives in a tab.
//!
//! Explore reuses a working Main tab. A pin's document is identity: SameTab on
//! a pin focuses an existing tab of the target or mints a new Main tab. Search,
//! recents, history and `open_node` still reuse-or-push.

use koru_core::schema::WorkspaceDoc;
use koru_core::{Kind, NodeId};
use koru_editor::trigger::DocumentTarget;
use koru_editor::{ChatTarget, Editing, OpenWhere, Reference, SplitSide};

use super::MainView;
use super::place_chooser::split_dir;
use super::tab::{Tab, TabView};
use super::tab_rules::KoruTabs;

#[cfg(test)]
mod tests;

impl MainView {
    /// Where a click on `node` actually goes: the tab to open, and what to show
    /// inside it once it is open.
    ///
    /// **Only a `Kind::Document` can root a tab.** Anything else resolves to the
    /// nearest document above it and is *revealed* there — an image is a place
    /// in a document, not a document. A node with no document above it (a
    /// contact) resolves to nothing at all rather than opening the empty tab it
    /// used to: rooting a view at a childless node gives a `walk_under` of
    /// nothing and a blank page.
    ///
    /// `Kind::Outline` becomes one more arm here when it lands.
    pub(super) fn resolve_target(
        ws: &WorkspaceDoc,
        node: NodeId,
    ) -> Option<(NodeId, Option<NodeId>)> {
        if ws.read(node).ok()?.kind == Kind::Document {
            return Some((node, None));
        }
        // A ListBlock is a container (skipped by `Body::rebuild`, so it has no
        // row to scroll to). Reveal its first child instead — the first item.
        let reveal_id = {
            let node_kind = ws.read(node).ok()?.kind;
            if node_kind == Kind::ListBlock {
                ws.children(koru_core::schema::Parent::Node(node))
                    .ok()?
                    .into_iter()
                    .next()
            } else {
                None
            }
        };
        let reveal_id = reveal_id.unwrap_or(node);
        // Resolve to the nearest Document ancestor.
        let doc = ws
            .ancestors(reveal_id)
            .ok()?
            .into_iter()
            .find(|&a| ws.read(a).is_ok_and(|n| n.kind == Kind::Document))?;
        Some((doc, Some(reveal_id)))
    }

    /// Open whatever tab `node` belongs to, and show `node` inside it.
    pub(crate) fn open_node(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        // **An explore row names a *document*, not a node.** The pane lists what
        // the manifest holds, and those documents are not loaded — so there is
        // no node to open, and the id in the row is a `DocumentId` wearing a
        // `NodeId`'s type (see `ExploreView::rebuild_from_entries`).
        //
        // Asking the shell is the only way: it owns the workspace and the
        // connection. The row is recognised by *being one of the entries*,
        // which is exact — a node id that happened to collide would have to be
        // a v4 UUID minted for a document in this very workspace.
        if self.request_store_document(node, true) {
            Self::log_open(node, "reuse-or-push", "current-workspace-manifest");
            return true;
        }
        if self.open_node_with_centering(ws, node, false, None) {
            Self::log_open(node, "reuse-or-push", "current-open-document");
            return true;
        }
        // **A node this workspace can read is not a foreign document id.**
        // It resolved, and `land_on` still found nothing to open -- a contact,
        // say, which lives in the library and belongs to no document. That is
        // an honest "nothing to open", and asking Store to route it would send
        // the node's uuid off as a `DocumentId` it never was, to come back as
        // an error dialog about a document nobody named.
        //
        // The distinction is what makes the fallback below safe: a pill into
        // another workspace does *not* resolve here, because a document is its
        // own Loro document and only the open one is loaded.
        if ws.read(node).is_ok() {
            return false;
        }
        // A pre-Store document pill carries only the target document's stable
        // UUID. It may now live in another workspace, so the current manifest
        // cannot route it. Ask the shell to resolve it through Store's global
        // catalog; this is also resilient to a document having moved since the
        // pill was created.
        self.record_jump();
        tracing::info!(
            target: "koru::document_link",
            target = %node.to_storage(),
            branch = "reuse-or-push",
            route = "global-document-location",
            "document link requires Store routing"
        );
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: koru_core::DocumentId::from_uuid(node.as_uuid()),
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        });
        true
    }

    /// Explore click: reuse a working tab, except when a chat is active.
    pub(super) fn open_from_explore(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        if self.try_explore_store_document(node) {
            return true;
        }
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            if ws.read(node).is_ok() {
                return false;
            }
            return self.open_node(ws, node);
        };
        if let Some(i) = self.tabs().showing(root) {
            Self::log_open(node, "already-showing", "explore");
            self.record_jump();
            self.focus_tab(i);
            if let Some(target) = reveal {
                self.tabs_mut().active_view().reveal(target);
            }
            return true;
        }
        if self.ensure_working_tab() {
            Self::log_open(node, "replace-working", "explore");
            return self.replace_this_tab(ws, root, reveal);
        }
        Self::log_open(node, "mint-main", "explore");
        self.push_document_tab(ws, root, reveal, None)
    }

    /// Explore's *Open in new tab*: ask the shell and land with [`OpenWhere::NewTab`].
    pub(super) fn request_new_tab_document(&mut self, node: NodeId) -> bool {
        self.navigate_store_document(node, OpenWhere::NewTab)
    }

    fn navigate_store_document(&mut self, node: NodeId, where_: OpenWhere) -> bool {
        let Some(entries) = &self.entries else {
            return false;
        };
        let Some(entry) = entries.iter().find(|e| e.id.as_uuid() == node.as_uuid()) else {
            return false;
        };
        let id = entry.id;
        self.record_jump();
        let (branch, disposition) = match where_ {
            OpenWhere::NewTab => {
                self.pending_lands.push_back((id, OpenWhere::NewTab));
                (
                    "mint-main",
                    koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                )
            }
            OpenWhere::NewWindow => (
                "new-window",
                koru_workspace_host::SearchTargetDisposition::NewWindow,
            ),
            _ => return false,
        };
        Self::log_open(node, branch, "explore-menu");
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: id,
                node: None,
            },
            match_text: None,
            disposition,
        });
        true
    }

    /// Multi-select *Open in new window*: every page in **one** new window.
    pub(super) fn request_documents_in_new_window(&mut self, nodes: Vec<NodeId>) -> bool {
        let Some(entries) = &self.entries else {
            return false;
        };
        let documents: Vec<_> = nodes
            .iter()
            .filter_map(|node| {
                entries
                    .iter()
                    .find(|e| e.id.as_uuid() == node.as_uuid())
                    .map(|e| e.id)
            })
            .collect();
        if documents.is_empty() {
            return false;
        }
        self.record_jump();
        self.ask_pane(koru_workspace_host::PaneRequest::OpenDocumentsInNewWindow { documents })
    }

    /// Ask the shell for a Store document by the uuid an explore row or pill
    /// carries. `record` is true for a direct user open (Explore); false when
    /// the caller already recorded the place they left (a routed pill or search
    /// result), so a workspace switch's intermediate document is not inserted
    /// into Back.
    pub(super) fn request_store_document(&mut self, node: NodeId, record: bool) -> bool {
        self.request_store_document_with(
            node,
            record,
            koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        )
    }

    /// The same ask, **pinned to this window**.
    ///
    /// For the half-finished gestures — the context menu's *Rename* and its four
    /// *Attributes* rows, and an explore drop's parked pane placement — whose
    /// other half is waiting on this container in `pending_document_rename` /
    /// `pending_document_attrs` / `pending_pane_drops`. With a second
    /// window open on the same workspace, an ordinary
    /// [`CurrentWindow`](koru_workspace_host::SearchTargetDisposition::CurrentWindow)
    /// landing is offered to the window already holding the document *first*,
    /// which for a search hit is the right answer and for a gesture is the wrong
    /// one: the picker opens where nobody right-clicked and the request that
    /// asked for it is stranded. Q44.
    pub(super) fn request_store_document_here(&mut self, node: NodeId, record: bool) -> bool {
        self.request_store_document_with(
            node,
            record,
            koru_workspace_host::SearchTargetDisposition::SourceWindow,
        )
    }

    fn request_store_document_with(
        &mut self,
        node: NodeId,
        record: bool,
        disposition: koru_workspace_host::SearchTargetDisposition,
    ) -> bool {
        let Some(entries) = &self.entries else {
            return false;
        };
        let Some(entry) = entries.iter().find(|e| e.id.as_uuid() == node.as_uuid()) else {
            return false;
        };
        let id = entry.id;
        if record {
            self.record_jump();
        }
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: id,
                node: None,
            },
            match_text: None,
            disposition,
        });
        true
    }

    /// Open through the normal tab/history path, optionally giving an inner
    /// document match the centered landing used by search.
    pub(super) fn open_node_with_centering(
        &mut self,
        ws: &WorkspaceDoc,
        node: NodeId,
        center_reveal: bool,
        match_text: Option<&str>,
    ) -> bool {
        // **The one choke point for back/forward** on a user gesture. Shell
        // landings use [`land_on`](Self::land_on) instead — they must not mint a
        // history stop for a document the person never chose.
        self.record_jump();
        self.land_on(ws, node, center_reveal, match_text)
    }

    /// Put the window on `node` without touching history.
    ///
    /// Used when the shell announces the active document after a load or a
    /// workspace switch. The place the person left was already recorded at the
    /// gesture that asked for this arrival; recording again would insert the
    /// workspace's default document between a cross-workspace pill and its
    /// real target.
    pub(super) fn land_on(
        &mut self,
        ws: &WorkspaceDoc,
        node: NodeId,
        center_reveal: bool,
        match_text: Option<&str>,
    ) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            return false;
        };
        // A canvas this build hides opens nowhere (`canvas_hidden`).
        if self.canvas_hidden(self.active_document) {
            return false;
        }
        // The recents themselves are recorded in `show_active_document`, where
        // the shell names the active store id. This is their titled twin,
        // recorded here because a landing is the one moment the root node and
        // the store id are both in hand.
        if let Some(document) = self.active_document {
            let title = koru_workspace::name_of(ws, root);
            self.recent_documents.retain(|(id, _)| *id != document);
            self.recent_documents.insert(0, (document, title));
            self.recent_documents
                .truncate(koru_workspace_host::MAX_RECENTS);
        }
        // **By document when there is one to go by**, and by node only when
        // there is not — a workspace with no manifest, which is every workspace
        // in the editor's own test suite. `showing` compares a `NodeId` inside a
        // tab's document, and a tab rebuilt against a different document reports
        // *that* one's root, so it could claim a document it had never shown.
        // Only the arrival this disposition was asked for may consume it; any
        // other arrival means the request was superseded, and the disposition
        // goes with it rather than waiting for a stranger.
        // Claim the disposition for *this* arrival only; leave others queued.
        let landed = self
            .active_document
            .and_then(|active| {
                self.pending_lands
                    .iter()
                    .position(|(document, _)| *document == active)
                    .map(|i| self.pending_lands.remove(i).map(|(_, w)| w))
            })
            .flatten();
        match landed {
            Some(OpenWhere::SameTab) => {
                return self.honor_same_tab_at(ws, root, reveal);
            }
            Some(OpenWhere::NewTab) => {
                return self.push_document_tab(
                    ws,
                    root,
                    reveal.or_else(|| match_text.map(|_| root)),
                    match_text,
                );
            }
            // The place chooser's arm, for a document the Store had to open:
            // the gesture recorded the place it left, so this records none.
            Some(OpenWhere::Split(side)) => {
                return self.split_document_tab(
                    ws,
                    root,
                    reveal.or_else(|| match_text.map(|_| root)),
                    match_text,
                    side,
                );
            }
            Some(OpenWhere::BackgroundTab)
            | Some(OpenWhere::Reuse)
            | Some(OpenWhere::NewWindow)
            | None => {}
        }
        let already = self
            .active_document
            .and_then(|id| self.tabs().showing_document(id))
            .or_else(|| self.tabs().showing(root));
        match already {
            Some(i) => {
                let rows = match &self.tabs().get(i).map(|t| &t.view) {
                    Some(super::tab::TabView::Leaf(l)) => l.body().rows.len(),
                    _ => 0,
                };
                crate::mainview::nav_debug::nav_debug!(
                    "[koru:nav] land already_showing=true history_pending={} tab_pending={} rows={rows}",
                    self.history_pending.is_some(),
                    self.tab_pending.is_some(),
                );
                let home = self.current_workspace();
                if let Some(tab) = self.tabs_mut().get_mut(i) {
                    if tab.home.is_none() {
                        tab.home = home;
                    }
                }
                self.tabs_mut().select(i);
                self.size_visible_groups();
                self.apply_tab_pending(ws);
            }
            None => {
                if self.restoring {
                    return self.replace_this_tab(ws, root, reveal);
                }
                let name = koru_workspace::name_of(ws, root);
                let active = self.active_document;
                let view = self.document_view(ws, root, active);
                let home = self.current_workspace();
                self.tabs_mut()
                    .push(Tab::document_at(view, &name, active, home));
                self.mark_tab_widths_stale();
                self.size_visible_groups();
            }
        }
        self.ensure_tab_visible(self.tabs().active());
        if let Some(target) = reveal.or_else(|| match_text.map(|_| root)) {
            if center_reveal {
                if let Some(match_text) = match_text {
                    self.tabs_mut()
                        .active_view()
                        .reveal_search_match(target, match_text);
                } else {
                    self.tabs_mut().active_view().reveal_centered(target);
                }
            } else {
                self.tabs_mut().active_view().reveal(target);
            }
        }
        true
    }

    /// Follow `node` **in this tab**, replacing what it was showing.
    ///
    /// The browser's plain click, and what Enter on a selected pill means.
    /// [`open_node`](Self::open_node) is not this: it reuses a tab already on
    /// the document and otherwise opens a *second* one, which is the right
    /// answer for clicking a pill and the wrong one for "open it here".
    ///
    /// A tab already showing the target is still preferred — following a
    /// reference to the document you are looking at somewhere else should take
    /// you there rather than give you two of it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn open_node_here(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            return false;
        };
        if self.canvas_hidden(self.active_document) {
            return false;
        }
        if let Some(i) = self.tabs().showing(root) {
            return self.select(i) && {
                if let Some(target) = reveal {
                    self.tabs_mut().active_view().reveal(target);
                }
                true
            };
        }
        self.honor_same_tab_at(ws, root, reveal)
    }

    pub(super) fn replace_this_tab(
        &mut self,
        ws: &WorkspaceDoc,
        root: NodeId,
        reveal: Option<NodeId>,
    ) -> bool {
        if !self.tabs().active_tab().is_reusable() {
            Self::log_open(root, "pin-redirect", "replace");
            return self.push_document_tab(ws, root, reveal, None);
        }
        self.record_jump();
        // The same swap `promote_draft` does: the tab keeps its place in the
        // strip and only what it is over changes.
        let home = self.current_workspace();
        let active = self.active_document;
        let view = self.document_view(ws, root, active);
        let tab = self.tabs_mut().active_mut();
        tab.view = view;
        tab.document = active;
        tab.home = home;
        tab.set_name(&koru_workspace::name_of(ws, root));
        tab.draft = None;
        // The leaf is a new one and has never been told the viewport.
        tab.invalidate_size();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.explore_stale = true;
        if let Some(target) = reveal {
            self.tabs_mut().active_view().reveal(target);
        }
        true
    }

    /// Open `node` in a **new** tab, even if one already shows it.
    ///
    /// The difference from [`open_node`](Self::open_node) is the whole point:
    /// that one reuses a tab already on the document, which is right for a click
    /// and wrong for Ctrl+Enter — a modifier that says "somewhere else" and then
    /// moved you to a tab you already had would look broken.
    pub(super) fn open_node_in_new_tab(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            return false;
        };
        if self.canvas_hidden(self.active_document) {
            return false;
        }
        self.record_jump();
        let name = koru_workspace::name_of(ws, root);
        let active = self.active_document;
        let view = self.document_view(ws, root, active);
        let home = self.current_workspace();
        self.tabs_mut()
            .push(Tab::document_at(view, &name, active, home));
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        if let Some(target) = reveal {
            self.tabs_mut().active_view().reveal(target);
        }
        true
    }

    /// Insert a tab for `node` beside the current one without selecting it.
    /// Reuses an existing tab on that document rather than duplicating it.
    pub(super) fn open_node_in_background_tab(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            return false;
        };
        if self.canvas_hidden(self.active_document) {
            return false;
        }
        if let Some(i) = self.tabs().showing(root) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] view background-tab already-showing i={i}"
            ));
            self.ensure_tab_visible(i);
            return true;
        }
        let name = koru_workspace::name_of(ws, root);
        let tab = Tab::document_at(
            self.document_view(ws, root, self.active_document),
            &name,
            self.active_document,
            self.current_workspace(),
        );
        let i = self.tabs_mut().insert_inactive(tab);
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view background-tab inserted i={i} pinned_showing={}",
            self.tabs().active_tab().is_pinned(),
        ));
        self.mark_tab_widths_stale();
        self.ensure_tab_visible(i);
        if let Some(target) = reveal
            && let Some(tab) = self.tabs_mut().get_mut(i)
        {
            tab.view.reveal(target);
        }
        true
    }

    /// Open `node` in a new **window**, on the channel a popped-out tab uses.
    ///
    /// Unlike [`pop_out`](Self::pop_out) it closes nothing: this document was
    /// never a tab here. The shell's `open_document` focuses an existing window
    /// already showing it rather than making a second — which is what you want
    /// from Shift+Enter, and is the one behaviour that differs from a new tab.
    pub(super) fn open_node_in_new_window(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some((root, _)) = Self::resolve_target(ws, node) else {
            return false;
        };
        self.wants_popout = Some(root);
        true
    }

    /// Drain the active tab's request to open something, now that a document is
    /// in hand.
    ///
    /// `View::take_wants_open` has no `&WorkspaceDoc`, and building a document
    /// view needs one — so the request is drained at the tail of the handlers
    /// that do have it. The container consumes it rather than passing it up:
    /// a document opens as a **tab**, and only Ctrl+N opens a window.
    pub(super) fn drain_wants_open(&mut self, ws: &WorkspaceDoc) -> bool {
        let Some((node, where_)) = self.active_view().take_wants_open() else {
            return false;
        };
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view drain-open where={where_:?} reusable={} pinned={}",
            self.tabs().active_tab().is_reusable(),
            self.tabs().active_tab().is_pinned(),
        ));
        self.open_node_where(ws, node, where_)
    }

    /// **Open a reference where the person asked** — the one door a click, an
    /// Enter and the place chooser all go through, whichever kind of
    /// reference it is: an older pill's node, a document (an `@` pill or a
    /// `koru://doc/…` link), or a chat (`koru://chat/…`).
    pub(crate) fn open_reference(
        &mut self,
        ws: &WorkspaceDoc,
        reference: Reference,
        where_: OpenWhere,
    ) -> bool {
        match reference {
            Reference::Node(node) => self.open_node_where(ws, node, where_),
            Reference::Document(target) => self.open_document_target(target, where_),
            Reference::Chat(chat) => self.open_chat_where(chat, where_),
        }
    }

    fn open_node_where(&mut self, ws: &WorkspaceDoc, node: NodeId, where_: OpenWhere) -> bool {
        match where_ {
            OpenWhere::Reuse => self.open_node(ws, node),
            OpenWhere::SameTab => self.honor_same_tab(ws, node),
            // A node this window does not hold names a document by its stable
            // id (a pill written before Store-per-document): the Store opens
            // it, and the landing makes the tab.
            OpenWhere::NewTab => {
                self.open_node_in_new_tab(ws, node)
                    || self.navigate_store_document(node, OpenWhere::NewTab)
            }
            OpenWhere::BackgroundTab => self.open_node_in_background_tab(ws, node),
            OpenWhere::NewWindow => self.open_node_in_new_window(ws, node),
            OpenWhere::Split(side) => self.open_node_in_split(ws, node, side),
        }
    }

    /// Open `node` in a new pane split off the focused one on `side` — an arm
    /// of the place chooser. The document stays where it was; the new pane
    /// takes the focus.
    pub(super) fn open_node_in_split(
        &mut self,
        ws: &WorkspaceDoc,
        node: NodeId,
        side: SplitSide,
    ) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            // Not a node this window holds: a document named by its stable
            // id. The Store opens it, and its landing splits.
            let Some(document) = self.entries.as_ref().and_then(|entries| {
                entries
                    .iter()
                    .find(|e| e.id.as_uuid() == node.as_uuid())
                    .map(|e| e.id)
            }) else {
                return false;
            };
            return self.open_document_target(
                DocumentTarget {
                    document,
                    node: None,
                },
                OpenWhere::Split(side),
            );
        };
        if self.canvas_hidden(self.active_document) {
            return false;
        }
        self.record_jump();
        self.split_document_tab(ws, root, reveal, None, side)
    }

    /// A new document tab on `root`, in a new pane split off the focused one.
    /// Never moves a tab that exists: the pane the reference was followed
    /// from keeps showing what it showed.
    fn split_document_tab(
        &mut self,
        ws: &WorkspaceDoc,
        root: NodeId,
        reveal: Option<NodeId>,
        match_text: Option<&str>,
        side: SplitSide,
    ) -> bool {
        let name = koru_workspace::name_of(ws, root);
        let active = self.active_document;
        let view = self.document_view(ws, root, active);
        let home = self.current_workspace();
        self.split_with(Tab::document_at(view, &name, active, home), side);
        if let Some(target) = reveal {
            match match_text {
                Some(text) => {
                    self.tabs_mut()
                        .active_view()
                        .reveal_search_match(target, text);
                }
                None => {
                    self.tabs_mut().active_view().reveal(target);
                }
            }
        }
        true
    }

    /// Put `tab` in a new pane on `side` of the focused one — the mechanism
    /// *Open in chat* splits with, on any side — and focus it.
    pub(super) fn split_with(&mut self, tab: Tab, side: SplitSide) {
        let beside = self.panes.focused_path().clone();
        self.panes.split(&beside, split_dir(side), tab);
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.slides_mut().clear();
        self.ensure_tab_visible(self.tabs().active());
    }

    /// **A document the Store routes — an `@` pill or a `koru://doc/…` link —
    /// opened where the person asked.** The arrival is announced by the
    /// shell, so a disposition that needs a tab waits in `pending_lands`,
    /// paired with its document, for [`land_on`](Self::land_on).
    pub(super) fn open_document_target(
        &mut self,
        target: DocumentTarget,
        where_: OpenWhere,
    ) -> bool {
        let match_text = target.node.map(|_| String::new());
        tracing::info!(
            target: "koru::document_link",
            document = %target.document.to_storage(),
            node = ?target.node.map(|node| node.to_storage()),
            disposition = ?where_,
            route = "store-document-location",
            "document link opened"
        );
        let navigate = |disposition| koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: target.document,
                node: target.node,
            },
            match_text: match_text.clone(),
            disposition,
        };
        match where_ {
            OpenWhere::BackgroundTab => {
                if let Some(i) = self.tabs().showing_document(target.document).or_else(|| {
                    self.tabs()
                        .showing(koru_core::NodeId::from_uuid(target.document.as_uuid()))
                }) {
                    koru_ui::keys::debug::write(format_args!(
                        "[Koru][input] view entity background-tab already-showing i={i}"
                    ));
                    self.ensure_tab_visible(i);
                } else {
                    koru_ui::keys::debug::write(format_args!(
                        "[Koru][input] view entity background-tab open-background"
                    ));
                    self.ask_pane(koru_workspace_host::PaneRequest::OpenBackground {
                        document: target.document,
                        node: target.node,
                    });
                }
            }
            OpenWhere::NewWindow => {
                self.ask_pane(navigate(
                    koru_workspace_host::SearchTargetDisposition::NewWindow,
                ));
            }
            OpenWhere::SameTab | OpenWhere::NewTab => {
                let land =
                    if where_ == OpenWhere::SameTab && !self.tabs().active_tab().is_reusable() {
                        if self.focus_showing_document(target.document) {
                            koru_ui::keys::debug::write(format_args!(
                                "[Koru][input] view entity same-tab already-showing"
                            ));
                            None
                        } else {
                            koru_ui::keys::debug::write(format_args!(
                                "[Koru][input] view entity same-tab pin-redirect=NewTab"
                            ));
                            Some(OpenWhere::NewTab)
                        }
                    } else {
                        Some(where_)
                    };
                if let Some(land) = land {
                    self.pending_lands.push_back((target.document, land));
                    self.ask_pane(navigate(
                        koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                    ));
                }
            }
            // **Pinned to this window**: the pane the split divides is this
            // window's, and an ordinary landing would be offered first to a
            // window already holding the document (Q44).
            OpenWhere::Split(side) => {
                self.record_jump();
                self.pending_lands
                    .push_back((target.document, OpenWhere::Split(side)));
                self.ask_pane(navigate(
                    koru_workspace_host::SearchTargetDisposition::SourceWindow,
                ));
            }
            OpenWhere::Reuse => {
                self.record_jump();
                self.ask_pane(navigate(
                    koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                ));
            }
        }
        true
    }

    /// Drain a `koru://chat/…` link the active document's click asked to open.
    pub(super) fn drain_wants_chat_open(&mut self) -> bool {
        let asked = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_wants_chat_open(),
            _ => None,
        };
        match asked {
            Some((chat, where_)) => self.open_chat_where(chat, where_),
            None => false,
        }
    }

    /// **A Koru chat a link names, opened inside this window** — as a chat
    /// tab of this window's profile, by the click rule a document follows.
    ///
    /// A chat of another profile is not shown here (AI schema 13): the chat
    /// view finds that out when it reads the chat, and hands it to a window
    /// of its profile through the route every chat surface uses
    /// (`open_chats_elsewhere`). Which is why only a chat this window is
    /// known to list ever *replaces* a tab — the tab handed back is closed,
    /// and it must not be the page the person was reading.
    pub(crate) fn open_chat_where(&mut self, target: ChatTarget, where_: OpenWhere) -> bool {
        if !koru_features::ai() {
            return false;
        }
        let id = koru_ai_ui::ChatId::from_uuid(target.chat);
        let block = target.block.map(koru_ai_ui::MessageBlockId::from_uuid);
        tracing::info!(
            target: "koru::chat_link",
            chat = %target.chat,
            disposition = ?where_,
            "chat link opened"
        );
        match where_ {
            OpenWhere::SameTab => self.open_chat_here(id, block),
            OpenWhere::BackgroundTab => self.open_chat_in_background(id, block),
            OpenWhere::Split(side) => self.open_chat_in_split(id, block, side),
            // A chat has no window of its own to open in — a chat tab cannot
            // be popped out — so Shift+click opens it here, in front, the way
            // a new tab does.
            OpenWhere::Reuse | OpenWhere::NewTab | OpenWhere::NewWindow => {
                self.open_ai_chat_tab_at(id, block)
            }
        }
    }

    /// The pane and index of a tab already showing `chat`, anywhere in the
    /// window.
    fn chat_tab_showing(
        &self,
        chat: koru_ai_ui::ChatId,
    ) -> Option<(super::panes::PanePath, usize)> {
        self.panes.iter_groups().find_map(|(path, group)| {
            group
                .tabs()
                .iter()
                .position(
                    |tab| matches!(&tab.view, TabView::AiChat(view) if view.chat_id() == Some(chat)),
                )
                .map(|index| (path, index))
        })
    }

    /// Whether this window already lists `chat` as one of its profile's — the
    /// Chats view's list or the Koru AI column's. Unknown is not "another
    /// profile's"; it is "not safe to replace a page with".
    fn chat_is_listed_here(&self, chat: koru_ai_ui::ChatId) -> bool {
        self.explore.chats().chats().iter().any(|c| c.id == chat)
            || self
                .ai_chat
                .recent_chats(usize::MAX)
                .iter()
                .any(|(listed, _)| *listed == chat)
    }

    /// Click: the chat **in this tab**, replacing the page the link was on —
    /// when the tab may be replaced and the chat is known to be this
    /// profile's. Otherwise the open-or-focus every chat link has had: a tab
    /// already on the chat is preferred, else a new one in front.
    fn open_chat_here(
        &mut self,
        id: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> bool {
        let replaceable = self.chat_tab_showing(id).is_none()
            && self.tabs().active_tab().is_reusable()
            && matches!(self.tabs().active_tab().view, TabView::Leaf(_))
            && self.chat_is_listed_here(id);
        if !replaceable {
            return self.open_ai_chat_tab_at(id, block);
        }
        self.record_jump();
        let left = self.tabs().active_tab().document;
        let mut tab = Tab::ai_chat_existing_at(id, block);
        let active = self.tabs_mut().active_mut();
        // The same tab, so Back returns to the page in it.
        tab.id = active.id;
        *active = tab;
        // A tab is what keeps a document open; this one no longer does.
        if let Some(document) = left
            && !self
                .panes
                .iter_groups()
                .any(|(_, group)| group.iter_tabs().any(|t| t.document == Some(document)))
        {
            self.ask_pane(koru_workspace_host::PaneRequest::CloseDocument(document));
        }
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.explore_stale = true;
        true
    }

    /// Ctrl+click: a chat tab beside this one, not made active. A tab already
    /// on the chat is kept rather than doubled, and is not switched to.
    fn open_chat_in_background(
        &mut self,
        id: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
    ) -> bool {
        if let Some((path, index)) = self.chat_tab_showing(id) {
            if &path == self.panes.focused_path() {
                self.ensure_tab_visible(index);
            }
            return true;
        }
        let i = self
            .tabs_mut()
            .insert_inactive(Tab::ai_chat_existing_at(id, block));
        self.mark_tab_widths_stale();
        self.ensure_tab_visible(i);
        true
    }

    /// A chooser arm: the chat in a new pane on `side` of this one. A tab
    /// already on the chat **moves** there rather than being doubled — one
    /// chat, one view of it.
    fn open_chat_in_split(
        &mut self,
        id: koru_ai_ui::ChatId,
        block: Option<koru_ai_ui::MessageBlockId>,
        side: SplitSide,
    ) -> bool {
        self.record_jump();
        let existing = self.chat_tab_showing(id).and_then(|(path, index)| {
            let tab_id = self.panes.group(&path)?.tabs().get(index)?.id;
            self.take_tab_from_group(&path, tab_id)
        });
        let Some(tab) = existing else {
            self.split_with(Tab::ai_chat_existing_at(id, block), side);
            return true;
        };
        let moved = tab.id;
        self.split_with(tab, side);
        if block.is_some()
            && let TabView::AiChat(view) = &mut self.tabs_mut().active_mut().view
        {
            view.open_link(id, block);
        }
        // The pane it left may be empty now; collapsing it can renumber the
        // panes, so the moved tab is found again by its id.
        self.settle_pane_drop(None);
        let found = self.panes.iter_groups().find_map(|(path, group)| {
            group
                .iter_tabs()
                .position(|t| t.id == moved)
                .map(|index| (path, index))
        });
        if let Some((path, index)) = found {
            self.panes.set_focus(path);
            let _ = self.select(index);
        }
        true
    }

    /// Focus a tab already showing `document` when SameTab must not replace.
    pub(super) fn focus_showing_document(&mut self, document: koru_core::DocumentId) -> bool {
        let Some(i) = self.tabs().showing_document(document) else {
            return false;
        };
        self.record_jump();
        self.focus_tab(i);
        true
    }

    fn try_explore_store_document(&mut self, node: NodeId) -> bool {
        let Some(entries) = &self.entries else {
            return false;
        };
        let Some(entry) = entries.iter().find(|e| e.id.as_uuid() == node.as_uuid()) else {
            return false;
        };
        let id = entry.id;
        if let Some(i) = self.tabs().showing_document(id) {
            Self::log_open(node, "already-showing", "explore-store");
            self.record_jump();
            self.focus_tab(i);
            return true;
        }
        self.record_jump();
        if self.ensure_working_tab() {
            Self::log_open(node, "replace-working", "explore-store");
            self.pending_lands.push_back((id, OpenWhere::SameTab));
        } else {
            Self::log_open(node, "mint-main", "explore-store");
            self.pending_lands.push_back((id, OpenWhere::NewTab));
        }
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: id,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        });
        true
    }

    fn ensure_working_tab(&mut self) -> bool {
        // Opening from a chat keeps the conversation in place and gives the
        // selected document its own tab, even if another working tab exists.
        if matches!(self.tabs().active_tab().view, TabView::AiChat(_)) {
            return false;
        }
        if self.tabs().active_tab().is_reusable() {
            return true;
        }
        let Some(i) = self.tabs().reusable_main_flat_index() else {
            return false;
        };
        self.tabs_mut().select(i);
        self.size_visible_groups();
        self.tabs().active_tab().is_reusable()
    }

    fn honor_same_tab(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            return false;
        };
        self.honor_same_tab_at(ws, root, reveal)
    }

    fn honor_same_tab_at(
        &mut self,
        ws: &WorkspaceDoc,
        root: NodeId,
        reveal: Option<NodeId>,
    ) -> bool {
        if !self.tabs().active_tab().is_reusable() {
            // **The target, not the tab we are on.** `active_document` is the
            // store id of the pin itself; looking that up always found the pin
            // and every SameTab pill after pinning was a no-op — Ctrl+click
            // included, once Jump List stole focus and cleared the Ctrl bit.
            if let Some(i) = self.tabs().showing(root) {
                Self::log_open(root, "already-showing", "same-tab");
                koru_ui::keys::debug::write(format_args!(
                    "[Koru][input] view same-tab already-showing i={i} pinned={}",
                    self.tabs().active_tab().is_pinned(),
                ));
                self.record_jump();
                self.focus_tab(i);
                if let Some(target) = reveal {
                    self.tabs_mut().active_view().reveal(target);
                }
                return true;
            }
            Self::log_open(root, "pin-redirect", "same-tab");
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] view same-tab pin-redirect pinned={}",
                self.tabs().active_tab().is_pinned(),
            ));
            return self.push_document_tab(ws, root, reveal, None);
        }
        self.replace_this_tab(ws, root, reveal)
    }

    /// `match_text` is a search landing's cue: `Some` reveals the row the
    /// way a search result is revealed, flashed, and `Some("")` flashes the
    /// whole row — a task followed from the Tasks page.
    fn push_document_tab(
        &mut self,
        ws: &WorkspaceDoc,
        root: NodeId,
        reveal: Option<NodeId>,
        match_text: Option<&str>,
    ) -> bool {
        self.record_jump();
        let name = koru_workspace::name_of(ws, root);
        let active = self.active_document;
        let view = self.document_view(ws, root, active);
        let home = self.current_workspace();
        self.tabs_mut()
            .push(Tab::document_at(view, &name, active, home));
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        if let Some(target) = reveal {
            match match_text {
                Some(text) => {
                    self.tabs_mut()
                        .active_view()
                        .reveal_search_match(target, text);
                }
                None => {
                    self.tabs_mut().active_view().reveal(target);
                }
            }
        }
        true
    }

    fn focus_tab(&mut self, i: usize) {
        if i != self.tabs().active() {
            let _ = self.select(i);
        } else {
            self.size_visible_groups();
        }
    }

    fn log_open(node: NodeId, branch: &'static str, route: &'static str) {
        tracing::info!(
            target: "koru::document_link",
            target = %node.to_storage(),
            branch,
            route,
            "document link opened"
        );
    }

    /// The pane a document dragged out of the column would land in: the slot
    /// marker over a lane, the split preview at a view's edge, nothing in the
    /// dead centre — the group itself is the answer there. Shares the tab
    /// drag's two state slots so the drawing answers both drags identically.
    /// Only a single dragged root can land: a multi-select drop stays the
    /// tree's move. Clears both states the moment the drag leaves the panes
    /// or was never armed. Over a pane the tree's carried ghost also goes
    /// away — the drop there opens the document, so the ghost's "move" would
    /// be lying.
    pub(super) fn update_explore_drop(&mut self, x: f32, y: f32) {
        let armed = self.explore.drag_armed() && self.explore.dragged_roots().len() == 1;
        let target = if armed { self.pane_at(x, y) } else { None };
        self.explore.set_carried_hidden(target.is_some());
        let Some(target) = target else {
            self.split_intent = None;
            self.drop_slot = None;
            return;
        };
        if let Some(lane) = self.lane_rect_for(&target) {
            if lane.contains(x, y) {
                self.split_intent = None;
                self.drop_slot = self
                    .panes
                    .group(&target)
                    .and_then(|g| g.drop_slot(lane, x))
                    .map(|(i, _)| (target, i));
                return;
            }
        }
        self.drop_slot = None;
        if !self.content().contains(x, y) {
            self.split_intent = None;
            return;
        }
        let view_rect = self
            .panes
            .view_rect_for(self.content(), &target)
            .unwrap_or_else(|| self.content());
        self.split_intent = super::panes::resolve_split_intent(
            view_rect,
            (x, y),
            &target,
            self.split_intent.as_ref(),
        );
    }

    /// Land a document dragged out of the explore column in a pane: at the
    /// strip slot the marker showed, as the split the preview showed, or as
    /// the group's new tab. The node stays in the tree — this is an opening,
    /// not a move. The same landing serves a tab dragged in from **another
    /// window** (`foreign_tab_drop`): the node is that tab's document root.
    pub(super) fn drop_document_into_pane(
        &mut self,
        ws: &WorkspaceDoc,
        node: NodeId,
        x: f32,
        y: f32,
        slot: Option<(super::panes::PanePath, usize)>,
        intent: Option<super::panes::SplitIntent>,
    ) -> bool {
        let target = intent
            .as_ref()
            .map(|i| i.target.clone())
            .or_else(|| slot.as_ref().map(|(p, _)| p.clone()))
            .or_else(|| self.pane_at(x, y))
            // A pane that went mid-drag is no landing place.
            .filter(|t| self.panes.contains_group(t));
        let Some(target) = target else {
            return false;
        };
        let Some((root, reveal)) = Self::resolve_target(ws, node) else {
            // The row named a manifest document this workspace does not have
            // loaded: the open goes through the store and there is no tab to
            // move yet. Park the placement — the pane, the slot, the split —
            // for the landing to honour, and pin the landing to this window:
            // the pane paths are this window's. A row the manifest does not
            // hold keeps the plain open.
            let document = self.entries.as_ref().and_then(|entries| {
                entries
                    .iter()
                    .find(|e| e.id.as_uuid() == node.as_uuid())
                    .map(|e| e.id)
            });
            let Some(document) = document else {
                return self.open_node(ws, node);
            };
            // A second drop of the same document supersedes the first.
            self.pending_pane_drops.retain(|p| p.document != document);
            self.pending_pane_drops.push(super::panes::PendingPaneDrop {
                document,
                target: target.clone(),
                slot: slot.map(|(_, i)| i),
                split: intent.map(|i| i.dir),
            });
            self.panes.set_focus(target);
            return self.request_store_document_here(node, true);
        };
        self.panes.set_focus(target.clone());
        // Always a new tab: the click path's working-tab reuse would replace
        // what the pane is showing, which a deliberate drop must never do.
        if !self.push_document_tab(ws, root, reveal, None) {
            return false;
        }
        if let Some(intent) = intent {
            let id = self.tabs().active_tab().id;
            if let Some(tab) = self.take_tab_from_group(&target, id) {
                let doc = tab.document;
                self.panes.split(&target, intent.dir, tab);
                self.settle_pane_drop(doc);
            }
            return true;
        }
        if let Some((_, i)) = slot {
            let active = self.tabs().active();
            if i != active {
                self.move_tab(active, i);
            }
        }
        true
    }

    /// Land the tab a store-routed explore drop was waiting on, where the drop
    /// put it: the split, the lane slot, or simply the pane. Runs on the
    /// arrival the drop named and no other; a pane that went while the
    /// document loaded leaves the tab where `land_on` put it.
    pub(super) fn settle_pending_pane_drop(&mut self) {
        let Some(active) = self.active_document else {
            return;
        };
        let Some(i) = self
            .pending_pane_drops
            .iter()
            .position(|p| p.document == active)
        else {
            return;
        };
        let pending = self.pending_pane_drops.remove(i);
        if !self.panes.contains_group(&pending.target) {
            return;
        }
        // The landing pushed or selected the tab in the focused group; the
        // drop said where it belongs. Only a tab actually showing the arrived
        // document may move — a landing that reused another document's tab is
        // not this arrival's tab.
        let source = self.panes.focused_path().clone();
        if self.tabs().active_tab().document != Some(pending.document) {
            return;
        }
        let id = self.tabs().active_tab().id;
        if let Some(dir) = pending.split {
            if let Some(tab) = self.take_tab_from_group(&source, id) {
                let doc = tab.document;
                self.panes.split(&pending.target, dir, tab);
                self.settle_pane_drop(doc);
            }
            return;
        }
        // A plain landing in the right pane needs no surgery.
        if source == pending.target && pending.slot.is_none() {
            return;
        }
        if let Some(tab) = self.take_tab_from_group(&source, id) {
            let doc = tab.document;
            match pending.slot {
                Some(slot) => self.panes.insert_tab_at(&pending.target, tab, slot),
                None => self.panes.insert_into_group(&pending.target, tab),
            }
            self.settle_pane_drop(doc);
        }
    }

    // ------------------------------------------------------ the cross-window drag
    //
    // A tab dragged out of another window's strip keeps its OS capture with
    // the source, so the pointer never arrives here as an event — the shell
    // asks these four instead. The preview is the same two states a local
    // drag drives, so the drawing answers both identically.

    /// The document an in-flight pane drag is carrying, if it is one a window
    /// may carry off: shell and agent tabs are window-local and answer
    /// nothing, the pop-out rule.
    pub(crate) fn pane_drag_document(&self) -> Option<NodeId> {
        let drag = self.pane_drag.as_ref()?;
        if !self.pane_drag_poppable(drag) {
            return None;
        }
        self.panes
            .group(&drag.source)?
            .iter_tabs()
            .find(|t| t.id == drag.tab_id)?
            .view
            .root()
    }

    /// Preview where a tab dragged from another window would land at `(x,
    /// y)`: the lane slot or the edge split, with no source strip to exclude.
    /// Answers whether the point is over a pane that can take the document at
    /// all — over the caption's window buttons or another window's dead space
    /// the answer is no, and the source keeps its pop-out.
    pub(crate) fn foreign_tab_hover(
        &mut self,
        ws: &WorkspaceDoc,
        doc: NodeId,
        x: f32,
        y: f32,
    ) -> bool {
        if Self::resolve_target(ws, doc).is_none() {
            return false;
        }
        let Some(target) = self.pane_at(x, y) else {
            self.foreign_tab_leave();
            return false;
        };
        if let Some(lane) = self.lane_rect_for(&target) {
            if lane.contains(x, y) {
                self.split_intent = None;
                self.drop_slot = self
                    .panes
                    .group(&target)
                    .and_then(|g| g.drop_slot(lane, x))
                    .map(|(i, _)| (target, i));
                return true;
            }
        }
        self.drop_slot = None;
        if !self.content().contains(x, y) {
            self.split_intent = None;
            return false;
        }
        let view_rect = self
            .panes
            .view_rect_for(self.content(), &target)
            .unwrap_or_else(|| self.content());
        self.split_intent = super::panes::resolve_split_intent(
            view_rect,
            (x, y),
            &target,
            self.split_intent.as_ref(),
        );
        true
    }

    /// Clear the preview a foreign hover set — the drag moved on, or ended.
    /// Answers whether anything was showing, for the redraw question.
    pub(crate) fn foreign_tab_leave(&mut self) -> bool {
        let had = self.split_intent.is_some() || self.drop_slot.is_some();
        self.split_intent = None;
        self.drop_slot = None;
        had
    }

    /// Land the tab another window was dragging at `(x, y)` — the drop the
    /// hover previewed, recomputed from the release point so a stale preview
    /// cannot misplace it.
    pub(crate) fn foreign_tab_drop(
        &mut self,
        ws: &WorkspaceDoc,
        doc: NodeId,
        x: f32,
        y: f32,
    ) -> bool {
        if !self.foreign_tab_hover(ws, doc, x, y) {
            return false;
        }
        let slot = self.drop_slot.take();
        let intent = self.split_intent.take();
        self.drop_document_into_pane(ws, doc, x, y, slot, intent)
    }

    /// The source half of a drop that landed in another window: commit the
    /// take the swallowed release would have committed — out of the group,
    /// settled — and no pop-out: the tab already has its window.
    pub(crate) fn complete_foreign_drop(&mut self) {
        let Some(drag) = self.pane_drag.take() else {
            return;
        };
        self.split_intent = None;
        self.drop_slot = None;
        self.foreign_targeted = false;
        if let Some(tab) = self.take_tab_from_group(&drag.source, drag.tab_id) {
            let doc = tab.document;
            self.settle_pane_drop(doc);
        }
    }

    /// The shell's answer to "is another window accepting this drag right
    /// now" — it is the only one that knows. While that is true the pop-out
    /// pill stays down: its promise is false over a receiving window, and the
    /// target's preview is the one speaking. Answers whether it changed.
    pub(crate) fn set_foreign_targeted(&mut self, targeted: bool) -> bool {
        std::mem::replace(&mut self.foreign_targeted, targeted) != targeted
    }
}
