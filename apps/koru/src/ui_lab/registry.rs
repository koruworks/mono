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

impl Platform {
    pub(crate) const ALL: [Self; 5] = [
        Self::Windows,
        Self::MacOs,
        Self::Linux,
        Self::Android,
        Self::Ios,
    ];
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

impl Styles {
    /// The path of the type the styles are values of.
    pub(crate) fn type_path(self) -> &'static str {
        match self {
            Self::Dialog(_) => "koru_ui::button::ButtonRole",
            Self::Compact(_) => "koru_ui::button::ActionStyle",
        }
    }

    pub(crate) fn len(self) -> usize {
        match self {
            Self::Dialog(styles) => styles.len(),
            Self::Compact(styles) => styles.len(),
        }
    }
}

/// One callable way to draw an entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recipe {
    /// The symbol a caller invokes.
    pub(crate) symbol: &'static str,
    pub(crate) styles: Styles,
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
    pub(crate) family: Family,
    pub(crate) source: Source,
    pub(crate) platforms: [(Platform, Availability); 5],
    pub(crate) recipes: &'static [Recipe],
}

impl Entry {
    pub(crate) fn availability(&self, platform: Platform) -> Availability {
        self.platforms
            .iter()
            .find(|(listed, _)| *listed == platform)
            .map_or(Availability::NotAudited, |(_, availability)| *availability)
    }
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

/// Where `style` sits in [`COMPACT_STYLES`].
pub(crate) const fn compact_slot(style: ActionStyle) -> usize {
    match style {
        ActionStyle::Primary => 0,
        ActionStyle::Secondary => 1,
        ActionStyle::Ghost => 2,
        ActionStyle::Destructive => 3,
    }
}

/// The catalog ID of the shared button.
pub(crate) const BUTTON: &str = "C-BUTTON";

/// The story the first trial runs: the dialog button, pressed for real.
pub(crate) const BUTTON_DIALOG_STORY: &str = "C-BUTTON/dialog";

/// Every entry, in the order the navigator lists them.
pub(crate) const ENTRIES: &[Entry] = &[Entry {
    catalog_id: BUTTON,
    public_name: "KoruButton",
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
            a2ui: A2ui::NotSupported,
            story: Story::Available(BUTTON_DIALOG_STORY),
        },
        Recipe {
            symbol: "koru_ui::button::ActionButton",
            styles: Styles::Compact(&COMPACT_STYLES),
            a2ui: A2ui::HostStyled,
            story: Story::NoStoryYet,
        },
    ],
}];

/// The entry filed under `catalog_id`.
pub(crate) fn entry(catalog_id: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.catalog_id == catalog_id)
}

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
