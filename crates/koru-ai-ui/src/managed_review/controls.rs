// SPDX-License-Identifier: MPL-2.0
//! The review card's chrome as measured trees: a header with the result's
//! place in the queue and its arrows and close, and a footer that keeps
//! *Show more* at the start and *Skip* at the end, wrapping at narrow widths.
use super::*;
use koru_draw::el::{
    El, FrameCx, Interaction, Key, Length, Look, Ring, Role, StateRecipe, col, divider, icon, row,
    spacer, text,
};
use koru_draw::theme::{IconSize, Ink, Line, Metric, Radius, Surface, roles};
use questions::elements::{self, ButtonSpec};

#[cfg(test)]
mod tests;

const HEADER: &str = "Routine review header";
const HEADER_BLOCK: &str = "Routine review heading";
const FOOTER: &str = "Routine review actions";
pub(super) const BODY: &str = "Routine review result";
/// The header's arrows and close: the reply action buttons' square.
const ICON_BUTTON: Metric = Metric::new("REVIEW_ICON_BUTTON", 28.0);

/// The header buttons light under the pointer and ring under the keyboard.
static ICON_STATES: StateRecipe = StateRecipe {
    name: "review header button",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// One header button: which part, whether it acts now, and its name.
pub(super) struct Arrow<'a> {
    pub part: Part,
    pub enabled: bool,
    pub name: &'a str,
}

pub(super) struct Spec<'a> {
    pub id: Uuid,
    pub heading: &'a str,
    pub position: Option<&'a str>,
    pub arrows: [Arrow<'a>; 3],
    /// The footer's leading icon buttons: the reply actions a chat reply
    /// has (copy, save as document), in the same glyphs.
    pub tools: &'a [Arrow<'a>],
    /// The footer's labelled actions, in focus order; `true` is primary.
    pub actions: &'a [(Part, String, bool)],
    pub focused: Option<Part>,
    pub busy: bool,
}

pub(super) struct Chrome {
    header: El,
    footer: Option<El>,
    keys: Vec<(Part, Key, bool)>,
    focused: Option<Key>,
}

fn key(id: Uuid, part: Part) -> Key {
    Key::model(id.as_u128(), part.name())
}

fn icon_button(key: Key, part: Part, name: &str, enabled: bool) -> El {
    let glyph = match part {
        Part::Previous => icons::CARET_LEFT,
        Part::Next => icons::CARET_RIGHT,
        Part::Copy => icons::COPY,
        Part::Save => icons::FLOPPY_DISK,
        _ => icons::WINDOW_CLOSE,
    };
    row("Routine review header button")
        .w(ICON_BUTTON)
        .h(ICON_BUTTON)
        .items_center()
        .radius(Radius::M)
        .child(spacer())
        .child(icon(glyph, IconSize::S).decorative().ink(if enabled {
            Ink::Secondary
        } else {
            Ink::Hint
        }))
        .child(spacer())
        .states(&ICON_STATES)
        .id(key)
        .hit()
        .focusable()
        .disabled(!enabled)
        .semantics(Role::Button)
        .name_text(name)
}

impl Chrome {
    pub fn new(engine: &mut TextEngine, spec: &Spec, width: f32) -> Self {
        let mut keys = vec![];
        // The heading gives way first: it is bounded to what the counter
        // and the three buttons leave, and elides past that.
        let inset = Space::S3.px(Profile::Desktop);
        let small = Space::S1.px(Profile::Desktop);
        let available = (width - 2.0 * inset).max(1.0);
        let position_w = spec.position.map_or(0.0, |position| {
            engine.simple_layout(position, crate::role::meta()).width() + small
        });
        let buttons_w = 3.0 * (ICON_BUTTON.value + small);
        // Emphasis is a little wider than the measured regular face.
        let heading_w = engine
            .simple_layout(spec.heading, crate::role::row())
            .width()
            * 1.1;
        let heading_w = heading_w.min((available - position_w - buttons_w).max(0.0));
        let mut header = row(HEADER)
            .pad_x(Space::S3)
            .pad_y(Space::S2)
            .gap(Space::S1)
            .items_center()
            .child(
                text(spec.heading)
                    .role(roles::ROW)
                    .ink(Ink::Primary)
                    .emphasis(true)
                    .truncate()
                    .w(Length::Runtime(heading_w, "bounded review heading")),
            );
        if let Some(position) = spec.position {
            header = header.child(text(position).role(roles::META).ink(Ink::Secondary));
        }
        header = header.child(spacer());
        for arrow in &spec.arrows {
            // Close stays usable while a request is pending.
            let enabled = arrow.enabled && (!spec.busy || arrow.part == Part::Close);
            let key = key(spec.id, arrow.part);
            keys.push((arrow.part, key, enabled));
            header = header.child(icon_button(key, arrow.part, arrow.name, enabled));
        }
        let header = col("Routine review header slot").child(
            col(HEADER_BLOCK)
                .child(header)
                .child(divider(Line::Divider)),
        );

        let gap = Space::S2.px(Profile::Desktop);
        let mut start = vec![];
        let mut end = vec![];
        let (mut start_w, mut end_w) = (0.0, 0.0);
        for tool in spec.tools {
            let enabled = tool.enabled && !spec.busy;
            let key = key(spec.id, tool.part);
            keys.push((tool.part, key, enabled));
            start_w += ICON_BUTTON.value + if start.is_empty() { 0.0 } else { gap };
            start.push(icon_button(key, tool.part, tool.name, enabled));
        }
        for (part, label, primary) in spec.actions {
            let style = if *primary {
                ActionStyle::Primary
            } else {
                ActionStyle::Secondary
            };
            let measured = ActionButton::new(label, style, None)
                .width(engine)
                .min(available);
            let key = key(spec.id, *part);
            keys.push((*part, key, !spec.busy));
            let button = elements::button(
                "Routine review action",
                &ButtonSpec {
                    key,
                    label: label.clone(),
                    width: measured,
                    primary: *primary,
                    disabled: spec.busy,
                    focused: spec.focused == Some(*part),
                    // *Skip* teaches its Alt+S where the label has the letter.
                    mnemonic: (*part == Part::Skip)
                        .then(|| {
                            match koru_keys::registry::get(koru_keys::ShortcutId::ReviewSkip)
                                .chord
                                .on
                            {
                                koru_keys::On::Letter(letter) => Some(letter),
                                koru_keys::On::Named(_) => None,
                            }
                        })
                        .flatten(),
                },
            )
            .w(Length::Runtime(measured, "measured review action"));
            // *Show more* reads the result, so it sits with it at the start;
            // the decisions about the result sit at the end.
            if *part == Part::More {
                start_w += measured + if start.is_empty() { 0.0 } else { gap };
                start.push(button);
            } else {
                end_w += measured + if end.is_empty() { 0.0 } else { gap };
                end.push(button);
            }
        }
        let footer = (!spec.actions.is_empty() || !spec.tools.is_empty()).then(|| {
            let line = |name, children: Vec<El>, lead: bool| {
                let mut line = row(name).gap(Space::S2).items_center();
                if lead {
                    line = line.child(spacer());
                }
                line.children(children)
            };
            if start.is_empty() || start_w + gap + end_w <= available {
                let mut one = row(FOOTER).pad(Space::S3).gap(Space::S2).items_center();
                one = one.children(start).child(spacer()).children(end);
                col("Routine review footer slot").child(one)
            } else {
                col("Routine review footer slot").child(
                    col(FOOTER)
                        .pad(Space::S3)
                        .gap(Space::S2)
                        .child(line("Routine review reading", start, false))
                        .child(line("Routine review decisions", end, true)),
                )
            }
        });
        Self {
            header,
            footer,
            keys,
            focused: spec.focused.map(|part| key(spec.id, part)),
        }
    }

    pub fn header_height(&self, width: f32) -> f32 {
        measure(&self.header, HEADER_BLOCK, width)
    }
    pub fn footer_height(&self, width: f32) -> f32 {
        self.footer
            .as_ref()
            .map_or(0.0, |f| measure(f, FOOTER, width))
    }
    /// The part under `(x, y)` that acts now, given where each tree sits.
    pub fn hit(&self, header: Rect, footer: Option<Rect>, x: f32, y: f32) -> Option<Part> {
        let mut hit = elements::layout(&self.header, header, FrameCx::root()).hit_at(x, y);
        if hit.is_none()
            && let (Some(tree), Some(rect)) = (&self.footer, footer)
        {
            hit = elements::layout(tree, rect, FrameCx::root()).hit_at(x, y);
        }
        let hit = hit?;
        // A disabled control still reports its key; it does not act.
        self.keys
            .iter()
            .find(|(_, key, enabled)| *key == hit && *enabled)
            .map(|(part, _, _)| *part)
    }
    /// Every control where `header` and `footer` put it, and whether it
    /// acts now: what the pointer's hover and tooltips are read against.
    pub fn targets(&self, header: Rect, footer: Option<Rect>) -> Vec<(Part, Rect, bool)> {
        let header = elements::layout(&self.header, header, FrameCx::root());
        let footer = self
            .footer
            .as_ref()
            .zip(footer)
            .map(|(tree, rect)| elements::layout(tree, rect, FrameCx::root()));
        self.keys
            .iter()
            .filter_map(|(part, key, enabled)| {
                header
                    .node(*key)
                    .or_else(|| footer.as_ref()?.node(*key))
                    .map(|node| (*part, node.rect, *enabled))
            })
            .collect()
    }
    #[cfg(test)]
    pub fn rect_of(&self, part: Part, header: Rect, footer: Option<Rect>) -> Option<Rect> {
        let (_, key, _) = self.keys.iter().find(|(p, _, _)| *p == part)?;
        elements::layout(&self.header, header, FrameCx::root())
            .node(*key)
            .map(|node| node.rect)
            .or_else(|| {
                let tree = self.footer.as_ref()?;
                elements::layout(tree, footer?, FrameCx::root())
                    .node(*key)
                    .map(|node| node.rect)
            })
    }
    fn paint(
        &self,
        tree: &El,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        pointer: (f32, f32),
    ) {
        let hot = elements::layout(tree, rect, FrameCx::root()).hit_at(pointer.0, pointer.1);
        let frame = elements::layout(
            tree,
            rect,
            FrameCx::root().with_interaction(Interaction {
                hot,
                pressed: None,
                focused: self.focused,
            }),
        );
        frame.paint(tree, canvas, root, engine);
    }
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        header: Rect,
        footer: Option<Rect>,
        pointer: (f32, f32),
    ) {
        self.paint(&self.header, canvas, root, engine, header, pointer);
        if let (Some(tree), Some(rect)) = (&self.footer, footer) {
            self.paint(tree, canvas, root, engine, rect, pointer);
        }
    }
}

/// The height of the part named `name` inside a slot laid out `width` wide.
pub(super) fn measure(tree: &El, name: &str, width: f32) -> f32 {
    elements::layout(
        tree,
        Rect::new(0.0, 0.0, width, f32::from(u16::MAX)),
        FrameCx::root(),
    )
    .rect_of(name)
    .map_or(0.0, |rect| rect.h)
}

/// The result's words: their own laid-out text, so a drag or a double
/// click selects in them as in a chat reply.
pub(super) const WORDS: &str = "Routine review words";

struct Words {
    layout: std::rc::Rc<parley::Layout<koru_draw::theme::Brush>>,
    selected: Option<std::ops::Range<usize>>,
}

impl koru_draw::el::Leaf for Words {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        (width.unwrap_or(self.layout.width()), self.layout.height())
    }
    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        _: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        _: koru_draw::el::ElState,
    ) {
        use parley::editing::{Cursor, Selection};
        use parley::layout::Affinity;
        if let Some(range) = self.selected.clone().filter(|r| !r.is_empty()) {
            Selection::new(
                Cursor::from_byte_index(&self.layout, range.start, Affinity::Downstream),
                Cursor::from_byte_index(&self.layout, range.end, Affinity::Upstream),
            )
            .geometry_with(&self.layout, |bb, _| {
                scene::fill(
                    canvas,
                    root,
                    theme::selection_color(),
                    &scene::rect(
                        at.x + bb.x0 as f32,
                        at.y + bb.y0 as f32,
                        (bb.x1 - bb.x0) as f32,
                        (bb.y1 - bb.y0) as f32,
                    ),
                );
            });
        }
        scene::draw_layout(
            canvas,
            root,
            &self.layout,
            at.x,
            at.y,
            Some(theme::p().text),
        );
    }
}

/// The result itself: where it came from and when, then its words.
pub(super) fn body(
    meta: &str,
    words: std::rc::Rc<parley::Layout<koru_draw::theme::Brush>>,
    selected: Option<std::ops::Range<usize>>,
) -> El {
    let height = words.height();
    col("Routine review result slot").child(
        col(BODY)
            .pad(Space::S3)
            .gap(Space::S1)
            .child(text(meta).role(roles::META).ink(Ink::Secondary).wrap())
            .child(
                koru_draw::el::leaf(
                    WORDS,
                    Words {
                        layout: words,
                        selected,
                    },
                )
                .semantic()
                .h(Length::Runtime(height, "laid-out result words")),
            ),
    )
}
