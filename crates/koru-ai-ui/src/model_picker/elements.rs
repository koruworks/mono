// SPDX-License-Identifier: MPL-2.0

//! **The picker's chrome as elements** (koru-ai D64, `koru_draw::el`): one
//! tree, laid out by the element solver, gives every part of the picker its
//! place — and the same tree, painted inside the *Popover surface* record,
//! nests everything drawn in the panel under it in the inspector.
//!
//! ```text
//! row  Model picker                     the panel, gap RAIL_CONTENT_GAP
//!   col  Agent rail                     PICKER_RAIL_W, marks centred
//!     Rail mark · Rail gap · [Rail divider] · … · Manage agent providers
//!   col  Picker content                 fill, PICKER_PAD at top, end, foot
//!     row  Search row                   PICKER_SEARCH_H
//!       row  Search box                 Search field (fill) · Refresh models
//!     Model list                        fill: the rows and their scrollbar
//!     Fast row · Context row · Effort footer
//! ```
//!
//! **Existing controls go in as leaves** (ELEMENTS.final §3.1, the custom
//! leaf): the search field's `TextInput`, the list with its scrollbar,
//! stars and flyout row, the Segmented rows and the effort slider have no
//! element form yet, so each leaf delegates its paint to the code that drew
//! it before, in the allocation the frame gives it. What the element layer
//! could not express is in `docs/contributing/refactor-ui/QUESTIONS.md`
//! (*Model picker on elements*).
//!
//! **Every value is a token or a tagged metric** (QUESTIONS E4): the
//! picker's own sizes are tagged here, so the inspector names them.

use std::cell::RefCell;

use koru_draw::el::{
    Action, Align, El, ElState, Frame, FrameCx, Key, Leaf, Role, TextCache, boxed, col, divider,
    leaf, row,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Line, Metric, Space, Tagged};
use vello::Scene;
use vello::kurbo::Affine;

use super::{
    CONTEXT_H, FAST_H, FOOTER_H, Geometry, RAIL_BUTTON, RAIL_W, ROW_H, RailEntry, SEARCH_H,
};

/// The rail's width ([`RAIL_W`]).
pub(crate) const RAIL_WIDTH: Metric = Metric::new("PICKER_RAIL_W", RAIL_W);
/// A rail mark's square, and the divider's width under it.
pub(crate) const RAIL_MARK: Metric = Metric::new("PICKER_RAIL_MARK", RAIL_BUTTON);
/// The search row, ↻ at its end ([`SEARCH_H`]).
pub(crate) const SEARCH_ROW_H: Metric = Metric::new("PICKER_SEARCH_H", SEARCH_H);

/// Air between the rail and the content ([`super::RAIL_CONTENT_GAP`]).
pub(crate) const CONTENT_GAP: Tagged<Space> = Tagged::new("RAIL_CONTENT_GAP", Space::S2);
/// The content's inset from the panel's top, end and foot ([`super::PAD`]).
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("PICKER_PAD", Space::S1);
/// The rail's inset above its first mark and below its last.
pub(crate) const RAIL_INSET: Tagged<Space> = Tagged::new("PICKER_RAIL_INSET", Space::S2);
/// The room after each mark ([`super::RAIL_GAP`]).
pub(crate) const MARK_GAP: Tagged<Space> = Tagged::new("PICKER_RAIL_GAP", Space::S1_5);
/// A rail divider's inset on every side of its hairline: with the hairline,
/// the divider's room ([`super::RAIL_DIVIDER`]).
pub(crate) const DIVIDER_INSET: Tagged<Space> = Tagged::new("PICKER_RAIL_DIVIDER_INSET", Space::S1);
/// The search box's inset in its row.
pub(crate) const SEARCH_INSET: Tagged<Space> = Tagged::new("PICKER_SEARCH_INSET", Space::S1);
/// Between the search field and ↻, and after ↻.
pub(crate) const REFRESH_GAP: Tagged<Space> = Tagged::new("PICKER_REFRESH_GAP", Space::S0_5);

/// ↻'s own size: the leaf's intrinsic extent, as `IconButton` is drawn.
pub(crate) const REFRESH_W: f32 = 26.0;
pub(crate) const REFRESH_H: f32 = SEARCH_H - 12.0;

/// The developer names the picker reads its geometry back by. Free text;
/// what a screen reader hears is each control's translated name.
pub(crate) const PICKER: &str = "Model picker";
pub(crate) const RAIL: &str = "Agent rail";
pub(crate) const CONTENT: &str = "Picker content";
pub(crate) const MARK: &str = "Rail mark";
pub(crate) const MANAGE_NAME: &str = "Manage agent providers";
pub(crate) const SEARCH_ROW: &str = "Search row";
pub(crate) const SEARCH_BOX: &str = "Search box";
pub(crate) const SEARCH_FIELD: &str = "Search field";
pub(crate) const REFRESH: &str = "Refresh models";
pub(crate) const LIST: &str = "Model list";
pub(crate) const FAST_ROW: &str = "Fast row";
pub(crate) const CONTEXT_ROW: &str = "Context row";
pub(crate) const EFFORT_FOOTER: &str = "Effort footer";

/// The picker, as its keys name it.
pub(crate) const SURFACE: &str = "model-picker";
pub(crate) const FAVORITES_KEY: Key = Key::surface(SURFACE, "favorites");
pub(crate) const MANAGE_KEY: Key = Key::surface(SURFACE, "manage-agents");
pub(crate) const SEARCH_KEY: Key = Key::surface(SURFACE, "search");
pub(crate) const REFRESH_KEY: Key = Key::surface(SURFACE, "refresh");
pub(crate) const LIST_KEY: Key = Key::surface(SURFACE, "list");
pub(crate) const FAST_KEY: Key = Key::surface(SURFACE, "fast");
pub(crate) const CONTEXT_KEY: Key = Key::surface(SURFACE, "context");
pub(crate) const EFFORT_KEY: Key = Key::surface(SURFACE, "effort");

/// A rail entry's key: *Favorites* by its keyword, an agent or a built-in
/// provider by its id — never its place on the rail, which the person can
/// reorder.
pub(crate) fn mark_key(entry: &RailEntry) -> Key {
    match entry {
        RailEntry::Favorites => FAVORITES_KEY,
        RailEntry::Provider(provider) => Key::model(stable_id(provider.as_str()), "provider"),
        RailEntry::Agent(agent) => Key::model(stable_id(agent.as_str()), "agent"),
    }
}

/// FNV-1a over `text`, widened: the same id gives the same key in every
/// frame and every run.
fn stable_id(text: &str) -> u128 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    u128::from(hash)
}

/// What a rail mark is called for a screen reader.
pub(crate) enum MarkName {
    Key(koru_i18n::Key),
    Content(String),
}

/// One rail mark.
pub(crate) struct MarkSpec {
    pub(crate) key: Key,
    pub(crate) name: MarkName,
    /// A divider sets it off from the marks after it.
    pub(crate) divider_after: bool,
    /// The entry whose models the list shows.
    pub(crate) selected: bool,
}

/// What the picker shows, as far as its layout and its declarations go.
#[derive(Default)]
pub(crate) struct Spec {
    pub(crate) marks: Vec<MarkSpec>,
    /// The **+** at the rail's end (D29).
    pub(crate) manage: bool,
    /// ↻, while an agent is selected, and whether it is disabled now.
    pub(crate) refresh: Option<bool>,
    pub(crate) fast: bool,
    pub(crate) context: bool,
    pub(crate) levels: bool,
}

/// A leaf of the picker, for the host to hand a painter.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Mark(usize),
    Manage,
    Search,
    Refresh,
    List,
    Fast,
    Context,
    Effort,
}

/// An existing control's paint, run in the leaf's allocation with the
/// element's resolved state.
pub(crate) type Paint = Box<dyn Fn(&mut Scene, Affine, &mut TextEngine, Rect, ElState)>;

/// **The adapter every picker leaf is**: an intrinsic size, and the existing
/// drawing it delegates to — none when the tree is laid out only for its
/// geometry.
pub(crate) struct HostLeaf {
    size: (f32, f32),
    paint: Option<Paint>,
}

impl HostLeaf {
    fn new(size: (f32, f32), paint: Option<Paint>) -> Self {
        Self { size, paint }
    }
}

impl Leaf for HostLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        self.size
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        state: ElState,
    ) {
        if let Some(paint) = &self.paint {
            paint(scene, root, engine, at, state);
        }
    }
}

/// **The rail**: its marks one under another, each followed by its room and,
/// where one is set off, a divider — the same column for the picker and for
/// [`super::rail_rects`].
pub(crate) fn rail_column(slots: impl IntoIterator<Item = (El, bool)>) -> El {
    let mut rail = col(RAIL).w(RAIL_WIDTH).pad_y(RAIL_INSET).items_center();
    for (mark, divider_after) in slots {
        rail = rail.child(mark).child(boxed("Rail gap").pad_top(MARK_GAP));
        if divider_after {
            rail = rail.child(
                boxed("Rail divider")
                    .w(RAIL_MARK)
                    .pad_x(DIVIDER_INSET)
                    .pad_y(DIVIDER_INSET)
                    .child(divider(Line::Border)),
            );
        }
    }
    rail
}

/// A mark's square with no paint and no declarations: the geometry of a
/// rail of `count` marks.
pub(crate) fn placeholder_mark() -> El {
    leaf(MARK, HostLeaf::new((RAIL_BUTTON, RAIL_BUTTON), None)).decorative()
}

/// **The picker as one tree.** `paint` hands each leaf its painter; a tree
/// laid out only for its geometry hands none.
pub(crate) fn tree(spec: &Spec, paint: &mut dyn FnMut(Part) -> Option<Paint>) -> El {
    let mut slots = Vec::with_capacity(spec.marks.len() + 1);
    for (index, mark) in spec.marks.iter().enumerate() {
        let el = leaf(
            MARK,
            HostLeaf::new((RAIL_BUTTON, RAIL_BUTTON), paint(Part::Mark(index))),
        )
        .semantic()
        .id(mark.key)
        .hit()
        .action(Action::Click)
        .semantics(Role::Button)
        .selected(mark.selected);
        let el = match &mark.name {
            MarkName::Key(key) => el.name(*key),
            MarkName::Content(text) => el.name_text(text.clone()),
        };
        slots.push((el, mark.divider_after));
    }
    if spec.manage {
        // No tooltip (D25): it is named for a screen reader instead.
        let plus = leaf(
            MANAGE_NAME,
            HostLeaf::new((RAIL_BUTTON, RAIL_BUTTON), paint(Part::Manage)),
        )
        .semantic()
        .id(MANAGE_KEY)
        .hit()
        .action(Action::Click)
        .semantics(Role::Button)
        .name(koru_i18n::Key::AiPickerManageAgents);
        slots.push((plus, false));
    }

    let refresh = leaf(
        REFRESH,
        HostLeaf::new((REFRESH_W, REFRESH_H), paint(Part::Refresh)),
    );
    let refresh = match spec.refresh {
        Some(disabled) => refresh
            .semantic()
            .id(REFRESH_KEY)
            .hit()
            .action(Action::Click)
            .semantics(Role::Button)
            .name(koru_i18n::Key::AiModelsRefresh)
            .disabled(disabled),
        // Its room is kept, so the field does not widen from agent to
        // built-in provider; nothing is drawn there.
        None => refresh.decorative(),
    }
    .align_self(Align::Centre);
    let search = row(SEARCH_ROW).h(SEARCH_ROW_H).pad_y(SEARCH_INSET).child(
        // The whole box presses the field, as it always has; ↻, painted
        // later inside it, is the one pressed over it.
        row(SEARCH_BOX)
            .fill()
            .gap(REFRESH_GAP)
            .pad_end(REFRESH_GAP)
            .hit_as(SEARCH_KEY)
            .child(
                leaf(SEARCH_FIELD, HostLeaf::new((0.0, 0.0), paint(Part::Search)))
                    .semantic()
                    .id(SEARCH_KEY)
                    .fill(),
            )
            .child(refresh),
    );
    let list = leaf(LIST, HostLeaf::new((0.0, 0.0), paint(Part::List)))
        .semantic()
        .id(LIST_KEY)
        .hit()
        .fill();
    let mut content = col(CONTENT)
        .fill()
        .pad_top(PANEL_INSET)
        .pad_end(PANEL_INSET)
        .pad_bottom(PANEL_INSET)
        .child(search)
        .child(list);
    for (shown, name, key, height, part) in [
        (spec.fast, FAST_ROW, FAST_KEY, FAST_H, Part::Fast),
        (
            spec.context,
            CONTEXT_ROW,
            CONTEXT_KEY,
            CONTEXT_H,
            Part::Context,
        ),
        (
            spec.levels,
            EFFORT_FOOTER,
            EFFORT_KEY,
            FOOTER_H,
            Part::Effort,
        ),
    ] {
        if shown {
            content = content.child(
                leaf(name, HostLeaf::new((0.0, height), paint(part)))
                    .semantic()
                    .id(key)
                    .hit(),
            );
        }
    }
    row(PICKER)
        .gap(CONTENT_GAP)
        .child(rail_column(slots))
        .child(content)
}

thread_local! {
    /// The engine and cache the picker's layouts run on. The tree holds no
    /// text of its own — its words are its leaves' — so neither shapes
    /// anything; the solver only asks for them. One per UI thread, as the
    /// switcher's input layouts have (`popover_elements::with_input_engine`).
    static LAYOUT: RefCell<Option<(TextEngine, TextCache)>> = const { RefCell::new(None) };
}

/// `tree` laid out in `area`, the panel, under `cx`.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    LAYOUT.with_borrow_mut(|slot| {
        let (engine, cache) = slot.get_or_insert_with(|| (TextEngine::new(), TextCache::default()));
        tree.layout(engine, cache, area, cx)
    })
}

/// The list's rows inside the room the frame gave it: whole rows only, from
/// its top — the list scrolls a row at a time.
pub(crate) fn list_rect(band: Rect) -> Rect {
    let rows = (band.h / ROW_H).floor().max(0.0);
    Rect::new(band.x, band.y, band.w, rows * ROW_H)
}

/// The picker's parts, read back from its frame.
pub(crate) fn geometry_of(frame: &Frame, panel: Rect) -> Geometry {
    let at = |name: &str| frame.rect_of(name);
    let none = Rect::new(panel.x, panel.y, 0.0, 0.0);
    Geometry {
        panel,
        rail: at(RAIL).unwrap_or(none),
        search: at(SEARCH_BOX).unwrap_or(none),
        field: at(SEARCH_FIELD).unwrap_or(none),
        refresh: at(REFRESH).unwrap_or(none),
        list: list_rect(at(LIST).unwrap_or(none)),
        footer: at(EFFORT_FOOTER),
        context: at(CONTEXT_ROW),
        fast: at(FAST_ROW),
    }
}
