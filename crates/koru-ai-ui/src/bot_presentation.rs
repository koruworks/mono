// SPDX-License-Identifier: MPL-2.0

//! Bot conversation presentation. Prose stays in the transcript; the live
//! status stays below it; retained work opens from the reply's anchor action.
//! No status invents speech or infers a tool result from elapsed time.
use super::*;
use koru_draw::el::{
    El, ElState, FrameCx, Interaction, Key as ElKey, Leaf, Length, Role, leaf, row,
};
use koru_draw::theme::{Profile, Radius, Space};
use work_view::{WorkHit, WorkInput, WorkLayout, WorkingBot};

mod activity;
mod progress;
pub(crate) use activity::Names as ActivityNames;
pub(crate) use activity::Progress;
pub(crate) use activity::is_current as current_activity;

pub(crate) const STATUS_H: f32 = Space::S8.px(Profile::Desktop);
pub(crate) const PROSE_PAD: f32 = Space::S2.px(Profile::Desktop);
pub(crate) const COLUMN_FRACTION: f32 = 0.8;
pub(crate) const BUBBLE_FRACTION: f32 = 0.8;
const FACE: f32 = Space::S5.px(Profile::Desktop);
const GAP: f32 = Space::S2.px(Profile::Desktop);
const CARET: f32 = Space::S3.px(Profile::Desktop);

pub(crate) fn wrap_width(role: ChatRole, width: f32, bot: bool) -> f32 {
    if bot {
        (width * BUBBLE_FRACTION - PROSE_PAD * 2.0).max(1.0)
    } else {
        message_wrap_width(role, width)
    }
}

pub(crate) fn status_text(lang: koru_i18n::Lang, input: &WorkInput<'_>) -> String {
    if !input.live {
        return koru_i18n::t(lang, koru_i18n::Key::InspectorDetails).into();
    }
    input
        .activity
        .filter(|call| activity::is_current(call))
        .or_else(|| {
            input
                .groups
                .iter()
                .flat_map(|group| group.rows.iter().copied())
                .find(|call| activity::is_current(call))
        })
        .map(|call| {
            activity::line(
                call,
                tool_groups::step_kind(call).unwrap_or(tool_groups::StepKind::Tool),
                lang,
                input.names,
            )
        })
        .unwrap_or_else(|| input.progress.text(lang).into())
}

pub(crate) fn status_label(
    engine: &mut TextEngine,
    width: f32,
    live: bool,
    label: &str,
) -> Layout<Brush> {
    let lead = if live { FACE + GAP } else { 0.0 };
    let label = engine.elided(label, role::ui(), (width - lead - CARET - GAP).max(1.0));
    engine.simple_layout(&label, role::ui())
}

fn status_key(work: &WorkLayout) -> ElKey {
    ElKey::model(work.message.as_uuid().as_u128(), "bot-work-disclosure")
}

pub(crate) fn status_rect(work: &WorkLayout, x: f32, y: f32, width: f32) -> Rect {
    let lead = if work.live { FACE + GAP } else { 0.0 };
    Rect::new(
        x,
        y,
        (lead + work.header.width() + GAP + CARET).min(width),
        STATUS_H,
    )
}

fn status_tree(work: &WorkLayout, bot: Option<&WorkingBot>, width: f32, sweep: Option<f32>) -> El {
    let rect = status_rect(work, 0.0, 0.0, width);
    let mut tree = row("Bot activity status")
        .h(Space::S8)
        .w(Length::Runtime(rect.w, "measured status and disclosure"))
        .gap(Space::S2)
        .id(status_key(work))
        .semantics(Role::Group)
        .name_text(&work.header_text);
    if work.live {
        tree = tree.child(
            leaf("Working bot avatar", Avatar(bot.cloned()))
                .semantic()
                .w(Space::S5)
                .h(Space::S5),
        );
    }
    tree.child(
        leaf("Bot status label", Label(Rc::clone(&work.header), sweep))
            .semantic()
            .fill(),
    )
}

pub(crate) fn draw_status(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    work: &WorkLayout,
    at: Rect,
    bot: Option<&WorkingBot>,
    hover: Option<WorkHit>,
    focused: Option<WorkHit>,
    sweep: Option<f32>,
) {
    if !work.live {
        return;
    }
    let tree = status_tree(work, bot, at.w, sweep);
    let key = status_key(work);
    let hit = WorkHit::Header(work.message);
    let frame = questions::elements::layout(
        &tree,
        at,
        FrameCx::root().with_interaction(Interaction {
            hot: (hover == Some(hit)).then_some(key),
            pressed: None,
            focused: (focused == Some(hit)).then_some(key),
        }),
    );
    frame.paint(&tree, canvas, root, engine);
    if focused == Some(hit) {
        let rect = status_rect(work, at.x, at.y, at.w);
        scene::stroke(
            canvas,
            root,
            theme::p().focus_ring,
            theme::BORDER_W,
            &scene::rrect(
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                Radius::M.px(Profile::Desktop, rect.w, rect.h),
            ),
        );
    }
}

struct Avatar(Option<WorkingBot>);
impl Leaf for Avatar {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (FACE, FACE)
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
        if let Some(bot) = &self.0 {
            bot.draw(canvas, root, engine, at);
        }
    }
}
struct Label(Rc<Layout<Brush>>, Option<f32>);
impl Leaf for Label {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (self.0.width(), self.0.height())
    }
    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        _: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        _: ElState,
    ) {
        scene::draw_layout(canvas, root, &self.0, at.x, at.y, Some(theme::p().text_dim));
        work_view::draw_sweep(canvas, root, &self.0, at.x, at.y, self.1);
    }
}

impl ChatSurface {
    /// A native thread can have several responders. The tab supplies only the
    /// animation pose; the exact reply identifies whose cached face to paint.
    pub(crate) fn working_bot_for(&self, message: MessageId) -> Option<WorkingBot> {
        let Some(state) = &self.managed else {
            return self.working_bot.clone();
        };
        let bot = state
            .snapshot
            .turns
            .iter()
            .find(|turn| turn.reply_message == Some(message.as_uuid()))
            .map(|turn| turn.bot)
            .or_else(|| {
                state
                    .live
                    .values()
                    .find(|live| live.reply == message)
                    .map(|live| live.bot)
            })?;
        let host = (bot == state.snapshot.conversation.label.bot_id.as_uuid())
            .then_some(self.working_bot.as_ref())
            .flatten();
        let name = state
            .snapshot
            .participants
            .iter()
            .find(|participant| participant.bot == bot)
            .map(|participant| participant.name.as_str())
            .or_else(|| host.map(|host| host.name.as_str()))
            .or_else(|| {
                (bot == state.snapshot.conversation.label.bot_id.as_uuid())
                    .then_some(state.snapshot.conversation.label.bot_name.as_str())
            })?;
        Some(WorkingBot {
            face: self
                .bot_faces
                .get(&bot)
                .copied()
                .or_else(|| host.map(|host| host.face))
                .unwrap_or(0),
            name: name.to_owned(),
            pose: self
                .working_bot
                .as_ref()
                .map(|host| host.pose)
                .unwrap_or_default(),
        })
    }

    pub(crate) fn shown_message_height(&self, message: &MessageLayout) -> f32 {
        if self.bot_chat_flow()
            && !message.state.is_terminal()
            && !message.folded_empty_answer()
            && message
                .id
                .is_some_and(|id| self.bot_work_replies.contains(&id))
        {
            return message_height(message)
                + Space::S7.px(Profile::Desktop)
                + Space::S2.px(Profile::Desktop);
        }
        if self.bot_chat_flow()
            && message.folded_empty_answer()
            && message
                .id
                .is_some_and(|id| self.bot_work_replies.contains(&id))
        {
            return Space::S7.px(Profile::Desktop);
        }
        if self.bot_chat_flow()
            && message.role == ChatRole::Assistant
            && message.state == MessageState::Pending
            && message.display.trim().is_empty()
            && message
                .id
                .is_some_and(|id| self.work_layouts.iter().any(|work| work.message == id))
        {
            0.0
        } else {
            message_height(message)
        }
    }

    pub(crate) fn ensure_bot_activity(&mut self, width: f32) {
        if !self.bot_chat_flow() {
            return;
        }
        let progress = self.bot_reply_progress();
        for message in &self.message_layouts {
            let Some(id) = message.id else {
                continue;
            };
            if message.role != ChatRole::Assistant
                || message.state != MessageState::Pending
                || self.work_layouts.iter().any(|work| work.message == id)
            {
                continue;
            }
            self.work_layouts.push(work_view::build(
                &mut self.engine,
                self.lang,
                (width - PROSE_PAD * 2.0).max(1.0),
                WorkInput {
                    message: id,
                    live: true,
                    bot: true,
                    progress: progress.get(&id).copied().unwrap_or_default(),
                    started_ms: 0,
                    worked_ms: 0,
                    items: vec![],
                    groups: vec![],
                    names: ActivityNames {
                        documents: Some(&self.document_titles),
                        bots: Some(&self.mention_names),
                    },
                    activity: self.transcript.as_ref().and_then(|transcript| {
                        transcript.tool_activities.iter().find(|call| {
                            call.assistant_message_id == id && activity::is_current(call)
                        })
                    }),
                },
                &work_view::Opened {
                    replies: &self.opened_work,
                    groups: &self.opened_step_groups,
                    calls: &self.opened_calls,
                },
                &mut |_, _, _| Rc::new(vec![]),
            ));
        }
    }

    pub(crate) fn settle_bot_activity(&mut self) {
        if !self.bot_chat_flow() {
            return;
        }
        for work in self
            .work_layouts
            .iter()
            .filter(|work| work.bot && work.live)
        {
            let ended = self.transcript.as_ref().is_some_and(|transcript| {
                transcript
                    .messages
                    .iter()
                    .any(|message| message.id == work.message && message.state.is_terminal())
            });
            if ended {
                self.opened_work.remove(&work.message);
            }
        }
    }
}

/// Paint only the prose; the hover actions and disclosure remain outside it.
pub(crate) fn draw_bubble(canvas: &mut Scene, root: Affine, card: Rect, message: &MessageLayout) {
    if message.display.trim().is_empty() {
        return;
    }
    let width = (message_content_width(message) + message.padding() * 2.0).min(card.w);
    let height = message_content_height(message) + message.padding() * 2.0;
    scene::fill(
        canvas,
        root,
        theme::p().chip_bg,
        &scene::rrect(
            card.x,
            card.y,
            width,
            height,
            Radius::L.px(Profile::Desktop, width, height),
        ),
    );
}

#[cfg(test)]
pub(crate) mod tests;
