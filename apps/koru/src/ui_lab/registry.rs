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
//! in memory: the module lists of `koru-ui`, `koru-draw` and the domain UI
//! crates, the token tables, the inspector identities painters register, the
//! option enums and the A2UI catalogs, so a module, token, identity or option
//! added or removed upstream fails until it is classified here. What no test
//! can see — a painter that registers no identity, a new native file — is the
//! manifest's to reconcile by hand (`registry/MANIFEST.md`).
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
//! **A record with a story names what the story drives as a [`Recipe`]**:
//! the symbol, its typed [`Styles`], the [`Control`]s the properties column
//! offers for it and the option sets it takes. Every other symbol is an
//! [`Api`]. The story IDs the Lab can run are the constants below; a story
//! is looked up by its ID through the gallery ([`story`]).
//!
//! **Where records live.** The Button is here because the trial story ran
//! it. The rest are filed a group at a time: [`foundations`] (tokens, the
//! element layer, drawing primitives and their support code), [`controls`]
//! (the shared widgets and the input support beside them) and
//! [`compositions`] (whole surfaces assembled from controls). The domain
//! crates' records are in [`domain`], a file per crate, because a crate's
//! surfaces, controls and support are reviewed together against its source;
//! each record still names the group it is shown under. The apps' own UI is
//! in `app`, a file per area, and the native platforms' — Kotlin and Swift,
//! written as string literals — in [`native`], a file per platform, each with
//! the [`Kind`] and [`Source::owner`] that say where it came from.

use koru_ui::button::{ActionStyle, ButtonRole};

mod app;
mod compositions;
mod controls;
mod domain;
mod foundations;
mod native;

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

    /// The stable part of the group's identity in the window.
    pub(crate) const fn part(self) -> &'static str {
        match self {
            Self::Foundations => "foundations",
            Self::Controls => "controls",
            Self::Compositions => "compositions",
        }
    }

    /// Where the group sits in [`Family::ALL`].
    pub(crate) const fn slot(self) -> usize {
        match self {
            Self::Foundations => 0,
            Self::Controls => 1,
            Self::Compositions => 2,
        }
    }

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
    DomainComposition,
    /// A control or painter private to one application.
    AppLocal,
    /// A platform's own control standing in for a shared one.
    NativeAdapter,
}

impl Kind {
    /// What a record's page calls the kind.
    pub(crate) const fn label(self) -> koru_i18n::Key {
        match self {
            Self::Token => koru_i18n::Key::UiLabKindToken,
            Self::Primitive => koru_i18n::Key::UiLabKindPrimitive,
            Self::Widget => koru_i18n::Key::UiLabKindWidget,
            Self::Support => koru_i18n::Key::UiLabKindSupport,
            Self::DomainComposition => koru_i18n::Key::UiLabKindDomain,
            Self::AppLocal => koru_i18n::Key::UiLabKindAppLocal,
            Self::NativeAdapter => koru_i18n::Key::UiLabKindNative,
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

impl Platform {
    /// The platforms the gallery covers. The phones' apps are Kotlin and
    /// Swift and are another effort: their records stay in the registry and
    /// the gallery neither lists them nor shows a phone's column.
    pub(crate) const DESKTOP: [Self; 3] = [Self::Windows, Self::MacOs, Self::Linux];

    /// The platform's own name: a product name, the same in every language.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Android => "Android",
            Self::Ios => "iOS",
        }
    }
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
    NotApplicable,
    /// The operating system or a provider owns that surface there.
    NativeOwned,
    /// Not looked at yet. Never read as a yes or as a no.
    NotAudited,
}

impl Availability {
    /// What a record's page says for a platform in this state.
    pub(crate) const fn label(self) -> koru_i18n::Key {
        match self {
            Self::Available => koru_i18n::Key::UiLabAvailAvailable,
            Self::Existing => koru_i18n::Key::UiLabAvailExisting,
            Self::SharedSourceUnverified => koru_i18n::Key::UiLabAvailShared,
            Self::NativePartial => koru_i18n::Key::UiLabAvailNativePartial,
            Self::Planned => koru_i18n::Key::UiLabAvailPlanned,
            Self::Unavailable => koru_i18n::Key::UiLabAvailUnavailable,
            Self::NotApplicable => koru_i18n::Key::UiLabAvailNotApplicable,
            Self::NativeOwned => koru_i18n::Key::UiLabAvailNativeOwned,
            Self::NotAudited => koru_i18n::Key::UiLabAvailNotAudited,
        }
    }
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

impl A2ui {
    /// What a record's page says, in the display language. The catalog name
    /// and the property path are identifiers and are filled in as they are.
    pub(crate) fn sentence(self) -> String {
        let lang = koru_i18n::current();
        let (key, identifier) = match self {
            Self::Component(name) => (koru_i18n::Key::UiLabA2uiComponent, Some(name)),
            Self::HostStyled(name) => (koru_i18n::Key::UiLabA2uiHostStyled, Some(name)),
            Self::Values { property, .. } => (koru_i18n::Key::UiLabA2uiValues, Some(property)),
            Self::HostOnly => (koru_i18n::Key::UiLabA2uiHostOnly, None),
            Self::NotSupported => (koru_i18n::Key::UiLabA2uiNotSupported, None),
        };
        match identifier {
            Some(identifier) => koru_i18n::fill(koru_i18n::t(lang, key), &[identifier]),
            None => koru_i18n::t(lang, key).to_owned(),
        }
    }

    /// Where the status sorts when a record's symbols differ: what a
    /// declaration can name first, what it cannot last.
    const fn rank(self) -> u8 {
        match self {
            Self::Component(_) => 0,
            Self::HostStyled(_) => 1,
            Self::Values { .. } => 2,
            Self::HostOnly => 3,
            Self::NotSupported => 4,
        }
    }
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
    PlatformOnly,
    /// Below the component level or owned by content: raw paint, text runs,
    /// a document's own marks. Recorded so it is accounted for, shown through
    /// the components that use it rather than as a gallery story.
    Boundary,
}

impl Story {
    /// The status a page states for a symbol or a record, said plainly.
    pub(crate) const fn label(self) -> koru_i18n::Key {
        match self {
            Self::Available(_) => koru_i18n::Key::UiLabStoryAvailable,
            Self::NoStoryYet => koru_i18n::Key::UiLabStoryNone,
            Self::Nonvisual => koru_i18n::Key::UiLabStoryNonvisual,
            Self::PlatformOnly => koru_i18n::Key::UiLabStoryPlatformOnly,
            Self::Boundary => koru_i18n::Key::UiLabStoryBoundary,
        }
    }

    /// One sentence on why there is nothing to run. `None` for a story,
    /// which is run instead of explained.
    pub(crate) const fn reason(self) -> Option<koru_i18n::Key> {
        match self {
            Self::Available(_) => None,
            Self::NoStoryYet => Some(koru_i18n::Key::UiLabStoryNoneWhy),
            Self::Nonvisual => Some(koru_i18n::Key::UiLabStoryNonvisualWhy),
            Self::PlatformOnly => Some(koru_i18n::Key::UiLabStoryPlatformOnlyWhy),
            Self::Boundary => Some(koru_i18n::Key::UiLabStoryBoundaryWhy),
        }
    }
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

/// A box a text input is drawn in: one of `koru_ui::text_input::InputStyle`'s
/// constructors. The production type is a struct its constructors fill, not
/// an enum, so the constructor is what a story chooses between.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum FieldStyle {
    /// `InputStyle::form`: a form's field, its border the accent while
    /// focused.
    #[default]
    Form,
    /// `InputStyle::search`: a filled box with no border.
    Search,
    /// `InputStyle::inline`: a row being edited in place.
    Inline,
}

impl FieldStyle {
    /// The constructor's own name: what a written recipe names.
    pub(crate) const fn ident(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::Search => "search",
            Self::Inline => "inline",
        }
    }

    /// What the style control calls it.
    pub(crate) const fn label(self) -> koru_i18n::Key {
        match self {
            Self::Form => koru_i18n::Key::UiLabOptionForm,
            Self::Search => koru_i18n::Key::UiLabOptionSearch,
            Self::Inline => koru_i18n::Key::UiLabOptionInline,
        }
    }
}

/// The boxes the text input's story draws it in, the default first.
pub(crate) const FIELD_STYLES: [FieldStyle; 3] =
    [FieldStyle::Form, FieldStyle::Search, FieldStyle::Inline];

/// The styles one recipe supports, as its own production type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Styles {
    /// `koru_ui::button::ButtonRole`: what a dialog button is for.
    Dialog(&'static [ButtonRole]),
    /// `koru_ui::button::ActionStyle`: a compact toolbar or panel action.
    Compact(&'static [ActionStyle]),
    /// `koru_ui::text_input::InputStyle`'s constructors.
    Field(&'static [FieldStyle]),
    /// The recipe takes no style: a layout box, a token table.
    Unstyled,
}

/// What kind of input a [`Control`] is, which is what decides how the
/// properties panel draws it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    /// Free text, typed into a field.
    Text,
    /// One of a list the story offers, each a production value.
    Choice,
    /// A yes or a no the production call takes as a `bool`: disabled,
    /// selected, with or without its icon.
    Toggle,
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
    /// The `placeholder: Option<&str>` a text input shows while empty: free
    /// text.
    Placeholder,
    /// The `koru_draw::theme::Space` a stack keeps between its children
    /// (`El::gap`): a choice.
    Gap,
    /// The `koru_draw::theme::Space` a box keeps round its content
    /// (`El::pad`): a choice.
    Padding,
    /// The recipe's tone: the option set of that name the recipe lists
    /// ([`OptionSet::name`]): a choice.
    #[allow(dead_code)] // Offered by the first widget story that has one.
    Tone,
    /// The recipe's size or metrics, from the option set of that name: a
    /// choice.
    #[allow(dead_code)] // Offered by the first widget story that has one.
    Size,
    /// The `disabled` or `enabled: bool` the production call takes: a yes
    /// or a no.
    Disabled,
    /// The `pending: bool` of a control whose change is in flight: a yes or
    /// a no.
    Pending,
    /// Whether the component leads with its optional glyph or mark slot
    /// (`Toggle::icon`, `Chip::leading`): a yes or a no.
    Leading,
}

impl Control {
    /// What the panel calls this input.
    pub(crate) fn name(self) -> koru_i18n::Key {
        match self {
            Self::Label => koru_i18n::Key::UiLabControlLabel,
            Self::Style => koru_i18n::Key::UiLabControlStyle,
            Self::Placeholder => koru_i18n::Key::UiLabControlPlaceholder,
            Self::Gap => koru_i18n::Key::UiLabControlGap,
            Self::Padding => koru_i18n::Key::UiLabControlPadding,
            Self::Tone => koru_i18n::Key::UiLabControlTone,
            Self::Size => koru_i18n::Key::UiLabControlSize,
            Self::Disabled => koru_i18n::Key::UiLabControlDisabled,
            Self::Pending => koru_i18n::Key::UiLabOptionPending,
            Self::Leading => koru_i18n::Key::UiLabControlLeading,
        }
    }

    /// The stable part of the control's identity in the window: what the
    /// pointer, the keyboard focus and a recipe's written steps refer to.
    pub(crate) fn part(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Style => "style",
            Self::Placeholder => "placeholder",
            Self::Gap => "gap",
            Self::Padding => "padding",
            Self::Tone => "tone",
            Self::Size => "size",
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Leading => "leading",
        }
    }

    /// The surface this choice's options are identified under while it is
    /// shown as segments, an option's own identifier being the part. One a
    /// control, so two choices on one page can each have an option of the
    /// same name.
    pub(crate) const fn options_surface(self) -> &'static str {
        match self {
            Self::Style => "ui-lab.style",
            Self::Gap => "ui-lab.gap",
            Self::Padding => "ui-lab.padding",
            Self::Tone => "ui-lab.tone",
            Self::Size => "ui-lab.size",
            // Free text and a yes or a no have no options.
            Self::Label | Self::Placeholder | Self::Disabled | Self::Pending | Self::Leading => {
                "ui-lab.text"
            }
        }
    }

    pub(crate) const fn input(self) -> Input {
        match self {
            Self::Label | Self::Placeholder => Input::Text,
            Self::Style | Self::Gap | Self::Padding | Self::Tone | Self::Size => Input::Choice,
            Self::Disabled | Self::Pending | Self::Leading => Input::Toggle,
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
    /// The production values the symbol takes beside its styles, a set per
    /// type: a token table's members, listed as the source spells them.
    pub(crate) options: &'static [OptionSet],
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

const fn api(symbol: &'static str, options: &'static [OptionSet], a2ui: A2ui, story: Story) -> Api {
    Api {
        symbol,
        options,
        a2ui,
        story,
    }
}

/// A symbol with no option of its own that the catalog cannot name.
const fn plain(symbol: &'static str, story: Story) -> Api {
    api(symbol, &[], A2ui::NotSupported, story)
}

/// A symbol whose values are listed but that the catalog cannot name.
const fn plain_with(symbol: &'static str, options: &'static [OptionSet], story: Story) -> Api {
    api(symbol, options, A2ui::NotSupported, story)
}

const fn values(
    name: koru_i18n::Key,
    source: &'static str,
    file: &'static str,
    declared: Declared,
    members: &'static [Member],
) -> OptionSet {
    OptionSet {
        name,
        source,
        file,
        declared,
        members,
    }
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

/// Shared desktop Rust, with what each phone has in its place. The phones
/// link no Koru drawing crate, so a phone cell is never this source: it is
/// the phone's own screen or control that plays the role
/// ([`Availability::NativePartial`], named in `registry/MANIFEST.md`), the
/// system's ([`Availability::NativeOwned`]), nothing
/// ([`Availability::Unavailable`]) or a decision that it never crosses
/// ([`Availability::NotApplicable`]). Settled against the native sources in
/// batch 4; the desktop cells are [`DESKTOP_SHARED`]'s.
const fn desktop_and(android: Availability, ios: Availability) -> [(Platform, Availability); 5] {
    [
        (Platform::Windows, Availability::Existing),
        (Platform::MacOs, Availability::SharedSourceUnverified),
        (Platform::Linux, Availability::SharedSourceUnverified),
        (Platform::Android, android),
        (Platform::Ios, ios),
    ]
}

/// Each phone has a native screen or control in this record's role.
const DESKTOP_PHONES_PARTIAL: [(Platform, Availability); 5] =
    desktop_and(Availability::NativePartial, Availability::NativePartial);

/// Android has a native counterpart; iOS has none.
const DESKTOP_ANDROID_PARTIAL: [(Platform, Availability); 5] =
    desktop_and(Availability::NativePartial, Availability::Unavailable);

/// iOS has a native counterpart; Android has none.
const DESKTOP_IOS_PARTIAL: [(Platform, Availability); 5] =
    desktop_and(Availability::Unavailable, Availability::NativePartial);

/// Neither phone has it.
const DESKTOP_NOT_ON_PHONES: [(Platform, Availability); 5] =
    desktop_and(Availability::Unavailable, Availability::Unavailable);

/// Decided desktop-only: it has no role on a phone.
const DESKTOP_ONLY: [(Platform, Availability); 5] =
    desktop_and(Availability::NotApplicable, Availability::NotApplicable);

/// The catalog ID of the shared button.
pub(crate) const BUTTON: &str = "C-BUTTON";

/// The story the first trial runs: the dialog button, pressed for real.
pub(crate) const BUTTON_DIALOG_STORY: &str = "C-BUTTON/dialog";
/// The shared text input, typed in for real.
pub(crate) const TEXT_INPUT_STORY: &str = "C-TEXT-INPUT/field";
/// The element layer's row and column, their gap and padding chosen from
/// the spacing scale.
pub(crate) const ROW_STORY: &str = "P-ROW/spacing";
pub(crate) const COLUMN_STORY: &str = "P-COLUMN/spacing";
/// The token tables, each drawn from the table itself.
pub(crate) const SPACE_STORY: &str = "T-SPACE/scale";
pub(crate) const SURFACE_STORY: &str = "T-SURFACE/roles";
pub(crate) const INK_STORY: &str = "T-INK/roles";
pub(crate) const LINE_STORY: &str = "T-LINE/roles";
pub(crate) const TEXT_ROLES_STORY: &str = "T-TEXT-ROLES/roles";

/// The shared choice widgets, each the production component pressed for
/// real (`stories/choices.rs`).
pub(crate) const TOGGLE_STORY: &str = "C-TOGGLE/switch";
pub(crate) const CHIP_STORY: &str = "C-CHIP/choice";
pub(crate) const SEGMENTED_STORY: &str = "C-SEGMENTED/choice";
pub(crate) const DROPDOWN_STORY: &str = "C-DROPDOWN/field";

/// A widget symbol a story drives: its inputs, the option sets it takes,
/// and the story's ID. It takes no style of the two button enums, and no
/// declaration can name it.
const fn staged(
    symbol: &'static str,
    controls: &'static [Control],
    options: &'static [OptionSet],
    story_id: &'static str,
) -> Recipe {
    Recipe {
        symbol,
        styles: Styles::Unstyled,
        controls,
        options,
        a2ui: A2ui::NotSupported,
        story: Story::Available(story_id),
    }
}

/// A symbol a story drives that takes no style and has no input: a token
/// table, shown whole.
const fn table(
    symbol: &'static str,
    options: &'static [OptionSet],
    a2ui: A2ui,
    story_id: &'static str,
) -> Recipe {
    Recipe {
        symbol,
        styles: Styles::Unstyled,
        controls: &[],
        options,
        a2ui,
        story: Story::Available(story_id),
    }
}

/// `record` as one the Lab runs: its story-driven symbols, and its Windows
/// cell saying the Lab draws it. The other cells are left as they were
/// recorded.
const fn storied(mut record: Entry, recipes: &'static [Recipe]) -> Entry {
    record.recipes = recipes;
    record.platforms[0] = (Platform::Windows, Availability::Available);
    record
}

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
            options: &[],
            a2ui: A2ui::NotSupported,
            story: Story::Available(BUTTON_DIALOG_STORY),
        },
        Recipe {
            symbol: "koru_ui::button::ActionButton",
            styles: Styles::Compact(&COMPACT_STYLES),
            controls: &[Control::Label, Control::Style],
            options: &[],
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

/// One symbol of a record, story-driven or not, as a page lists it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    /// The path a caller writes: a technical identifier.
    pub(crate) symbol: &'static str,
    /// The production values it takes, a set per type.
    pub(crate) options: &'static [OptionSet],
    pub(crate) a2ui: A2ui,
    pub(crate) story: Story,
}

impl Entry {
    /// What is recorded for the record on `platform`.
    pub(crate) fn availability(&self, platform: Platform) -> Availability {
        self.platforms
            .iter()
            .find(|(recorded, _)| *recorded == platform)
            .map_or(Availability::NotAudited, |(_, availability)| *availability)
    }

    /// Whether the record has any role on a desktop platform. Read from the
    /// record's own platform cells: one whose Windows, macOS and Linux cells
    /// all say it has no role there exists only on a phone.
    pub(crate) fn on_desktop(&self) -> bool {
        Platform::DESKTOP
            .into_iter()
            .any(|platform| self.availability(platform) != Availability::NotApplicable)
    }

    /// Every symbol of the record, the story-driven ones first.
    pub(crate) fn symbols(&self) -> impl Iterator<Item = Symbol> + use<> {
        let (recipes, apis) = (self.recipes, self.apis);
        recipes
            .iter()
            .map(|recipe| Symbol {
                symbol: recipe.symbol,
                options: recipe.options,
                a2ui: recipe.a2ui,
                story: recipe.story,
            })
            .chain(apis.iter().map(|api| Symbol {
                symbol: api.symbol,
                options: api.options,
                a2ui: api.a2ui,
                story: api.story,
            }))
    }

    /// What the record's symbols add up to: a story if any symbol has one;
    /// otherwise it waits for one if any visual symbol does; otherwise it is
    /// a boundary, platform-only or nonvisual, in that order.
    pub(crate) fn status(&self) -> Story {
        let stories: Vec<Story> = self.symbols().map(|symbol| symbol.story).collect();
        if let Some(available) = stories
            .iter()
            .find(|story| matches!(story, Story::Available(_)))
        {
            *available
        } else if stories.contains(&Story::NoStoryYet) {
            Story::NoStoryYet
        } else if stories.contains(&Story::Boundary) {
            Story::Boundary
        } else if stories.contains(&Story::PlatformOnly) {
            Story::PlatformOnly
        } else {
            Story::Nonvisual
        }
    }

    /// Whether the record is what `words` ask for: every word is part of
    /// what the record is called in the display language, of its public
    /// name, of its catalog ID or of one of its recorded symbols. `words`
    /// are [`search_words`]'s, already lower case. No word matches a
    /// purpose: a sentence would match half the catalog.
    pub(crate) fn matches(&self, words: &[String]) -> bool {
        let lang = koru_i18n::current();
        let name = koru_i18n::t(lang, self.name).to_lowercase();
        let public_name = self.public_name.to_lowercase();
        let catalog_id = self.catalog_id.to_lowercase();
        words.iter().all(|word| {
            name.contains(word)
                || public_name.contains(word)
                || catalog_id.contains(word)
                || self
                    .symbols()
                    .any(|symbol| symbol.symbol.to_lowercase().contains(word))
        })
    }

    /// The story the record's page runs, when it has one.
    pub(crate) fn story_id(&self) -> Option<&'static str> {
        match self.status() {
            Story::Available(id) => Some(id),
            _ => None,
        }
    }

    /// Each distinct thing the record's symbols say about A2UI, what a
    /// declaration can name first. One line when they all agree.
    pub(crate) fn a2ui(&self) -> Vec<A2ui> {
        let mut statuses: Vec<A2ui> = Vec::new();
        for symbol in self.symbols() {
            if !statuses.contains(&symbol.a2ui) {
                statuses.push(symbol.a2ui);
            }
        }
        statuses.sort_by_key(|status| status.rank());
        statuses
    }
}

/// Every record, a group's file at a time, in the order the navigator lists
/// each group.
pub(crate) const CATALOG: &[&[Entry]] = &[
    foundations::RECORDS,
    controls::RECORDS,
    compositions::RECORDS,
    domain::AGENTS,
    domain::AI,
    domain::APP_SHELL,
    domain::APPEARANCE,
    domain::ARTIFACTS,
    domain::CANVAS,
    domain::CHATS,
    domain::EDITOR,
    domain::MARKDOWN_VIEW,
    domain::RUNS,
    domain::SEARCH,
    domain::SESSION_VIEW,
    domain::SETTINGS,
    domain::SHELL_CHROME,
    domain::SOCIALS,
    domain::VOICE,
    domain::WEBVIEW,
    domain::WINDOW,
    domain::WORKSPACE_HOST,
    app::MINI_APP,
    app::TASKS_VIEW,
    app::LEAF,
    app::PAGES,
    app::MAINVIEW,
    app::PANES,
    app::COMPANION_APPS,
    native::ANDROID,
    native::IOS,
    native::MACOS,
    app::OS_SURFACES,
    domain::NOTIFICATIONS,
];

/// Every record, flattened.
pub(crate) fn catalog() -> impl Iterator<Item = &'static Entry> {
    CATALOG.iter().flat_map(|records| records.iter())
}

/// **What the gallery shows**: every record with a role on a desktop
/// platform. The navigator, the search, the counts beside a group and the
/// story lookup all read the catalog through this, so a record that exists
/// only on Android or iOS is in none of them. Nothing is removed from the
/// registry: [`catalog`] still has every record.
pub(crate) fn desktop() -> impl Iterator<Item = &'static Entry> {
    catalog().filter(|entry| entry.on_desktop())
}

/// What a search is looking for: the words typed, lower case. No words is
/// no search.
pub(crate) fn search_words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// The entry and recipe a story ID runs.
pub(crate) fn story(story_id: &str) -> Option<(&'static Entry, &'static Recipe)> {
    desktop().find_map(|entry| {
        entry
            .recipes
            .iter()
            .find(|recipe| matches!(recipe.story, Story::Available(id) if id == story_id))
            .map(|recipe| (entry, recipe))
    })
}

#[cfg(test)]
mod tests;
