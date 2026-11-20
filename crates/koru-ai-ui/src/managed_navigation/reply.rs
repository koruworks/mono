// SPDX-License-Identifier: MPL-2.0

//! Read-only returned words, measured once with the transcript's Markdown
//! renderer. The element column owns the sender, blocks and continuation link.
use super::*;
use koru_draw::el::{
    El, ElState, Frame, FrameCx, Key as ElKey, Leaf, Length, Role, col, leaf, text,
};
use koru_draw::theme::{Profile, Space, roles};
use std::rc::Rc;

pub(super) struct Reply {
    pub target: exchange::Target,
    pub preview: koru_ai_core::bots::BotReplyPreview,
    pub blocks: Vec<Rc<BlockLayout>>,
}

impl Reply {
    pub fn new(target: exchange::Target, preview: koru_ai_core::bots::BotReplyPreview) -> Self {
        Self {
            target,
            preview,
            blocks: Vec::new(),
        }
    }
    pub fn key(&self) -> ElKey {
        ElKey::model(self.preview.message.as_uuid().as_u128(), "exchange-reply")
    }
    fn block_key(&self, ordinal: usize) -> ElKey {
        ElKey::model(
            self.preview
                .message
                .as_uuid()
                .as_u128()
                .wrapping_add(ordinal as u128),
            "exchange-reply-block",
        )
    }
}

pub(super) fn prepare(row: &mut Row, engine: &mut TextEngine, width: f32) {
    let width = (width - Space::S3.px(Profile::Desktop) * 2.0).max(1.0);
    for reply in &mut row.replies {
        reply.blocks = koru_ai_core::cut_markdown(&reply.preview.markdown)
            .into_iter()
            .map(|part| {
                let mut block =
                    render_chat_block(engine, MessageBlockId::new(), &part.markdown, width);
                // The preview does not initiate image loads. Its alt text remains
                // readable; the exact full message is available in the thread.
                block.image = None;
                Rc::new(block)
            })
            .collect();
    }
}

pub(super) fn tree(
    reply: &Reply,
    engine: &mut TextEngine,
    faces: &HashMap<Uuid, u8>,
    width: f32,
    label: &str,
) -> El {
    let available = (width - Space::S3.px(Profile::Desktop) * 2.0).max(1.0);
    let identity = chip::BotChip::bounded(
        engine,
        faces.get(&reply.target.bot.as_uuid()).copied().unwrap_or(0),
        &reply.target.name,
        available,
    );
    let size = identity.measure(None).0.min(available);
    let mut tree = col("Bot reply")
        .pad_x(Space::S3)
        .pad_y(Space::S2)
        .gap(Space::S2)
        .child(
            leaf("Reply sender", identity)
                .semantic()
                .id(reply.key())
                .w(Length::Runtime(size, "measured reply sender"))
                .hit()
                .focusable()
                .action(koru_draw::el::Action::Click)
                .semantics(Role::Button)
                .name_text(&reply.target.name)
                .states(&THREAD_STATES),
        );
    for (ordinal, block) in reply.blocks.iter().enumerate() {
        tree = tree.child(
            leaf("Reply paragraph", Paragraph(Rc::clone(block)))
                .semantic()
                .id(reply.block_key(ordinal))
                .name_text(&block.text),
        );
    }
    if reply.preview.truncated && !label.is_empty() {
        tree = tree.child(
            text(label)
                .role(roles::META)
                .id(ElKey::model(
                    reply.preview.message.as_uuid().as_u128(),
                    "exchange-reply-more",
                ))
                .hit()
                .focusable()
                .action(koru_draw::el::Action::Click)
                .semantics(Role::Button)
                .states(&THREAD_STATES),
        );
    }
    tree
}

pub(super) fn link_at(row: &Row, frame: &Frame, x: f32, y: f32) -> Option<String> {
    for reply in &row.replies {
        for (ordinal, block) in reply.blocks.iter().enumerate() {
            let at = frame.node(reply.block_key(ordinal))?.rect;
            if at.contains(x, y)
                && let Some(link) = block.link_at(x - at.x, y - at.y)
            {
                return Some(link.url.to_owned());
            }
        }
    }
    None
}

pub(super) fn target_at<'a>(
    row: &'a Row,
    frame: &Frame,
    x: f32,
    y: f32,
) -> Option<&'a exchange::Target> {
    let hit = frame.hit_at(x, y)?;
    row.replies
        .iter()
        .find(|reply| {
            hit == reply.key()
                || hit
                    == ElKey::model(
                        reply.preview.message.as_uuid().as_u128(),
                        "exchange-reply-more",
                    )
        })
        .map(|reply| &reply.target)
}

struct Paragraph(Rc<BlockLayout>);
impl Leaf for Paragraph {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (0.0, self.0.height())
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
        if let Some(table) = &self.0.table {
            koru_markdown::table::draw_grid(canvas, root, table, at.x, at.y, 0.0, None);
        } else {
            scene::draw_layout(
                canvas,
                root,
                &self.0.layout,
                at.x,
                at.y,
                Some(theme::p().text),
            );
        }
    }
}
