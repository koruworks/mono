// SPDX-License-Identifier: MPL-2.0

//! **The skill list as elements** (koru-ai D121; `koru_draw::el`): one tree
//! gives every row of the open list its place, and the same tree, painted
//! inside the panel's `FloatingPanel` record, nests everything drawn in the
//! panel under it in the inspector.
//!
//! ```text
//! col  Skill list                     the panel, SKILL_PANEL_PAD round
//!   row  Skill row                    SKILL_ROW_H, a skill a row
//!     col  Skill words                fill, centred
//!       Skill name                    row role, elided
//!       Skill line                    meta role, secondary, elided
//!   …                                 the rows that fit, the keyboard's among them
//! ```
//!
//! A row lights on the hover plate under the pointer, and stays lit while
//! the keyboard is on it. Nothing here is a leaf: every part is a box or
//! text. Every value is a token or a tagged metric ([`super::tokens`]); this
//! file is on the source scan's migrated list (`koru_draw::theme::ui_scan`).

use koru_draw::el::{Align, El, Frame, FrameCx, Key, Look, Role, StateRecipe, col, row, text};
use koru_draw::geometry::Rect;
use koru_draw::theme::{Ink, Radius, Surface, roles};

pub(crate) use super::tokens::*;

/// The developer names the list reads its geometry back by.
pub(crate) const PANEL: &str = "Skill list";
pub(crate) const SKILL_ROW: &str = "Skill row";
pub(crate) const SKILL_WORDS: &str = "Skill words";
pub(crate) const SKILL_NAME: &str = "Skill name";
pub(crate) const SKILL_LINE: &str = "Skill line";

/// A skill's row, by its name: the same skill gives the same key in every
/// frame, wherever the filter puts it.
pub(crate) fn row_key(name: &str) -> Key {
    Key::model(crate::model_picker::stable_id(name), "skill")
}

/// One skill as the list draws it.
pub(crate) struct SkillSpec {
    pub(crate) key: Key,
    /// The agent's name to show, else the skill's own.
    pub(crate) title: String,
    /// One line about it; empty when the agent gave none.
    pub(crate) line: String,
    /// The keyboard is on it: lit whatever the pointer does.
    pub(crate) lit: bool,
}

/// **A row's states**: lit on the hover plate under the pointer.
pub(crate) static ROW_STATES: StateRecipe = StateRecipe {
    name: "skill row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: None,
};

/// The row the keyboard is on: on the hover plate whatever the pointer
/// does.
pub(crate) static ROW_LIT_STATES: StateRecipe = StateRecipe {
    name: "skill row, the keyboard's",
    base: Look::bg(Surface::Hover),
    ..ROW_STATES
};

/// **A row of the list**: the skill's name, and its line under it, both
/// elided to the row.
fn skill_row(spec: &SkillSpec) -> El {
    let mut words = col(SKILL_WORDS)
        .fill()
        .gap(WORDS_GAP)
        .align_self(Align::Centre)
        .child(
            text(spec.title.clone())
                .named(SKILL_NAME)
                .role(roles::ROW)
                .ink(Ink::Primary)
                .truncate(),
        );
    if !spec.line.is_empty() {
        words = words.child(
            text(spec.line.clone())
                .named(SKILL_LINE)
                .role(roles::META)
                .ink(Ink::Secondary)
                .truncate(),
        );
    }
    row(SKILL_ROW)
        .h(ROW_HEIGHT)
        .radius(Radius::M)
        .pad_x(ROW_INSET)
        .items_center()
        .states(if spec.lit {
            &ROW_LIT_STATES
        } else {
            &ROW_STATES
        })
        .id(spec.key)
        .hit()
        .semantics(Role::MenuItem)
        .name_text(spec.title.clone())
        .child(words)
}

/// **The list as one tree**: a row a skill shown, in the list's order,
/// inside the panel's inset.
pub(crate) fn tree(skills: &[SkillSpec]) -> El {
    col(PANEL)
        .pad(PANEL_INSET)
        .semantics(Role::Menu)
        .name(koru_i18n::Key::AiSkills)
        .children(skills.iter().map(skill_row))
}

/// `tree` laid out in `area`, the panel, under `cx`: on the engine and cache
/// the composer's panels share with the model picker.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    crate::model_picker::layout_elements(tree, area, cx)
}
