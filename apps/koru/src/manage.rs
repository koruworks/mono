// SPDX-License-Identifier: MPL-2.0

//! The Manage tab: one surface for this device's Profiles, the New profile
//! form, its linked devices and Koru Cloud — a nav down the left, the chosen
//! page beside it, laid out and styled as Settings is (the shared
//! `koru_ui::PageShell`; maintainer, 2026-10-07).
//!
//! The domain side lives in [`profiles`]: a controller a view drives. A view
//! turns a click or a key into an `Intent`, performs it, drains the `Effect`s
//! the controller raises for the window to carry out, and draws from the
//! controller's state. Nothing in that module measures text, owns a viewport
//! or knows where a button is: it was extracted from the Manage Profiles tab
//! this one replaced (ADR-449-onemanagetab), and its suites came with it.
//!
//! This file is the tab itself — what it owns, the window's fan-out into its
//! controller, and the `View` it is to the container. Where things go is
//! [`nav`] and [`page`]; a Profile's page is [`profile_page`] with its
//! Workspaces in [`workspaces`] and its *Sign-in and sync* in [`sign_in`];
//! *Linked devices* is [`devices`]; *Koru Cloud* — the account switcher, the
//! subscription, which Profiles sync and their progress — is [`cloud`]; the
//! questions it asks are [`dialogs`];
//! the embedded create form is [`new_profile`]; what a screen reader is
//! offered is [`accessibility`].
//!
//! **What only the window can do leaves as a [`Request`]**: renaming a
//! Profile in place (the window's rename field) and its icon and color (the
//! window's panel), opening a workspace, the workspace menu, the name prompt
//! for a new one and the import menu — and, from *Linked devices*, whatever
//! Agent does with a device ([`DeviceRequest`]). Everything about accounts and Profiles
//! leaves as the controller's `Effect`s, as it always has; everything about a
//! workspace's catalog row is the Workspaces controller's own Gateway call.
//!
//! **English only for now** (maintainers' decision 15): the strings this tab adds are
//! literals, with no translation rows.

pub(crate) mod accessibility;
pub(crate) mod cloud;
pub(crate) mod devices;
mod dialogs;
mod nav;
mod new_profile;
mod page;
mod profile_page;
pub mod profiles;
mod sign_in;
#[cfg(test)]
mod tests;
pub(crate) mod workspaces;

pub use devices::DeviceRequest;
pub use page::Page;
#[cfg(test)]
pub(crate) use profile_page::Control;

use koru_core::schema::WorkspaceDoc;
use koru_core::{ProfileId, ProfileSummary};
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, inspect, scene, theme};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use profiles::{
    AccountStage, Confirm, ConfirmOutcome, Effect, HostedDeleteStage, Intent, ProfilesController,
};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// How far one wheel notch moves the nav or the page.
const LINE: f32 = 40.0;

/// What the tab asks of the window, which owns the field, the panel, the
/// menus and what a workspace opening means.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// The Profile header's name was pressed: rename it in place.
    Rename(ProfileId),
    /// The Profile header's tile was pressed: the icon and color panel.
    ChangeIconAndColor(ProfileId),
    /// *New workspace* on a Profile's page: the name prompt, then the
    /// workspace made in that Profile.
    NewWorkspace(ProfileId),
    /// *Import…*: the import menu, into the open workspace.
    ImportContent,
    /// A workspace row was double-clicked or Entered: open it here, switching
    /// Profile first when it is another Profile's.
    OpenWorkspace(koru_core::WorkspaceId),
    /// A row's `•••`: the window's workspace menu, its top-right corner at
    /// `at` (the button's bottom-right, in the tab's coordinates, scrolled).
    WorkspaceMenu {
        workspace: koru_core::WorkspaceId,
        at: (f32, f32),
    },
    /// What the *Linked devices* page asks of Agent or the window.
    Device(DeviceRequest),
}

/// The window owns the controller; Manage borrows the same state for its page.
pub(crate) type SharedWorkspaces =
    std::rc::Rc<std::cell::RefCell<koru_workspaces_ui::WorkspacesController>>;

pub struct ManageView {
    c: ProfilesController,
    /// The window controller borrowed by this page. Its link, confirmations,
    /// and completion handling continue independently of this tab.
    w: SharedWorkspaces,
    /// The workspace this window has open, for its row's *Open* tag.
    current_workspace: Option<koru_core::WorkspaceId>,
    /// The page showing. `None` only until the window has said which Profile
    /// is open, when it becomes that Profile's page.
    page: Option<Page>,
    /// What Agent last said about this computer, its linked devices and the
    /// relay — the window's snapshot — with this tab's own history page and
    /// presence deadline.
    devices: crate::mainview::linked_devices::LinkedDevices,
    /// This computer's rename field, while it is open.
    device_rename: Option<koru_ui::TextInput>,
    /// The account switcher, which replaces the page while it is open — Koru
    /// Cloud's *Accounts…* and the nav's *{n} Profiles hidden*.
    switcher: koru_workspace_host::account_ui::switcher::AccountSwitcher,
    /// The Profile whose sync progress is open on Koru Cloud (decision 20).
    /// One at a time: the poll reads one Profile's workspaces.
    disclosed: Option<ProfileId>,
    /// Whether the window has this tab in front: Cloud is read and progress
    /// polled only while it does.
    in_front: bool,
    viewport: (f32, f32),
    engine: TextEngine,
    pointer: (f32, f32),
    /// The compact nav's chips, from the last relayout — empty while the nav
    /// has its column — and the band they make across the top.
    nav_chips: Vec<(nav::Entry, koru_draw::geometry::Rect)>,
    band_h: f32,
    hover: Option<nav::Entry>,
    /// The nav row the keyboard is on, once a key has moved it. Never set
    /// at once with `page_focus`: the keyboard is in one place.
    focus: Option<nav::Entry>,
    /// Where a Profile's page put everything, for its current state.
    laid: profile_page::Laid,
    page_hover: Option<profile_page::Control>,
    /// The page control the keyboard is on.
    page_focus: Option<profile_page::Control>,
    /// The question over the tab, if any.
    dialog: Option<dialogs::Dialog>,
    requests: Vec<Request>,
    nav_scroll: f32,
    page_scroll: f32,
}

impl Default for ManageView {
    fn default() -> Self {
        Self::new()
    }
}

impl ManageView {
    pub fn new() -> Self {
        Self::with_workspace_controller(std::rc::Rc::new(std::cell::RefCell::new(
            koru_workspaces_ui::WorkspacesController::with_links(
                koru_workspaces_ui::WorkspaceLinks::inert(),
            ),
        )))
    }

    pub(crate) fn with_workspace_controller(w: SharedWorkspaces) -> Self {
        w.borrow_mut().set_lang(crate::format::lang());
        Self {
            c: ProfilesController::new(),
            w,
            current_workspace: None,
            page: None,
            devices: Default::default(),
            device_rename: None,
            switcher: Default::default(),
            disclosed: None,
            in_front: false,
            viewport: (900.0, 600.0),
            engine: TextEngine::new(),
            pointer: (-1.0, -1.0),
            nav_chips: Vec::new(),
            band_h: 0.0,
            hover: None,
            focus: None,
            laid: Default::default(),
            page_hover: None,
            page_focus: None,
            dialog: None,
            requests: Vec::new(),
            nav_scroll: 0.0,
            page_scroll: 0.0,
        }
    }

    /// Show `page`. A Profile's page selects it in the controller, so what
    /// the controller says about "the selected Profile" is about this page;
    /// the New profile page opens the embedded create form, as Manage
    /// Profiles' *Create Profile* does.
    pub fn show(&mut self, page: Page) {
        // **The Koru Cloud page is unreachable in a build that hides Koru
        // Cloud** (koru-features). Whoever asks for it — the nav, a pointer
        // on a Profile's page, the switcher's default before the window knows
        // its Profile — lands on a Profile's page instead: the first one this
        // device has, or *Linked devices* when there is none yet.
        let page = if page == Page::Cloud && !koru_features::cloud_sync() {
            self.c
                .profiles()
                .first()
                .map_or(Page::Devices, |row| Page::Profile(row.id))
        } else {
            page
        };
        if self.page != Some(page) {
            self.page_scroll = 0.0;
            self.page_focus = None;
            self.page_hover = None;
        }
        if self.page == Some(Page::Devices) && page != Page::Devices {
            self.leave_devices_page();
        }
        if page == Page::Devices {
            self.enter_devices_page();
        }
        if page != Page::Cloud {
            self.switcher.open = false;
        }
        self.page = Some(page);
        match page {
            Page::Profile(profile) => {
                self.c.perform(Intent::Select(profile));
            }
            Page::NewProfile => self.c.start_profile_creation(),
            Page::Devices | Page::Cloud => {}
        }
        self.sync();
    }

    /// Run `call` on the controller, then apply what it asked of the view.
    /// The window's whole fan-out comes through here.
    pub(crate) fn delegate<R>(&mut self, call: impl FnOnce(&mut ProfilesController) -> R) -> R {
        let result = call(&mut self.c);
        self.sync();
        result
    }

    pub fn take_effect(&mut self) -> Option<Effect> {
        self.delegate(|c| c.take_effect())
    }

    /// The next thing only the window can do, oldest first.
    pub fn take_request(&mut self) -> Option<Request> {
        (!self.requests.is_empty()).then(|| self.requests.remove(0))
    }

    /// Where a Profile's name is drawn on its page, in the tab's
    /// coordinates and scrolled — for the window's rename field, which goes
    /// over it. `None` unless that Profile's page is showing.
    pub fn profile_name_rect(&self, profile: ProfileId) -> Option<koru_draw::geometry::Rect> {
        if self.laid.profile != Some(profile) || self.page != Some(Page::Profile(profile)) {
            return None;
        }
        let name = self.laid.header.name;
        let line = theme::role::title().metrics.line_height;
        // At least a comfortable field's width, never past the title's.
        let w = name.w.max(220.0).min(self.laid.header.text_w);
        Some(koru_draw::geometry::Rect::new(
            name.x,
            name.y - self.page_scroll,
            w,
            line,
        ))
    }

    pub fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.c.set_client(client);
    }

    pub fn is_dragging(&self) -> bool {
        false
    }

    fn perform(&mut self, intent: Intent) -> bool {
        let taken = self.c.perform(intent);
        self.sync();
        taken
    }

    /// Apply what the controller asked of the view, and keep the page true
    /// to the controller's state.
    fn sync(&mut self) {
        // What the controller reads, and which Profile's progress it polls,
        // follow the page showing — before the page is laid out from it.
        self.apply_cloud_visibility();
        let hints = self.c.take_hints();
        if hints.reset_focus || hints.clear_hover {
            self.hover = None;
            self.page_hover = None;
        }
        if hints.reset_focus {
            self.page_focus = None;
        }
        self.settle_page();
        // A page that fell back follows too.
        self.apply_cloud_visibility();
        self.settle_dialogs();
        self.relayout();
        // After the layout, which says where the form starts.
        if hints.size_creation_form {
            self.size_creation_form();
        }
        if hints.clamp_scroll {
            self.clamp_scroll();
        }
    }

    /// A page that no longer names anything falls back to the open Profile's:
    /// a Profile removed from this device, or the New profile page once its
    /// form has finished — which lands on the Profile it made or chose.
    fn settle_page(&mut self) {
        let fallback = |c: &ProfilesController| {
            c.selected()
                .filter(|id| c.profiles().iter().any(|row| row.id == *id))
                .or(c.current())
                .map(Page::Profile)
        };
        let next = match self.page {
            None => self.c.current().map(Page::Profile),
            Some(Page::Profile(id)) if !self.c.profiles().iter().any(|row| row.id == id) => {
                if self.c.profiles().is_empty() {
                    return;
                }
                self.c.current().map(Page::Profile)
            }
            Some(Page::NewProfile) if !self.c.creating() => fallback(&self.c),
            _ => return,
        };
        if let Some(page) = next
            && Some(page) != self.page
        {
            self.page = Some(page);
            self.page_scroll = 0.0;
            // Not `Intent::Select`: nobody chose this Profile, and the notice
            // that explains why the page moved has to survive the move.
            if let Page::Profile(profile) = page
                && self.c.selected() != Some(profile)
            {
                self.c.follow_page(profile);
                let _ = self.c.take_hints();
            }
        }
    }

    /// Which page a nav row leads to, or what it does instead.
    fn activate(&mut self, entry: nav::Entry) -> bool {
        self.focus = Some(entry);
        self.page_focus = None;
        match entry {
            nav::Entry::Profile(profile) => self.show(Page::Profile(profile)),
            nav::Entry::NewProfile => self.show(Page::NewProfile),
            nav::Entry::Devices => self.show(Page::Devices),
            nav::Entry::Cloud => self.show(Page::Cloud),
            // *{n} Profiles hidden* opens the account switcher, which lives on
            // the Koru Cloud page (maintainers' decision 16).
            nav::Entry::Hidden => self.open_account_switcher(),
            nav::Entry::Other(profile) => {
                if self.c.connect_label(profile).is_none() {
                    return false;
                }
                self.perform(Intent::Connect(profile));
            }
        }
        true
    }

    /// Tab: the keyboard walks the nav's rows and then the page's controls,
    /// wrapping at either end. From nowhere it starts where the arrows do.
    fn move_tab_focus(&mut self, forward: bool) {
        #[derive(Clone, Copy, PartialEq)]
        enum Stop {
            Nav(nav::Entry),
            Page(profile_page::Control),
        }
        let order: Vec<Stop> = self
            .focus_order()
            .into_iter()
            .map(Stop::Nav)
            .chain(self.page_focus_order().into_iter().map(Stop::Page))
            .collect();
        let here = match (self.focus, self.page_focus) {
            (_, Some(control)) => Some(Stop::Page(control)),
            (Some(entry), None) => Some(Stop::Nav(entry)),
            (None, None) => None,
        };
        let Some(at) = here.and_then(|here| order.iter().position(|stop| *stop == here)) else {
            self.move_focus(forward);
            return;
        };
        let next = if forward {
            (at + 1) % order.len()
        } else {
            at.checked_sub(1).unwrap_or(order.len() - 1)
        };
        match order[next] {
            Stop::Nav(entry) => {
                self.focus = Some(entry);
                self.page_focus = None;
                self.reveal_focus();
            }
            Stop::Page(control) => {
                self.page_focus = Some(control);
                self.focus = None;
                self.reveal_page_focus();
            }
        }
    }

    /// Up and down: within the page's controls while the keyboard is on the
    /// page, along the nav's rows otherwise.
    fn move_arrow_focus(&mut self, forward: bool) {
        let Some(focus) = self.page_focus else {
            self.move_focus(forward);
            return;
        };
        let order = self.page_focus_order();
        let Some(at) = order.iter().position(|control| *control == focus) else {
            return;
        };
        let next = if forward {
            (at + 1) % order.len()
        } else {
            at.checked_sub(1).unwrap_or(order.len() - 1)
        };
        self.page_focus = order.get(next).copied();
        self.reveal_page_focus();
    }

    /// Enter or Space on whatever the keyboard is on. On a workspace's row
    /// that is opening it, as a double-click is.
    fn press_focus(&mut self) {
        if let Some(profile_page::Control::WorkspaceRow(id)) = self.page_focus {
            self.w.borrow_mut().set_selected(Some(id));
            self.open_row(id);
        } else if let Some(control) = self.page_focus {
            self.activate_control(control);
        } else if let Some(entry) = self.focus {
            self.activate(entry);
        }
    }

    /// Move the keyboard along the nav's rows, wrapping at either end. The
    /// first press lands on the current page's row.
    fn move_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        if order.is_empty() {
            return;
        }
        let next = match self.focus.and_then(|f| order.iter().position(|e| *e == f)) {
            Some(i) if forward => (i + 1) % order.len(),
            Some(i) => i.checked_sub(1).unwrap_or(order.len() - 1),
            None => self
                .page
                .map(nav::Entry::of)
                .and_then(|here| order.iter().position(|e| *e == here))
                .unwrap_or_default(),
        };
        self.focus = order.get(next).copied();
        self.page_focus = None;
        self.reveal_focus();
    }

    fn content_page_open(&self) -> bool {
        self.page == Some(Page::NewProfile) && self.c.creating()
    }
}

impl View for ManageView {
    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        [self.c.maintenance_deadline(), self.devices_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    fn maintain(&mut self, _ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        // Presence is re-asked while *Linked devices* is in front; what comes
        // back is a snapshot, which repaints when it arrives.
        self.maintain_devices(now);
        self.delegate(|c| c.maintain(now))
    }

    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if let Some((confirm, answer)) = self.c.prompt_mut() {
            let outcome = confirm.key(key);
            return self.perform(answer(outcome)) || outcome == ConfirmOutcome::Keep;
        }
        if let Some(taken) = self.switcher_key(key, mods) {
            return taken;
        }
        if self.dialog.is_some() {
            return self.dialog_key(key);
        }
        if let Some(taken) = self.device_rename_key(key, text, mods) {
            return taken;
        }
        if self.content_page_open() {
            let changed = self
                .c
                .with_creation_form(|form| form.key(key, text, mods))
                .unwrap_or(false);
            self.sync();
            self.size_creation_form();
            self.reveal_creation_focus();
            return changed;
        }
        if !mods.is_empty() && mods != ModifiersState::SHIFT {
            return false;
        }
        match key {
            // Escape puts the Profile's account repair aside, or answers its
            // cancel, as Manage Profiles' did.
            Key::Named(NamedKey::Escape)
                if mods.is_empty()
                    && self
                        .laid
                        .profile
                        .is_some_and(|profile| self.repair_surface(profile).is_some()) =>
            {
                self.perform(Intent::DismissAccountSurface);
            }
            // Escape on a device's history is its back row.
            Key::Named(NamedKey::Escape) if mods.is_empty() && self.devices_escape() => {}
            Key::Named(NamedKey::ArrowDown) if mods.is_empty() => self.move_arrow_focus(true),
            Key::Named(NamedKey::Tab) if mods.is_empty() => self.move_tab_focus(true),
            Key::Named(NamedKey::ArrowUp) if mods.is_empty() => self.move_arrow_focus(false),
            Key::Named(NamedKey::Tab) if mods == ModifiersState::SHIFT => {
                self.move_tab_focus(false)
            }
            Key::Named(NamedKey::Enter) if mods.is_empty() => self.press_focus(),
            Key::Character(value) if value == " " && mods.is_empty() => self.press_focus(),
            _ => return false,
        }
        true
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        if let Some((confirm, answer)) = self.c.prompt_mut() {
            let outcome = confirm.mouse_down(&mut self.engine, self.viewport, x, y);
            return self.perform(answer(outcome)) || outcome == ConfirmOutcome::Keep;
        }
        if let Some(taken) = self.switcher_mouse_down(x, y) {
            return taken;
        }
        if self.dialog.is_some() {
            return self.dialog_mouse_down(x, y);
        }
        if let Some(taken) = self.device_rename_mouse_down(x, y) {
            return taken;
        }
        self.pointer = (x, y);
        if self.in_nav(x, y) {
            return match self.nav_hit(x, y) {
                Some(entry) => self.activate(entry),
                None => false,
            };
        }
        // The New profile page's X is the page's, over the form's head and
        // outside the form: it answers before the form is asked.
        if self.content_page_open()
            && let Some(control @ profile_page::Control::CloseCreation) = self.page_hit(x, y)
        {
            return self.activate_control(control);
        }
        if self.content_page_open() {
            let (fx, fy) = self.form_point(x, y);
            let changed = self
                .c
                .with_creation_form(|form| form.mouse_down(fx, fy))
                .unwrap_or(false);
            self.sync();
            self.size_creation_form();
            self.reveal_creation_focus();
            return changed;
        }
        match self.page_hit(x, y) {
            // A single press on this computer's row only focuses it; the
            // rename opens on the double-click.
            Some(control @ profile_page::Control::ThisDevice) => {
                self.page_focus = Some(control);
                self.focus = None;
                true
            }
            Some(control) => self.activate_control(control),
            None => false,
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some((confirm, _)) = self.c.prompt_mut() {
            return confirm.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        if self.switcher.open {
            self.pointer = (x, y);
            return false;
        }
        if self.dialog.is_some() {
            return self.dialog_mouse_move(x, y);
        }
        if self.device_rename_drag(x) {
            return true;
        }
        self.pointer = (x, y);
        let next = self.nav_hit(x, y);
        let mut changed = next != self.hover;
        self.hover = next;
        let page = self.page_hit(x, y);
        changed |= page != self.page_hover;
        self.page_hover = page;
        if self.content_page_open() {
            let (fx, fy) = self.form_point(x, y);
            if let Some(form) = self.c.creation_form_mut() {
                changed |= form.mouse_move(fx, fy);
            }
        }
        changed
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        self.device_rename_release();
        if self.content_page_open()
            && let Some(form) = self.c.creation_form_mut()
        {
            form.mouse_up();
            return true;
        }
        false
    }

    /// A double-click on a workspace's row opens it; on this computer's row
    /// it renames this computer.
    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if !self.no_question_up() || self.content_page_open() {
            return false;
        }
        match self.page_hit(x, y) {
            Some(profile_page::Control::WorkspaceRow(id)) => self.open_row(id),
            Some(control) => self.device_double_click(control),
            None => false,
        }
    }

    /// Whether the archived list is open is window furniture the Workspaces
    /// tab persisted; this tab keeps it in the same place.
    fn restore(&mut self, _ws: &Workspace, settings: &koru_workspace_host::Settings) {
        self.w
            .borrow_mut()
            .set_show_archived(settings.show_archived);
        self.relayout();
    }

    fn capture(&self, settings: &mut koru_workspace_host::Settings) {
        settings.show_archived = self.w.borrow().show_archived();
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if self.switcher_scroll(delta) {
            return;
        }
        let (x, y) = self.pointer;
        let in_nav = self.in_nav(x, y);
        if !in_nav
            && self.content_page_open()
            && let Some(form) = self.c.creation_form_mut()
        {
            if form.scroll_by(delta) {
                return;
            }
        }
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * LINE,
            MouseScrollDelta::PixelDelta(point) => -point.y as f32,
        };
        if in_nav {
            self.nav_scroll += amount;
        } else {
            self.page_scroll += amount;
        }
        self.clamp_scroll();
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.relayout();
        self.size_creation_form();
        self.clamp_scroll();
    }

    fn cursor_icon(&self) -> winit::window::CursorIcon {
        if self.content_page_open()
            && !self.in_nav(self.pointer.0, self.pointer.1)
            && let Some(form) = self.c.creation_form()
        {
            return form.cursor_icon();
        }
        winit::window::CursorIcon::Default
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        self.engine = TextEngine::new();
        self.relayout();
        self.size_creation_form();
    }

    fn draw(
        &mut self,
        canvas: &mut Scene,
        _workspace: &Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.paint(canvas, root);
    }

    fn reseat(&mut self, _ws: &WorkspaceDoc) {}
}

impl ManageView {
    /// Everything, in paint order: the background, the nav, the page, and any
    /// question the controller has up over all of it.
    fn paint(&mut self, canvas: &mut Scene, root: Affine) {
        let (vw, vh) = self.viewport;
        let p = theme::p();
        scene::fill(canvas, root, p.bg, &scene::rect(0.0, 0.0, vw, vh));
        let _manage = inspect::scope(
            "KoruManage",
            "component",
            root,
            scene::rect(0.0, 0.0, vw, vh),
            |r| {
                r.property(
                    "implementation",
                    "apps/koru manage::ManageView",
                    "app implementation",
                );
                r.property(
                    "page",
                    self.page
                        .map_or("none".to_owned(), |page| format!("{page:?}")),
                    "instance state",
                );
                r.property("docs", "docs/product/INITIATIVES.md", "initiative");
            },
        );
        // Measured again each frame: the theme or the language may have
        // changed since the state last did.
        self.relayout();
        self.paint_page(canvas, root);
        self.paint_nav(canvas, root);
        self.paint_dialog(canvas, root);
        for confirm in self.c.prompts() {
            let _prompt = inspect::scope(
                "Manage prompt",
                "part",
                root,
                scene::rect(0.0, 0.0, vw, vh),
                |r| r.property("title", confirm.title(), "the controller's question"),
            );
            confirm.draw(canvas, root, &mut self.engine, self.viewport);
        }
    }

    fn clamp_scroll(&mut self) {
        let vh = self.viewport.1;
        let nav_max = self.nav_max_scroll();
        self.nav_scroll = self.nav_scroll.clamp(0.0, nav_max);
        let page_max = (self.page_content_height() - vh).max(0.0);
        self.page_scroll = self.page_scroll.clamp(0.0, page_max);
    }
}

#[cfg(test)]
impl ManageView {
    pub(crate) fn page(&self) -> Option<Page> {
        self.page
    }
    pub(crate) fn controller(&self) -> &ProfilesController {
        &self.c
    }
    pub(crate) fn focus(&self) -> Option<nav::Entry> {
        self.focus
    }
    pub(crate) fn page_focus(&self) -> Option<profile_page::Control> {
        self.page_focus
    }
    pub(crate) fn dialog(&self) -> Option<&dialogs::Dialog> {
        self.dialog.as_ref()
    }
    pub(crate) fn laid(&self) -> &profile_page::Laid {
        &self.laid
    }
    pub(crate) fn linked(&self) -> &crate::mainview::linked_devices::LinkedDevices {
        &self.devices
    }
    pub(crate) fn switcher_open(&self) -> bool {
        self.switcher.open
    }
    pub(crate) fn disclosed(&self) -> Option<ProfileId> {
        self.disclosed
    }
}

/// The current Profile's dot and the subscribed Cloud's dot: theme colors,
/// by name.
fn dot_current() -> vello::peniko::Color {
    theme::p().accent
}

fn dot_on() -> vello::peniko::Color {
    theme::accent_tone("green").map_or(theme::p().accent, |tone| tone.swatch)
}

/// What the window keeps for a Profile it lists: its id. The nav reads the
/// rest from the controller each frame.
fn profile_ids(c: &ProfilesController) -> Vec<ProfileId> {
    c.profiles().iter().map(|row| row.id).collect()
}
