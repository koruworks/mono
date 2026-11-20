// SPDX-License-Identifier: MPL-2.0

//! A Bot's retained work belongs to one reply, in a window-modal reader.
//! Its scroll and focus never change the conversation's scroll or draft.

use super::*;
use koru_draw::el::{
    self, Action, El, ElState, Frame, FrameCx, Key as ElKey, Leaf, Length, Role, TextCache,
};
use koru_draw::theme::tokens::{Profile, Radius, Space};
use koru_i18n::{Key as TextKey, Lang, t};
use koru_ui::icon_button::{IconButton, State as ButtonState};
use work_view::{WorkHit, WorkLayout};

const ROOT: ElKey = ElKey::surface("bot-work", "dialog");
const CLOSE: ElKey = ElKey::surface("bot-work", "close");
const BODY: ElKey = ElKey::surface("bot-work", "body");
const CONTENT: ElKey = ElKey::surface("bot-work", "content");
const PAD: f32 = Space::S4.px(Profile::Desktop);
const BUTTON: f32 = Space::S7.px(Profile::Desktop);

/// Plain host projection of the same controls painted in the dialog.
#[derive(Clone)]
pub struct BotWorkControl {
    pub key: ElKey,
    pub bounds: Rect,
    pub name: String,
}

#[derive(Clone)]
pub struct BotWorkSemantics {
    pub bounds: Rect,
    pub name: String,
    pub focus: ElKey,
    pub controls: Vec<BotWorkControl>,
    pub details: String,
}

pub(crate) struct Dialog {
    chat: ChatId,
    reply: MessageId,
    pub layout: Option<Rc<WorkLayout>>,
    scroll: f32,
    focus: Option<WorkHit>,
    hover: Option<WorkHit>,
    close_hover: bool,
    return_focus: Focus,
    pub groups: HashSet<(MessageId, usize)>,
    pub calls: HashSet<ToolInvocationId>,
    frame: Option<Frame>,
    controls: Vec<(WorkHit, Rect, String)>,
    panel: Rect,
    width: f32,
    grab: Option<f32>,
    cache: TextCache,
    placed_scroll: f32,
    link_keys: Vec<(WorkHit, ElKey)>,
    names: HashMap<ToolInvocationId, String>,
    details: String,
}

fn key(hit: WorkHit) -> ElKey {
    match hit {
        WorkHit::Header(id) => ElKey::model(id.as_uuid().as_u128(), "bot-work-anchor"),
        WorkHit::Group(id, _) => ElKey::model(id.as_uuid().as_u128(), "bot-work-group"),
        WorkHit::Call(id) => ElKey::model(id.as_uuid().as_u128(), "bot-work-call"),
        WorkHit::CopyOutput(id) => ElKey::model(id.as_uuid().as_u128(), "bot-work-copy"),
        WorkHit::Toggle(id) => ElKey::model(id.as_uuid().as_u128(), "bot-work-expand"),
        // Dialog::key supplies the composite call/position identity for links.
        WorkHit::Link(id, _) => ElKey::model(id.as_uuid().as_u128(), "bot-work-link"),
        WorkHit::Path(id, _) => ElKey::model(id.as_uuid().as_u128(), "bot-work-path"),
    }
}

fn panel(size: (f32, f32)) -> Rect {
    let w = (size.0 - PAD * 2.0).max(1.0).min(CONTENT_MAX_W);
    let h = (size.1 - PAD * 2.0).max(1.0);
    Rect::new((size.0 - w) * 0.5, (size.1 - h) * 0.5, w, h)
}

impl ChatSurface {
    /// Cheap native accessibility invalidation: content changes bump at layout,
    /// while focus and scroll are retained scalar values. No detail text clone.
    pub fn bot_work_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.bot_work_generation.hash(&mut hash);
        self.work_focus().hash(&mut hash);
        self.conversation_scroll.to_bits().hash(&mut hash);
        if let Some(open) = &self.bot_work {
            open.reply.hash(&mut hash);
            open.focus.hash(&mut hash);
            open.scroll.to_bits().hash(&mut hash);
        }
        hash.finish()
    }

    #[cfg(feature = "test-support")]
    pub fn pretend_transcript(&mut self, transcript: ChatTranscript) {
        self.apply_update(crate::reader::Update::Transcript(transcript));
    }

    pub(crate) fn bot_work_point(&self, x: f32, y: f32) -> (f32, f32) {
        self.viewer_lift
            .map_or((x - self.viewport.x, y - self.viewport.y), |lift| {
                (x + lift.origin.0, y + lift.origin.1)
            })
    }

    pub(crate) fn bot_work_anchor(&self, reply: MessageId) -> Option<Rect> {
        if !self.bot_chat_flow() || !self.bot_work_replies.contains(&reply) {
            return None;
        }
        let message = self
            .message_layouts
            .iter()
            .find(|message| message.id == Some(reply))?;
        Some(self.bot_work_anchor_for(message))
    }

    pub(crate) fn bot_work_anchor_for(&self, message: &MessageLayout) -> Rect {
        let area = self.conversation_hit_rect;
        let card = message_card(
            area,
            area.y - self.conversation_scroll + message.top,
            self.shown_message_height(message),
            message,
        );
        let index = self
            .reply_actions(message)
            .position(|action| action == ReplyAction::Details)
            .unwrap_or(0);
        self.reply_action_rect(message, card, index)
    }

    pub(crate) fn bot_work_anchor_at(&self, x: f32, y: f32) -> Option<MessageId> {
        self.message_layouts
            .iter()
            .filter(|message| {
                message
                    .id
                    .is_some_and(|id| self.bot_work_replies.contains(&id))
            })
            .find(|message| self.bot_work_anchor_for(message).contains(x, y))
            .and_then(|message| message.id)
    }

    pub fn bot_work_anchors(&self) -> Vec<BotWorkControl> {
        self.message_layouts
            .iter()
            .filter_map(|message| {
                let reply = message.id?;
                if !self.bot_work_replies.contains(&reply) {
                    return None;
                }
                let bounds = intersection(
                    self.bot_work_anchor_for(message),
                    self.conversation_hit_rect,
                )?;
                Some(BotWorkControl {
                    key: key(WorkHit::Header(reply)),
                    bounds,
                    name: t(self.lang, TextKey::AiReplyDetails).into(),
                })
            })
            .collect()
    }

    pub fn bot_work_focused_anchor(&self) -> Option<ElKey> {
        if self.bot_work_open() {
            return None;
        }
        let WorkHit::Header(reply) = self.work_focus()? else {
            return None;
        };
        self.bot_work_replies
            .contains(&reply)
            .then(|| key(WorkHit::Header(reply)))
    }

    pub fn activate_bot_work_anchor(&mut self, target: ElKey) -> bool {
        let reply = self
            .message_layouts
            .iter()
            .filter_map(|message| message.id)
            .find(|id| key(WorkHit::Header(*id)) == target);
        reply.is_some_and(|reply| self.open_bot_work(reply))
    }

    pub fn bot_work_open(&self) -> bool {
        self.bot_work.as_ref().is_some_and(|dialog| {
            self.current_chat_id() == Some(dialog.chat) && self.unavailable.is_none()
        })
    }

    pub(crate) fn open_bot_work(&mut self, reply: MessageId) -> bool {
        if !self.bot_chat_flow() {
            return false;
        }
        let Some(transcript) = &self.transcript else {
            return false;
        };
        let rows = reply_rows(transcript, reply);
        if rows.is_empty() {
            return false;
        }
        // A decode requested before this dialog opened must not later replace
        // its input owner with an attachment viewer beneath its paint.
        self.viewer_opening = None;
        self.viewer = None;
        self.bot_work = Some(Dialog {
            chat: transcript.chat.id,
            reply,
            layout: None,
            scroll: 0.0,
            focus: None,
            hover: None,
            close_hover: false,
            return_focus: Focus::Work(reply),
            groups: rows.iter().enumerate().map(|(i, _)| (reply, i)).collect(),
            calls: HashSet::new(),
            frame: None,
            controls: Vec::new(),
            panel: panel(self.viewer_viewport()),
            width: panel(self.viewer_viewport()).w - PAD * 2.0,
            grab: None,
            cache: TextCache::default(),
            placed_scroll: -1.0,
            link_keys: Vec::new(),
            names: HashMap::new(),
            details: String::new(),
        });
        self.composer.set_focused(false);
        self.reply_tooltip.clear();
        self.messages_dirty = true;
        self.prepare_bot_work();
        true
    }

    pub fn close_bot_work(&mut self) -> bool {
        let Some(dialog) = self.bot_work.take() else {
            return false;
        };
        if self.current_chat_id() == Some(dialog.chat) {
            self.focus = dialog.return_focus;
        }
        true
    }

    /// Called only during a real transcript/layout rebuild, never per mouse move.
    pub(crate) fn rebuild_bot_work(
        dialog: &mut Option<Dialog>,
        transcript: &ChatTranscript,
        engine: &mut TextEngine,
        lang: Lang,
        detail: &mut work_view::Detail<'_>,
    ) {
        let Some(open) = dialog.as_mut() else {
            return;
        };
        let rows = reply_rows(transcript, open.reply);
        if transcript.chat.id != open.chat || rows.is_empty() {
            *dialog = None;
            return;
        }
        // One retained call per group, including discovery and read-only
        // permission/question evidence. No hidden conversation filter here.
        let groups: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                open.groups.insert((open.reply, index));
                tool_groups::StepGroup {
                    kind: tool_groups::step_kind(row).unwrap_or(tool_groups::StepKind::Tool),
                    rows: vec![*row],
                    offset: row.text_offset.map(|value| value as usize),
                    asked: Vec::new(),
                }
            })
            .collect();
        open.names = groups
            .iter()
            .flat_map(|group| {
                group
                    .rows
                    .iter()
                    .map(|row| (row.id, tool_groups::step_call_line(group, row, lang)))
            })
            .collect();
        open.details = rows
            .iter()
            .map(|row| {
                let title = open.names.get(&row.id).map_or("", String::as_str);
                if !open.calls.contains(&row.id) {
                    return title.to_owned();
                }
                format!(
                    "{title}\n{}\n{}",
                    row.agent_input.as_deref().unwrap_or(""),
                    tool_output::copy_text(row).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let replies = HashSet::from([open.reply]);
        let mut layout = work_view::build(
            engine,
            lang,
            open.width,
            work_view::WorkInput {
                message: open.reply,
                live: false,
                bot: false,
                progress: Default::default(),
                started_ms: 0,
                worked_ms: 0,
                items: (0..groups.len())
                    .map(tool_groups::WorkItem::Group)
                    .collect(),
                groups,
                names: bot_presentation::ActivityNames {
                    documents: None,
                    bots: None,
                },
                activity: None,
            },
            &work_view::Opened {
                replies: &replies,
                groups: &open.groups,
                calls: &open.calls,
            },
            detail,
        );
        // The dialog supplies its own heading. Keep call lines and their full
        // existing detail renderer; group summaries are redundant here.
        layout
            .lines
            .retain(|line| !matches!(line.hit, Some(WorkHit::Group(..))));
        let mut y = 0.0;
        for line in &mut layout.lines {
            line.y = y;
            line.indent = (line.indent - Space::S4.px(Profile::Desktop)).max(0.0);
            y += line.height() + Space::S2.px(Profile::Desktop);
        }
        layout.height = y;
        for hit in layout
            .lines
            .iter()
            .filter_map(|line| line.hit)
            .filter(|hit| matches!(hit, WorkHit::Link(..) | WorkHit::Path(..)))
        {
            if !open.link_keys.iter().any(|(existing, _)| *existing == hit) {
                let (call, index, kind) = match hit {
                    WorkHit::Link(call, index) => (call, index, b"link".as_slice()),
                    WorkHit::Path(call, index) => (call, index, b"path".as_slice()),
                    _ => unreachable!(),
                };
                let mut identity = kind.to_vec();
                identity.extend_from_slice(&(index as u64).to_le_bytes());
                open.link_keys.push((
                    hit,
                    ElKey::model(
                        uuid::Uuid::new_v5(&call.as_uuid(), &identity).as_u128(),
                        "bot-work-link",
                    ),
                ));
            }
        }
        open.layout = Some(Rc::new(layout));
        open.frame = None;
    }

    fn prepare_bot_work(&mut self) {
        if self.bot_work.is_none() {
            return;
        }
        if !self.bot_work_open() {
            self.close_bot_work();
            return;
        }
        let bounds = panel(self.viewer_viewport());
        if let Some(open) = &mut self.bot_work {
            let width = (bounds.w - PAD * 2.0).max(1.0);
            if (open.width - width).abs() > 0.5 {
                open.width = width;
                self.messages_dirty = true;
            }
            if open.panel != bounds {
                open.panel = bounds;
                open.frame = None;
            }
        }
        if self.messages_dirty {
            let width = self.conversation_rect().w;
            self.rebuild_messages(width);
        }
        let Some(open) = &mut self.bot_work else {
            return;
        };
        if open.frame.is_some() && open.placed_scroll == open.scroll {
            return;
        }
        let tree = dialog_tree(open, self.lang, &self.images);
        let frame = tree.layout(&mut self.engine, &mut open.cache, bounds, FrameCx::root());
        open.scroll = frame
            .node(BODY)
            .and_then(|node| node.scroll)
            .map_or(0.0, |(_, offset)| offset);
        open.placed_scroll = open.scroll;
        open.controls.clear();
        if let (Some(layout), Some(content), Some(body)) =
            (&open.layout, frame.node(CONTENT), frame.node(BODY))
        {
            for line in &layout.lines {
                if let Some(hit) = line.hit {
                    let rect = line.action_rect(content.rect.x, content.rect.y, content.rect.w);
                    let name = match hit {
                        WorkHit::Call(call) => open
                            .names
                            .get(&call)
                            .cloned()
                            .unwrap_or_else(|| t(self.lang, TextKey::AiToolOutput).into()),
                        WorkHit::Link(..) | WorkHit::Path(..) => {
                            line.url.clone().unwrap_or_default()
                        }
                        _ => tool_control_name(line, self.lang),
                    };
                    open.controls.push((hit, rect, name));
                }
                if let Some(call) = line.copy_output {
                    let row = line.action_rect(content.rect.x, content.rect.y, content.rect.w);
                    open.controls.push((
                        WorkHit::CopyOutput(call),
                        Rect::new(
                            content.rect.x + content.rect.w - BUTTON,
                            row.y,
                            BUTTON,
                            row.h,
                        ),
                        t(self.lang, TextKey::MenuCopy).into(),
                    ));
                }
            }
            // Keep offscreen controls for Tab/reveal; pointer and accessibility
            // bounds are clipped to this actual scroll viewport below.
            if open.focus.is_some_and(|hit| {
                !open.controls.iter().any(|(candidate, rect, _)| {
                    *candidate == hit && intersection(*rect, body.rect).is_some()
                })
            }) {
                open.focus = None;
            }
        }
        open.frame = Some(frame);
    }

    pub fn bot_work_semantics(&self) -> Option<BotWorkSemantics> {
        let open = self.bot_work.as_ref().filter(|_| self.bot_work_open())?;
        let frame = open.frame.as_ref()?;
        let mut controls = vec![BotWorkControl {
            key: open.close_key(),
            bounds: frame.node(CLOSE)?.rect,
            name: t(self.lang, TextKey::ViewerClose).into(),
        }];
        let body = frame.node(BODY)?.rect;
        for (hit, bounds, name) in &open.controls {
            // Each retained link/path uses its stable call-and-position key.
            if let Some(bounds) = intersection(*bounds, body) {
                controls.push(BotWorkControl {
                    key: open.key(*hit),
                    bounds,
                    name: name.clone(),
                });
            }
        }
        Some(BotWorkSemantics {
            bounds: open.panel,
            name: t(self.lang, TextKey::InspectorDetails).into(),
            focus: open
                .focus
                .map_or_else(|| open.close_key(), |hit| open.key(hit)),
            controls,
            details: open.details.clone(),
        })
    }

    pub fn bot_work_activate(&mut self, target: ElKey) -> bool {
        self.prepare_bot_work();
        if !self.bot_work_open() {
            return false;
        }
        if self
            .bot_work
            .as_ref()
            .is_some_and(|open| target == open.close_key())
        {
            return self.close_bot_work();
        }
        let hit = self.bot_work.as_ref().and_then(|open| {
            open.controls
                .iter()
                .find(|(hit, _, _)| open.key(*hit) == target)
                .map(|(hit, _, _)| *hit)
        });
        hit.is_some_and(|hit| self.bot_work_action(hit))
    }

    pub fn focus_bot_work_control(&mut self, target: ElKey) -> bool {
        if !self.bot_work_open() {
            let reply = self
                .message_layouts
                .iter()
                .filter_map(|message| message.id)
                .find(|id| key(WorkHit::Header(*id)) == target);
            if let Some(reply) = reply {
                self.focus_work(WorkHit::Header(reply));
                return true;
            }
            return false;
        }
        self.prepare_bot_work();
        let Some(open) = self.bot_work.as_mut() else {
            return false;
        };
        if target == open.close_key() {
            open.focus = None;
            return true;
        }
        let hit = open
            .controls
            .iter()
            .find(|(hit, _, _)| open.key(*hit) == target)
            .map(|(hit, _, _)| *hit);
        if let Some(hit) = hit {
            open.focus = Some(hit);
            return true;
        }
        false
    }

    fn bot_work_action(&mut self, hit: WorkHit) -> bool {
        match hit {
            WorkHit::Call(call) => {
                if let Some(open) = &mut self.bot_work {
                    if !open.calls.remove(&call) {
                        open.calls.insert(call);
                    }
                }
                self.messages_dirty = true;
            }
            WorkHit::Link(..) | WorkHit::Path(..) => {
                let url = self
                    .bot_work
                    .as_ref()
                    .and_then(|open| open.layout.as_ref())
                    .and_then(|layout| layout.lines.iter().find(|line| line.hit == Some(hit)))
                    .and_then(|line| line.url.clone());
                if let Some(url) = url {
                    if matches!(hit, WorkHit::Link(..)) {
                        open_web_link(&url);
                    } else {
                        self.open_changed_file(&url);
                    }
                }
            }
            _ => {
                self.toggle_work(hit);
            }
        }
        self.prepare_bot_work();
        true
    }

    pub fn bot_work_key(&mut self, pressed: &Key, mods: ModifiersState) -> bool {
        if !self.bot_work_open() {
            return false;
        }
        self.prepare_bot_work();
        if self.bot_work.is_none() {
            return true;
        }
        if !mods.is_empty()
            && !(matches!(pressed, Key::Named(NamedKey::Tab)) && mods == ModifiersState::SHIFT)
        {
            return true;
        }
        match pressed {
            Key::Named(NamedKey::Escape) => {
                self.close_bot_work();
            }
            Key::Named(NamedKey::Tab) => {
                let open = self.bot_work.as_mut().unwrap();
                let mut order = vec![None];
                order.extend(open.controls.iter().map(|(hit, _, _)| Some(*hit)));
                let at = order.iter().position(|hit| *hit == open.focus).unwrap_or(0);
                open.focus = order[if mods.shift_key() {
                    (at + order.len() - 1) % order.len()
                } else {
                    (at + 1) % order.len()
                }];
                if let Some(hit) = open.focus {
                    if let Some((_, rect, _)) = open
                        .controls
                        .iter()
                        .find(|(candidate, _, _)| *candidate == hit)
                    {
                        let body = open.frame.as_ref().unwrap().node(BODY).unwrap().rect;
                        if rect.y < body.y {
                            open.scroll = (open.scroll + rect.y - body.y).max(0.0);
                        } else if rect.y + rect.h > body.y + body.h {
                            open.scroll += rect.y + rect.h - body.y - body.h;
                        }
                    }
                }
                self.prepare_bot_work();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(hit) = self.bot_work.as_ref().and_then(|open| open.focus) {
                    self.bot_work_action(hit);
                } else {
                    self.close_bot_work();
                }
            }
            Key::Named(NamedKey::PageDown | NamedKey::PageUp | NamedKey::Home | NamedKey::End) => {
                let open = self.bot_work.as_mut().unwrap();
                let height = open
                    .frame
                    .as_ref()
                    .and_then(|frame| frame.node(BODY))
                    .map_or(0.0, |node| node.rect.h);
                open.scroll = match pressed {
                    Key::Named(NamedKey::Home) => 0.0,
                    Key::Named(NamedKey::End) => f32::MAX,
                    Key::Named(NamedKey::PageUp) => (open.scroll - height).max(0.0),
                    _ => open.scroll + height,
                };
                self.prepare_bot_work();
            }
            _ => {}
        }
        true
    }

    /// Host supplies window coordinates; local chat routing converts first.
    pub fn bot_work_pointer(&mut self, x: f32, y: f32, press: bool) -> bool {
        if !self.bot_work_open() {
            return false;
        }
        self.bot_work_press |= press;
        self.prepare_bot_work();
        let Some(open) = self.bot_work.as_mut() else {
            return true;
        };
        let frame = open.frame.as_ref().unwrap();
        if let Some(grab) = open.grab {
            if let Some(offset) = frame.drag_offset(BODY, y, grab) {
                open.scroll = offset;
            }
            self.prepare_bot_work();
            return true;
        }
        open.close_hover = frame
            .node(CLOSE)
            .is_some_and(|node| node.rect.contains(x, y));
        let body = frame.node(BODY).unwrap().rect;
        open.hover = body
            .contains(x, y)
            .then(|| {
                open.controls
                    .iter()
                    .find(|(_, rect, _)| rect.contains(x, y))
                    .map(|(hit, _, _)| *hit)
            })
            .flatten();
        if press {
            if !open.panel.contains(x, y) || open.close_hover {
                self.close_bot_work();
            } else if let Some(bar) = frame.scrollbar(BODY).filter(|bar| bar.in_gutter(x, y)) {
                open.grab = Some(if bar.on_thumb(x, y) {
                    y - bar.thumb_top
                } else {
                    bar.thumb_h * 0.5
                });
                if let Some(offset) = frame.drag_offset(BODY, y, open.grab.unwrap()) {
                    open.scroll = offset;
                }
            } else if let Some(hit) = open.hover {
                open.focus = Some(hit);
                self.bot_work_action(hit);
            }
        }
        true
    }

    pub fn bot_work_release(&mut self) -> bool {
        let captured = std::mem::take(&mut self.bot_work_press);
        if let Some(open) = &mut self.bot_work {
            open.grab = None;
        }
        captured
    }

    pub fn bot_work_scroll(&mut self, delta: MouseScrollDelta) -> bool {
        let Some(open) = &mut self.bot_work else {
            return false;
        };
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * Space::S8.px(Profile::Desktop),
            MouseScrollDelta::PixelDelta(point) => point.y as f32,
        };
        open.scroll = (open.scroll - dy).max(0.0);
        self.prepare_bot_work();
        true
    }

    pub fn draw_bot_work(&mut self, canvas: &mut Scene, root: Affine) {
        self.prepare_bot_work();
        let Some(open) = &self.bot_work else {
            return;
        };
        let tree = dialog_tree(open, self.lang, &self.images);
        let Some(frame) = &open.frame else {
            return;
        };
        let at = open.panel;
        let size = self.viewer_viewport();
        koru_ui::modal::draw(canvas, root, size, |canvas| {
            let _panel = scene::floating_panel_scope(
                canvas,
                root,
                at.x,
                at.y,
                at.w,
                at.h,
                &scene::Panel::popover(Radius::XL.px(Profile::Desktop, at.w, at.h)),
            );
            frame.paint(&tree, canvas, root, &mut self.engine);
        });
    }
}

impl Dialog {
    fn close_key(&self) -> ElKey {
        ElKey::model(self.reply.as_uuid().as_u128(), "bot-work-close")
    }
    fn key(&self, hit: WorkHit) -> ElKey {
        self.link_keys
            .iter()
            .find(|(item, _)| *item == hit)
            .map_or_else(|| key(hit), |(_, key)| *key)
    }
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.w).min(b.x + b.w);
    let bottom = (a.y + a.h).min(b.y + b.h);
    (right > x && bottom > y).then(|| Rect::new(x, y, right - x, bottom - y))
}

fn tool_control_name(line: &work_view::WorkLine, lang: Lang) -> String {
    if matches!(line.hit, Some(WorkHit::Toggle(..))) {
        return t(lang, TextKey::InspectorDetails).into();
    }
    // Layout's source is retained in the call; the full visible text is also
    // painted. A stable localized action label never exposes internal IDs.
    t(lang, TextKey::AiToolOutput).into()
}

struct CloseButton(bool, bool);
impl Leaf for CloseButton {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (BUTTON, BUTTON)
    }
    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        _: ElState,
    ) {
        IconButton::new(icons::WINDOW_CLOSE, at)
            .with_state(ButtonState {
                hovered: self.0,
                active: false,
                disabled: false,
            })
            .draw(canvas, root, engine);
        if self.1 {
            scene::stroke(
                canvas,
                root,
                theme::p().focus_ring,
                theme::BORDER_W,
                &scene::rrect(
                    at.x,
                    at.y,
                    at.w,
                    at.h,
                    Radius::M.px(Profile::Desktop, at.w, at.h),
                ),
            );
        }
    }
}

struct Body {
    work: Rc<WorkLayout>,
    lang: Lang,
    hover: Option<WorkHit>,
    focus: Option<WorkHit>,
    images: HashMap<String, LoadedImage>,
}
impl Leaf for Body {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        (width.unwrap_or(1.0), self.work.height)
    }
    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        _: ElState,
    ) {
        work_view::draw_lines(
            canvas,
            root,
            engine,
            self.lang,
            &self.work,
            at,
            self.hover,
            self.focus,
            &mut |canvas, root, key, rect| {
                if let Some(image) = self.images.get(key) {
                    draw_image_fitted(canvas, root, image, rect);
                }
            },
        );
    }
}

fn dialog_tree(open: &Dialog, lang: Lang, images: &HashMap<String, ImageLoad>) -> El {
    let mut body = el::col("Retained work content").id(CONTENT);
    if let Some(work) = &open.layout {
        let pictures = work
            .lines
            .iter()
            .filter_map(|line| line.picture.as_ref())
            .filter_map(|picture| {
                images
                    .get(&picture.key)
                    .and_then(|load| load.ready.as_ref())
                    .map(|loaded| (picture.key.clone(), loaded.clone()))
            })
            .collect();
        body = body.child(
            el::leaf(
                "Retained tool details",
                Body {
                    work: Rc::clone(work),
                    lang,
                    hover: open.hover,
                    focus: open.focus,
                    images: pictures,
                },
            )
            .semantic()
            .h(Length::Runtime(work.height, "retained tool details height")),
        );
    }
    el::col("Bot work dialog")
        .id(ROOT)
        .pad(Space::S4)
        .gap(Space::S4)
        .semantics(Role::Group)
        .name(TextKey::InspectorDetails)
        .child(
            el::row("Work dialog header")
                .gap(Space::S4)
                .child(el::text(t(lang, TextKey::InspectorDetails)).fill())
                .child(
                    el::leaf(
                        "Close work details",
                        CloseButton(open.close_hover, open.focus.is_none()),
                    )
                    .semantic()
                    .id(CLOSE)
                    .w(Space::S7)
                    .h(Space::S7)
                    .hit()
                    .focusable()
                    .action(Action::Click)
                    .semantics(Role::Button)
                    .name(TextKey::ViewerClose),
                ),
        )
        .child(
            el::scroll_y("Work details viewport")
                .id(BODY)
                .fill()
                .offset(open.scroll)
                .scrollbar()
                .child(body),
        )
}

#[cfg(test)]
mod tests;
