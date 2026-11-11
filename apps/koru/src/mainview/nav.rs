// SPDX-License-Identifier: MPL-2.0

//! Where the window is standing, and how it gets back.
//!
//! History and the pin-safe Store swap share one [`Spot`]. A tab click that
//! must reload a document waits on [`MainView::tab_pending`], not
//! [`MainView::history_pending`] — one waiter serving both is how a restore
//! lands on the wrong document.

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;

use super::MainView;
use super::nav_debug::nav_debug;
use super::tab::TabView;
use super::tab_rules::KoruTabs;
use koru_editor::Editing;

/// One place in the workspace, as back and forward understand it.
///
/// Node ids, not indexes: a tab index moves when a tab left of it closes, and a
/// row index moves when anything above it is edited. Both would silently name
/// something else by the time you pressed back.
#[derive(Copy, Clone, PartialEq, Debug)]
pub(super) struct Spot {
    /// Route to the workspace that owns this place. History crosses workspace
    /// swaps, so the document id alone is not enough to reopen it.
    pub profile: Option<koru_core::ProfileId>,
    pub workspace: Option<koru_core::WorkspaceId>,
    /// Stable identity in a Store-backed workspace. A document's root is a
    /// node inside that document and has a different UUID, so it cannot be
    /// looked up in the manifest as though it were the `DocumentId`.
    pub document: Option<koru_core::DocumentId>,
    /// Which tab instance this place was in. Two tabs on the same document are
    /// different stops; Back should return to that tab, not rewrite the current
    /// one.
    pub tab: u64,
    /// **The Tasks page this stop was on**, or `None` for a document row.
    ///
    /// A Tasks tab was not a place at all, and that is what made the journey
    /// one-way: following a task opened its document, and neither Back nor
    /// Forward could return to the queue you came from, because nothing had
    /// been written down. The queue *is* the place — not the tab, which
    /// re-points, and not a row, which the page refetches every half minute.
    ///
    /// The row and caret fields below are the document's and are ignored on a
    /// Tasks stop; `doc` and `row` carry the tab's own id there, so every
    /// existing comparison on them is still answering about *this* stop and
    /// never accidentally about a document.
    pub tasks: Option<crate::panes::explore::rail::RailTarget>,
    /// **The run this stop was on**, or `None` for anything else.
    ///
    /// The same shape as `tasks`, and for the same reason: a run tab is a
    /// place — you got to it by pressing a pill in a document — but it holds
    /// no document and no row, so nothing about the fields below applies to
    /// it. Without this, clicking a run pill left the window standing
    /// somewhere history had never heard of: Back skipped past the row the
    /// pill was on to whatever came before it, and Forward could not return
    /// to the run at all.
    ///
    /// `doc` and `row` carry the tab's own id here, exactly as they do on a
    /// Tasks stop, so `history.last() == Some(&spot)` still tells two run
    /// tabs apart.
    pub run: Option<koru_delegate_core::RunId>,
    /// **The chat this stop was on**, or `None` for anything else (koru-ai
    /// D102).
    ///
    /// The run's shape, for the run's reason: a chat tab is a place — you got
    /// to it from the Chats list, with `Ctrl+Shift+↑`/`↓`, or by leaving it
    /// for a document a reply linked — but it holds no document and no row.
    /// Without this a chat stopped existing as far as Back and Forward were
    /// concerned: leaving one recorded nothing, so Back from the document a
    /// reply opened skipped past the chat, and Forward could never return to
    /// one. `doc` and `row` carry the tab's own id, as on a run stop, and
    /// `scroll` is the conversation's.
    pub chat: Option<koru_ai_ui::ChatId>,
    /// The document's root — which document this is.
    pub doc: NodeId,
    /// The row inside it the caret was on.
    pub row: NodeId,
    /// Byte offset of the caret in that row. `reveal` always places at 0, so
    /// this is written back afterwards.
    pub caret: usize,
    /// A live text selection on that row, or `None` when the caret is collapsed.
    pub selection: Option<(usize, usize)>,
    /// Where it was scrolled to. `reveal` only scrolls *minimally*, so without
    /// this a long document comes back at a different offset than you left it.
    pub scroll: f32,
}

/// How many places back you can go. Deep enough to cover a session's wandering,
/// shallow enough that the list is not a second copy of the document tree.
const HISTORY_CAP: usize = 50;

impl MainView {
    /// Where the window is standing right now, if that is a place at all.
    ///
    /// `None` over a welcome tab or a draft: neither is somewhere you can be
    /// sent back to, and recording one would make Back a press that appears to
    /// do nothing.
    pub(super) fn here(&self) -> Option<Spot> {
        self.here_for_tab(self.tabs().active())
    }

    /// The same question about tab `i`, whether or not it is showing.
    pub(super) fn here_for_tab(&self, i: usize) -> Option<Spot> {
        let route = self
            .pane_context
            .as_ref()
            .and_then(|(_, profile, _, workspace)| {
                workspace.map(|workspace| (*profile, workspace))
            });
        let tab = self.tabs().get(i)?;
        if tab.draft.is_some() {
            return None;
        }
        // **A Tasks page is a place, and its identity is its target.** Taken
        // before `root()`, which a Tasks tab has none of — that `?` is exactly
        // where the queue you were reading used to be forgotten.
        if let TabView::Tasks(page) = &tab.view {
            // The tab's own id stands in for the document node here. It is
            // never read as a node on this arm (`restore_spot` branches on
            // `tasks` first), and giving it a *distinct* value per tab is what
            // keeps `history.last() == Some(&spot)` honest about two Tasks tabs
            // being two places.
            let stand_in = NodeId::from_uuid(uuid::Uuid::from_u64_pair(0, tab.id));
            return Some(Spot {
                profile: route.map(|route| route.0),
                workspace: route.map(|route| route.1),
                document: None,
                tab: tab.id,
                tasks: Some(page.target()),
                run: None,
                chat: None,
                doc: stand_in,
                row: stand_in,
                caret: 0,
                selection: None,
                scroll: tab.view.scroll(),
            });
        }
        // **A run tab is a place too**, and taken here for the same reason the
        // Tasks arm is: it has no `root()`, so without this arm the `?` below
        // is where a run the person had just opened stopped existing as far as
        // Back and Forward were concerned. A run that has not answered yet
        // names none, and is not somewhere to be sent back to.
        if let TabView::Run(view) = &tab.view {
            let run = view.run()?;
            let stand_in = NodeId::from_uuid(uuid::Uuid::from_u64_pair(0, tab.id));
            return Some(Spot {
                profile: route.map(|route| route.0),
                workspace: route.map(|route| route.1),
                document: None,
                tab: tab.id,
                tasks: None,
                run: Some(run),
                chat: None,
                doc: stand_in,
                row: stand_in,
                caret: 0,
                selection: None,
                scroll: tab.view.scroll(),
            });
        }
        // A Reminders page is a place too, on the Tasks arm's terms: its
        // identity is the rail's Reminders target.
        if matches!(tab.view, TabView::Reminders(_)) {
            let stand_in = NodeId::from_uuid(uuid::Uuid::from_u64_pair(0, tab.id));
            return Some(Spot {
                profile: route.map(|route| route.0),
                workspace: route.map(|route| route.1),
                document: None,
                tab: tab.id,
                tasks: Some(crate::panes::explore::rail::RailTarget::Reminders),
                run: None,
                chat: None,
                doc: stand_in,
                row: stand_in,
                caret: 0,
                selection: None,
                scroll: tab.view.scroll(),
            });
        }
        // **A chat tab is a place** (D102), on the run arm's terms: its
        // identity is its chat. A new chat not sent yet names none and is not
        // somewhere to be sent back to — a draft's rule.
        if let TabView::AiChat(view) = &tab.view {
            let chat = view.chat_id()?;
            let stand_in = NodeId::from_uuid(uuid::Uuid::from_u64_pair(0, tab.id));
            return Some(Spot {
                profile: route.map(|route| route.0),
                workspace: route.map(|route| route.1),
                document: None,
                tab: tab.id,
                tasks: None,
                run: None,
                chat: Some(chat),
                doc: stand_in,
                row: stand_in,
                caret: 0,
                selection: None,
                scroll: view.conversation_scroll(),
            });
        }
        let doc = tab.view.root()?;
        let TabView::Leaf(leaf) = &tab.view else {
            return None;
        };
        let focused = leaf.body().focused();
        let caret = focused.map(|row| row.caret()).unwrap_or(0);
        let selection = focused.and_then(|row| {
            let range = row.range();
            (!range.is_empty()).then_some((range.start, range.end))
        });
        // **The tab's own workspace, not the window's.** A tab is kept across
        // a workspace switch (2026-11-10), so the document in it can live in
        // a workspace other than the one showing; a stop labelled with the
        // window's would send Back to the wrong place.
        Some(Spot {
            profile: route.map(|route| route.0),
            workspace: tab.home.or(route.map(|route| route.1)),
            document: tab.document,
            tab: tab.id,
            tasks: None,
            run: None,
            chat: None,
            doc,
            row: leaf.body().focused_id().unwrap_or(doc),
            caret,
            selection,
            scroll: tab.view.scroll(),
        })
    }

    /// Note where we were, because we are about to be somewhere else.
    ///
    /// Called at the top of every jump. The **forward branch is dropped**: once
    /// you go back and then somewhere new, the way you originally came is no
    /// longer reachable — the browser rule, and the one that keeps this a
    /// history rather than a graph.
    pub(super) fn record_jump(&mut self) {
        if self.restoring {
            return;
        }
        let Some(spot) = self.here() else { return };
        let rows = match &self.tabs().active_tab().view {
            TabView::Leaf(l) => l.body().rows.len(),
            _ => 0,
        };
        nav_debug!(
            "[koru:nav] tab-leave tab={} doc={} row={} caret={} scroll={} rows={rows}",
            spot.tab,
            spot.doc.to_storage(),
            spot.row.to_storage(),
            spot.caret,
            spot.scroll as i32,
        );
        self.history.truncate(self.history_at);
        // Standing still is not a jump. Pressing a pill that opens the tab you
        // are already on would otherwise fill the list with one place.
        if self.history.last() == Some(&spot) {
            self.history_at = self.history.len();
            return;
        }
        self.history.push(spot);
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.history_at = self.history.len();
    }

    /// Put the window back on `spot`. `false` if it no longer names anything —
    /// the document was deleted, the row inside it was, or it has since been
    /// moved to the trash. A trashed document still *reads*, so `read` alone
    /// would walk Back into the bin; `step_history` already skips rather than
    /// stops, so refusing here is all it takes.
    pub(super) fn restore_spot(&mut self, ws: &WorkspaceDoc, spot: Spot) -> bool {
        // **A Tasks stop is answered here and goes no further.** Everything
        // below is about finding a document and revealing a row in it, and a
        // queue has neither. `open_tasks_tab` is the one door: it re-points the
        // Tasks tab in front, or opens one when the tab this stop was made in
        // has since been closed — which is the same forgiveness the document
        // arm shows when its tab has gone.
        if let Some(target) = spot.tasks {
            self.restoring = true;
            let found = self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|t| t.id == spot.tab)
                    .map(|i| (path, i))
            });
            if let Some((path, i)) = found {
                self.panes.set_focus(path);
                self.tabs_mut().select(i);
                self.size_visible_groups();
                self.ensure_tab_visible(i);
            }
            let opened = self.open_tasks_tab(target, false);
            self.tabs_mut().active_view().set_scroll(spot.scroll);
            self.restoring = false;
            return opened;
        }
        // **A run stop is answered here and goes no further**, for the reason
        // the Tasks arm gives: there is no document to find and no row to
        // reveal. `open_run_tab` is the one door, and it already focuses the
        // tab this stop was made in or opens one when it has been closed —
        // the same forgiveness the document arm shows. `restoring` is set, so
        // the open it performs records no jump of its own.
        if let Some(run) = spot.run {
            self.restoring = true;
            let opened = self.open_run_tab(run);
            if opened {
                self.tabs_mut().active_view().set_scroll(spot.scroll);
            }
            self.restoring = false;
            return opened;
        }
        // **A chat stop is answered here too** (D102): no document, no row.
        if let Some(chat) = spot.chat {
            // **Only this profile's chats** (AI schema 13): a stop made
            // before the window switched profile names a chat of another,
            // which never opens here — Back and Forward step over it.
            if spot.profile.is_some() && spot.profile != self.current_profile_id() {
                return false;
            }
            self.restoring = true;
            let opened = self.restore_chat_spot(chat, spot.tab, spot.scroll);
            self.restoring = false;
            return opened;
        }
        if spot.tab != 0 {
            let found = self.tabs().iter().position(|t| t.id == spot.tab);
            if let Some(i) = found {
                self.restoring = true;
                if i != self.tabs().active() {
                    self.tabs_mut().select(i);
                    self.size_visible_groups();
                    self.ensure_tab_visible(i);
                }
                let tab_doc = self.tabs().active_tab().document;
                let same_document = match (spot.document, tab_doc) {
                    (Some(want), Some(have)) => want == have,
                    _ => self.tabs_mut().active_view().root() == Some(spot.doc),
                };
                if same_document {
                    if let Some(doc) = tab_doc
                        && self.active_document != Some(doc)
                    {
                        // **Routed when the stop is in another workspace**,
                        // as the branch below decides it: the shell switches
                        // first and opens the target a second time, and that
                        // second open must not count as a new jump. Before
                        // 2026-11-10 a switch closed every tab, so a Back
                        // across workspaces never found its tab here.
                        let crosses = spot.profile.zip(spot.workspace).is_some_and(|route| {
                            self.pane_context
                                .as_ref()
                                .and_then(|(_, profile, _, workspace)| {
                                    workspace.map(|workspace| (*profile, workspace))
                                })
                                != Some(route)
                        });
                        self.history_pending = Some(spot);
                        self.history_pending_routed = crosses;
                        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                            target: koru_workspace_host::NavigationTarget::Document {
                                document: doc,
                                node: None,
                            },
                            match_text: None,
                            disposition:
                                koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                        });
                        return true;
                    }
                    if ws.read(spot.doc).is_ok() && !ws.in_trash(spot.doc) {
                        self.apply_spot_view(spot);
                    }
                    self.restoring = false;
                    return true;
                }
                self.restoring = false;
            }
        }
        let route = spot.profile.zip(spot.workspace);
        let current_route = self
            .pane_context
            .as_ref()
            .and_then(|(_, profile, _, workspace)| {
                workspace.map(|workspace| (*profile, workspace))
            });
        if let Some((profile, workspace)) = route
            && current_route != Some((profile, workspace))
        {
            let Some(document) = spot.document else {
                return false;
            };
            self.restoring = true;
            self.history_pending = Some(spot);
            self.history_pending_routed = true;
            self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                target: koru_workspace_host::NavigationTarget::Document {
                    document,
                    node: None,
                },
                match_text: None,
                disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            });
            return true;
        }
        // Under the Store, `ws` is only the document currently under the
        // caret. A history entry for another document therefore cannot resolve
        // here; validate it against the live manifest listing and ask the shell
        // to load it. The exact row and scroll are applied when that load is
        // pushed back through `show_active_document`.
        if self.entries.is_some() {
            let Some(document) = self.entries.as_ref().and_then(|entries| {
                spot.document
                    .and_then(|document| entries.iter().find(|entry| entry.id == document))
                    // Compatibility for the in-memory harness, where a
                    // manifest row is synthesized from the root UUID.
                    .or_else(|| {
                        entries
                            .iter()
                            .find(|entry| entry.id.as_uuid() == spot.doc.as_uuid())
                    })
                    .map(|entry| entry.id)
            }) else {
                return false;
            };
            if self.active_document != Some(document) {
                self.restoring = true;
                self.history_pending = Some(spot);
                self.history_pending_routed = false;
                self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                    target: koru_workspace_host::NavigationTarget::Document {
                        document,
                        node: None,
                    },
                    match_text: None,
                    disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                });
                return true;
            }
        }
        if ws.read(spot.doc).is_err() || ws.in_trash(spot.doc) {
            return false;
        }
        self.restoring = true;
        let ok = self.open_node(ws, spot.doc);
        if ok {
            // `reveal` scrolls only as far as it has to, so the offset is put
            // back explicitly — otherwise a long document comes back at a
            // different place than you left it.
            self.apply_spot_view(spot);
        }
        self.restoring = false;
        ok
    }

    /// Back on a chat: the tab the stop was made in while it still shows
    /// the chat, else any tab on it, else a tab opened on it again — the
    /// forgiveness every other arm shows a closed tab.
    ///
    /// **A tab still open is only brought forward.** Its surface kept its own
    /// scroll, and whether it was following a reply as it streamed; putting
    /// back the scroll recorded on the way out would pin a reader the reply
    /// had since carried on below. Only a chat opened again is given the
    /// recorded scroll, applied when its read lands. Nothing here stops or
    /// touches a running reply: the tab is selected, never reseated.
    fn restore_chat_spot(&mut self, chat: koru_ai_ui::ChatId, tab: u64, scroll: f32) -> bool {
        let on_chat = |candidate: &super::tab::Tab| matches!(&candidate.view, TabView::AiChat(view) if view.chat_id() == Some(chat));
        let find = |want: &dyn Fn(&super::tab::Tab) -> bool| {
            self.panes.iter_groups().find_map(|(path, group)| {
                group
                    .tabs()
                    .iter()
                    .position(|candidate| want(candidate))
                    .map(|index| (path, index))
            })
        };
        let found =
            find(&|candidate| candidate.id == tab && on_chat(candidate)).or_else(|| find(&on_chat));
        if let Some((path, index)) = found {
            self.panes.set_focus(path);
            let selected = self.select(index);
            self.ensure_tab_visible(index);
            return selected;
        }
        // Deleted since: skipped, as a deleted document is.
        let listed = self.explore.chats_mut().chats();
        if !listed.is_empty() && !listed.iter().any(|listed| listed.id == chat) {
            return false;
        }
        if !self.open_ai_chat_tab_for(chat) {
            return false;
        }
        if let TabView::AiChat(view) = self.tabs_mut().active_view() {
            view.restore_conversation_scroll(scroll);
        }
        true
    }

    pub(super) fn apply_spot_view(&mut self, spot: Spot) {
        nav_debug!(
            "[koru:nav] apply-spot tab={} doc={} row={} caret={} scroll={}",
            spot.tab,
            spot.doc.to_storage(),
            spot.row.to_storage(),
            spot.caret,
            spot.scroll as i32,
        );
        self.tabs_mut().active_view().reveal(spot.row);
        self.tabs_mut()
            .active_view()
            .restore_insertion(spot.caret, spot.selection);
        self.tabs_mut().active_view().set_scroll(spot.scroll);
    }

    /// If a pin-safe tab click is waiting for this document, reseat and restore.
    pub(super) fn apply_tab_pending(&mut self, ws: &WorkspaceDoc) {
        let Some(spot) = self.tab_pending else {
            return;
        };
        let matches = match (spot.document, self.active_document) {
            (Some(want), Some(have)) => want == have,
            _ => self.tabs_mut().active_view().root() == Some(spot.doc),
        };
        if !matches {
            return;
        }
        self.tab_pending = None;
        self.reseat_active_if_needed(ws);
        self.apply_spot_view(spot);
    }

    /// Attach a fresh tracker and rebuild from the document now loaded.
    ///
    /// The previous tracker was bound to a `WorkspaceDoc` the window dropped
    /// when it navigated away. Skip if this leaf's root is still not in `ws`.
    ///
    /// **`Body::catch_up` deliberately, not `Editing::catch_up`** — the opposite
    /// of the rule `TabView::catch_up` follows. That one settles a view whose
    /// rows changed *underneath* it, and re-anchoring an open picker against the
    /// merged document is part of settling (ADR-343-overlaycatchup). This is not
    /// that: `resubscribe` has just marked everything pending because the
    /// document was **replaced**, and a picker's Loro cursors belong to the
    /// `WorkspaceDoc` the window no longer holds. Resolving them here would ask
    /// a stranger where somebody's typing went. The caller restores the caret
    /// itself (`apply_spot_view`), which is the other half of why this path
    /// arranges its own settling.
    pub(super) fn reseat_active_if_needed(&mut self, ws: &WorkspaceDoc) {
        let TabView::Leaf(leaf) = self.tabs_mut().active_view() else {
            return;
        };
        let Some(root) = leaf.body().root else {
            return;
        };
        if ws.read(root).is_err() {
            return;
        }
        leaf.body_mut().resubscribe(ws);
        leaf.body_mut().catch_up(ws);
        leaf.reflow();
    }

    /// Step `delta` places through the history — `-1` back, `+1` forward.
    ///
    /// A spot whose document has gone is **skipped**, not stopped on, and the
    /// walk carries on in the same direction: a deleted document must not become
    /// a wall you cannot press past.
    pub(super) fn step_history(&mut self, ws: &WorkspaceDoc, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        // Going back from the newest place means the place we are standing has
        // to be in the list first, or there is nothing to come *forward* to.
        if delta < 0 && self.history_at == self.history.len() {
            if let Some(spot) = self.here() {
                if self.history.last() != Some(&spot) {
                    self.history.push(spot);
                }
                self.history_at = self.history.len() - 1;
            } else {
                // A draft is intentionally not a place, but Ctrl+T recorded
                // the real page immediately before it. Start just beyond that
                // entry so Back can leave the draft without ever putting its
                // private node id in history.
                self.history_at = self.history.len();
            }
        }
        let step = if delta < 0 { -1 } else { 1 };
        let mut at = self.history_at as isize;
        loop {
            at += step;
            if at < 0 || at as usize >= self.history.len() {
                return false;
            }
            let spot = self.history[at as usize];
            if self.restore_spot(ws, spot) {
                self.history_at = at as usize;
                return true;
            }
            // Gone. Skipped rather than removed: the walk carries on in the same
            // direction, and leaving the entry costs one comparison next time
            // while removing it would shift every index including the cursor's.
        }
    }
}

#[cfg(test)]
mod chat_tests;

#[cfg(test)]
mod tests {
    use super::super::harness::*;
    use super::*;
    use crate::leaf::Leaf;
    use crate::mainview::tab::Tab;
    use koru_core::WorkspaceId;
    use koru_editor::Editing;
    use koru_workspace_host::View;

    #[test]
    fn here_for_tab_matches_here_on_the_active_tab() {
        let (v, _w) = on_document();
        assert_eq!(v.here(), v.here_for_tab(v.tabs().active()));
    }

    #[test]
    fn catch_up_against_a_stranger_does_not_wipe_the_pin() {
        let (mut v, mut w) = on_document();
        fill(&v, &mut w.ws, 3);
        v.adopt(&w.ws);
        v.size_visible_groups();
        assert!(pin_active_document(&mut v));
        let TabView::Leaf(leaf) = v.tabs_mut().active_view() else {
            panic!("document tab");
        };
        let rows = leaf.body().rows.len();
        let focused = leaf.body().focused_id();
        assert!(rows > 1, "the pin has a body");

        let stranger = WorkspaceDoc::create(WorkspaceId::new(), "other").unwrap();
        v.catch_up_active(&stranger);

        let TabView::Leaf(leaf) = v.tabs_mut().active_view() else {
            panic!("document tab");
        };
        assert_eq!(leaf.body().rows.len(), rows, "the cache was not wiped");
        assert_eq!(leaf.body().focused_id(), focused);
    }

    #[test]
    fn tab_select_across_a_store_swap_restores_the_spot() {
        let (mut v, mut w) = on_document();
        fill(&v, &mut w.ws, 3);
        v.adopt(&w.ws);
        v.size_visible_groups();
        assert!(pin_active_document(&mut v));
        let a_id = v.tabs().active_tab().document.expect("pinned document");
        v.tabs_mut().active_view().set_scroll(24.0);
        if let TabView::Leaf(leaf) = v.tabs_mut().active_view() {
            Editing::place_caret(leaf, 2);
            leaf.reflow();
        }
        let want = v.here().expect("pin is a place");

        let mut b_ws = WorkspaceDoc::create(WorkspaceId::new(), "b").unwrap();
        let b = a_document(&mut b_ws, "Brief");
        let b_id = koru_core::DocumentId::new();
        v.tabs_mut().push(Tab::document_of(
            Leaf::document(&b_ws, b),
            "Brief",
            Some(b_id),
        ));
        v.active_document = Some(b_id);

        assert!(v.select(0));
        assert!(v.tab_pending.is_some(), "the pin is waiting for A");
        v.catch_up_active(&b_ws);
        let TabView::Leaf(leaf) = v.tabs_mut().active_view() else {
            panic!("pin");
        };
        assert!(
            !leaf.body().rows.is_empty(),
            "catch-up against B did not wipe A"
        );

        assert!(View::show_active_document(&mut v, &w.ws, Some(a_id)));
        assert!(v.tab_pending.is_none());
        let got = v.here().expect("restored");
        assert_eq!(got.scroll, want.scroll);
        assert_eq!(got.caret, want.caret);
        assert_eq!(got.row, want.row);
    }
}
