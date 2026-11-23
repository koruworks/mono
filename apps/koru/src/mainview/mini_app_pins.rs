// SPDX-License-Identifier: MPL-2.0
//! Bounded pin I/O and the rail's pin-only menu. All durable work belongs to
//! the gateway; this host holds only scoped projections and frozen intents.
use super::*;
use koru_settings::pins::{Mutation, Pin, PinId, Request, Scope};
use koru_ui::{Menu, MenuItem};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};
use std::{
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};
use winit::keyboard::NamedKey;

#[derive(Clone)]
pub(super) enum Choice {
    Open(Pin),
    Unpin(Pin),
    Page(usize, String),
    Retry,
}
impl MenuItem for Choice {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Open(p) => Cow::Borrowed(&p.title),
            Self::Unpin(p) => Cow::Owned(format!(
                "{} · {}",
                label(koru_i18n::Key::MiniAppUnpin),
                p.title
            )),
            Self::Page(_, s) => Cow::Borrowed(s),
            Self::Retry => Cow::Borrowed(label(koru_i18n::Key::MiniAppPinRetry)),
        }
    }
}
fn label(key: koru_i18n::Key) -> &'static str {
    koru_i18n::t(crate::format::lang(), key)
}
enum Command {
    Activate {
        scope: Option<Scope>,
        epoch: u64,
    },
    Change {
        request: Request,
        tab: Option<u64>,
        epoch: u64,
    },
    Wake,
}
struct Answer {
    scope: Scope,
    epoch: u64,
    tab: Option<u64>,
    mutation: Option<Mutation>,
    result: std::result::Result<Vec<Pin>, String>,
}
#[derive(Default)]
pub(super) struct State {
    scope: Option<Scope>,
    sender: Option<SyncSender<Command>>,
    answers: Option<Receiver<Answer>>,
    wake: Option<crate::mini_app::artifact::Waker>,
    pub(super) menu: Option<Menu<Choice>>,
    pub(super) highlight: Option<PinId>,
    page: usize,
    pub(super) pending_writes: usize,
    epoch: u64,
    stop: Arc<AtomicBool>,
    authoritative: Vec<Pin>,
    failed_rail: Option<Request>,
    refresh_pending: bool,
    pub(super) menu_instance: u64,
    inspection_rows: std::collections::BTreeMap<(PinId, u8), u64>,
}
impl Drop for State {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(Command::Wake);
        }
    }
}
impl State {
    pub(super) fn set_waker(&mut self, wake: crate::mini_app::artifact::Waker) {
        self.wake = Some(wake);
    }
    fn send(&mut self, command: Command) -> bool {
        if self.sender.is_none() {
            if cfg!(test) {
                return false;
            }
            let (sender, commands) = mpsc::sync_channel(16);
            let (answers, receiver) = mpsc::sync_channel(16);
            let root = koru_workspace_host::cli::root_from_args();
            let wake = self.wake.clone();
            let event_sender = sender.clone();
            let stopping = self.stop.clone();
            if std::thread::Builder::new()
                .name("koru-mini-app-pins".into())
                .spawn(move || {
                    let scopes = [
                        "settings.read",
                        "settings.write",
                        "store.read_documents",
                        "artifacts.read",
                        "events.read",
                    ];
                    let dirty = Arc::new(AtomicBool::new(false));
                    let mut watch = None;
                    let mut active = None;
                    let mut epoch = 0;
                    loop {
                        let command = if stopping.load(Ordering::Acquire) {
                            match commands.try_recv() {
                                Ok(c) => c,
                                Err(_) => break,
                            }
                        } else {
                            match commands.recv() {
                                Ok(c) => c,
                                Err(_) => break,
                            }
                        };
                        // A failed initial discovery is retried on the next action,
                        // never retained as a permanently failed client.
                        let client = match &root {
                            Some(root) => koru_api_client::Client::open_at_with_scopes(
                                koru_channel::segment(),
                                root,
                                &scopes,
                            ),
                            None => koru_api_client::Client::open_with_scopes(
                                koru_channel::segment(),
                                &scopes,
                            ),
                        };
                        if watch.is_none()
                            && !stopping.load(Ordering::Acquire)
                            && let Ok(client) = &client
                        {
                            let invalidated = dirty.clone();
                            let events = event_sender.clone();
                            let stop = stopping.clone();
                            let reconnect_dirty = dirty.clone();
                            let reconnect_sender = event_sender.clone();
                            let reconnect_stop = stopping.clone();
                            // Cursor acquisition precedes the snapshot read.
                            watch = client
                                .events()
                                .on_with_reconnect(
                                    None,
                                    koru_api_client::events::Filter::names(
                                        koru_api_client::mini_app_pins::CHANGED_EVENT,
                                    ),
                                    move |stored| {
                                        if !koru_api_client::mini_app_pins::changed(
                                            stored.envelope.name.as_str(),
                                        ) {
                                            return;
                                        }
                                        if !stop.load(Ordering::Acquire)
                                            && !invalidated.swap(true, Ordering::AcqRel)
                                        {
                                            let _ = events.try_send(Command::Wake);
                                        }
                                    },
                                    move || {
                                        if !reconnect_stop.load(Ordering::Acquire)
                                            && !reconnect_dirty.swap(true, Ordering::AcqRel)
                                        {
                                            let _ = reconnect_sender.try_send(Command::Wake);
                                        }
                                    },
                                )
                                .ok();
                        }
                        let mut read = false;
                        match command {
                            Command::Activate { scope, epoch: next } => {
                                active = scope;
                                epoch = next;
                                read = true;
                            }
                            Command::Wake => {}
                            Command::Change {
                                request,
                                tab,
                                epoch,
                            } => {
                                let result = client
                                    .as_ref()
                                    .map_err(|_| "gateway unavailable".into())
                                    .and_then(|c| {
                                        c.mutate_mini_app_pins(&request).map_err(|e| e.to_string())
                                    });
                                if answers
                                    .send(Answer {
                                        scope: request.scope,
                                        epoch,
                                        tab,
                                        mutation: Some(request.mutation),
                                        result,
                                    })
                                    .is_err()
                                {
                                    stopping.store(true, Ordering::Release);
                                }
                                if let Some(wake) = &wake {
                                    wake();
                                }
                            }
                        }
                        read |= dirty.swap(false, Ordering::AcqRel);
                        if read
                            && !stopping.load(Ordering::Acquire)
                            && let Some(scope) = active.clone()
                        {
                            let result = client
                                .as_ref()
                                .map_err(|_| "gateway unavailable".into())
                                .and_then(|c| c.mini_app_pins(&scope).map_err(|e| e.to_string()));
                            if answers
                                .send(Answer {
                                    scope,
                                    epoch,
                                    tab: None,
                                    mutation: None,
                                    result,
                                })
                                .is_err()
                            {
                                stopping.store(true, Ordering::Release);
                            }
                            if let Some(wake) = &wake {
                                wake();
                            }
                        }
                    }
                })
                .is_err()
            {
                return false;
            }
            self.sender = Some(sender);
            self.answers = Some(receiver);
        }
        let writing = matches!(command, Command::Change { .. });
        let refreshing = matches!(command, Command::Activate { .. });
        if self.sender.as_ref().unwrap().try_send(command).is_ok() {
            if refreshing {
                self.refresh_pending = true;
            }
            if writing {
                self.pending_writes += 1;
            }
            true
        } else {
            false
        }
    }
}
impl MainView {
    pub(super) fn invalidate_mini_app_pin_scope(&mut self) {
        self.mini_app_pins.scope = None;
        self.mini_app_pins.epoch = self.mini_app_pins.epoch.wrapping_add(1);
        self.mini_app_pins.refresh_pending = false;
        self.mini_app_pins.menu = None;
        self.mini_app_pins.highlight = None;
        self.mini_app_pins.authoritative.clear();
        self.mini_app_pins.failed_rail = None;
        self.explore.replace_mini_app_pins(vec![]);
        self.explore.pins_menu_needed = false;
    }
    pub(super) fn refresh_mini_app_pins(&mut self) {
        if !self.mini_app_pins.refresh_pending && self.pin_scope().is_some() {
            let scope = self.pin_scope();
            let epoch = self.mini_app_pins.epoch;
            self.mini_app_pins.send(Command::Activate { scope, epoch });
        }
    }
    fn pin_scope(&self) -> Option<Scope> {
        self.pane_context.as_ref().and_then(|(_, p, _, w)| {
            w.map(|w| Scope {
                profile: p.to_string(),
                workspace: w.to_string(),
            })
        })
    }
    pub(super) fn maintain_mini_app_pins(&mut self) -> bool {
        let scope = self.pin_scope();
        let mut changed = false;
        if self.mini_app_pins.scope != scope {
            self.mini_app_pins.scope = scope.clone();
            self.mini_app_pins.menu = None;
            self.mini_app_pins.highlight = None;
            self.mini_app_pins.epoch = self.mini_app_pins.epoch.wrapping_add(1);
            self.mini_app_pins.authoritative.clear();
            self.mini_app_pins.failed_rail = None;
            self.explore.replace_mini_app_pins(vec![]);
            if !self.mini_app_pins.send(Command::Activate {
                scope: scope.clone(),
                epoch: self.mini_app_pins.epoch,
            }) && scope.is_some()
            {
                self.show_notice(label(koru_i18n::Key::MiniAppPinFailed).into());
            }
            changed = true;
        }
        if let Some((id, to)) = self.explore.take_pin_move() {
            self.change_pin(Mutation::Move { id, to }, None);
            changed = true;
        }
        let answers = self
            .mini_app_pins
            .answers
            .as_ref()
            .map(|r| r.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for answer in answers {
            if scope.as_ref() == Some(&answer.scope)
                && self.mini_app_pins.epoch == answer.epoch
                && answer.tab.is_none()
                && let Some(mutation) = &answer.mutation
            {
                let request = Request {
                    scope: answer.scope.clone(),
                    mutation: mutation.clone(),
                };
                if answer.result.is_err() {
                    self.mini_app_pins.failed_rail = Some(request);
                } else if self.mini_app_pins.failed_rail.as_ref() == Some(&request) {
                    self.mini_app_pins.failed_rail = None;
                }
            }
            if answer.mutation.is_some() {
                self.mini_app_pins.pending_writes =
                    self.mini_app_pins.pending_writes.saturating_sub(1);
            }
            let success = answer.result.is_ok();
            if let Some(tab) = answer.tab {
                for (_, group) in self.panes.iter_groups_mut() {
                    for candidate in group.tabs_mut().iter_mut() {
                        if candidate.id == tab
                            && let TabView::MiniApp(app) = &mut candidate.view
                        {
                            if app
                                .artifact
                                .owner
                                .is_some_and(|p| p.to_string() == answer.scope.profile)
                                && app.workspace.to_string() == answer.scope.workspace
                                && app.pin.in_flight
                                && app.pin.mutation == answer.mutation
                            {
                                app.pin.in_flight = false;
                                app.pin.failed = !success;
                                if success {
                                    app.pin.mutation = None;
                                }
                            }
                        }
                    }
                }
            }
            if scope.as_ref() == Some(&answer.scope) && self.mini_app_pins.epoch == answer.epoch {
                if answer.mutation.is_none() {
                    self.mini_app_pins.refresh_pending = false;
                }
                match answer.result {
                    Ok(pins) => {
                        self.mini_app_pins.authoritative = pins.clone();
                        if self.mini_app_pins.pending_writes == 0 {
                            self.adopt_pin_projection(pins);
                        }
                        changed = true;
                        self.sync_mini_app_pin_flags();
                        if self.mini_app_pins.menu.is_some() {
                            self.refresh_pin_menu();
                        }
                    }
                    Err(_) => {
                        self.adopt_pin_projection(self.mini_app_pins.authoritative.clone());
                        self.show_notice(label(koru_i18n::Key::MiniAppPinFailed).into());
                        changed = true;
                    }
                }
            }
        }
        let mut requests = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::MiniApp(app) = &mut tab.view {
                    if app.pin.requested && !app.pin.in_flight {
                        app.pin.requested = false;
                        if let (Some(profile), Some(mutation)) =
                            (app.artifact.owner, app.pin.mutation.clone())
                        {
                            app.pin.in_flight = true;
                            requests.push((
                                tab.id,
                                Request {
                                    scope: Scope {
                                        profile: profile.to_string(),
                                        workspace: app.workspace.to_string(),
                                    },
                                    mutation,
                                },
                            ));
                        } else {
                            app.pin.failed = true;
                        }
                    }
                }
            }
        }
        for (tab, request) in requests {
            let show_failure = scope.as_ref() == Some(&request.scope);
            if !self.mini_app_pins.send(Command::Change {
                request,
                tab: Some(tab),
                epoch: self.mini_app_pins.epoch,
            }) {
                for (_, group) in self.panes.iter_groups_mut() {
                    for candidate in group.tabs_mut().iter_mut() {
                        if candidate.id == tab
                            && let TabView::MiniApp(app) = &mut candidate.view
                        {
                            app.pin.in_flight = false;
                            app.pin.failed = true;
                        }
                    }
                }
                if show_failure {
                    self.show_notice(label(koru_i18n::Key::MiniAppPinFailed).into());
                }
            }
            changed = true;
        }
        self.explore.pins_busy = self.mini_app_pins.pending_writes > 0;
        self.explore.pins_menu_needed = self.mini_app_pins.failed_rail.is_some();
        self.sync_mini_app_pin_flags();
        changed
    }
    fn sync_mini_app_pin_flags(&mut self) {
        let scope = self.pin_scope();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::MiniApp(app) = &mut tab.view {
                    if scope.as_ref().is_some_and(|scope| {
                        app.workspace.to_string() == scope.workspace
                            && app
                                .artifact
                                .owner
                                .is_some_and(|p| p.to_string() == scope.profile)
                    }) && !app.pin.pending()
                        && !app.pin.failed
                    {
                        let version =
                            app.pin
                                .target
                                .as_ref()
                                .map(|p| p.version.clone())
                                .or_else(|| {
                                    app.artifact.saved.as_ref().map(|s| s.version.to_storage())
                                });
                        let found = version
                            .and_then(|v| {
                                self.explore.mini_app_pins.iter().find(|p| p.version == v)
                            })
                            .cloned();
                        app.pin.pinned = found.is_some();
                        if let Some(pin) = found {
                            app.pin.target = Some(pin);
                        }
                    }
                }
            }
        }
    }
    pub(super) fn adopt_pin_projection(&mut self, pins: Vec<Pin>) {
        self.mini_app_pins.authoritative = pins.clone();
        use crate::panes::sidebar::apps_rail::Hit;
        let focused = match self.focused_rail_identity() {
            Some(Hit::Pin(id)) => Some(id),
            Some(Hit::PinOverflow) => self.mini_app_pins.highlight,
            _ => None,
        };
        self.explore.replace_mini_app_pins(pins);
        if let Some(id) = focused {
            self.retain_pin_focus(id);
        }
    }
    fn change_pin(&mut self, mutation: Mutation, tab: Option<u64>) -> bool {
        let Some(scope) = self.pin_scope() else {
            return false;
        };
        let request = Request { scope, mutation };
        if !self.mini_app_pins.send(Command::Change {
            request: request.clone(),
            tab,
            epoch: self.mini_app_pins.epoch,
        }) {
            self.mini_app_pins.failed_rail = Some(request);
            self.adopt_pin_projection(self.mini_app_pins.authoritative.clone());
            self.show_notice(label(koru_i18n::Key::MiniAppPinFailed).into());
            return false;
        }
        self.explore.pins_busy = true;
        true
    }
    pub(super) fn open_mini_app_pin(&mut self, id: PinId) -> bool {
        let version = self
            .explore
            .mini_app_pins
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.version.parse().ok());
        version.is_some_and(|version| self.request_artifact_open(version))
    }
    pub(super) fn retain_pin_focus(&mut self, id: PinId) {
        use crate::panes::sidebar::apps_rail::Hit;
        let id = if self.explore.mini_app_pins.iter().any(|p| p.id == id) {
            Some(id)
        } else {
            self.explore.mini_app_pins.first().map(|p| p.id)
        };
        let controls = self.explore.rail_controls(self.rail_area());
        let index = controls
            .iter()
            .position(|h| Some(*h) == id.map(Hit::Pin))
            .or_else(|| controls.iter().position(|h| *h == Hit::PinOverflow))
            .or_else(|| controls.iter().position(|h| *h == Hit::Help));
        if let Some(index) = index {
            self.chrome_focus = Some(chrome_focus::ChromeStop::Apps(index));
        }
        self.mini_app_pins.highlight = id;
    }
    pub(super) fn open_mini_app_pins(&mut self, highlight: Option<PinId>) -> bool {
        self.refresh_mini_app_pins();
        if self.explore.mini_app_pins.is_empty() && self.mini_app_pins.failed_rail.is_none() {
            return false;
        }
        self.menu = None;
        self.explore.close_popover();
        if self.mini_app_pins.menu.is_none() {
            self.mini_app_pins.menu_instance = context_menu::mint_menu_instance(4);
            self.mini_app_pins.inspection_rows.clear();
        }
        self.mini_app_pins.highlight = highlight
            .or(self.mini_app_pins.highlight)
            .or_else(|| self.explore.mini_app_pins.first().map(|p| p.id));
        self.refresh_pin_menu();
        true
    }
    pub(super) fn refresh_pin_menu(&mut self) {
        let hovered = self
            .mini_app_pins
            .menu
            .as_ref()
            .and_then(|m| m.hover.and_then(|i| m.items().get(i)))
            .cloned();
        let selected = self
            .mini_app_pins
            .highlight
            .filter(|id| self.explore.mini_app_pins.iter().any(|p| p.id == *id))
            .or_else(|| self.explore.mini_app_pins.first().map(|p| p.id));
        self.mini_app_pins.highlight = selected;
        if selected.is_none() && self.mini_app_pins.failed_rail.is_none() {
            self.mini_app_pins.menu = None;
            return;
        }
        let count = self.pin_page_size();
        let index = self
            .explore
            .mini_app_pins
            .iter()
            .position(|p| Some(p.id) == selected)
            .unwrap_or(0);
        let page = index / count;
        self.mini_app_pins.page = page;
        let mut choices = vec![];
        if self
            .mini_app_pins
            .failed_rail
            .as_ref()
            .is_some_and(|r| Some(&r.scope) == self.mini_app_pins.scope.as_ref())
        {
            choices.push(Choice::Retry);
        }
        if page > 0 {
            choices.push(Choice::Page(
                page - 1,
                label(koru_i18n::Key::MiniAppPreviousPins).into(),
            ));
        }
        for pin in self
            .explore
            .mini_app_pins
            .iter()
            .skip(page * count)
            .take(count)
        {
            choices.push(Choice::Open(pin.clone()));
            choices.push(Choice::Unpin(pin.clone()));
        }
        if (page + 1) * count < self.explore.mini_app_pins.len() {
            choices.push(Choice::Page(
                page + 1,
                label(koru_i18n::Key::MiniAppNextPins).into(),
            ));
        }
        let rail = self.explore.apps_rail_rects(self.rail_area());
        let anchor = if rail.pin_overflow.w > 0.0 {
            rail.pin_overflow
        } else {
            rail.pins.last().map_or(rail.overflow, |(_, r)| *r)
        };
        let mut menu = Menu::new(&mut self.engine, (anchor.x + anchor.w, anchor.y), choices);
        for choice in menu.items() {
            let key = match choice {
                Choice::Open(p) => Some((p.id, 0)),
                Choice::Unpin(p) => Some((p.id, 1)),
                _ => None,
            };
            if let Some(key) = key {
                self.mini_app_pins
                    .inspection_rows
                    .entry(key)
                    .or_insert_with(|| context_menu::mint_menu_instance(0));
            }
        }
        menu.hover = hovered
            .as_ref()
            .and_then(|old| {
                menu.items().iter().position(|new| match (old, new) {
                    (Choice::Open(a), Choice::Open(b)) | (Choice::Unpin(a), Choice::Unpin(b)) => {
                        a.id == b.id
                    }
                    (Choice::Page(a, _), Choice::Page(b, _)) => a == b,
                    (Choice::Retry, Choice::Retry) => true,
                    _ => false,
                })
            })
            .or_else(|| {
                menu.items()
                    .iter()
                    .position(|c| matches!(c,Choice::Open(p) if Some(p.id)==selected))
            });
        if menu.hover.is_none() && !menu.items().is_empty() {
            menu.hover = Some(0);
        }
        self.mini_app_pins.menu = Some(menu);
    }
    fn close_pin_menu(&mut self) {
        self.mini_app_pins.menu = None;
        if let Some(id) = self.mini_app_pins.highlight {
            self.retain_pin_focus(id);
        }
    }
    pub(super) fn pin_menu_row_instance(&self, i: usize) -> Option<u64> {
        let choice = self.mini_app_pins.menu.as_ref()?.items().get(i)?;
        match choice {
            Choice::Open(p) => self.mini_app_pins.inspection_rows.get(&(p.id, 0)).copied(),
            Choice::Unpin(p) => self.mini_app_pins.inspection_rows.get(&(p.id, 1)).copied(),
            Choice::Page(page, _) => Some(
                self.mini_app_pins.menu_instance
                    + if *page < self.mini_app_pins.page {
                        1
                    } else {
                        2
                    },
            ),
            Choice::Retry => Some(self.mini_app_pins.menu_instance + 3),
        }
    }
    fn pin_page_size(&self) -> usize {
        ((((self.viewport.1 - 4.0 * koru_ui::menu::EDGE_MARGIN) / theme::ROW_H).floor() as usize)
            .saturating_sub(3)
            .max(2)
            / 2)
        .clamp(1, 8)
    }
    fn choose_pin(&mut self, choice: Choice) {
        match choice {
            Choice::Retry => {
                if let Some(request) = self.mini_app_pins.failed_rail.clone() {
                    if Some(&request.scope) == self.mini_app_pins.scope.as_ref() {
                        self.change_pin(request.mutation, None);
                    }
                }
            }
            Choice::Open(pin) => {
                self.mini_app_pins.highlight = Some(pin.id);
                self.close_pin_menu();
                self.open_mini_app_pin(pin.id);
            }
            Choice::Unpin(pin) => {
                self.change_pin(Mutation::Remove { id: pin.id }, None);
            }
            Choice::Page(page, _) => {
                let count = self.explore.mini_app_pins.len();
                let pages = self.pin_page_size();
                if let Some(pin) = self
                    .explore
                    .mini_app_pins
                    .get((page * pages).min(count.saturating_sub(1)))
                {
                    self.mini_app_pins.highlight = Some(pin.id);
                }
                self.refresh_pin_menu();
            }
        }
    }
    pub(super) fn pin_menu_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let choice = self
            .mini_app_pins
            .menu
            .as_ref()
            .and_then(|m| m.hover.and_then(|i| m.items().get(i)))
            .cloned();
        if let Some(Choice::Open(pin)) = &choice {
            let up = koru_keys::registry::get(koru_keys::ShortcutId::MoveRowUp)
                .chord
                .matches(key, text, mods);
            let down = koru_keys::registry::get(koru_keys::ShortcutId::MoveRowDown)
                .chord
                .matches(key, text, mods);
            if up || down {
                self.mini_app_pins.highlight = Some(pin.id);
                self.explore.nudge_pin(self.rail_area(), pin.id, up);
                self.refresh_pin_menu();
                return true;
            }
        }
        match key {
            Key::Named(NamedKey::Escape) => self.close_pin_menu(),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                if let Some(choice) = choice {
                    self.choose_pin(choice);
                }
            }
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) => {
                if let Some(menu) = &mut self.mini_app_pins.menu {
                    menu.step(
                        &vec![true; menu.items().len()],
                        if matches!(key, Key::Named(NamedKey::ArrowUp)) {
                            -1
                        } else {
                            1
                        },
                    );
                    if let Some(Choice::Open(pin)) = menu.hover.and_then(|i| menu.items().get(i)) {
                        self.mini_app_pins.highlight = Some(pin.id);
                    }
                }
            }
            _ => {}
        }
        true
    }
    pub(super) fn pin_menu_press(&mut self, x: f32, y: f32) -> bool {
        let Some(menu) = &self.mini_app_pins.menu else {
            return false;
        };
        let choice = menu
            .item_at(self.viewport, x, y)
            .and_then(|i| menu.items().get(i))
            .cloned();
        if let Some(choice) = choice {
            self.choose_pin(choice);
        } else {
            self.close_pin_menu();
        }
        true
    }
    pub(super) fn pin_menu_move(&mut self, x: f32, y: f32) -> bool {
        let Some(menu) = &mut self.mini_app_pins.menu else {
            return false;
        };
        menu.hover = menu.item_at(self.viewport, x, y);
        if let Some(Choice::Open(pin)) = menu.hover.and_then(|i| menu.items().get(i)) {
            self.mini_app_pins.highlight = Some(pin.id);
        }
        true
    }
    pub(super) fn pin_menu_semantics(&self, cx: &accessibility::Shell) -> Option<SemanticNode> {
        let menu = self.mini_app_pins.menu.as_ref()?;
        let children = menu
            .items()
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let occurrence = match c {
                    Choice::Open(p) => Occurrence::model(
                        cx.window,
                        cx.view,
                        uuid::Uuid::from_u128(p.id.0),
                        ControlPart::new("pin-menu-open"),
                    ),
                    Choice::Unpin(p) => Occurrence::model(
                        cx.window,
                        cx.view,
                        uuid::Uuid::from_u128(p.id.0),
                        ControlPart::new("pin-menu-unpin"),
                    ),
                    Choice::Retry => cx.surface("mini-app-pins", ControlPart::new("retry")),
                    Choice::Page(page, _) => cx.surface(
                        "mini-app-pins",
                        ControlPart::new(if *page < self.mini_app_pins.page {
                            "previous"
                        } else {
                            "next"
                        }),
                    ),
                };
                SemanticNode::new(occurrence, accesskit::Role::MenuItem)
                    .with_label(c.label().into_owned())
                    .with_bounds(menu.item_rect(self.viewport, i))
                    .with_actions([accesskit::Action::Click, accesskit::Action::Focus])
            })
            .collect();
        Some(
            SemanticNode::new(
                cx.surface("mini-app-pins", ControlPart::new("menu")),
                accesskit::Role::Menu,
            )
            .with_label(label(koru_i18n::Key::MiniAppPins))
            .with_bounds(menu.panel(self.viewport))
            .with_children(children),
        )
    }
    pub(super) fn pin_menu_focus(&self, cx: &accessibility::Shell) -> Option<Occurrence> {
        let menu = self.mini_app_pins.menu.as_ref()?;
        let tree = self.pin_menu_semantics(cx)?;
        menu.hover
            .and_then(|i| tree.children.get(i))
            .map(|n| n.occurrence)
            .or(Some(tree.occurrence))
    }
    pub(super) fn pin_menu_is_target(&self, target: Occurrence) -> bool {
        let cx = accessibility::Shell {
            window: target.window,
            view: target.view,
            viewport: self.viewport,
            lang: crate::format::lang(),
        };
        self.pin_menu_semantics(&cx)
            .is_some_and(|menu| menu.children.iter().any(|node| node.occurrence == target))
    }
    pub(super) fn revise_pin_menu(&self, rev: &mut koru_workspace_host::accessibility::Revision) {
        rev.bool(self.mini_app_pins.menu.is_some());
        if let Some(menu) = &self.mini_app_pins.menu {
            rev.u64(self.mini_app_pins.menu_instance)
                .u64(self.mini_app_pins.page as u64)
                .u64(menu.hover.map_or(u64::MAX, |i| i as u64));
            for choice in menu.items() {
                match choice {
                    Choice::Open(pin) => {
                        rev.byte(1)
                            .uuid(uuid::Uuid::from_u128(pin.id.0))
                            .str(&pin.title);
                    }
                    Choice::Unpin(pin) => {
                        rev.byte(2)
                            .uuid(uuid::Uuid::from_u128(pin.id.0))
                            .str(&pin.title);
                    }
                    Choice::Page(page, label) => {
                        rev.byte(3).u64(*page as u64).str(label);
                    }
                    Choice::Retry => {
                        rev.byte(4);
                    }
                }
            }
        }
    }
    pub(super) fn pin_menu_action(
        &mut self,
        target: Occurrence,
        action: accesskit::Action,
    ) -> bool {
        let Some(menu) = &self.mini_app_pins.menu else {
            return false;
        };
        let index =
            menu.items()
                .iter()
                .position(|c| match (c, target.object, target.part.name()) {
                    (
                        Choice::Open(p),
                        koru_workspace_host::accessibility::ObjectKey::Model(id),
                        "pin-menu-open",
                    )
                    | (
                        Choice::Unpin(p),
                        koru_workspace_host::accessibility::ObjectKey::Model(id),
                        "pin-menu-unpin",
                    ) => p.id.0 == id.as_u128(),
                    (
                        Choice::Retry,
                        koru_workspace_host::accessibility::ObjectKey::Surface("mini-app-pins"),
                        "retry",
                    ) => true,
                    (
                        Choice::Page(page, _),
                        koru_workspace_host::accessibility::ObjectKey::Surface("mini-app-pins"),
                        part,
                    ) => {
                        (*page < self.mini_app_pins.page && part == "previous")
                            || (*page > self.mini_app_pins.page && part == "next")
                    }
                    _ => false,
                });
        let Some(index) = index else {
            return false;
        };
        self.mini_app_pins.menu.as_mut().unwrap().hover = Some(index);
        match action {
            accesskit::Action::Focus => true,
            accesskit::Action::Click => {
                self.pin_menu_key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty());
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests;
