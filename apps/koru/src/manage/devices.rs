// SPDX-License-Identifier: MPL-2.0

//! The *Linked devices* page: this computer, its relay switch, every device
//! linked to it, and — a click away — one device's history.
//!
//! ```text
//! Linked devices                                  [Link a device…]
//! Linked devices have full access to every profile, workspace and
//! document on this computer.
//! This computer                                    a Settings caption
//! ┌────────────────────────────────────────────────────────────────┐
//! │ [WIN] Studio PC                                 This computer  │  double-click
//! │       0.0.4 · nightly · Windows 11 · double-click to rename    │  renames it
//! ├────────────────────────────────────────────────────────────────┤
//! │ Use relay                                                 (●) │
//! │ Off stops relayed connections for this device only; …          │
//! │ dev1.relay.koru.do — connected                                 │
//! └────────────────────────────────────────────────────────────────┘
//! Other devices
//! ┌────────────────────────────────────────────────────────────────┐
//! │ [AND] Pixel                                          Unlink…   │  a click opens
//! │       Linked (online, direct) · 0.0.4 · … · last synced …      │  its history
//! ├────────────────────────────────────────────────────────────────┤
//! │ [MAC] Laptop                                                   │  via another
//! │       Linked (offline) · last seen … · via Studio PC · …       │  device: no Unlink
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! **The data is the window's [`LinkedDevices`] snapshot**, handed to the tab
//! whole each time Agent answers; this page keeps its own copy because the
//! history page and the presence deadline are its own (maintainers' decision 13: a
//! device row drills into its history, in [`history`]). Everything Agent has
//! to do — rename this computer, the relay, unlink, a page of history — and
//! opening a document leave as [`DeviceRequest`]s for the window.
//!
//! **Presence is re-asked only while this page is in front.** Showing it
//! arms `LinkedDevices::watch_presence`; leaving it stops the deadline, and
//! the window asks only the tab in front for its deadline, so a hidden page
//! costs no wake.
//!
//! The status line is today's vocabulary, word for word (`device_status_
//! label`): online with its path, offline with last seen or the linked date,
//! *via* an introducer, *Not syncing · update …* leading in the severity's
//! tone, and the last sync.

use super::profile_page::{Control, Look, Placed};
use super::*;
use crate::mainview::linked_devices::LinkedDevices;
use koru_agent_wire::{LinkedDeviceView, RelayView};
use koru_core::DocumentId;
use koru_draw::geometry::Rect;
use koru_ui::button::{self, ActionButton, ActionStyle, ButtonRole};
use koru_ui::card::{Card, CardRow};
use koru_ui::text_paragraph::KoruTextParagraph;
use koru_ui::toggle::Toggle;
use koru_ui::{InputStyle, KoruTextTitle, TextInput};

pub(crate) mod history;
#[cfg(test)]
mod tests;

/// What the page asks of Agent or of the window.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceRequest {
    /// Re-ask for the list: the page was shown, or presence is due.
    Refresh,
    /// A page of one device's history; `before` is the older page's cursor.
    LoadHistory {
        device_id: String,
        before: Option<i64>,
    },
    /// Unlink a directly linked device, once the person confirmed.
    Unlink(String),
    RenameThisDevice(String),
    /// The relay switch's new, exact state.
    SetRelay(bool),
    /// A document event in a device's history was pressed.
    OpenDocument(DocumentId),
    /// *Link a device…*: the Link dialog, on its *New device* tab.
    LinkDevice,
}

const TITLE: &str = "Linked devices";
const LEAD: &str =
    "Linked devices have full access to every profile, workspace and document on this computer.";
const LINK: &str = "Link a device\u{2026}";
const UNLINK: &str = "Unlink\u{2026}";
const THIS_COMPUTER: &str = "This computer";
const OTHER_DEVICES: &str = "Other devices";
const RELAY: &str = "Use relay";
const RELAY_HINT: &str =
    "Off stops relayed connections for this device only; direct and same-network syncing continue.";
const NONE_LINKED: &str = "No other devices are linked.";
const RENAMING: &str = "Enter to save \u{b7} Esc to cancel";

/// A device's platform badge.
const BADGE_W: f32 = 38.0;
const BADGE_H: f32 = 22.0;
const BADGE_GAP: f32 = 12.0;
/// The rename field over this computer's name.
const FIELD_H: f32 = 30.0;

/// One device's row: this computer's (`index: None`) or a linked one's.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DeviceRow {
    /// Into `LinkedDevices::devices`; `None` for this computer.
    pub index: Option<usize>,
    pub rect: Rect,
    pub badge: Rect,
    pub badge_text: String,
    pub name: Rect,
    pub name_text: String,
    pub full_name: String,
    /// The status line, wrapped to `sub.w`.
    pub sub: Rect,
    pub sub_text: String,
    /// `build_severity`, for the status line's tone.
    pub severity: Option<String>,
    /// *This computer*, on this computer's row.
    pub tag: Option<Rect>,
}

/// The relay row: the shared `CardRow`, and the status line under its hint.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RelayRow {
    pub rect: Rect,
    /// The `CardRow` part of it, without the status line.
    pub row: Rect,
    pub text_w: f32,
    pub status: Rect,
    pub status_text: String,
}

/// Where everything on the page goes, unscrolled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DevicesLaid {
    /// The title and the lead.
    pub head: page::Head,
    /// This computer and the relay, their caption included.
    pub computer_card: Option<Rect>,
    pub this_row: Option<DeviceRow>,
    pub relay: Option<RelayRow>,
    /// Every linked device, the caption included.
    pub list_card: Option<Rect>,
    pub rows: Vec<DeviceRow>,
    /// *Other devices*, over the list or over the line saying there is none.
    pub others_caption: Option<Rect>,
    /// *No other devices are linked.*
    pub empty: Option<Rect>,
    /// While a device's history is open, the page is that.
    pub history: Option<history::HistoryLaid>,
}

impl Default for DevicesLaid {
    fn default() -> Self {
        Self {
            head: page::Head::default(),
            computer_card: None,
            this_row: None,
            relay: None,
            list_card: None,
            rows: Vec::new(),
            others_caption: None,
            empty: None,
            history: None,
        }
    }
}

impl ManageView {
    /// Adopt the window's latest snapshot. The page's own history page and
    /// presence deadline survive it; a rename in progress survives unless
    /// this computer stopped being known.
    pub fn set_linked_devices(&mut self, devices: &LinkedDevices) -> bool {
        let changed = !same_snapshot(&self.devices, devices);
        if !changed {
            return false;
        }
        self.devices.set_linked(
            devices.this_device().cloned(),
            devices.devices().to_vec(),
            devices.relay().cloned(),
        );
        if self.devices.this_device().is_none() {
            self.device_rename = None;
        }
        self.relayout();
        self.clamp_scroll();
        true
    }

    /// A page of one device's history arrived.
    pub fn set_device_history(
        &mut self,
        device_id: &str,
        events: Vec<koru_agent_wire::DeviceStateEventView>,
        next_before: Option<i64>,
    ) -> bool {
        let open = self
            .devices
            .history_device()
            .is_some_and(|device| device.device_id == device_id);
        self.devices.set_history(device_id, events, next_before);
        if open {
            self.relayout();
            self.clamp_scroll();
        }
        open
    }

    /// How many devices are linked, for the nav.
    pub(super) fn linked_count(&self) -> usize {
        self.devices.devices().len()
    }

    /// The page is coming into view: a fresh look at the list, starting the
    /// presence refresh.
    pub(super) fn enter_devices_page(&mut self) {
        self.devices.reset_view();
        self.device_rename = None;
        self.devices.watch_presence(std::time::Instant::now());
        self.requests.push(Request::Device(DeviceRequest::Refresh));
    }

    /// The page is going: no history, no rename, no presence deadline.
    pub(super) fn leave_devices_page(&mut self) {
        self.devices.reset_view();
        self.device_rename = None;
    }

    /// The presence deadline, while the page is showing.
    pub(super) fn devices_deadline(&self) -> Option<std::time::Instant> {
        (self.page == Some(Page::Devices))
            .then(|| self.devices.presence_deadline())
            .flatten()
    }

    /// Ask again for presence when it is due.
    pub(super) fn maintain_devices(&mut self, now: std::time::Instant) {
        if self.page != Some(Page::Devices) {
            return;
        }
        self.devices.maintain(now);
        if self.devices.take_presence_refresh() {
            self.requests.push(Request::Device(DeviceRequest::Refresh));
        }
    }

    /// The page's heading: the device's name while its history is open.
    pub(super) fn devices_title(&self) -> String {
        self.devices
            .history_device()
            .map_or_else(|| TITLE.to_owned(), |device| device.display_name.clone())
    }

    // ---- layout --------------------------------------------------------------

    pub(super) fn lay_devices_page(&mut self) -> profile_page::Laid {
        let column = self.column();
        let mut controls = Vec::new();
        if self.devices.history_device().is_some() {
            let (history, bottom) = self.lay_history(column, &mut controls);
            return profile_page::Laid {
                devices: Some(DevicesLaid {
                    history: Some(history),
                    ..Default::default()
                }),
                controls,
                height: bottom - column.y,
                ..Default::default()
            };
        }

        // The title and the lead, with *Link a device…* at the title line's
        // right end.
        let link_w = button::button_width(&mut self.engine, LINK);
        let head = self.lay_head(column, 0.0, link_w + BADGE_GAP, TITLE, LEAD);
        let line = theme::role::title().metrics.line_height;
        controls.push(Placed {
            control: Control::LinkDevice,
            rect: Rect::new(
                column.x + column.w - link_w,
                column.y + ((line - button::H) * 0.5).max(0.0),
                link_w,
                button::H,
            ),
            label: LINK.into(),
            look: Look::Button(ButtonRole::Primary),
        });
        let mut y = head.bottom();
        let header = Card::new(Some(THIS_COMPUTER)).header_h();

        // This computer and the relay, in one card under its caption.
        let card_top = y;
        y += header;
        let this_row = self.devices.this_device().cloned().map(|device| {
            let row = self.lay_device_row(column, y, &device, None, &mut controls);
            y = row.rect.y + row.rect.h;
            row
        });
        let relay = self.devices.relay().cloned().map(|relay| {
            let row = self.lay_relay_row(column, y, &relay, &mut controls);
            y = row.rect.y + row.rect.h;
            row
        });
        let computer_card =
            (y > card_top + header).then(|| Rect::new(column.x, card_top, column.w, y - card_top));
        match computer_card {
            Some(_) => y += Card::GAP,
            None => y = card_top,
        }

        // Every linked device under *Other devices*; a long list scrolls with
        // the page.
        let devices = self.devices.devices().to_vec();
        let mut rows = Vec::new();
        let list_top = y;
        let others_caption = Some(Rect::new(column.x, y, column.w, Card::TITLE_H));
        y += header;
        for (index, device) in devices.iter().enumerate() {
            let row = self.lay_device_row(column, y, device, Some(index), &mut controls);
            y = row.rect.y + row.rect.h;
            rows.push(row);
        }
        let list_card =
            (!rows.is_empty()).then(|| Rect::new(column.x, list_top, column.w, y - list_top));
        let empty = rows.is_empty().then(|| {
            let h = theme::role::ui().metrics.line_height;
            let rect = Rect::new(column.x, y, column.w, h);
            y += h;
            rect
        });
        profile_page::Laid {
            devices: Some(DevicesLaid {
                head,
                computer_card,
                this_row,
                relay,
                list_card,
                rows,
                others_caption,
                empty,
                history: None,
            }),
            controls,
            height: y - column.y,
            ..Default::default()
        }
    }

    fn lay_device_row(
        &mut self,
        column: Rect,
        y: f32,
        device: &LinkedDeviceView,
        index: Option<usize>,
        controls: &mut Vec<Placed>,
    ) -> DeviceRow {
        let x = column.x + CardRow::PAD_X;
        let right_edge = column.x + column.w - CardRow::PAD_X;
        // The row's one trailing thing: *Unlink…* for a directly linked
        // device, the *This computer* tag for this one, nothing for a device
        // that arrived through another (it goes when that link does).
        let unlink = (index.is_some() && device.introduced_by.is_none()).then(|| {
            ActionButton::new(UNLINK, ActionStyle::Destructive, None).width(&mut self.engine)
        });
        // The *This computer* tag: the one chip component, compact and
        // neutral, as the workspace list's tags are.
        let tag_chip = koru_ui::LabelChip::new(THIS_COMPUTER);
        let tag_w = index.is_none().then(|| tag_chip.width(&mut self.engine));
        let trailing_w = unlink.or(tag_w);
        let text_x = x + BADGE_W + BADGE_GAP;
        let text_right = trailing_w.map_or(right_edge, |w| right_edge - w - BADGE_GAP);
        let text_w = (text_right - text_x).max(1.0);
        let name_style = theme::role::row();
        let name_line = name_style.metrics.line_height;
        let name_text = self.engine.elided(&device.display_name, name_style, text_w);
        let name_w = self
            .engine
            .simple_layout(&name_text, name_style)
            .width()
            .min(text_w);
        let sub_text = match index {
            None if self.device_rename.is_some() => RENAMING.to_owned(),
            None => this_device_hint(device),
            Some(_) => device_status_label(device),
        };
        let sub_h = KoruTextParagraph::new(&sub_text, theme::role::meta(), "", theme::p().text_dim)
            .measure(&mut self.engine, text_w);
        let h = (CardRow::PAD_Y * 2.0 + name_line + 2.0 + sub_h).max(CardRow::MIN_H);
        let rect = Rect::new(column.x, y, column.w, h);
        let name = Rect::new(text_x, y + CardRow::PAD_Y, name_w, name_line);
        let sub = Rect::new(text_x, name.y + name_line + 2.0, text_w, sub_h);
        let badge = Rect::new(x, y + (h - BADGE_H) * 0.5, BADGE_W, BADGE_H);
        let (control, label) = match index {
            None => (Control::ThisDevice, device.display_name.clone()),
            Some(index) => (Control::DeviceRow(index), device.display_name.clone()),
        };
        controls.push(Placed {
            control,
            rect,
            label,
            look: Look::Row,
        });
        if let (Some(index), Some(w)) = (index, unlink) {
            controls.push(Placed {
                control: Control::UnlinkDevice(index),
                rect: Rect::new(
                    right_edge - w,
                    y + (h - ActionButton::H) * 0.5,
                    w,
                    ActionButton::H,
                ),
                // *Unlink…* alone (the review's note): the row names the device,
                // and the button is measured for this word — the name made it
                // overflow and elide. The screen reader still hears the name
                // (`control_name`).
                label: UNLINK.to_owned(),
                look: Look::Action(ActionStyle::Destructive),
            });
        }
        let tag_h = tag_chip.height();
        let tag = tag_w.map(|w| Rect::new(right_edge - w, y + (h - tag_h) * 0.5, w, tag_h));
        DeviceRow {
            index,
            rect,
            badge,
            badge_text: platform_badge(device),
            name,
            name_text,
            full_name: device.display_name.clone(),
            sub,
            sub_text,
            severity: device.build_severity.clone(),
            tag,
        }
    }

    fn lay_relay_row(
        &mut self,
        column: Rect,
        y: f32,
        relay: &RelayView,
        controls: &mut Vec<Placed>,
    ) -> RelayRow {
        let toggle = Toggle::new(relay.enabled);
        let text_w = CardRow::text_w(column.w, toggle.width());
        let base_h = CardRow::new(RELAY)
            .hint(RELAY_HINT)
            .height(&mut self.engine, text_w, Toggle::H)
            .max(CardRow::MIN_H);
        let row = Rect::new(column.x, y, column.w, base_h);
        controls.push(Placed {
            control: Control::Relay,
            rect: CardRow::control_slot(row, toggle.width(), Toggle::H),
            label: RELAY.into(),
            look: Look::Toggle(toggle),
        });
        let line = theme::role::meta().metrics.line_height;
        let status = Rect::new(
            column.x + CardRow::PAD_X,
            y + base_h - CardRow::PAD_Y + 2.0,
            text_w,
            line,
        );
        RelayRow {
            rect: Rect::new(column.x, y, column.w, base_h + line + 2.0),
            row,
            text_w,
            status,
            status_text: relay_status_label(relay),
        }
    }

    /// Where this computer's rename field goes, scrolled — over its name.
    pub(super) fn device_rename_rect(&self) -> Option<Rect> {
        let row = self.laid.devices.as_ref()?.this_row.as_ref()?;
        Some(Rect::new(
            row.name.x - 4.0,
            row.name.y + (row.name.h - FIELD_H) * 0.5 - self.page_scroll,
            row.sub.w + 4.0,
            FIELD_H,
        ))
    }

    // ---- input ---------------------------------------------------------------

    /// What a page control of this page does. `None` for one that is not.
    pub(super) fn activate_device_control(&mut self, control: Control) -> Option<bool> {
        let request = match control {
            Control::LinkDevice => Some(DeviceRequest::LinkDevice),
            // The keyboard's and a screen reader's way to rename; the
            // pointer's is the double-click.
            Control::ThisDevice => {
                self.open_device_rename();
                None
            }
            Control::Relay => self.devices.toggle_relay().map(DeviceRequest::SetRelay),
            Control::DeviceRow(index) => {
                let device_id = self.devices.open_history(index);
                self.page_scroll = 0.0;
                self.page_hover = None;
                device_id.map(|device_id| DeviceRequest::LoadHistory {
                    device_id,
                    before: None,
                })
            }
            Control::UnlinkDevice(index) => {
                self.ask_to_unlink_device(index);
                None
            }
            Control::HistoryBack => {
                let index = self.devices.history_device().and_then(|open| {
                    self.devices
                        .devices()
                        .iter()
                        .position(|device| device.device_id == open.device_id)
                });
                let keyboard = self.page_focus == Some(Control::HistoryBack);
                self.devices.close_history();
                self.page_scroll = 0.0;
                self.page_hover = None;
                self.relayout();
                // The keyboard lands back on the row it came from.
                if keyboard {
                    self.page_focus = index.map(Control::DeviceRow);
                    self.reveal_page_focus();
                }
                return Some(true);
            }
            Control::HistoryEvent(index) => self
                .history_document(index)
                .map(DeviceRequest::OpenDocument),
            Control::OlderHistory => {
                self.devices
                    .take_older_history()
                    .map(|(device_id, before)| DeviceRequest::LoadHistory {
                        device_id,
                        before: Some(before),
                    })
            }
            _ => return None,
        };
        if let Some(request) = request {
            self.requests.push(Request::Device(request));
        }
        self.relayout();
        self.clamp_scroll();
        Some(true)
    }

    fn open_device_rename(&mut self) {
        let Some(name) = self
            .devices
            .this_device()
            .map(|device| device.display_name.clone())
        else {
            return;
        };
        let mut field = TextInput::new(&mut self.engine, theme::role::row());
        field.set_text(&mut self.engine, &name);
        field.select_all(&mut self.engine);
        self.device_rename = Some(field);
        self.relayout();
    }

    /// Close the rename field, sending its name when there is one.
    fn commit_device_rename(&mut self) {
        let Some(field) = self.device_rename.take() else {
            return;
        };
        let name = field.value().trim().to_owned();
        if !name.is_empty() {
            self.requests
                .push(Request::Device(DeviceRequest::RenameThisDevice(name)));
        }
        self.relayout();
    }

    /// The rename field owns the keyboard while it is open: Enter commits,
    /// Escape cancels.
    pub(super) fn device_rename_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let field = self.device_rename.as_mut()?;
        match key {
            Key::Named(NamedKey::Enter) => self.commit_device_rename(),
            Key::Named(NamedKey::Escape) => {
                self.device_rename = None;
                self.relayout();
            }
            _ => {
                field.key(&mut self.engine, key, text, mods);
            }
        }
        Some(true)
    }

    /// A press while the rename field is open: inside it moves the caret;
    /// anywhere else commits — the exits a workspace rename has — and the
    /// press then does what it landed on (`None`).
    pub(super) fn device_rename_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        self.device_rename.as_ref()?;
        let rect = self.device_rename_rect()?;
        if rect.contains(x, y) {
            let field = self.device_rename.as_mut()?;
            field.click(&mut self.engine, rect, x, y);
            return Some(true);
        }
        self.commit_device_rename();
        None
    }

    pub(super) fn device_rename_drag(&mut self, x: f32) -> bool {
        let Some(rect) = self.device_rename_rect() else {
            return false;
        };
        self.device_rename
            .as_mut()
            .is_some_and(|field| field.drag(&mut self.engine, rect, x))
    }

    pub(super) fn device_rename_release(&mut self) {
        if let Some(field) = self.device_rename.as_mut() {
            field.release();
        }
    }

    /// A double-click on this computer's row renames it.
    pub(super) fn device_double_click(&mut self, control: Control) -> bool {
        if control != Control::ThisDevice || self.device_rename.is_some() {
            return false;
        }
        self.open_device_rename();
        true
    }

    /// Escape on a device's history goes back to the list.
    pub(super) fn devices_escape(&mut self) -> bool {
        if self.page != Some(Page::Devices) || !self.devices.history_open() {
            return false;
        }
        self.activate_device_control(Control::HistoryBack);
        true
    }

    // ---- paint ---------------------------------------------------------------

    pub(super) fn paint_devices_page(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(laid) = self.laid.devices.clone() else {
            return;
        };
        let column = self.column();
        let p = theme::p();
        let _page = inspect::scope(
            "Manage linked devices",
            "part",
            root,
            scene::rect(column.x, column.y, column.w, self.laid.height),
            |r| {
                r.property("devices", self.devices.devices().len(), "LinkedDevices");
                r.property(
                    "presence refresh",
                    self.devices.presence_deadline().is_some(),
                    "only while this page is in front",
                );
                r.property(
                    "implementation",
                    "apps/koru manage::devices",
                    "app implementation",
                );
                r.property("docs", "docs/product/INITIATIVES.md", "initiative");
            },
        );
        if let Some(history) = &laid.history {
            self.paint_history(canvas, root, history);
            return;
        }
        self.paint_head(canvas, root, &laid.head);
        if let Some(placed) = self.laid.placed(Control::LinkDevice).cloned() {
            self.paint_control(canvas, root, &placed);
        }
        if let Some(card) = laid.computer_card {
            let mut dividers = Vec::new();
            if let (Some(_), Some(relay)) = (&laid.this_row, &laid.relay) {
                dividers.push(relay.rect.y);
            }
            let _card = Card::new(Some(THIS_COMPUTER)).draw(
                canvas,
                root,
                &mut self.engine,
                card,
                &dividers,
                1.0,
            );
            if let Some(row) = &laid.this_row {
                self.paint_device_row(canvas, root, row);
            }
            if let Some(relay) = &laid.relay {
                self.paint_relay_row(canvas, root, relay);
            }
        }
        if let Some(card) = laid.list_card {
            let dividers: Vec<f32> = laid.rows.iter().skip(1).map(|row| row.rect.y).collect();
            let _card = Card::new(Some(OTHER_DEVICES)).draw(
                canvas,
                root,
                &mut self.engine,
                card,
                &dividers,
                1.0,
            );
            for row in &laid.rows {
                self.paint_device_row(canvas, root, row);
            }
        }
        if let Some(rect) = laid.empty {
            // No card to carry the caption: it stands over the sentence.
            if let Some(caption) = laid.others_caption {
                self.paint_caption(canvas, root, caption, OTHER_DEVICES);
            }
            KoruTextParagraph::new(
                NONE_LINKED,
                theme::role::ui(),
                "theme::role::ui()",
                p.text_dim,
            )
            .draw(canvas, root, &mut self.engine, rect);
        }
        // The rename field goes over the name, in the scrolled space it was
        // laid out in — its rect is scrolled, so it is drawn unscrolled.
        if let (Some(rect), Some(row)) = (self.device_rename_rect(), &laid.this_row) {
            let rect = Rect::new(
                rect.x,
                row.name.y + (row.name.h - FIELD_H) * 0.5,
                rect.w,
                rect.h,
            );
            if let Some(field) = self.device_rename.as_mut() {
                field.draw(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    InputStyle::search(),
                    Some("Device name"),
                    true,
                );
            }
        }
    }

    fn paint_device_row(&mut self, canvas: &mut Scene, root: Affine, row: &DeviceRow) {
        let p = theme::p();
        let r = row.rect;
        let control = match row.index {
            None => Control::ThisDevice,
            Some(index) => Control::DeviceRow(index),
        };
        let hovered = self.page_hover == Some(control);
        let focused = self.page_focus == Some(control);
        let _row = inspect::scope(
            "Manage device row",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property("this computer", row.index.is_none(), "LinkedDevices");
                rec.property("badge", row.badge_text.clone(), "the build's OS");
                rec.property(
                    "severity",
                    row.severity.clone().unwrap_or_else(|| "none".into()),
                    "build_severity",
                );
                rec.property(
                    "press",
                    match row.index {
                        None => "double-click renames this computer",
                        Some(_) => "its history (decision 13)",
                    },
                    "manage::devices",
                );
            },
        );
        let inner = scene::rrect(r.x + 4.0, r.y + 3.0, r.w - 8.0, r.h - 6.0, 7.0);
        if hovered && row.index.is_some() {
            scene::fill(canvas, root, p.hover_tint, &inner);
        }
        if focused {
            scene::stroke(canvas, root, p.focus_ring, theme::BORDER_W * 2.0, &inner);
        }
        let b = row.badge;
        scene::fill(
            canvas,
            root,
            p.chip_bg,
            &scene::rrect(b.x, b.y, b.w, b.h, 5.0),
        );
        let style = theme::role::hint().with_weight(theme::FontWeight::SemiBold);
        let text = self.engine.simple_layout(&row.badge_text, style);
        let (w, h) = (text.width(), scene::first_line_height(&text));
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &row.badge_text,
            b.x + (b.w - w) * 0.5,
            b.y + (b.h - h) * 0.5,
            style,
            "theme::role::hint() semibold — the platform badge",
            p.text_dim,
        );
        let renaming = row.index.is_none() && self.device_rename.is_some();
        if !renaming {
            koru_ui::text_paragraph::draw_line(
                canvas,
                root,
                &mut self.engine,
                &row.name_text,
                row.name.x,
                row.name.y,
                theme::role::row(),
                "theme::role::row()",
                p.text,
            );
        }
        KoruTextParagraph::new(
            &row.sub_text,
            theme::role::meta(),
            "theme::role::meta() — today's status vocabulary",
            status_color(row.severity.as_deref(), p),
        )
        .draw(canvas, root, &mut self.engine, row.sub);
        if let Some(tag) = row.tag {
            koru_ui::LabelChip::new(THIS_COMPUTER).draw(
                canvas,
                root,
                &mut self.engine,
                tag,
                koru_ui::label_chip::State::default(),
            );
        }
        if let Some(index) = row.index
            && let Some(placed) = self.laid.placed(Control::UnlinkDevice(index)).cloned()
        {
            self.paint_control(canvas, root, &placed);
        }
    }

    fn paint_relay_row(&mut self, canvas: &mut Scene, root: Affine, relay: &RelayRow) {
        let r = relay.rect;
        let _relay = inspect::scope(
            "Manage relay",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| {
                rec.property(
                    "enabled",
                    self.devices.relay().is_some_and(|relay| relay.enabled),
                    "RelayView",
                );
                rec.property("status", relay.status_text.clone(), "RelayView");
                rec.property("writes", "Agent's relay preference", "SetRelay");
            },
        );
        let _row = CardRow::new(RELAY).hint(RELAY_HINT).draw(
            canvas,
            root,
            &mut self.engine,
            relay.row,
            relay.text_w,
            1.0,
        );
        if let Some(placed) = self.laid.placed(Control::Relay).cloned() {
            self.paint_control(canvas, root, &placed);
        }
        koru_ui::text_paragraph::draw_line(
            canvas,
            root,
            &mut self.engine,
            &relay.status_text,
            relay.status.x,
            relay.status.y,
            theme::role::meta(),
            "theme::role::meta()",
            theme::p().text_dim,
        );
    }
}

/// Whether two snapshots say the same thing — the history each fetched
/// aside, which a snapshot never carries.
fn same_snapshot(a: &LinkedDevices, b: &LinkedDevices) -> bool {
    let bare = |device: &LinkedDeviceView| LinkedDeviceView {
        history: Vec::new(),
        ..device.clone()
    };
    a.this_device() == b.this_device()
        && a.relay() == b.relay()
        && a.devices().len() == b.devices().len()
        && a.devices()
            .iter()
            .zip(b.devices())
            .all(|(x, y)| bare(x) == bare(y))
}

/// The badge's three letters, from the OS the device's build named
/// (`version · channel · branch · OS`); a device that never said gets its
/// name's initial.
pub(crate) fn platform_badge(device: &LinkedDeviceView) -> String {
    let named = device.build_label.as_deref().and_then(|label| {
        label.split(" \u{b7} ").find_map(|part| {
            let word = part.split_whitespace().next()?.to_ascii_lowercase();
            match word.as_str() {
                "windows" => Some("WIN"),
                "macos" | "mac" | "darwin" => Some("MAC"),
                "android" => Some("AND"),
                "ios" | "ipados" => Some("IOS"),
                "linux" => Some("LNX"),
                _ => None,
            }
        })
    });
    match named {
        Some(badge) => badge.to_owned(),
        None => device
            .display_name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().collect()),
    }
}

/// The line under this computer's name: what it runs, then how to rename
/// it. *This computer* is the tag on the row's right.
pub(crate) fn this_device_hint(device: &LinkedDeviceView) -> String {
    match &device.build_label {
        Some(build) => format!("{build} \u{b7} double-click to rename"),
        None => "Double-click to rename".into(),
    }
}

/// A linked device's status line, in the words the Link dialog's list used.
pub(crate) fn device_status_label(device: &LinkedDeviceView) -> String {
    let presence = if device.connected {
        match device.connection.as_deref() {
            Some(path) => format!("Linked (online, {path})"),
            None => "Linked (online)".into(),
        }
    } else if let Some(last_seen) = device.last_seen_millis {
        format!(
            "Linked (offline) \u{b7} last seen {}",
            date_time_label(last_seen)
        )
    } else {
        format!(
            "Linked (offline) \u{b7} linked {}",
            date_label(device.linked_at_millis)
        )
    };
    let relationship = match &device.introduced_by {
        Some(introducer) => format!("{presence} \u{b7} via {introducer}"),
        None => presence,
    };
    // The build is what the device said of itself, as its latest signed
    // description put it; a device that never said shows nothing rather
    // than a guess.
    let status = match &device.build_label {
        Some(build) => format!("{relationship} \u{b7} {build}"),
        None => relationship,
    };
    // Two devices on different peer protocol numbers do not sync; that
    // leads the line, and says which side the update is on. The device
    // itself stays linked, online, renameable and unlinkable as ever.
    let status = match device.update_needed_on.as_deref() {
        Some("this_device") => format!("Not syncing \u{b7} update this device \u{b7} {status}"),
        Some(_) => format!(
            "Not syncing \u{b7} update {} \u{b7} {status}",
            device.display_name
        ),
        None => status,
    };
    match device.last_sync_millis {
        Some(at) => format!("{status} \u{b7} last synced {}", date_time_label(at)),
        None => format!("{status} \u{b7} no state changes received yet"),
    }
}

/// The tone of a status line, by how far the device's build is from ours:
/// a minor apart in the notice register (a fact worth knowing, not a
/// failure), a major apart or an incompatible protocol in the destructive
/// red *Unlink…* already wears — no new color.
pub(crate) fn status_color(severity: Option<&str>, p: &theme::Palette) -> vello::peniko::Color {
    match severity {
        Some("danger") => p.close_hover,
        Some("warning") => p.notice_fg,
        _ => p.text_dim,
    }
}

pub(crate) fn relay_status_label(relay: &RelayView) -> String {
    match relay.host.as_deref() {
        Some(host) => format!("{host} \u{2014} {}", relay.state),
        None if relay.enabled => relay.state.clone(),
        None => "Relay off".into(),
    }
}

fn date_label(millis: i64) -> String {
    // Local wall time in the reader's date order — never UTC, which was an
    // hour and sometimes a day off from the clock beside the device.
    crate::format::local_parts(millis)
        .map(|parts| koru_i18n::time::absolute_date(crate::format::lang(), parts, true))
        .unwrap_or_else(|| "previously".into())
}

pub(crate) fn date_time_label(millis: i64) -> String {
    crate::format::local_parts(millis)
        .map(|parts| koru_i18n::time::absolute(crate::format::lang(), parts, true))
        .unwrap_or_else(|| "previously".into())
}
