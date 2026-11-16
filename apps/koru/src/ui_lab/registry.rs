// SPDX-License-Identifier: MPL-2.0

//! **What the Lab lists, and where each thing really lives.**
//!
//! One record a component, token table, primitive or piece of support code:
//! a stable catalog ID, the navigator group it is filed under, what kind of
//! thing it is, the production source that owns it, where it is available,
//! and — a callable symbol at a time — the options that symbol's own types
//! offer, whether a model-written declaration may use it, and whether the Lab
//! has a story for it. Consumer routes and dated evidence are not here: they
//! arrive with the stories that produce them.
//!
//! **The records are a hand-reviewed inventory, not a scan.** Each was written
//! from the production source it names. `tests.rs` holds them to that source
//! in memory: the module lists of `koru-ui` and `koru-draw`, the token tables,
//! the inspector identities painters register, the option enums and the A2UI
//! catalog, so a module, token, identity or option added or removed upstream
//! fails until it is classified here. What no test can see — a new painter in
//! an app, a native control — is the manifest's to reconcile by hand
//! (`registry/MANIFEST.md`).
//!
//! **Options are the production type's own values.** A dialog button is styled
//! by [`ButtonRole`] and a compact action by [`ActionStyle`]; they are two
//! enums with different members and different paint, so [`Styles`] carries
//! each as itself rather than flattening both into one invented "variant".
//! The slot functions below match on every member of each enum, so a style
//! added to or removed from `koru-ui` stops this file compiling until the
//! record is brought back in line. Every other record lists its options as
//! [`OptionSet`]s: the identifiers the source declares, in its order, with a
//! translated label where a person would choose one.
//!
//! **Where records live.** The Button is here because the trial story runs
//! it. The rest are filed a group at a time: [`foundations`] (tokens, the
//! element layer, drawing primitives and their support code), [`controls`]
//! (the shared widgets and the input support beside them) and
//! [`compositions`] (whole surfaces assembled from controls). A later batch
//! adds its records to the file of the group they are shown under, with the
//! [`Kind`] and [`Source::owner`] that say where they came from.

use koru_ui::button::{ActionStyle, ButtonRole};

mod compositions;
mod controls;
mod foundations;

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

/// What a record is, whatever group it is shown under. Two records in one
/// group can be different kinds: a spacing table and a row are both
/// foundations, one a token and one a primitive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A named design value a style setter takes: a spacing step, a color
    /// role, a theme.
    Token,
    /// The element layer and the drawing marks below the component level.
    Primitive,
    /// A shared component with paint, geometry or state of its own.
    Widget,
    /// Code a component needs that is not a component: input plumbing,
    /// calendar arithmetic, the text engine, the inspection seam.
    Support,
    /// A surface owned by one domain crate (search, settings, chats, …).
    #[expect(
        dead_code,
        reason = "the domain-crate batch registers these; the expectation fails once one does"
    )]
    DomainComposition,
    /// A control or painter private to one application.
    #[expect(
        dead_code,
        reason = "the app-local batch registers these; the expectation fails once one does"
    )]
    AppLocal,
    /// A platform's own control standing in for a shared one.
    #[expect(
        dead_code,
        reason = "the native-adapter batch registers these; the expectation fails once one does"
    )]
    NativeAdapter,
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
    /// The production implementation ships there; the Lab has no story of it.
    Existing,
    /// The same Rust source builds there; nobody has checked the result.
    SharedSourceUnverified,
    /// A native control covers part of the contract, not all of it.
    NativePartial,
    /// An implementation is intended, with an owner, and is not usable yet.
    Planned,
    /// Confirmed: there is no implementation there.
    Unavailable,
    /// It has no role on that platform, for a recorded reason.
    #[expect(
        dead_code,
        reason = "no shared API or token is without a role anywhere; the domain and native batches record the first"
    )]
    NotApplicable,
    /// The operating system or a provider owns that surface there.
    #[expect(
        dead_code,
        reason = "no shared API or token is OS-owned; the native batch records the first"
    )]
    NativeOwned,
    /// Not looked at yet. Never read as a yes or as a no.
    NotAudited,
}

/// Whether a model-written composition may ask for this symbol, checked
/// against the composition catalog the playground validates with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum A2ui {
    /// A declaration names it as this catalog component.
    Component(&'static str),
    /// In the composition catalog under this name. The host picks the style;
    /// a declaration cannot.
    HostStyled(&'static str),
    /// A declaration names these values in the named properties: each pair
    /// is the declared value and the production member it resolves to.
    /// Members missing from the list are not declarable.
    Values {
        property: &'static str,
        mapped: &'static [(&'static str, &'static str)],
    },
    /// The host's adapter uses it to draw a declaration; a declaration never
    /// names it.
    HostOnly,
    /// In neither A2UI catalog: nothing a declaration can name.
    NotSupported,
}

/// Whether the Lab can show this symbol running, or why it never will.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Story {
    /// A story with this stable ID.
    Available(&'static str),
    /// Listed, with nothing to run yet. Never a stand-in preview.
    NoStoryYet,
    /// It draws nothing: a rule, a model or plumbing a component uses.
    Nonvisual,
    /// It exists only as a native adapter on a platform the Lab does not run on.
    #[expect(
        dead_code,
        reason = "the native-adapter batch records these; the expectation fails once one does"
    )]
    PlatformOnly,
    /// Below the component level or owned by content: raw paint, text runs,
    /// a document's own marks. Recorded so it is accounted for, shown through
    /// the components that use it rather than as a gallery story.
    Boundary,
}

/// The production source that owns an entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// The crate, as its manifest names it.
    pub(crate) owner: &'static str,
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

/// One callable way to draw an entry that a story drives: its styles are
/// typed, so a story's style control holds the production value itself.
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

/// How the source declares an option set's values, which says how the
/// registry's tests hold the list to it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Declared {
    /// The members of a `pub enum`, in declaration order.
    Enum,
    /// Associated or module constants of that name.
    Consts,
    /// Constructors or functions of that name.
    Functions,
    /// A token table the production code exports (`ALL`, a name list), read
    /// as a value rather than as text.
    Table,
}

/// One value of an option set.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    /// The identifier exactly as the source spells it: `Ghost`, `S4`,
    /// `STANDARD`, `search`. A recipe names this, in every language.
    pub(crate) ident: &'static str,
    /// What a person choosing it reads, where it is a human choice. A token
    /// identifier has none and is shown as itself.
    pub(crate) label: Option<koru_i18n::Key>,
}

/// The values one production type offers: a style enum, a set of metric
/// constants, a family of constructors, a token table. Never a universal
/// "variant" field: each set is its own type's, named by its own path.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct OptionSet {
    /// What a properties panel calls the choice.
    pub(crate) name: koru_i18n::Key,
    /// The production type or namespace the values belong to.
    pub(crate) source: &'static str,
    /// The file that declares them, from the repository root.
    pub(crate) file: &'static str,
    pub(crate) declared: Declared,
    pub(crate) members: &'static [Member],
}

/// One callable symbol of an entry that no story drives yet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Api {
    /// The symbol a caller invokes or names.
    pub(crate) symbol: &'static str,
    /// The production values it takes, a set per type.
    pub(crate) options: &'static [OptionSet],
    pub(crate) a2ui: A2ui,
    pub(crate) story: Story,
}

/// One record the Lab lists.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Stable across renames; the component catalog's ID where it gives one.
    pub(crate) catalog_id: &'static str,
    /// The public name, a technical identifier and never translated: the
    /// KoruX name where the catalog gives one, else the source's own.
    pub(crate) public_name: &'static str,
    /// What a person calls it, in the display language.
    pub(crate) name: koru_i18n::Key,
    /// One line on what it is for. Not documentation: the contract stays
    /// with the component's owner.
    pub(crate) purpose: koru_i18n::Key,
    pub(crate) family: Family,
    pub(crate) kind: Kind,
    /// The component catalog's family ID (`C-CHOICE`, `C-OVERLAY`, …), as a
    /// literal. `None` where the catalog has no row for it.
    pub(crate) catalog_family: Option<&'static str>,
    /// The component names its painters register with the inspector
    /// (`koru_draw::inspect::scope(…, "component", …)`). Empty when it paints
    /// inside another component's identity or registers none.
    pub(crate) identities: &'static [&'static str],
    pub(crate) source: Source,
    pub(crate) platforms: [(Platform, Availability); 5],
    /// The symbols a story drives, with typed styles.
    pub(crate) recipes: &'static [Recipe],
    /// Every other symbol that belongs to the record.
    pub(crate) apis: &'static [Api],
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

/// A label a person reads for an option.
const fn labelled(ident: &'static str, label: koru_i18n::Key) -> Member {
    Member {
        ident,
        label: Some(label),
    }
}

/// A token identifier, shown as itself.
const fn literal(ident: &'static str) -> Member {
    Member { ident, label: None }
}

/// Shared Rust that ships in the desktop app: the Windows source is the
/// production one, macOS and Linux build the same source unverified, and no
/// mobile equivalent has been audited.
const DESKTOP_SHARED: [(Platform, Availability); 5] = [
    (Platform::Windows, Availability::Existing),
    (Platform::MacOs, Availability::SharedSourceUnverified),
    (Platform::Linux, Availability::SharedSourceUnverified),
    (Platform::Android, Availability::NotAudited),
    (Platform::Ios, Availability::NotAudited),
];

/// The catalog ID of the shared button.
pub(crate) const BUTTON: &str = "C-BUTTON";

/// The story the first trial runs: the dialog button, pressed for real.
pub(crate) const BUTTON_DIALOG_STORY: &str = "C-BUTTON/dialog";

const BUTTON_SOURCE: &str = "crates/koru-ui/src/button.rs";

/// `ButtonRole` as an option set: the same members as [`DIALOG_STYLES`].
const DIALOG_OPTIONS: OptionSet = OptionSet {
    name: koru_i18n::Key::UiLabControlStyle,
    source: "koru_ui::button::ButtonRole",
    file: BUTTON_SOURCE,
    declared: Declared::Enum,
    members: &[
        labelled("Secondary", koru_i18n::Key::UiLabStyleSecondary),
        labelled("Primary", koru_i18n::Key::UiLabStylePrimary),
        labelled("Destructive", koru_i18n::Key::UiLabStyleDestructive),
    ],
};

/// `ActionStyle` as an option set: the same members as [`COMPACT_STYLES`].
const COMPACT_OPTIONS: OptionSet = OptionSet {
    name: koru_i18n::Key::UiLabControlStyle,
    source: "koru_ui::button::ActionStyle",
    file: BUTTON_SOURCE,
    declared: Declared::Enum,
    members: &[
        labelled("Primary", koru_i18n::Key::UiLabStylePrimary),
        labelled("Secondary", koru_i18n::Key::UiLabStyleSecondary),
        labelled("Ghost", koru_i18n::Key::UiLabOptionGhost),
        labelled("Destructive", koru_i18n::Key::UiLabStyleDestructive),
    ],
};

/// The shared button: the trial's one story.
const BUTTON_ENTRY: Entry = Entry {
    catalog_id: BUTTON,
    public_name: "KoruButton",
    name: koru_i18n::Key::UiLabButtonName,
    purpose: koru_i18n::Key::UiLabButtonPurpose,
    family: Family::Controls,
    kind: Kind::Widget,
    catalog_family: Some("C-BUTTON"),
    identities: &["KoruButton"],
    source: Source {
        owner: "koru-ui",
        module: "koru_ui::button",
        file: BUTTON_SOURCE,
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
            a2ui: A2ui::HostStyled("Button"),
            story: Story::NoStoryYet,
        },
    ],
    apis: &[
        Api {
            symbol: "koru_ui::button::draw_button_enabled",
            options: &[DIALOG_OPTIONS],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::ActionButton::draw_disabled",
            options: &[COMPACT_OPTIONS],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::ActionButton::draw_focused_elided",
            options: &[COMPACT_OPTIONS],
            a2ui: A2ui::HostOnly,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::draw_google_button",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::draw_apple_button",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::draw_help_button",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::draw_failed_outline",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::NoStoryYet,
        },
        Api {
            symbol: "koru_ui::button::button_width",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::Nonvisual,
        },
        Api {
            symbol: "koru_ui::button::row_from_right",
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::Nonvisual,
        },
    ],
};

/// The entries the navigator lists at this checkpoint: the ones a story can
/// run. The gallery's search and scrolling list (L3b) shows the whole
/// [`CATALOG`]; until then a row with nothing behind it would be a dead end.
pub(crate) const ENTRIES: &[Entry] = &[BUTTON_ENTRY];

/// Every record, a group's file at a time, in the order the navigator lists
/// each group.
pub(crate) const CATALOG: &[&[Entry]] = &[
    foundations::RECORDS,
    controls::RECORDS,
    compositions::RECORDS,
];

/// Every record, flattened.
pub(crate) fn catalog() -> impl Iterator<Item = &'static Entry> {
    CATALOG.iter().flat_map(|records| records.iter())
}

/// The entry and recipe a story ID runs.
pub(crate) fn story(story_id: &str) -> Option<(&'static Entry, &'static Recipe)> {
    catalog().find_map(|entry| {
        entry
            .recipes
            .iter()
            .find(|recipe| matches!(recipe.story, Story::Available(id) if id == story_id))
            .map(|recipe| (entry, recipe))
    })
}

#[cfg(test)]
mod tests;
