// SPDX-License-Identifier: MPL-2.0

//! **What the Lab lists, and where each thing really lives.**
//!
//! One record a component: a stable catalog ID, the family it is filed under,
//! the production source that owns it, where it is available, and — a recipe
//! at a time — the callable symbol, the styles that symbol's own type offers,
//! whether a model-written declaration may use it, and whether the Lab has a
//! story for it.
//!
//! **A seed, not the inventory.** It holds the one component the first trial
//! shows. The full audit of every shared, domain and native surface comes
//! after that trial and adds its records here; nothing in this file claims a
//! count.
//!
//! **Styles are the production type's own values.** A dialog button is styled
//! by [`ButtonRole`] and a compact action by [`ActionStyle`]; they are two
//! enums with different members and different paint, so [`Styles`] carries
//! each as itself rather than flattening both into one invented "variant".
//! The slot functions below match on every member of each enum, so a style
//! added to or removed from `koru-ui` stops this file compiling until the
//! record is brought back in line.

use koru_ui::button::{ActionStyle, ButtonRole};

/// The three groups the navigator files entries under.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// Tokens and the element layer: spacing, color roles, typography.
    Foundations,
    /// Shared widgets: something a person presses, types into or chooses from.
    Controls,
    /// Surfaces assembled from controls over application state.
    Compositions,
}

impl Family {
    /// The groups in the order the navigator shows them, whatever order the
    /// entries were registered in.
    pub(crate) const ALL: [Self; 3] = [Self::Foundations, Self::Controls, Self::Compositions];

    /// The group's heading in the navigator.
    pub(crate) fn heading(self) -> koru_i18n::Key {
        match self {
            Self::Foundations => koru_i18n::Key::UiLabFamilyFoundations,
            Self::Controls => koru_i18n::Key::UiLabFamilyControls,
            Self::Compositions => koru_i18n::Key::UiLabFamilyCompositions,
        }
    }
}

/// A platform an entry's availability is recorded for, one at a time: shared
/// Rust source on one says nothing about a native adapter on another.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    MacOs,
    Linux,
    Android,
    Ios,
}

/// What is known about an entry on one platform. Availability, not
/// acceptance: none of these says a person has verified it there.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    /// The production implementation the Lab itself draws.
    Available,
    /// The same Rust source builds there; nobody has checked the result.
    SharedSourceUnverified,
    /// A native control covers part of the contract, not all of it.
    NativePartial,
    /// Not looked at yet.
    NotAudited,
}

/// Whether a model-written composition may ask for this recipe.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum A2ui {
    /// In the composition catalog. The host picks the style; a declaration
    /// cannot.
    HostStyled,
    /// Not something a declaration can name.
    NotSupported,
}

/// Whether the Lab can show this recipe running.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Story {
    /// A story with this stable ID.
    Available(&'static str),
    /// Listed, with nothing to run yet. Never a stand-in preview.
    NoStoryYet,
}

/// The production source that owns an entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// The module path, as a caller writes it.
    pub(crate) module: &'static str,
    /// The file, from the repository root.
    pub(crate) file: &'static str,
}

/// The styles one recipe supports, as its own production type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Styles {
    /// `koru_ui::button::ButtonRole`: what a dialog button is for.
    Dialog(&'static [ButtonRole]),
    /// `koru_ui::button::ActionStyle`: a compact toolbar or panel action.
    Compact(&'static [ActionStyle]),
}

/// One input of a recipe that a story lets a person change. The properties
/// panel is built from a recipe's list of these, so it offers exactly the
/// inputs the production call takes and nothing invented beside them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// The `label: &str` the recipe draws: free text.
    Label,
    /// One of the recipe's [`Styles`]: a choice.
    Style,
}

impl Control {
    /// What the panel calls this input.
    pub(crate) fn name(self) -> koru_i18n::Key {
        match self {
            Self::Label => koru_i18n::Key::UiLabControlLabel,
            Self::Style => koru_i18n::Key::UiLabControlStyle,
        }
    }

    /// The stable part of the control's identity in the window: what the
    /// pointer, the keyboard focus and a recipe's written steps refer to.
    pub(crate) fn part(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Style => "style",
        }
    }
}

/// One callable way to draw an entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recipe {
    /// The symbol a caller invokes.
    pub(crate) symbol: &'static str,
    pub(crate) styles: Styles,
    /// The inputs a story of this recipe exposes, in panel order.
    pub(crate) controls: &'static [Control],
    pub(crate) a2ui: A2ui,
    pub(crate) story: Story,
}

/// One component the Lab lists.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Stable across renames; the same ID the component catalog files it by.
    pub(crate) catalog_id: &'static str,
    /// The component's public name, a technical identifier and never
    /// translated.
    pub(crate) public_name: &'static str,
    /// What a person calls it, in the display language.
    pub(crate) name: koru_i18n::Key,
    /// One line on what it is for. Not documentation: the contract stays
    /// with the component's owner.
    pub(crate) purpose: koru_i18n::Key,
    pub(crate) family: Family,
    pub(crate) source: Source,
    pub(crate) platforms: [(Platform, Availability); 5],
    pub(crate) recipes: &'static [Recipe],
}

/// The dialog button's styles, in the order `ButtonRole` declares them. The
/// first is its default.
pub(crate) const DIALOG_STYLES: [ButtonRole; 3] = [
    ButtonRole::Secondary,
    ButtonRole::Primary,
    ButtonRole::Destructive,
];

/// The compact action's styles, in the order `ActionStyle` declares them.
pub(crate) const COMPACT_STYLES: [ActionStyle; 4] = [
    ActionStyle::Primary,
    ActionStyle::Secondary,
    ActionStyle::Ghost,
    ActionStyle::Destructive,
];

/// Where `role` sits in [`DIALOG_STYLES`]. Every member is matched, so the
/// list cannot fall behind the enum.
pub(crate) const fn dialog_slot(role: ButtonRole) -> usize {
    match role {
        ButtonRole::Secondary => 0,
        ButtonRole::Primary => 1,
        ButtonRole::Destructive => 2,
    }
}

/// What the style control calls `role`. The option's label is translated; the
/// enum member it stands for is not, and is what a written recipe names.
pub(crate) const fn dialog_style_label(role: ButtonRole) -> koru_i18n::Key {
    match role {
        ButtonRole::Secondary => koru_i18n::Key::UiLabStyleSecondary,
        ButtonRole::Primary => koru_i18n::Key::UiLabStylePrimary,
        ButtonRole::Destructive => koru_i18n::Key::UiLabStyleDestructive,
    }
}

/// Where `style` sits in [`COMPACT_STYLES`].
pub(crate) const fn compact_slot(style: ActionStyle) -> usize {
    match style {
        ActionStyle::Primary => 0,
        ActionStyle::Secondary => 1,
        ActionStyle::Ghost => 2,
        ActionStyle::Destructive => 3,
    }
}

// Each list is its enum, member for member and in order, or this file does
// not compile: the slot functions match every member, so a list of the same
// length whose every member sits in its own slot can be missing none.
const _: () = {
    let mut slot = 0;
    while slot < DIALOG_STYLES.len() {
        assert!(dialog_slot(DIALOG_STYLES[slot]) == slot);
        slot += 1;
    }
    let mut slot = 0;
    while slot < COMPACT_STYLES.len() {
        assert!(compact_slot(COMPACT_STYLES[slot]) == slot);
        slot += 1;
    }
};

/// The catalog ID of the shared button.
pub(crate) const BUTTON: &str = "C-BUTTON";

/// The story the first trial runs: the dialog button, pressed for real.
pub(crate) const BUTTON_DIALOG_STORY: &str = "C-BUTTON/dialog";

/// Every entry, in the order the navigator lists them.
pub(crate) const ENTRIES: &[Entry] = &[Entry {
    catalog_id: BUTTON,
    public_name: "KoruButton",
    name: koru_i18n::Key::UiLabButtonName,
    purpose: koru_i18n::Key::UiLabButtonPurpose,
    family: Family::Controls,
    source: Source {
        module: "koru_ui::button",
        file: "crates/koru-ui/src/button.rs",
    },
    platforms: [
        (Platform::Windows, Availability::Available),
        (Platform::MacOs, Availability::SharedSourceUnverified),
        (Platform::Linux, Availability::SharedSourceUnverified),
        (Platform::Android, Availability::NotAudited),
        (Platform::Ios, Availability::NativePartial),
    ],
    recipes: &[
        Recipe {
            symbol: "koru_ui::button::draw_button",
            styles: Styles::Dialog(&DIALOG_STYLES),
            controls: &[Control::Label, Control::Style],
            a2ui: A2ui::NotSupported,
            story: Story::Available(BUTTON_DIALOG_STORY),
        },
        Recipe {
            symbol: "koru_ui::button::ActionButton",
            styles: Styles::Compact(&COMPACT_STYLES),
            controls: &[Control::Label, Control::Style],
            a2ui: A2ui::HostStyled,
            story: Story::NoStoryYet,
        },
    ],
}];

/// The entry and recipe a story ID runs.
pub(crate) fn story(story_id: &str) -> Option<(&'static Entry, &'static Recipe)> {
    ENTRIES.iter().find_map(|entry| {
        entry
            .recipes
            .iter()
            .find(|recipe| matches!(recipe.story, Story::Available(id) if id == story_id))
            .map(|recipe| (entry, recipe))
    })
}

#[cfg(test)]
mod tests;
