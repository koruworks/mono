// SPDX-License-Identifier: MPL-2.0

//! What the container does about the Tasks tab.
//!
//! The Trash tab's `mainview/trash.rs`, for the same reason: the view reports a
//! gesture and performs none of it, and everything here needs something a view
//! cannot hold — a tab set, the store client, or a `&mut WorkspaceDoc` for a
//! document this window may not even have open.
//!
//! ## Where a task mutation is written
//!
//! **Not in the tab, and not through the tab's document — through the task's
//! own.** A task lives in whichever document it was typed in, so ticking one on
//! this list is a write into a document nobody is looking at. `Workspace` knows
//! how to park such a document (`ensure_document`, and D-3's `create_native_task`
//! before it), and [`Workspace::run_task_command`] is the door: it parks, runs
//! the canonical command, publishes immediately, and hands back the
//! [`TaskOutcome`] so the activity attempt happens on the ordinary drain.
//!
//! Two of the six menu rows deliberately do *not* come through here.
//! **Due… and Priority… open the node and then open the editor's own picker
//! over it** — the same *go there and act* shape an Explore row's *Due…*
//! already has (`pending_document_attrs`). A second date grid and a second
//! priority list living in the container would be two more places for the
//! rules to drift, and the picker is a component of the editor rather than of
//! the menu.
//!
//! [`Workspace::run_task_command`]: koru_workspace::Workspace::run_task_command
//! [`TaskOutcome`]: koru_core::tasks::TaskOutcome

use crate::panes::explore::rail::{Bucket, RailTarget};
use crate::tasks_view::query::WriteRequirement;
use koru_core::{DocumentId, NodeId};
use koru_workspace::Workspace;
use koru_workspace_host::tasks::TaskCommand;

use super::MainView;
use super::reminders_view;
use super::tab::{Tab, TabView};

/// A queued command, plus what a confirm needs to say about it.
///
/// The command is the wire vocabulary
/// ([`koru_workspace_host::tasks::TaskCommand`]); the title rides beside it so
/// the E-3 confirm can quote the row without re-fetching a page that has since
/// moved on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct PendingTaskEdit {
    pub command: koru_workspace_host::tasks::TaskCommand,
    pub title: String,
}

/// The queue a Tasks or Reminders tab is showing; `None` for any other tab.
fn tasks_target_of(tab: &Tab) -> Option<RailTarget> {
    match &tab.view {
        TabView::Tasks(t) => Some(t.target()),
        TabView::Reminders(_) => Some(RailTarget::Reminders),
        _ => None,
    }
}

impl MainView {
    /// Open the Tasks tab on `target`, or point the existing one at it.
    ///
    /// **One per window.** The rail holds one selection, so a second Tasks tab
    /// would be a second answer to *which bucket am I looking at* — pressing
    /// another bucket re-points the tab that is already there, which is also
    /// what makes the rail's carding honest.
    /// `new_tab` opens a second one instead of re-pointing.
    ///
    /// **The one-per-window rule was about the rail, and it was one step too
    /// broad.** A plain press still re-points, because the rail has one
    /// selection and two tabs answering *which queue is selected* would be two
    /// answers to one question. Ctrl+click and *Open in new tab* say the
    /// opposite out loud, and a page you asked for by name is not the rail's
    /// answer any more — it is a page you parked. The card follows the tab you
    /// are on, which is what keeps the rail honest with several open.
    ///
    /// Re-pointing prefers the tab **in front** when it is a Tasks or a
    /// Reminders tab — the two are one re-pointable slot whose *view* the
    /// target swaps. With one open the two readings agree; with three,
    /// pressing a queue moves the one you are looking at rather than reaching
    /// past it to whichever was opened first. A swap invalidates the tab's
    /// cached size, the idiom every view swap keeps (`open.rs`'s is the
    /// spelling): the cache belongs to the view that was replaced, and a view
    /// never told a viewport draws nothing.
    pub(super) fn open_tasks_tab(&mut self, target: RailTarget, new_tab: bool) -> bool {
        // **The one door a Tasks or Reminders tab opens by**, so the product
        // flags are answered here once rather than at every press, chord,
        // restore and reveal that reaches it (koru-features).
        if !rail_target_shown(target) {
            return false;
        }
        // **A pinned Tasks tab is kept on its queue**: it is a page somebody
        // parked at the left of the strip, so a press re-points an unpinned
        // Tasks tab (or opens one) and never moves the pin. A press for the
        // queue a pin already shows selects that pin instead of opening a
        // second page on it.
        let repointable =
            |t: &Tab| !t.is_pinned() && matches!(t.view, TabView::Tasks(_) | TabView::Reminders(_));
        let existing = (!new_tab).then(|| {
            self.tabs()
                .iter()
                .position(|t| t.is_pinned() && tasks_target_of(t) == Some(target))
                .or_else(|| {
                    repointable(self.tabs().active_tab())
                        .then(|| self.tabs().active())
                        .or_else(|| self.tabs().iter().position(repointable))
                })
        });
        match existing.flatten() {
            Some(i) => {
                // **Re-pointed before it is selected.** Selecting a tab
                // activates the document it edits, and a Tasks tab still on
                // its old page edits the old page's note — a stale activation
                // the window would have no tab for, so it landed as an
                // untitled document tab (acceptance round 2). Re-pointed
                // first, the tab edits nothing until the shell names the new
                // page's note, and that activation is the one that lands.
                //
                // **One tab, its content swaps.** A bucket press on a tab
                // showing the Reminders page trades the view back to Tasks;
                // the Reminders row on a tasks page trades it the other way.
                // Same variant keeps the view — and its scroll, its chooser,
                // its page state — exactly as re-pointing a queue always did.
                let tab = self.tabs_mut().get_mut(i).expect("found above");
                let swapped = match (target, &mut tab.view) {
                    (RailTarget::Reminders, TabView::Reminders(_)) => false,
                    (RailTarget::Reminders, view) => {
                        *view = TabView::Reminders(reminders_view::RemindersView::new());
                        true
                    }
                    (_, TabView::Tasks(t)) => {
                        t.set_target(target);
                        false
                    }
                    (_, view) => {
                        *view = TabView::Tasks(crate::tasks_view::TasksView::new(target));
                        true
                    }
                };
                if swapped {
                    // The tab's size cache belongs to the view it just
                    // replaced: the new view has never seen a viewport, and
                    // `ensure_sized` would early-return on the old view's
                    // size — leaving the new one laying out at 0×0 forever.
                    tab.invalidate_size();
                }
                self.select(i);
            }
            None => {
                // **Inactive when it is a second tab.** Ctrl+click's whole
                // meaning is *without leaving where I am*, and the library rows
                // beside this one answer the modifier the same way.
                if new_tab && self.tabs().len() > 0 {
                    let i = self.tabs_mut().insert_inactive(Tab::tasks(target));
                    self.slides_mut().clear();
                    self.relayout();
                    self.mark_tab_widths_stale();
                    // Not laid out here: a background tab sizes itself when it
                    // is next shown, which is the rule `relayout` states and
                    // what keeps opening one O(1).
                    let _ = i;
                    return true;
                }
                self.tabs_mut().push(Tab::tasks(target));
                self.slides_mut().clear();
                self.relayout();
            }
        }
        self.tasks_page_in_front(target)
    }

    /// The page on `target` is the active tab now: name it and card the rail.
    fn tasks_page_in_front(&mut self, target: RailTarget) -> bool {
        // **The strip names the page**, so it moves when the page does. This is
        // the pressed half; a list renamed underneath the tab is caught by
        // MainView::refresh_labels, which runs on the tick nobody pressed.
        let tab = self.tabs_mut().active_mut();
        if tab.refresh_tasks_label() {
            self.mark_tab_widths_stale();
        }
        self.size_visible_groups();
        // The rail and the tab hold one answer; this is the half that runs when
        // the tab was restored from a session rather than pressed for.
        self.explore.rail_mut().set_selected(Some(target));
        true
    }

    /// **A Tasks rail row's page, where the place chooser said** — Alt+click
    /// on a queue, a list or the Reminders row (ADR-58). An arm puts a page of
    /// its own in a new pane split off the focused one; the middle opens one
    /// in a new tab, in front. Neither re-points the tab already on a page: a
    /// page you asked to put somewhere is a page you parked, the reason
    /// Ctrl+click opens a second one. The rail cards it, because it is the tab
    /// in front.
    ///
    /// The other dispositions are the rail's own gestures, so they go through
    /// [`open_tasks_tab`](Self::open_tasks_tab); a page has no window of its
    /// own to open in.
    pub(super) fn open_tasks_page_where(
        &mut self,
        target: RailTarget,
        where_: koru_editor::OpenWhere,
    ) -> bool {
        use koru_editor::OpenWhere;
        match where_ {
            OpenWhere::Reuse | OpenWhere::SameTab => return self.open_tasks_tab(target, false),
            OpenWhere::BackgroundTab => return self.open_tasks_tab(target, true),
            OpenWhere::NewWindow => return false,
            OpenWhere::NewTab | OpenWhere::Split(_) => {}
        }
        // The product flags, answered at this door as `open_tasks_tab`
        // answers them at its own.
        if !rail_target_shown(target) {
            return false;
        }
        match where_ {
            OpenWhere::Split(side) => self.split_with(Tab::tasks(target), side),
            _ => {
                self.tabs_mut().push(Tab::tasks(target));
                self.slides_mut().clear();
                self.relayout();
            }
        }
        self.tasks_page_in_front(target)
    }

    /// **A task's node, where the place chooser said** — Alt+click on a task
    /// row (ADR-58). The latch [`open_task_node`](Self::open_task_node) sets,
    /// so a row whose task has gone is refused at the landing; then the door
    /// every reference goes through, with the node, so the landing flashes
    /// the row it arrives on as the row's own open does.
    pub(super) fn open_task_where(
        &mut self,
        ws: &koru_core::schema::WorkspaceDoc,
        document: DocumentId,
        node: NodeId,
        where_: koru_editor::OpenWhere,
    ) -> bool {
        self.pending_task_open = Some((document, node, Self::gesture_deadline()));
        self.open_reference(
            ws,
            koru_editor::Reference::Document(koru_editor::trigger::DocumentTarget {
                document,
                node: Some(node),
            }),
            where_,
        )
    }

    /// What the Tasks tab asked for.
    ///
    /// **One path per verb**, so ticking from the row control and *Complete*
    /// from the menu are the same call — and so the confirm on *Remove from
    /// Tasks* cannot be skipped by coming in the other way.
    /// **Tasks carried over the sidebar**: while rows dragged off the Tasks
    /// page in front are past the threshold, the list row under the pointer
    /// is where they would land — lit in the rail, and the page offers no
    /// gap of its own meanwhile. Answers whether anything changed.
    pub(super) fn track_task_drop(&mut self, x: f32, y: f32) -> bool {
        let carrying = matches!(
            &self.tabs().active_tab().view,
            TabView::Tasks(t) if t.carrying_rows()
        );
        let target = if carrying
            && self.sidebar.is_open()
            && self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Tasks
        {
            let area = self.explore_area();
            let top = self.explore.rail_top(area);
            match area
                .contains(x, y)
                .then(|| self.explore.rail().target_at(area, top, x, y))
                .flatten()
            {
                Some(RailTarget::List(id)) => Some(id),
                _ => None,
            }
        } else {
            None
        };
        let mut changed = self.explore.rail_mut().set_drop_target(target);
        if let TabView::Tasks(t) = &mut self.tabs_mut().active_mut().view {
            changed |= t.set_rows_outside(target.is_some());
        }
        changed
    }

    /// The release of carried tasks over a list in the sidebar: they join
    /// it. `false` — and nothing done — when no list was lit.
    pub(super) fn drop_tasks_on_list(&mut self) -> bool {
        let Some(list) = self.explore.rail().drop_target() else {
            return false;
        };
        self.explore.rail_mut().set_drop_target(None);
        let dropped = match &mut self.tabs_mut().active_mut().view {
            TabView::Tasks(t) => t.drop_rows_into_list(list),
            _ => false,
        };
        if dropped {
            self.drain_tasks_request();
        }
        dropped
    }

    pub(super) fn drain_tasks_request(&mut self) -> bool {
        let request = match &mut self.tabs_mut().active_mut().view {
            TabView::Tasks(t) => t.take_request(),
            _ => None,
        };
        let Some(request) = request else {
            return false;
        };
        self.handle_tasks_request(request);
        true
    }

    /// One request from the Tasks tab, through the door its verb has.
    fn handle_tasks_request(&mut self, request: crate::tasks_view::Request) {
        // **A tab reports in its own coordinates.** The container passes the
        // content rect's size as the viewport and subtracts its origin from
        // every pointer coordinate before delegating, so anything a view hands
        // back has to come the other way before a menu — drawn in window space
        // — can use it.
        let c = self.regions.content;
        let to_window = |at: (f32, f32)| (at.0 + c.x, at.1 + c.y);
        match request {
            crate::tasks_view::Request::Open {
                document,
                node,
                new_tab,
            } => {
                self.open_task_node(document, node, new_tab);
            }
            // Alt+click on a row: the place chooser at the press, over the
            // pane the page is in, whose answer opens the task (ADR-58).
            crate::tasks_view::Request::ChoosePlace { document, node, at } => {
                let page = self.focused_view_rect();
                self.open_place_chooser(
                    super::place_chooser::PlaceTarget::Task { document, node },
                    (at.0 + page.x, at.1 + page.y),
                );
            }
            crate::tasks_view::Request::RowMenu {
                document,
                node,
                completed,
                native,
                list,
                pinned_today,
                at,
            } => {
                let (subject, items) = crate::tasks_view::row_menu(
                    document,
                    node,
                    completed,
                    native,
                    list,
                    pinned_today,
                );
                self.open_subject_menu(subject, items, to_window(at));
            }
            crate::tasks_view::Request::Complete {
                document,
                node,
                completed,
            } => self.queue_task_command(
                TaskCommand::Complete {
                    document,
                    node,
                    completed,
                },
                false,
            ),
            crate::tasks_view::Request::Capture {
                title,
                due,
                list,
                priority,
                rank,
            } => self.queue_task_command(
                TaskCommand::Create {
                    title,
                    due,
                    list,
                    priority,
                    rank,
                },
                false,
            ),
            // **Membership is an ordinary task command**, queued beside the
            // ticks and the captures — so it is written into the task's own
            // document by the one door, and the page and the rail are asked
            // again the moment it lands.
            crate::tasks_view::Request::SetList {
                document,
                node,
                list,
            } => self.queue_task_command(
                TaskCommand::SetList {
                    document,
                    node,
                    list,
                },
                false,
            ),
            // **The two in-place writes leave by the same door.** The page's
            // calendar and priority picker answer a request; this is the one
            // place a request becomes a task command, so there is no second
            // road from a picker to the host.
            crate::tasks_view::Request::SetDue {
                document,
                node,
                due,
            } => self.queue_task_command(
                TaskCommand::SetDue {
                    document,
                    node,
                    due,
                },
                false,
            ),
            crate::tasks_view::Request::SetPriority {
                document,
                node,
                priority,
            } => self.queue_task_command(
                TaskCommand::SetPriority {
                    document,
                    node,
                    priority,
                },
                false,
            ),
            // **A batch is a run of ordinary edits**, each through its own
            // door, so the rail's numbers and the page's echoes see every
            // one of them exactly as they see one.
            crate::tasks_view::Request::SetColor {
                document,
                node,
                color,
            } => self.queue_task_command(
                TaskCommand::SetColor {
                    document,
                    node,
                    color,
                },
                false,
            ),
            // **A drag's ranks leave by the same door**, one per moved row.
            crate::tasks_view::Request::SetRank {
                document,
                node,
                rank,
            } => self.queue_task_command(
                TaskCommand::SetRank {
                    document,
                    node,
                    rank,
                },
                false,
            ),
            // **A rename leaves by the same door** as every other edit.
            crate::tasks_view::Request::Retitle {
                document,
                node,
                title,
            } => self.queue_task_command(
                TaskCommand::Retitle {
                    document,
                    node,
                    title,
                },
                false,
            ),
            crate::tasks_view::Request::Many(requests) => {
                for request in requests {
                    self.handle_tasks_request(request);
                }
            }
            // The note's corner control: its document in a tab of its own,
            // the way a Ctrl+Enter on a pill opens one — even when a tab is
            // already on it, since the person asked for another.
            crate::tasks_view::Request::OpenNote { document } => {
                self.open_document_target(
                    koru_editor::trigger::DocumentTarget {
                        document,
                        node: None,
                    },
                    koru_editor::OpenWhere::NewTab,
                );
            }
            // The heading's colour square: the rail row's colour rows, at
            // the press — the same subject, so the same action applies it.
            crate::tasks_view::Request::ListColor { list, at } => {
                let mut menu = super::context_menu::ContextMenu::new(
                    &mut self.engine,
                    super::context_menu::MenuSubject::List(list),
                    to_window(at),
                    super::context_menu::list_color_items(),
                );
                let enabled = self.enabled_items(&menu);
                menu.step(&enabled, 1);
                self.menu = Some(menu);
                self.submenu = None;
                self.nested = None;
            }
            // The heading's words: through the rail's own door, so the list
            // has one rename whichever surface asked.
            crate::tasks_view::Request::RenameList { list, name } => {
                self.apply_tasks_rail(crate::panes::explore::rail::TasksRailRequest::RenameList {
                    id: list,
                    name,
                });
            }
            crate::tasks_view::Request::RowsMenu {
                tasks,
                native,
                completed,
                at,
            } => {
                let (subject, items) = crate::tasks_view::rows_menu(tasks, native, completed);
                self.open_subject_menu(subject, items, to_window(at));
            }
            crate::tasks_view::Request::RemoveTasks { tasks } => {
                self.ask_remove_tasks(tasks);
            }
        }
    }

    /// What the Reminders page asked for.
    ///
    /// The component reports `reminder_shell::Intent`s — the one vocabulary
    /// both hosts speak — so this is a take and a hand-off:
    /// [`apply_reminder_shell_intent`](MainView::apply_reminder_shell_intent)
    /// is host-agnostic, and applying the intent to the bell's shell too is
    /// what keeps the popover's filter and the page's in one answer.
    pub(super) fn drain_reminders_request(&mut self) -> bool {
        let intent = match &mut self.tabs_mut().active_mut().view {
            TabView::Reminders(r) => r.take_intent(),
            _ => None,
        };
        let Some(intent) = intent else {
            return false;
        };
        self.apply_reminder_shell_intent(intent)
    }

    /// **The one door a task command goes into the queue by.** What the write
    /// will do to the rail's numbers is worked out here, from the row as the
    /// page shows it, and applied when the receipt says the write landed — so
    /// the number beside the queue moves with the row rather than a second
    /// after it. `echo` is for a command the container made on its own (a
    /// menu row's *Complete*, a confirmed *Remove*); a command the page asked
    /// for has already echoed itself on the way out.
    pub(super) fn queue_task_command(&mut self, command: TaskCommand, echo: bool) {
        self.queue_task_command_landing(command, echo, None);
    }

    /// [`queue_task_command`](Self::queue_task_command), with what its
    /// receipt is owed said by the caller rather than read off the command —
    /// *Add document* for several tasks gives each a home and opens none,
    /// where one task's opens its new page.
    pub(super) fn queue_task_command_landing(
        &mut self,
        command: TaskCommand,
        echo: bool,
        landing: Option<super::Landing>,
    ) {
        crate::tasks::debug_line(format_args!(
            "command queued: {} (echoed by the container: {echo})",
            command.token()
        ));
        let row = match &command {
            TaskCommand::Complete { document, node, .. }
            | TaskCommand::Remove { document, node }
            | TaskCommand::SetDue { document, node, .. }
            | TaskCommand::SetPriority { document, node, .. }
            | TaskCommand::SetColor { document, node, .. }
            | TaskCommand::SetRank { document, node, .. }
            | TaskCommand::Retitle { document, node, .. }
            | TaskCommand::SetList { document, node, .. }
            | TaskCommand::PinToday { document, node, .. }
            | TaskCommand::CreateHome { document, node } => self.task_row(*document, *node),
            TaskCommand::Create { .. } => None,
        };
        let delta = crate::tasks_view::echo::delta_of_command(
            &command,
            row.as_ref(),
            self.tasks_today(),
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap_or_default();
        self.pending_count_deltas.push(delta);
        let landing = match &command {
            _ if landing.is_some() => landing.unwrap_or(super::Landing::Plain),
            TaskCommand::CreateHome { .. } => super::Landing::OpenHome,
            TaskCommand::Create {
                due,
                list,
                priority,
                ..
            } => self.capture_landing(due.as_ref(), *list, *priority),
            _ => super::Landing::Plain,
        };
        self.pending_landings.push_back(landing);
        if echo {
            self.each_tab_mut(|tab| {
                if let TabView::Tasks(t) = &mut tab.view {
                    t.echo_command(&command);
                }
                false
            });
        }
        self.task_commands.push(command);
    }

    /// **Show this task on the Tasks page** — the return leg of *View in
    /// origin*.
    ///
    /// Which queue it opens on is asked of `koru_core::tasks::bucket`, the
    /// domain function SPEC §7 defines and the store's own placement is held
    /// equal to. **Not a comparison against today written here**: a second copy
    /// of that rule is a second answer, and the copy that used to live in
    /// `taskcounts.rs` had already drifted.
    ///
    /// A task the rule places in Completed opens the Completed page, which is
    /// where it is — the alternative is opening a queue that does not contain
    /// the row somebody just asked to see.
    pub(super) fn reveal_task_in_tasks_view(
        &mut self,
        ws: &koru_core::schema::WorkspaceDoc,
        node: NodeId,
    ) -> bool {
        // With the Tasks view hidden there is nowhere to reveal a task: the
        // due chip's press, *View in Tasks* and every other way here are
        // answered with nothing (koru-features).
        if !koru_features::tasks() {
            return false;
        }
        let Ok(read) = ws.read(node) else {
            return false;
        };
        let fields = koru_core::tasks::TaskFields::of(&read);
        // Priority is an ordinary attribute rather than one of the four task
        // fields, and `bucket` needs it: an undated task at High or above is
        // Next rather than Later, which is the one rule a due alone cannot
        // answer.
        let priority = read
            .content
            .attrs()
            .iter()
            .find(|a| a.role == koru_core::attributes::KEY_PRIORITY)
            .and_then(|a| a.as_text())
            .and_then(koru_core::attributes::Priority::parse);
        let today = koru_core::time::DateOnly::new(self.tasks_today());
        let bucket = koru_core::tasks::bucket(
            fields.due.as_ref(),
            fields.lifecycle,
            priority,
            today,
            &koru_editor::utils::zone::SystemZone::detect(),
        );
        let target = RailTarget::Bucket(Self::rail_bucket(bucket));
        self.open_tasks_tab(target, false);
        self.explore.show_tasks();
        // **The queue is the answer; the selected row is a bonus.**
        //
        // Selecting needs the `DocumentId` the store keyed the row under, and
        // the tab in front is what knows it — the request came from a menu,
        // and a menu is only ever open over the active tab. `active_document`
        // is the shell’s answer to the same question on a different clock.
        //
        // When neither can name it, the page still opens on the right queue
        // rather than the gesture doing nothing: *take me to my task* is
        // answered by the page it is on, and the highlight is what makes it
        // quicker to find.
        if let Some(document) = self.tabs().active_tab().document.or(self.active_document) {
            // Held rather than applied: the page may not hold the row yet.
            self.pending_task_reveal = Some((document, node, Self::gesture_deadline()));
            self.finish_task_reveal();
        }
        true
    }

    /// **Open a list's page** — *Go to list* on a row's menu, or its list chip
    /// pressed. The same door a press on the rail's row is: the Tasks tab in
    /// front moves to the list, or one opens, and the rail's selection follows.
    ///
    /// Only while the table still has the list: a membership the table cannot
    /// name draws no chip and offers no row, and a list deleted next door
    /// between the menu opening and the row being chosen is the guard the
    /// list's own menu already has.
    pub(super) fn go_to_task_list(&mut self, list: koru_core::ids::ListId) -> bool {
        if !koru_features::tasks() {
            return false;
        }
        if !self.list_table.iter().any(|entry| entry.id == list) {
            return false;
        }
        // A jump: the queue this was pressed on is where Back comes back to.
        self.record_jump();
        self.open_tasks_tab(RailTarget::List(list), false);
        self.explore.show_tasks();
        true
    }

    /// The other half: select the row once the page has it.
    ///
    /// **Spent by arrival or by the deadline, never by trying once.** A page is
    /// asked for again when the target changes and answers on a worker thread,
    /// so the row this was asked about is very often not there on the turn the
    /// ask was made.
    pub(super) fn finish_task_reveal(&mut self) -> bool {
        let Some((document, node, until)) = self.pending_task_reveal else {
            return false;
        };
        let landed = self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Tasks(t) => t.reveal_task(document, node),
            _ => false,
        });
        if landed || std::time::Instant::now() >= until {
            self.pending_task_reveal = None;
        }
        landed
    }

    /// *List…*: open the membership picker over the row it was asked for.
    ///
    /// **Not the *go there and act* shape `Due…` and `Priority…` have**, and
    /// the difference is not a preference: those two open the *editor's* picker
    /// over the node, and there is no editor picker for `koru.list` — its
    /// values live in the workspace's system document, not in the attribute
    /// registry. So the chooser is the Tasks view's own, anchored on the row's
    /// `…`, and nothing has to be navigated to.
    pub(super) fn open_task_list_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Tasks(t) => t.open_list_picker(document, node),
            _ => false,
        })
    }

    /// *Due…* on a task row: the shared calendar, over the row, on this page.
    ///
    /// **Nothing on the Tasks page opens a document to make a change.** This
    /// used to go to the node — root a tab on its document and hang the
    /// editor's picker there — and you set a date and were somewhere else
    /// (finding 11). The gesture that means *take me there* is *View in
    /// origin*; this one means *set the date*, and does it here.
    pub(super) fn open_task_due_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Tasks(t) => t.open_due_picker(document, node),
            _ => false,
        })
    }

    /// *Priority…* on a task row: the four levels, over the row, on this page.
    pub(super) fn open_task_priority_picker(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Tasks(t) => t.open_priority_picker(document, node),
            _ => false,
        })
    }

    /// Open a document in a tab of its own with the caret on `node` — no task
    /// latch, since the node is a plain paragraph: a new home's first line.
    pub(super) fn open_document_at(&mut self, document: DocumentId, node: NodeId) -> bool {
        self.record_jump();
        self.pending_lands
            .push_back((document, koru_editor::OpenWhere::NewTab));
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: Some(node),
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    }

    /// Open a task's node in its own document.
    ///
    /// **Through the resolver, not straight to the node.**
    /// [`resolve_task_ref`] is what says the reference still names a task; a
    /// row unmarked since the page was fetched must not quietly open the
    /// paragraph underneath it, which is the fallback navigation SPEC §3 rules
    /// out. The check cannot be made here — the document that answers is very
    /// often not the one this window has open, and a `NodeId` is
    /// document-scoped — so the gesture is **latched** and spent by
    /// [`refuse_stale_task_open`](Self::refuse_stale_task_open) at the landing,
    /// where `ws` is the task's own document. What leaves here is the ordinary
    /// ask, unchanged, so every route the navigation already knows how to take
    /// (a document this window holds, one it has to fetch, one in another
    /// workspace) still takes it.
    ///
    /// [`resolve_task_ref`]: koru_core::tasks::refs::resolve_task_ref
    pub(super) fn open_task_node(
        &mut self,
        document: DocumentId,
        node: NodeId,
        new_tab: bool,
    ) -> bool {
        self.record_jump();
        if new_tab {
            self.pending_lands
                .push_back((document, koru_editor::OpenWhere::NewTab));
        }
        self.pending_task_open = Some((document, node, Self::gesture_deadline()));
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: Some(node),
            },
            // **An empty match is the whole row**: the landing takes the
            // search result's path and flashes the paragraph it arrived on,
            // as a global-search hit does, so the eye finds the task in the
            // page it was sent to.
            match_text: Some(String::new()),
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    }

    /// **Whether this landing is a task open whose task has gone**, asked on the
    /// one line that would otherwise reveal the node.
    ///
    /// `true` means *do not navigate*. It answers `false` for every landing this
    /// window did not ask for as a task — a search hit, a pill, Back — because
    /// the latch names the exact node, and for a task that still resolves.
    ///
    /// **The refusal is a toast and a refresh, not a blank page.** The Tasks
    /// view's notice state is the page's *empty* state and using it here would
    /// throw away rows that are perfectly good; the container's toast is the
    /// surface that already says "the thing you pressed could not be done"
    /// ([`task_command_failed`](Self::task_command_failed) raises it for a
    /// refused command). The sentence is the one
    /// [`resolve_task_ref`]'s own documentation offers, because the domain
    /// answers a single word and phrasing it is the UI's job. The refresh is
    /// what takes the stale row off the page, so pressing it again is not the
    /// same disappointment twice.
    ///
    /// A new-tab open that is refused takes its `pending_lands` reservation back
    /// with it: leaving one queued would send an unrelated later opening of the
    /// same document to a tab of its own.
    ///
    /// [`resolve_task_ref`]: koru_core::tasks::refs::resolve_task_ref
    pub(super) fn refuse_stale_task_open(
        &mut self,
        ws: &koru_core::schema::WorkspaceDoc,
        node: NodeId,
    ) -> bool {
        let Some((document, wanted, until)) = self.pending_task_open else {
            return false;
        };
        if std::time::Instant::now() >= until {
            self.pending_task_open = None;
            return false;
        }
        if wanted != node {
            return false;
        }
        self.pending_task_open = None;
        let Some(workspace) = ws.id() else {
            return false;
        };
        let reference = koru_core::EntityRef::task(workspace, document, node);
        if koru_core::tasks::refs::resolve_task_ref(ws, &reference)
            != koru_core::tasks::refs::TaskRefResolution::NotFound
        {
            return false;
        }
        // The Due…/Priority… half of the same gesture goes with it: there is no
        // task to hang a picker on.
        if let Some(i) = self
            .pending_lands
            .iter()
            .position(|(waiting, _)| *waiting == document)
        {
            self.pending_lands.remove(i);
        }
        // **The one sentence**, shared with the in-place writes' guard: a
        // task that stopped being one is told about in the same words
        // whichever leg noticed.
        self.toast = Some((
            koru_ui::Toast::new(
                koru_workspace_host::tasks::TASK_GONE.to_owned(),
                None,
                std::time::Instant::now(),
            ),
            super::ToastPayload::None,
        ));
        self.tasks_changed();
        true
    }

    /// Hand the queue to the shell, which is the side that holds a
    /// `&mut Workspace`.
    ///
    /// **Every verb leaves through here.** Ticking from the row control and
    /// *Complete* from the menu queue the same command, so the two cannot drift
    /// - and *Remove from Tasks* cannot reach the store without passing the
    /// confirm, because the confirm is what puts it in this queue.
    /// Put commands in the queue directly. **Test-only**, for the suite that
    /// proves the wrapper hands the queue over: the four verbs reach it by four
    /// different gestures, and what is being tested is the one step they share.
    #[cfg(test)]
    pub(super) fn queue_task_commands_for_test(&mut self, commands: Vec<TaskCommand>) {
        self.task_commands.extend(commands);
    }

    pub(super) fn take_wants_task_commands(&mut self) -> Vec<TaskCommand> {
        // **Taken, not marked.** This used to invalidate the three surfaces
        // here — before the shell had run anything — so the page's next
        // question was caused by the intention to write and was answered,
        // nearly always, from a projection that had not seen the write. The
        // mark moved to [`task_command_landed`](Self::task_command_landed):
        // after the write, with its version in hand.
        std::mem::take(&mut self.task_commands)
    }

    /// A command the shell ran, and the receipt for it.
    ///
    /// **This is where the surfaces are told**, and it is after the write
    /// rather than before it. A published receipt becomes a requirement the
    /// page and the rail carry to the read model until an answer includes it;
    /// an unpublished one — a stopped session — is an invalidation and
    /// nothing more, because a wait on a store that has not been told would
    /// burn its whole budget every time.
    ///
    /// **And the echo is armed and the numbers move.** The page's provisional
    /// row takes the receipt's ids; the delta computed when the request was
    /// made goes onto the rail's numbers, drawn from here until the rail's
    /// wait ends with an answer that has the write.
    pub(super) fn task_command_landed(
        &mut self,
        landed: &koru_workspace_host::tasks::Landed,
    ) -> bool {
        self.each_tab_mut(|tab| {
            if let TabView::Tasks(t) = &mut tab.view {
                t.echo_landed(landed);
            }
            false
        });
        let mut changed = false;
        if !self.pending_count_deltas.is_empty() {
            let delta = self.pending_count_deltas.remove(0);
            if self.task_counts.expect(delta) {
                self.explore
                    .rail_mut()
                    .set_counts(self.task_counts.counts());
                self.refresh_rail_lists();
                changed = true;
            }
        }
        match self.pending_landings.pop_front() {
            // **A new home is opened the moment it exists**, in a tab of its
            // own with the caret on the task — so Back, and closing the tab,
            // both come back to the list it was made from.
            Some(super::Landing::OpenHome) => {
                if let Some(node) = landed.node {
                    changed |= match landed.caret {
                        // The page is the task; the caret goes under its
                        // title, where the notes are written.
                        Some(caret) => self.open_document_at(landed.document, caret),
                        None => self.open_task_node(landed.document, node, true),
                    };
                }
            }
            // **A capture that went somewhere else says so** the moment it
            // exists — with the way there and the way back on the same chip,
            // because a row that vanished from the page you typed it on reads
            // as a row that was never made.
            Some(super::Landing::Announce { message, target }) => {
                if let Some(node) = landed.node {
                    self.toast = Some((
                        koru_ui::Toast::new(message, Some("View task"), std::time::Instant::now())
                            .with_secondary("Undo"),
                        super::ToastPayload::TaskAdded {
                            document: landed.document,
                            node,
                            target,
                        },
                    ));
                    changed = true;
                }
            }
            Some(super::Landing::Plain) | None => {}
        }
        self.tasks_changed_with(WriteRequirement::of_landed(landed));
        changed
    }

    /// Where a capture lands, against the page it was typed on.
    ///
    /// `Plain` when it lands where the reader is looking — the bucket the
    /// due and the level put it in is the page's, or the list is the page's
    /// list — and otherwise the toast's words and the place *View task* goes:
    /// the list when one was chosen, since a task filed in a list is found
    /// there, else the queue. The bucket is `bucket`'s answer, never a table
    /// of offsets here.
    fn capture_landing(
        &self,
        due: Option<&koru_core::time::Temporal>,
        list: Option<koru_core::ids::ListId>,
        priority: Option<koru_core::attributes::Priority>,
    ) -> super::Landing {
        let TabView::Tasks(page) = &self.tabs().active_tab().view else {
            return super::Landing::Plain;
        };
        let today = koru_core::time::DateOnly::new(self.tasks_today());
        let bucket = Self::rail_bucket(koru_core::tasks::bucket(
            due,
            koru_core::attributes::Lifecycle::Open,
            priority,
            today,
            &koru_editor::utils::zone::SystemZone::detect(),
        ));
        let lands_here = match page.target() {
            RailTarget::Bucket(here) => here == bucket,
            RailTarget::List(here) => list == Some(here),
            RailTarget::Reminders => unreachable!(
                "the rail's Reminders row swaps the tab's view; a Tasks page never targets it"
            ),
        };
        if lands_here {
            return super::Landing::Plain;
        }
        let (message, target) = match list.and_then(|id| {
            self.list_table
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| (id, entry.name.clone()))
        }) {
            Some((id, name)) => (format!("Added to {name}"), RailTarget::List(id)),
            None => (
                format!(
                    "Added to {}",
                    crate::panes::explore::rail::bucket_label(bucket)
                ),
                RailTarget::Bucket(bucket),
            ),
        };
        super::Landing::Announce { message, target }
    }

    /// The rail's word for the domain's bucket — one mapping, used wherever a
    /// task's place is worked out from its fields.
    pub(super) fn rail_bucket(bucket: koru_core::tasks::Bucket) -> Bucket {
        match bucket {
            koru_core::tasks::Bucket::Today => Bucket::Today,
            koru_core::tasks::Bucket::Next => Bucket::Next,
            koru_core::tasks::Bucket::Upcoming => Bucket::Upcoming,
            koru_core::tasks::Bucket::Later => Bucket::Later,
            koru_core::tasks::Bucket::Completed => Bucket::Completed,
        }
    }

    /// *View task* on the capture's toast: the queue or list it went to, with
    /// the row selected — the *View in Tasks* shape, from a receipt's ids.
    pub(super) fn view_added_task(
        &mut self,
        document: koru_core::DocumentId,
        node: NodeId,
        target: RailTarget,
    ) -> bool {
        self.record_jump();
        self.open_tasks_tab(target, false);
        self.explore.show_tasks();
        self.pending_task_reveal = Some((document, node, Self::gesture_deadline()));
        self.finish_task_reveal();
        true
    }

    /// *Undo* on the capture's toast: the task comes back off Tasks by the one
    /// door — echoed off every page and out of the numbers at once.
    pub(super) fn undo_added_task(&mut self, document: koru_core::DocumentId, node: NodeId) {
        self.queue_task_command(TaskCommand::Remove { document, node }, true);
    }

    /// The editor's own gesture — a checkbox, a `/due`, a menu row — noticed
    /// by the activity sweep with the written document in hand.
    ///
    /// **The second producer of a requirement**, and the conservative one:
    /// the vector is read from the document *this* dispatch wrote, one turn
    /// after the write rather than at it, so it is at least the write and
    /// never less. Activity whose subject is another document — a command
    /// that landed on a tab rooted elsewhere — invalidates without a
    /// requirement, because the vector in hand is not that document's.
    pub fn note_task_activity(
        &mut self,
        ws: &koru_core::schema::WorkspaceDoc,
        owed: &[koru_editor::tasks::TaskActivity],
    ) {
        // **Silent when nothing is owed, and that is worth knowing.** Only a
        // canonical command mints activity, so a write that produces none — an
        // undo, a redo — invalidates nothing here and the two store-backed
        // surfaces go on drawing their last answer. Not logged: this runs on
        // every dispatch, so a line here is a line per keystroke.
        if owed.is_empty() {
            return;
        }
        let here = ws.own_document_id();
        let names_here = owed
            .iter()
            .flat_map(|activity| activity.intents.iter())
            .any(|intent| {
                matches!(
                    intent.subject(),
                    koru_core::EntityRef::Task { document, .. } if Some(*document) == here
                )
            });
        self.tasks_changed_with(if names_here {
            WriteRequirement::of_document(ws)
        } else {
            None
        });
    }

    /// A command the shell could not run.
    ///
    /// **A failed capture gives the typing back.** Losing what somebody typed
    /// is the one unrecoverable bug, and a toast that names the text without
    /// restoring it is a receipt rather than a recovery.
    pub(super) fn task_command_failed(&mut self, command: &TaskCommand, error: &str) -> bool {
        // The echo was a promise; the refusal takes it back, and the number
        // it would have moved stays where the store has it.
        if !self.pending_count_deltas.is_empty() {
            self.pending_count_deltas.remove(0);
        }
        self.pending_landings.pop_front();
        self.each_tab_mut(|tab| {
            if let TabView::Tasks(t) = &mut tab.view {
                t.echo_failed();
            }
            false
        });
        if let TaskCommand::Create { title, .. } = command {
            self.each_tab_mut(|tab| {
                if let TabView::Tasks(t) = &mut tab.view {
                    t.restore_capture(title);
                }
                false
            });
        }
        self.toast = Some((
            koru_ui::Toast::new(error.to_owned(), None, std::time::Instant::now()),
            super::ToastPayload::None,
        ));
        true
    }

    /// Put *Remove from Tasks* behind the E-3 confirm, worded as it is drawn.
    ///
    /// **The confirm is the only door.** `settle_confirm` re-arms the queue on
    /// yes and clears it on no, so there is no path from the menu row to the
    /// store that does not go past this sentence.
    pub(super) fn ask_remove_from_tasks(&mut self, document: DocumentId, node: NodeId) -> bool {
        let row = self.task_row(document, node);
        let title = row
            .as_ref()
            .map(|r| r.title.clone())
            .unwrap_or_else(|| "This task".to_owned());
        let due = row
            .as_ref()
            .map(|r| crate::tasks_view::due_sentence(r, self.tasks_today()))
            .unwrap_or_default();
        let context = row
            .as_ref()
            .map(|r| r.breadcrumb.clone())
            .unwrap_or_default();
        let where_it_is = if context.is_empty() {
            String::new()
        } else {
            format!(" in {context}")
        };
        self.confirm = Some((
            koru_ui::Confirm::new(
                "Remove this task?".to_owned(),
                format!(
                    "\u{201C}{title}\u{201D}{due} Removing it clears the due date on the \
                     row{where_it_is}; the row itself stays."
                ),
                "Remove",
            ),
            super::trash::ConfirmSubject::RemoveFromTasks(vec![PendingTaskEdit {
                command: TaskCommand::Remove { document, node },
                title,
            }]),
        ));
        true
    }

    /// *Remove from Tasks* — or Delete — for a selection: **one question for
    /// the set**, and every task is removed on the one yes. A set of one asks
    /// the single question, which names the task and its date.
    pub(super) fn ask_remove_tasks(&mut self, tasks: Vec<(DocumentId, NodeId)>) -> bool {
        match tasks.as_slice() {
            [] => return false,
            &[(document, node)] => return self.ask_remove_from_tasks(document, node),
            _ => {}
        }
        let n = tasks.len();
        let pending: Vec<PendingTaskEdit> = tasks
            .into_iter()
            .map(|(document, node)| PendingTaskEdit {
                command: TaskCommand::Remove { document, node },
                title: self
                    .task_row(document, node)
                    .map(|row| row.title)
                    .unwrap_or_default(),
            })
            .collect();
        self.confirm = Some((
            koru_ui::Confirm::new(
                format!("Remove {n} tasks?"),
                "Removing them clears their due dates. Lines in documents stay where they are."
                    .to_owned(),
                "Remove",
            ),
            super::trash::ConfirmSubject::RemoveFromTasks(pending),
        ));
        true
    }

    /// The row the Tasks tab is showing for this node, if it is showing one.
    pub(super) fn task_row(
        &self,
        document: DocumentId,
        node: NodeId,
    ) -> Option<crate::tasks_view::TaskEntry> {
        self.panes.iter_groups().find_map(|(_, group)| {
            group.tabs().iter().find_map(|tab| match &tab.view {
                TabView::Tasks(t) => t.row_of(document, node).cloned(),
                _ => None,
            })
        })
    }

    /// The day the Tasks tab is drawn for, or this machine's if there is none.
    pub(super) fn tasks_today(&self) -> chrono::NaiveDate {
        self.panes
            .iter_groups()
            .find_map(|(_, group)| {
                group.tabs().iter().find_map(|tab| match &tab.view {
                    TabView::Tasks(t) => Some(t.today()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| chrono::Local::now().date_naive())
    }

    /// Something in this window changed a task. Ask both surfaces again now
    /// rather than in half a minute.
    ///
    /// **Also the rollover's door.** Midnight changes no data and produces no
    /// answer, but it makes every bucket edge wrong at once — which is the same
    /// thing this function already means: *what these surfaces are showing is no
    /// longer what the store would say.* One door rather than a second path that
    /// invalidates a different subset.
    pub(super) fn tasks_changed(&mut self) {
        self.tasks_changed_with(None);
    }

    /// [`tasks_changed`](Self::tasks_changed), with the write that caused it
    /// when there is one.
    ///
    /// `Some` is a local write the read model may not have seen yet: the two
    /// store-backed surfaces carry it to their queries and keep asking, for a
    /// bounded period, while the answer says it is behind. `None` is the
    /// rollover, a stale navigation, an unpublished write — nothing to wait
    /// for, so the ordinary *ask once now* is all that happens.
    pub(super) fn tasks_changed_with(&mut self, requirement: Option<WriteRequirement>) {
        // **The fork.** `Some` arms a wait both surfaces can end on; `None`
        // invalidates and waits for nothing, which is what an unpublished
        // write, an undo, a rollover and a stale navigation all produce. The
        // second is where a number can move and move back, so the log says
        // which happened rather than leaving it to be inferred.
        crate::tasks::debug_line(format_args!(
            "invalidate: requirement={}",
            if requirement.is_some() {
                "armed"
            } else {
                "none (nothing to wait on)"
            }
        ));
        match &requirement {
            Some(requirement) => self.task_counts.require(requirement.clone()),
            None => self.task_counts.invalidate(),
        }
        // The context column's section is a third reading of the same facts.
        self.context_tasks_stale = true;
        self.each_tab_mut(|tab| {
            if let TabView::Tasks(t) = &mut tab.view {
                match &requirement {
                    Some(requirement) => t.panel_mut().require(requirement.clone()),
                    None => t.panel_mut().invalidate(),
                }
                t.mark_stale();
            }
            false
        });
    }

    /// **The rollover, as a function of the day it is handed.**
    ///
    /// The clock is read once, on the maintenance tick, and everything that
    /// follows from a date change is here — which is what lets midnight be
    /// driven in a test rather than waited for. Answers whether the date moved.
    pub(super) fn day_rolled(&mut self, today: chrono::NaiveDate) -> bool {
        if !self.tasks_day.rolled(today) {
            return false;
        }
        self.tasks_changed();
        true
    }

    /// Fill the context column's **Tasks** section from the open document.
    ///
    /// **The open document answers, not the store** — and that is a decision
    /// rather than a convenience. This section is *this document's* tasks, the
    /// column already holds the document, and reading it here means the section
    /// is right the instant a `/due` is typed rather than up to half a minute
    /// later. The store's read model is the right source for the Tasks *tab*,
    /// which asks about a workspace no window can enumerate; it is the wrong one
    /// for a question the document in hand can answer exactly.
    ///
    /// **Nothing walks the document on a timer.** Three gates, in this order: the
    /// section has to be on screen, something has to have changed, and only then
    /// is the walk paid for. A column shut, or open on Comments, costs two
    /// comparisons per tick and nothing else — and the stale flag survives while
    /// it is shut, so opening it rebuilds rather than showing yesterday's rows.
    pub(super) fn refresh_context_tasks(&mut self, ws: &koru_core::schema::WorkspaceDoc) -> bool {
        let visible = self.context.is_open()
            && self.context_view.section() == crate::panes::context::Section::Tasks;
        if !visible {
            return false;
        }
        // The document moving is a change like any other, and cheaper to notice
        // here than to thread through every path that opens one.
        let stale = std::mem::take(&mut self.context_tasks_stale);
        if !stale && self.context_tasks_doc == self.active_document {
            return false;
        }
        self.context_tasks_doc = self.active_document;
        let today = self.tasks_today();
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.context_view
            .set_tasks(context_task_rows(ws, today, now_ms))
    }

    /// Push what the Tasks tab needs from the workspace: its `task-list`
    /// documents, so a row typed straight into this view can say that it names
    /// no page, and the list table behind the membership chip.
    ///
    /// The list table rides along, because a membership chip draws the list's
    /// *name* and the wire carries only its id.
    ///
    /// **This runs on the draw path**, which is why the first line is a
    /// `matches!` over the tabs rather than a manifest walk: a window with no
    /// Tasks tab must pay nothing for one, and `task_list_documents` filters
    /// every manifest entry and allocates. Pushed from here rather than from
    /// `View::set_list_table` for the reason that push exists — a tab opened
    /// *since* the last change still has to be told, and `set_list_table`
    /// returns early when the table has not moved.
    pub(super) fn refresh_tasks_context(&mut self, workspace: &Workspace) -> bool {
        if !self.panes.iter_groups().any(|(_, group)| {
            group
                .tabs()
                .iter()
                .any(|tab| matches!(tab.view, TabView::Tasks(_)))
        }) {
            return false;
        }
        let documents = workspace.task_list_documents();
        let lists: Vec<crate::tasks_view::ListFace> = self
            .list_table
            .iter()
            .map(|entry| crate::tasks_view::ListFace {
                id: entry.id,
                name: entry.name.clone(),
                color: entry.color.clone(),
            })
            .collect();
        let mut changed = false;
        self.each_tab_mut(|tab| {
            if let TabView::Tasks(t) = &mut tab.view {
                changed |= t.set_task_documents(documents.clone());
                changed |= t.set_lists(lists.clone());
            }
            // **The table push is what renames the tab.** A list showing on a
            // Tasks tab takes its name from this table, so the moment the table
            // moves the strip is stale — and nobody pressed anything, which is
            // why it cannot be done where the queue is pressed.
            changed |= tab.refresh_tasks_label();
            false
        });
        if changed {
            self.mark_tab_widths_stale();
        }
        changed
    }
}

/// The open document's tasks, in document order, as the context column needs
/// them.
///
/// **`is_task` is the whole derivation** ([ADR-285]): a marker *or* a due, read
/// through [`TaskFields`], and nothing here has a second opinion about what a
/// task is. The walk is the trash-filtered one, so a binned row does not appear
/// in a section about the document you are reading.
///
/// Every string is finished here — the title, and the due as a word — because
/// the context column (`panes::context`) holds no timezone, no locale and no
/// model.
///
/// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
/// [`TaskFields`]: koru_core::tasks::TaskFields
fn context_task_rows(
    ws: &koru_core::schema::WorkspaceDoc,
    today: chrono::NaiveDate,
    now_ms: i64,
) -> Vec<crate::panes::context::TaskRow> {
    use koru_core::tasks::{TaskFields, is_task};
    let Ok(nodes) = ws.walk_all() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (node, _) in nodes {
        let Ok(read) = ws.read(node) else {
            continue;
        };
        let fields = TaskFields::of(&read);
        if !is_task(&fields) {
            continue;
        }
        rows.push(crate::panes::context::TaskRow {
            node,
            // **The row's own line**, which is what a task is — not
            // `name_of`, which answers a *document's* name and calls every
            // paragraph in the workspace "Untitled". The same reading the
            // column's own subject line takes, one line above these rows.
            title: read
                .content
                .as_text()
                .map(str::to_owned)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled".to_owned()),
            due: fields
                .due
                .as_ref()
                .map(|due| koru_editor::chips::due_label(due, today)),
            completed: fields.lifecycle == koru_core::attributes::Lifecycle::Completed,
            overdue: fields
                .due
                .as_ref()
                .is_some_and(|due| overdue_here(due, today, now_ms)),
        });
    }
    rows
}

/// Whether a stored due has passed, **each temporal type in its own terms**
/// ([ADR-183]): a day is late once the day has ended, an instant the moment it
/// passes. The Tasks tab's rule, spelled from `Temporal` rather than from the
/// wire's two strings, because that is the shape a document holds.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md
fn overdue_here(due: &koru_core::time::Temporal, today: chrono::NaiveDate, now_ms: i64) -> bool {
    match due {
        koru_core::time::Temporal::Zoned(at) => at.instant_ms() < now_ms,
        other => other.zone_free_date().is_some_and(|day| day.date() < today),
    }
}

/// Whether this build shows the page a rail target opens: the Reminders page
/// with reminders, every queue and list with tasks (koru-features).
pub(super) fn rail_target_shown(target: RailTarget) -> bool {
    match target {
        RailTarget::Reminders => koru_features::reminders(),
        _ => koru_features::tasks(),
    }
}
