// SPDX-License-Identifier: MPL-2.0

//! **What the Manage tab offers a screen reader, and what a request to one
//! of its controls means.**
//!
//! The Tasks page's pattern (`tasks_view/accessibility.rs`), at this tab's
//! size: this module turns the nav's already laid-out rows into semantic
//! nodes and a request back into the press the pointer makes. Where the tab
//! sits in the window is the shell's (`mainview/accessibility.rs`).
//!
//! ```text
//! Group        the tab            s:manage#page        label = "Manage"
//! ├── ListBox  the nav            s:manage#nav         size_of_set = its rows
//! │   ├── ListBoxOption  a Profile        n:<profile uuid>#manage-profile
//! │   ├── ListBoxOption  hidden line      s:manage#hidden
//! │   ├── ListBoxOption  New profile      s:manage#new-profile
//! │   ├── ListBoxOption  Linked devices   s:manage#devices
//! │   └── ListBoxOption  Koru Cloud       s:manage#cloud
//! ├── Heading  the page's name    s:manage#heading     level 1
//! ├── Button   a Profile page's control   s:manage#<control>, or
//! │            manage-unlink:<n> / manage-repair:<n> for the indexed ones
//! ├── ListBox  its Workspaces             s:manage#workspaces
//! │   └── ListBoxOption  a workspace      n:<workspace uuid>#manage-row
//! │       └── Button  its ••• / Unarchive / Restore
//! │                   n:<workspace uuid>#manage-more / -unarchive / -restore
//! ├── Switch   Cloud sync                 s:manage#cloud-sync
//! ├── Button   Link a device…              s:manage#link-device   (Linked devices)
//! ├── Button   this computer's row         s:manage#this-computer — Enter renames
//! ├── Switch   Use relay                   s:manage#relay
//! ├── ListBox  the linked devices          s:manage#devices-list
//! │   └── ListBoxOption  a device          manage-device:<n> — its history
//! │       └── Button  Unlink…              manage-device-unlink:<n>
//! ├── Button   a history's back row        s:manage#history-back, and its
//! │            document events             manage-history-event:<n>
//! ├── Button   Accounts…                   s:manage#accounts      (Koru Cloud)
//! ├── Button   can't-connect answers       manage-cloud-repair:<n>
//! ├── Button   another account's answers   s:manage#different-create / …
//! ├── Button   Subscribe / Manage …        s:manage#cloud-primary, learn-more
//! ├── Switch   a Profile's Cloud sync      n:<profile uuid>#manage-sync
//! ├── Switch   an account Profile not here n:<profile uuid>#manage-bring
//! ├── Button   Sign in…                    n:<profile uuid>#manage-sign-in
//! ├── Button   its progress disclosure     n:<profile uuid>#manage-progress
//! ├── Button   a held workspace's row      n:<workspace uuid>#manage-progress-row
//! └── Dialog   the question up, if any    s:manage#dialog — the page's, the
//!              Workspaces controller's archive, delete or move, or the
//!              account switcher
//! ```
//!
//! A Profile page's controls are its laid-out [`Control`]s, in keyboard
//! order, each where it is drawn; pressing one is the click.
//!
//! **English names** (maintainers' decision 15): this tab's copy is English literals,
//! and so are the names a reader hears; a Profile's name is the person's own
//! and is never translated.
//!
//! **No `selected` on an option**, for the reason the Tasks page gives: it
//! would cost the row its Invoke pattern. Which page is showing is the
//! heading, and which row the keyboard is on is focus.

use accesskit::{Action, ActionData, Role};
use koru_core::ProfileId;
use koru_draw::geometry::Rect;
use koru_workspace_host::accessibility::{
    ControlPart, Occurrence, Revision, SemanticNode, ViewSlot, WindowSlot,
};

use super::nav::Entry;
use super::profile_page::{Attention, Control, Look};
use super::{ManageView, Page};

const PAGE: &str = "manage";

pub(crate) mod parts {
    use koru_workspace_host::accessibility::ControlPart;

    pub const PAGE: ControlPart = ControlPart::new("page");
    pub const NAV: ControlPart = ControlPart::new("nav");
    pub const HEADING: ControlPart = ControlPart::new("heading");
    /// Rows that stand for a Profile, scoped by its uuid. Prefixed, because
    /// the same uuid names the Profile in the explore column's switcher.
    pub const PROFILE: ControlPart = ControlPart::new("manage-profile");
    pub const HIDDEN: ControlPart = ControlPart::new("hidden");
    pub const NEW_PROFILE: ControlPart = ControlPart::new("new-profile");
    pub const DEVICES: ControlPart = ControlPart::new("devices");
    pub const CLOUD: ControlPart = ControlPart::new("cloud");
    pub const DIALOG: ControlPart = ControlPart::new("dialog");
    /// The Workspaces section's list of rows.
    pub const WORKSPACES: ControlPart = ControlPart::new("workspaces");
    /// *Linked devices*' list of linked devices.
    pub const DEVICES_LIST: ControlPart = ControlPart::new("devices-list");
}

/// A Profile page control's part name. The two indexed kinds are instances
/// of their own, numbered by position, which is what they are.
fn control_part(control: Control) -> ControlPart {
    ControlPart::new(match control {
        Control::Tile => "change-icon",
        Control::Name => "rename",
        Control::Switch => "switch-profile",
        Control::CloudPointer => "cloud-pointer",
        Control::Attention(answer) => match answer {
            Attention::RetryDelete => "retry-delete",
            Attention::RemoveAfterFailure => "remove-after-failure",
            Attention::CancelFailure => "cancel-delete",
            Attention::FinishCleanup => "finish-cleanup",
            Attention::CleanupLater => "cleanup-later",
            Attention::Restore => "add-again",
            Attention::RemoveDiscovered => "remove-deleted",
            Attention::ContinueOffline => "continue-offline",
        },
        Control::Repair(_) => "repair",
        Control::Resume => "resume",
        Control::Session => "session",
        Control::Attach => "attach",
        Control::Unlink(_) => "unlink",
        Control::AddGoogle => "add-google",
        Control::MakeOffline => "make-offline",
        Control::SignOut => "sign-out",
        Control::CloudSync => "cloud-sync",
        Control::GetCloud => "get-cloud",
        Control::Delete => "delete-profile",
        Control::RemoveFromDevice => "remove-from-device",
        Control::DeleteFromKoru => "delete-from-koru",
        Control::ImportWorkspaces => "import",
        Control::NewWorkspace => "new-workspace",
        Control::WorkspaceRow(_) => "manage-row",
        Control::WorkspaceMore(_) => "manage-more",
        Control::ShowArchived => "show-archived",
        Control::ShowTrash => "show-trash",
        Control::Unarchive(_) => "manage-unarchive",
        Control::RestoreWorkspace(_) => "manage-restore",
        Control::NoticeRetry => "notice-retry",
        Control::NoticeDismiss => "notice-dismiss",
        Control::UnassignedRow(_) => "unassigned-row",
        Control::UnassignedMore(_) => "unassigned-more",
        Control::RestoreUnassignedProfile(_) => "unassigned-restore",
        Control::RestoreDeletedProfile(_) => "restore-deleted",
        Control::LinkDevice => "link-device",
        Control::CloseCreation => "close-creation",
        Control::ThisDevice => "this-computer",
        Control::Relay => "relay",
        Control::DeviceRow(_) => "manage-device",
        Control::UnlinkDevice(_) => "manage-device-unlink",
        Control::HistoryBack => "history-back",
        Control::HistoryEvent(_) => "manage-history-event",
        Control::OlderHistory => "load-older",
        Control::Accounts => "accounts",
        Control::Unreachable(_) => "manage-cloud-repair",
        Control::DifferentCreate => "different-create",
        Control::DifferentSignIn => "different-sign-in",
        Control::DifferentOffline => "different-offline",
        Control::CloudPrimary => "cloud-primary",
        Control::CloudLearnMore => "learn-more",
        Control::ProfileSync(_) => "manage-sync",
        Control::ProfileBring(_) => "manage-bring",
        Control::ProfileSignIn(_) => "manage-sign-in",
        Control::Progress(_) => "manage-progress",
        Control::ProgressRow(_) => "manage-progress-row",
    })
}

/// What the tab needs from the window: its slots and where its content
/// region is in window coordinates.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Context {
    pub window: WindowSlot,
    pub view: ViewSlot,
    pub area: Rect,
}

impl Context {
    fn surface(&self, part: ControlPart) -> Occurrence {
        Occurrence::surface(self.window, self.view, PAGE, part)
    }

    fn profile(&self, id: ProfileId, part: ControlPart) -> Occurrence {
        Occurrence::model(self.window, self.view, id.as_uuid(), part)
    }

    /// **The one translation between the tab's space and the window's.**
    fn place(&self, r: Rect) -> Rect {
        Rect::new(r.x + self.area.x, r.y + self.area.y, r.w, r.h)
    }

    fn occurrence(&self, entry: Entry) -> Occurrence {
        match entry {
            Entry::Profile(id) => self.profile(id, parts::PROFILE),
            Entry::Hidden => self.surface(parts::HIDDEN),
            Entry::NewProfile => self.surface(parts::NEW_PROFILE),
            Entry::Devices => self.surface(parts::DEVICES),
            Entry::Cloud => self.surface(parts::CLOUD),
        }
    }

    fn control(&self, control: Control) -> Occurrence {
        let part = control_part(control);
        match control {
            Control::Unlink(index) => {
                Occurrence::instance(self.window, self.view, "manage-unlink", index as u64, part)
            }
            Control::Repair(index) => {
                Occurrence::instance(self.window, self.view, "manage-repair", index as u64, part)
            }
            // A device and its events are numbered by their place in Agent's
            // list, which is all the page knows them by.
            Control::DeviceRow(index) => {
                Occurrence::instance(self.window, self.view, "manage-device", index as u64, part)
            }
            Control::UnlinkDevice(index) => Occurrence::instance(
                self.window,
                self.view,
                "manage-device-unlink",
                index as u64,
                part,
            ),
            Control::HistoryEvent(index) => Occurrence::instance(
                self.window,
                self.view,
                "manage-history-event",
                index as u64,
                part,
            ),
            Control::Unreachable(index) => Occurrence::instance(
                self.window,
                self.view,
                "manage-cloud-repair",
                index as u64,
                part,
            ),
            // Koru Cloud's per-Profile controls are about that Profile, and
            // a progress row about its workspace.
            Control::ProfileSync(id)
            | Control::ProfileBring(id)
            | Control::ProfileSignIn(id)
            | Control::Progress(id) => self.profile(id, part),
            Control::ProgressRow(id) => {
                Occurrence::model(self.window, self.view, id.as_uuid(), part)
            }
            // A workspace's row and its trailing control are about that
            // workspace, wherever it moves in the list.
            Control::WorkspaceRow(id)
            | Control::WorkspaceMore(id)
            | Control::Unarchive(id)
            | Control::RestoreWorkspace(id)
            | Control::UnassignedRow(id)
            | Control::UnassignedMore(id)
            | Control::RestoreUnassignedProfile(id) => {
                Occurrence::model(self.window, self.view, id.as_uuid(), part)
            }
            // A deleted Profile's *Restore* is about that Profile.
            Control::RestoreDeletedProfile(id) => self.profile(id, part),
            _ => self.surface(part),
        }
    }
}

impl ManageView {
    /// The tab, as a screen reader sees it right now.
    pub(crate) fn semantics(&self, cx: &Context) -> SemanticNode {
        let laid = self.nav_layout();
        let options: Vec<SemanticNode> = laid
            .rows
            .iter()
            .enumerate()
            .map(|(index, (entry, rect))| {
                SemanticNode::new(cx.occurrence(*entry), Role::ListBoxOption)
                    .with_label(self.entry_name(*entry))
                    .with_bounds(cx.place(*rect))
                    .with_position(index)
                    .with_actions([Action::Click, Action::Focus])
            })
            .collect();
        let nav = SemanticNode::new(cx.surface(parts::NAV), Role::ListBox)
            .with_label("Manage")
            .with_bounds(cx.place(self.sidebar()))
            .with_size_of_set(options.len())
            .with_children(options);
        let heading = SemanticNode::new(cx.surface(parts::HEADING), Role::Heading)
            .with_label(self.page_title())
            .with_level(1)
            .with_bounds(cx.place(self.heading_rect()));
        let mut children = vec![nav, heading];
        let scrolled = |r: Rect| cx.place(Rect::new(r.x, r.y - self.page_scroll, r.w, r.h));
        let button = |placed: &super::profile_page::Placed| {
            let node = match placed.look {
                Look::Toggle(_) => SemanticNode::new(cx.control(placed.control), Role::Switch)
                    .with_toggled(self.toggled(placed.control)),
                _ => SemanticNode::new(cx.control(placed.control), Role::Button),
            };
            node.with_label(self.control_name(placed))
                .with_bounds(scrolled(placed.rect))
                .with_actions([Action::Click, Action::Focus])
        };
        let in_list = |control: Control| {
            matches!(
                control,
                Control::WorkspaceRow(_)
                    | Control::WorkspaceMore(_)
                    | Control::Unarchive(_)
                    | Control::RestoreWorkspace(_)
            )
        };
        let in_devices =
            |control: Control| matches!(control, Control::DeviceRow(_) | Control::UnlinkDevice(_));
        let mut listed = false;
        let mut devices_listed = false;
        for placed in &self.laid.controls {
            if in_devices(placed.control) {
                if !devices_listed {
                    devices_listed = true;
                    children.push(self.devices_list(cx, &button));
                }
                continue;
            }
            if !in_list(placed.control) {
                children.push(button(placed));
                continue;
            }
            // The rows are one ListBox, placed where the first of them is in
            // keyboard order; each row's `•••`, *Unarchive* or *Restore* is
            // its child.
            if !listed {
                listed = true;
                children.push(self.workspaces_list(cx, &button));
            }
        }
        if self.content_page_open() {
            let form = self.form_rect();
            let dx = cx.area.x + form.x;
            let dy = cx.area.y + form.y - self.page_scroll;
            let mut form_nodes = self.c.creation_semantics(cx.window, cx.view);
            fn translate(node: &mut SemanticNode, dx: f32, dy: f32) {
                if let Some(bounds) = node.bounds.as_mut() {
                    bounds.x += dx;
                    bounds.y += dy;
                }
                for child in &mut node.children {
                    translate(child, dx, dy);
                }
            }
            for node in &mut form_nodes {
                translate(node, dx, dy);
            }
            children.extend(form_nodes);
        }
        if self.switcher.open {
            children.push(
                SemanticNode::new(cx.surface(parts::DIALOG), Role::Dialog)
                    .with_label("Accounts")
                    .with_bounds(cx.place(self.page_area())),
            );
        } else if let Some(dialog) = &self.dialog {
            children.push(
                SemanticNode::new(cx.surface(parts::DIALOG), Role::Dialog)
                    .with_label(dialog.title().to_owned())
                    .with_bounds(cx.area),
            );
        }
        SemanticNode::new(cx.surface(parts::PAGE), Role::Group)
            .with_label("Manage")
            .with_bounds(cx.area)
            .with_children(children)
    }

    /// The Workspaces section's rows as one ListBox: an option per
    /// workspace, named as drawn — its name, *pinned*, *open*, its sub-line —
    /// with its trailing control as a Button child.
    fn workspaces_list(
        &self,
        cx: &Context,
        button: &dyn Fn(&super::profile_page::Placed) -> SemanticNode,
    ) -> SemanticNode {
        let Some(section) = self.laid.workspaces.as_ref() else {
            return SemanticNode::new(cx.surface(parts::WORKSPACES), Role::ListBox);
        };
        let place = |r: Rect| cx.place(Rect::new(r.x, r.y - self.page_scroll, r.w, r.h));
        let options: Vec<SemanticNode> = section
            .rows()
            .enumerate()
            .map(|(index, (list, row))| {
                let mut said = row.full_name.clone();
                for (_, tag) in &row.tags {
                    said.push_str(", ");
                    said.push_str(&tag.to_lowercase());
                }
                match list.kind {
                    super::workspaces::ListKind::Archived => said.push_str(", archived"),
                    super::workspaces::ListKind::Trash => said.push_str(", in trash"),
                    super::workspaces::ListKind::Active => {}
                }
                if let Some((_, sub)) = &row.sub {
                    said.push_str(", ");
                    said.push_str(sub);
                }
                let trailing: Vec<SemanticNode> = [
                    Control::WorkspaceMore(row.id),
                    Control::Unarchive(row.id),
                    Control::RestoreWorkspace(row.id),
                ]
                .into_iter()
                .filter_map(|control| self.laid.placed(control))
                .map(button)
                .collect();
                SemanticNode::new(
                    cx.control(Control::WorkspaceRow(row.id)),
                    Role::ListBoxOption,
                )
                .with_label(said)
                .with_bounds(place(row.rect))
                .with_position(index)
                .with_actions([Action::Click, Action::Focus])
                .with_children(trailing)
            })
            .collect();
        let whole = section.lists.first().map_or(section.rect, |first| {
            let last = section.lists.last().unwrap_or(first);
            Rect::new(
                first.card.x,
                first.card.y,
                first.card.w,
                (last.card.y + last.card.h - first.card.y).max(0.0),
            )
        });
        SemanticNode::new(cx.surface(parts::WORKSPACES), Role::ListBox)
            .with_label(super::workspaces::TITLE)
            .with_bounds(place(whole))
            .with_size_of_set(options.len())
            .with_children(options)
    }

    /// *Linked devices*' rows as one ListBox: an option per device, named as
    /// drawn — its name and its status line — with its *Unlink…* as a
    /// Button child.
    fn devices_list(
        &self,
        cx: &Context,
        button: &dyn Fn(&super::profile_page::Placed) -> SemanticNode,
    ) -> SemanticNode {
        let place = |r: Rect| cx.place(Rect::new(r.x, r.y - self.page_scroll, r.w, r.h));
        let Some(laid) = self.laid.devices.as_ref() else {
            return SemanticNode::new(cx.surface(parts::DEVICES_LIST), Role::ListBox);
        };
        let options: Vec<SemanticNode> = laid
            .rows
            .iter()
            .enumerate()
            .filter_map(|(position, row)| {
                let index = row.index?;
                let trailing: Vec<SemanticNode> = self
                    .laid
                    .placed(Control::UnlinkDevice(index))
                    .map(button)
                    .into_iter()
                    .collect();
                Some(
                    SemanticNode::new(cx.control(Control::DeviceRow(index)), Role::ListBoxOption)
                        .with_label(format!("{}, {}", row.full_name, row.sub_text))
                        .with_bounds(place(row.rect))
                        .with_position(position)
                        .with_actions([Action::Click, Action::Focus])
                        .with_children(trailing),
                )
            })
            .collect();
        let whole = laid.list_card.unwrap_or(laid.head.title);
        SemanticNode::new(cx.surface(parts::DEVICES_LIST), Role::ListBox)
            .with_label("Linked devices")
            .with_bounds(place(whole))
            .with_size_of_set(options.len())
            .with_children(options)
    }

    /// What a reader hears for a page control: its label, or — for this
    /// computer's row — what the row says, and for Koru Cloud's per-Profile
    /// controls which Profile they are about.
    fn control_name(&self, placed: &super::profile_page::Placed) -> String {
        let profile_name = |id: ProfileId| {
            self.c
                .profiles()
                .iter()
                .find(|row| row.id == id)
                .map_or_else(|| "Profile".to_owned(), |row| row.name.clone())
        };
        match placed.control {
            Control::ProfileSignIn(id) if placed.label == super::cloud::CONNECT => {
                format!("Connect {}\u{2026}", profile_name(id))
            }
            Control::ProfileSignIn(id) => format!("Sign in to {}\u{2026}", profile_name(id)),
            // *Restore* names the deleted Profile it brings back, since a
            // reader may reach the button without the row.
            Control::RestoreDeletedProfile(id) => {
                let name = self
                    .w
                    .borrow()
                    .deleted_profiles()
                    .iter()
                    .find(|row| row.profile == super::workspaces::catalog_profile(id))
                    .and_then(|row| row.name.clone())
                    .unwrap_or_else(|| profile_name(id));
                format!("{}: {name}", placed.label)
            }
            Control::Progress(id) => format!("{}, {}", placed.label, profile_name(id)),
            Control::ThisDevice => self
                .laid
                .devices
                .as_ref()
                .and_then(|laid| laid.this_row.as_ref())
                .map_or_else(
                    || placed.label.clone(),
                    |row| format!("{}, this computer, {}", row.full_name, row.sub_text),
                ),
            // The button reads *Unlink…*; its name says which device, since a
            // reader may reach it without the row.
            Control::UnlinkDevice(index) => self
                .laid
                .devices
                .as_ref()
                .and_then(|laid| laid.rows.iter().find(|row| row.index == Some(index)))
                .map_or_else(
                    || placed.label.clone(),
                    |row| format!("Unlink {}\u{2026}", row.full_name),
                ),
            _ => placed.label.clone(),
        }
    }

    /// A switch's state: the relay's on *Linked devices*, the Profile's
    /// Cloud sync otherwise.
    fn toggled(&self, control: Control) -> bool {
        match control {
            Control::Relay => self.devices.relay().is_some_and(|relay| relay.enabled),
            Control::ProfileSync(profile) => self.c.cloud_sync_on(profile),
            // Not on this computer, so it does not sync here yet.
            Control::ProfileBring(_) => false,
            _ => self.c_profile_syncs(),
        }
    }

    /// Whether the page's Profile syncs, for its switch's state.
    fn c_profile_syncs(&self) -> bool {
        self.laid
            .profile
            .is_some_and(|profile| self.c.cloud_sync_on(profile))
    }

    /// What a reader hears for one row: what is drawn on it.
    fn entry_name(&self, entry: Entry) -> String {
        let profile_name = |id: ProfileId| {
            self.c
                .profiles()
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.name.clone())
        };
        match entry {
            Entry::Profile(id) => {
                let name = profile_name(id).unwrap_or_else(|| "Profile".into());
                let mut said = name;
                if self.c.current() == Some(id) {
                    said.push_str(", open");
                }
                if self.c.needs_attention(id) {
                    said.push_str(", needs attention");
                }
                said
            }
            Entry::Hidden => format!("{} Profiles hidden", self.c.hidden_profile_count()),
            Entry::NewProfile => "New profile".into(),
            Entry::Devices => format!("Linked devices, {}", self.linked_count()),
            Entry::Cloud => "Koru Cloud".into(),
        }
    }

    /// Everything the tree is built from, folded into the shell's revision.
    pub(crate) fn revise(&self, rev: &mut Revision) {
        if self.content_page_open()
            && let Some(form) = self.c.creation_form()
        {
            rev.u64(form.semantics_revision());
        }
        match self.page {
            Some(Page::Profile(id)) => {
                rev.byte(1).uuid(id.as_uuid());
            }
            Some(Page::NewProfile) => {
                rev.byte(2);
            }
            Some(Page::Devices) => {
                rev.byte(3);
            }
            Some(Page::Cloud) => {
                rev.byte(4);
            }
            None => {
                rev.byte(0);
            }
        }
        let laid = self.nav_layout();
        rev.u64(laid.rows.len() as u64);
        for (entry, _) in &laid.rows {
            rev.str(&self.entry_name(*entry));
        }
        rev.str(&self.page_title());
        rev.u64(self.laid.controls.len() as u64);
        for placed in &self.laid.controls {
            rev.str(&placed.label);
            rev.f32(placed.rect.x).f32(placed.rect.y);
        }
        rev.u64(u64::from(self.c_profile_syncs()));
        rev.bool(self.switcher.open)
            .opt_uuid(self.disclosed.map(|id| id.as_uuid()));
        if let Some(laid) = &self.laid.cloud {
            for row in &laid.rows {
                rev.str(&row.sub_text)
                    .bool(self.c.cloud_sync_on(row.profile));
            }
        }
        rev.bool(self.devices.relay().is_some_and(|relay| relay.enabled))
            .bool(self.devices.history_open())
            .bool(self.device_rename.is_some());
        if let Some(laid) = &self.laid.devices {
            for row in laid.this_row.iter().chain(&laid.rows) {
                rev.str(&row.sub_text);
            }
        }
        rev.bool(self.w.borrow().is_confirming())
            .bool(self.w.borrow().show_archived())
            .bool(self.w.borrow().show_trash())
            .opt_uuid(self.w.borrow().selected().map(|id| id.as_uuid()));
        if let Some(section) = &self.laid.workspaces {
            for (_, row) in section.rows() {
                rev.str(&row.full_name);
                rev.u64(row.tags.len() as u64);
            }
        }
        match (&self.dialog, self.page_focus) {
            (Some(dialog), _) => {
                rev.str(dialog.title());
            }
            (None, Some(control)) => {
                rev.str(&format!("{control:?}"));
            }
            (None, None) => {
                rev.byte(0);
            }
        }
        rev.u64(self.focus_order().len() as u64);
        rev.f32(self.nav_scroll).f32(self.page_scroll);
        rev.f32(self.viewport.0).f32(self.viewport.1);
        match self.focus {
            Some(entry) => {
                rev.str(&format!("{entry:?}"));
            }
            None => {
                rev.byte(0);
            }
        }
    }

    /// Which control has the keyboard: the question up, the focused page
    /// control, the focused nav row, or the tab.
    pub(crate) fn accessible_focus(&self, cx: &Context) -> Occurrence {
        if self.dialog.is_some() || self.w.borrow().is_confirming() || self.switcher.open {
            return cx.surface(parts::DIALOG);
        }
        if self.content_page_open()
            && let Some(form) = self.c.creation_form()
            && let Some(focus) = form.accessibility_focus(cx.window, cx.view)
        {
            return focus;
        }
        match (self.page_focus, self.focus) {
            (Some(control), _) => cx.control(control),
            (None, Some(entry)) => cx.occurrence(entry),
            (None, None) => cx.surface(parts::PAGE),
        }
    }

    /// Do what a client asked, through the press that already exists.
    pub(crate) fn accessibility_action(
        &mut self,
        cx: &Context,
        target: Occurrence,
        action: Action,
    ) -> bool {
        if !self.no_question_up() || self.switcher.open {
            return false;
        }
        let control = self
            .laid
            .controls
            .iter()
            .map(|placed| placed.control)
            .find(|control| cx.control(*control) == target);
        if let Some(control) = control {
            return match action {
                Action::Focus => {
                    self.page_focus = Some(control);
                    self.focus = None;
                    self.reveal_page_focus();
                    true
                }
                // A row's default action is the double-click's: open it.
                Action::Click => match control {
                    Control::WorkspaceRow(id) => {
                        self.page_focus = Some(control);
                        self.focus = None;
                        self.open_row(id)
                    }
                    _ => self.activate_control(control),
                },
                _ => false,
            };
        }
        let entry = self
            .nav_layout()
            .rows
            .into_iter()
            .map(|(entry, _)| entry)
            .find(|entry| cx.occurrence(*entry) == target);
        let Some(entry) = entry else {
            return false;
        };
        match action {
            Action::Focus => {
                self.focus = Some(entry);
                self.page_focus = None;
                self.reveal_focus();
                true
            }
            Action::Click => self.activate(entry),
            _ => false,
        }
    }

    pub(crate) fn creation_accessibility_action(
        &mut self,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        if !self.content_page_open() || !self.no_question_up() || self.switcher.open {
            return false;
        }
        let done = self
            .c
            .with_creation_form(|form| form.accessibility_action(target, action, data))
            .unwrap_or(false);
        if done {
            self.sync();
            self.size_creation_form();
            self.reveal_creation_focus();
        }
        done
    }
}
