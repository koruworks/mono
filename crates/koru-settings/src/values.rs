// SPDX-License-Identifier: MPL-2.0

//! The typed values a setting can hold, and the exact strings they persist as.
//!
//! **Every codec here reproduces a spelling that already exists on somebody's
//! disk.** They were read from `koru_draw::theme`, `koru_workspace_host::
//! settings`, `koru_session_view::cursor` and `koru_voice` during the SP-6
//! survey, and the round-trip goldens in `crate::storage::tests` are what
//! keeps them honest. A tolerant reader and an exact writer is the rule
//! throughout: an unknown token falls back to the default rather than failing
//! a load, because a file nobody can read is a person's preferences lost.
//!
//! This crate deliberately does **not** link `koru-draw`. The authority is
//! hosted inside the API gateway process, and a UI crate reaching in here
//! would put parley and vello behind an HTTP handler. The cost is that the
//! spellings live in two places until SP-6b deletes the other one; the
//! `crate::storage::tests` goldens are written from real current-format
//! fixtures so a drift shows up as a failing test rather than as a font that
//! silently resets.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// A per-field patch instruction.
///
/// **Absent, null and a value are three different things**, which is the whole
/// of what "different fields never clobber" needs at the wire: a body that does
/// not mention a field leaves it alone, `null` clears a nullable one, and a
/// value sets it. A plain `Option<T>` cannot say the middle one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Field<T> {
    /// The patch did not mention this field.
    #[default]
    Unchanged,
    /// The patch named this field as `null` — clear it.
    Clear,
    /// The patch set this field.
    Set(T),
}

impl<T> Field<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }

    /// Fold this instruction onto a nullable current value.
    pub fn apply(self, current: &mut Option<T>) {
        match self {
            Self::Unchanged => {}
            Self::Clear => *current = None,
            Self::Set(value) => *current = Some(value),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Absence is handled by `#[serde(default)]` on the containing field,
        // so anything that reaches here is either `null` or a value.
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // A serialized patch is only ever produced by a client building
            // one, and `Unchanged` is expressed by omitting the key. Callers
            // use `skip_serializing_if = "Field::is_unchanged"`.
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => serializer.serialize_some(value),
        }
    }
}

impl<T: schemars::JsonSchema> schemars::JsonSchema for Field<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(format!("Nullable_{}", T::schema_name()))
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let inner = generator.subschema_for::<T>();
        schemars::json_schema!({
            "anyOf": [inner, { "type": "null" }],
        })
    }
}

/// Declare a closed token enum with a tolerant parser and an exact writer.
///
/// `$fallback` is what an unrecognized token becomes, and it is always the
/// variant marked `default` — a load never fails on a value it does not know.
macro_rules! token_enum {
    (
        $(#[$meta:meta])*
        $name:ident, default = $fallback:ident, fold_case = $fold:literal,
        { $( $variant:ident => $token:literal $(, also $($alias:literal),+ )? ; )+ }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
        pub enum $name {
            $(
                #[serde(rename = $token)]
                $variant,
            )+
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$fallback
            }
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant ),+ ];

            /// The exact token this value is written as.
            pub fn token(self) -> &'static str {
                match self {
                    $( Self::$variant => $token, )+
                }
            }

            /// Read a token. Unknown input is the default, never an error.
            pub fn parse(text: &str) -> Self {
                let trimmed = text.trim();
                let owned;
                let candidate = if $fold {
                    owned = trimmed.to_ascii_lowercase();
                    owned.as_str()
                } else {
                    trimmed
                };
                match candidate {
                    $(
                        $token $( $( | $alias )+ )? => Self::$variant,
                    )+
                    _ => Self::$fallback,
                }
            }
        }
    };
}

token_enum! {
    /// `theme=` in `preferences.conf`, and the terminal row's `theme=`.
    ///
    /// Matches `koru_draw::theme::ThemePreference`: case-sensitive, because
    /// that is what the existing parser does and a file written in another
    /// case has never been produced by any build.
    ThemeChoice, default = System, fold_case = false,
    {
        System => "system";
        Light => "light";
        Dark => "dark";
    }
}

token_enum! {
    /// The in-app vector artwork, persisted as `icon_pack=`.
    IconPackChoice, default = Phosphor, fold_case = true,
    {
        Phosphor => "phosphor";
        Tabler => "tabler";
        Lucide => "lucide";
        Remix => "remix";
    }
}

token_enum! {
    /// `caret_color=`, and the terminal row's `cursor_color=`.
    CaretColorChoice, default = Default, fold_case = true,
    {
        Default => "default";
        Blue => "blue";
        Amber => "amber";
        Violet => "violet";
    }
}

token_enum! {
    /// `caret_width=`.
    ///
    /// `x4`/`x8` are accepted on read and never written — an asymmetry
    /// inherited from `koru_draw::theme::CaretWidth::parse`, kept because
    /// files carrying them exist.
    CaretWidthChoice, default = Medium, fold_case = true,
    {
        Thin => "thin";
        Medium => "medium";
        Thick => "thick";
        X4 => "4x", also "x4";
        X8 => "8x", also "x8";
    }
}

token_enum! {
    /// `caret_height=`.
    CaretHeightChoice, default = Medium, fold_case = true,
    {
        Short => "short";
        Medium => "medium";
        Tall => "tall";
    }
}

token_enum! {
    /// `density=`. Case-sensitive, matching `koru_draw::theme::Density`.
    DensityChoice, default = Normal, fold_case = false,
    {
        Normal => "normal";
        Compact => "compact";
        Comfortable => "comfortable";
    }
}

token_enum! {
    /// `popout_close=`. Note `RestoreTab` persists as `restore`.
    PopoutCloseChoice, default = Ask, fold_case = false,
    {
        Ask => "ask";
        Close => "close";
        RestoreTab => "restore";
    }
}

token_enum! {
    /// `view_mode=` — the default view mode for a newly opened document.
    ///
    /// **Narrow until somebody chooses** (the user's call, 2026-10-31): a
    /// readable column is what a new page should look like. Unknown input
    /// becomes the default too.
    ViewModeChoice, default = Narrow, fold_case = false,
    {
        Page => "page";
        Narrow => "narrow";
        Full => "full";
    }
}

token_enum! {
    /// `dictation_quality=`. `koru_voice::ModelTier`'s two tiers.
    DictationTier, default = Compact, fold_case = false,
    {
        Compact => "compact";
        HighAccuracy => "high_accuracy";
    }
}

token_enum! {
    /// `close_behavior` in `desktop-v1.json`, whose variants are kebab-case.
    CloseBehaviorChoice, default = Background, fold_case = true,
    {
        Ask => "ask";
        Background => "background";
        Quit => "quit";
    }
}

token_enum! {
    /// **The permission a new agent chat starts in** (Settings → Coding →
    /// Agent providers), in `desktop-v1.json`. Each agent's own modes are
    /// what a chat runs in; this names which of them a new chat starts from
    /// (`koru-ai-ui`'s `approval_modes` does the mapping): *Manual* is the
    /// agent's asking mode, *Auto* the mode that approves safe actions on its
    /// own, *Yolo* the one that runs everything without asking. An agent that
    /// offers no such mode starts in *Manual*.
    AgentPermission, default = Manual, fold_case = true,
    {
        Manual => "manual";
        Auto => "auto";
        Yolo => "yolo";
    }
}

token_enum! {
    /// The terminal row's `cursor_width=`.
    ///
    /// `application` means "follow the app's caret width" — the terminal's own
    /// spelling of a follow state, and unrelated to the four font settings.
    TerminalCursorWidth, default = Application, fold_case = true,
    {
        Application => "application";
        Thin => "thin";
        Medium => "medium";
        Thick => "thick";
        X4 => "4x", also "x4";
        X8 => "8x", also "x8";
    }
}

token_enum! {
    /// The terminal row's `cursor_height=`.
    TerminalCursorHeight, default = Application, fold_case = true,
    {
        Application => "application";
        Short => "short";
        Medium => "medium";
        Tall => "tall";
    }
}

token_enum! {
    /// The terminal row's `cursor_blink=`.
    TerminalCursorBlink, default = Application, fold_case = true,
    {
        Application => "application";
        On => "on", also "true", "1";
        Off => "off", also "false", "0";
    }
}

token_enum! {
    /// Which AI provider is selected. The closed set `ai.db`'s CHECK allows.
    AiProvider, default = OpenAi, fold_case = true,
    {
        OpenAi => "openai";
        Ollama => "ollama";
        Koru => "koru";
    }
}

token_enum! {
    /// A provider's reasoning setting — `ai.db`'s nine-token CHECK.
    ReasoningChoice, default = Default, fold_case = true,
    {
        Default => "default";
        Off => "off";
        On => "on";
        Minimal => "minimal";
        Low => "low";
        Medium => "medium";
        High => "high";
        XHigh => "xhigh";
        Max => "max";
    }
}

/// `dictation_toggle=` as a named gesture rather than a bare flag.
///
/// **The disk spelling and the API spelling are deliberately different.** On
/// disk it is `0`/`1`, because that is the byte already written in every
/// `preferences.conf` and three parsers read it. Over the API it is
/// `push_to_talk`/`toggle`, because `"dictation_gesture": "1"` is a riddle at
/// the call site — and the two parsers that read `dictation_toggle` disagree
/// about `true` (one accepts it, one does not), which is exactly the kind of
/// question a typed value should stop anybody having to ask.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DictationGesture {
    #[default]
    PushToTalk,
    Toggle,
}

impl DictationGesture {
    /// The `0`/`1` byte written to `preferences.conf`.
    pub fn token(self) -> &'static str {
        match self {
            Self::PushToTalk => "0",
            Self::Toggle => "1",
        }
    }

    /// Read `dictation_toggle=`.
    ///
    /// Only `1` means toggle. `koru-dictation` compares against `"1"` exactly
    /// and `koru-workspace-host` does the same, so accepting `true` here would
    /// make the authority disagree with the file's own history.
    pub fn parse(text: &str) -> Self {
        if text.trim() == "1" {
            Self::Toggle
        } else {
            Self::PushToTalk
        }
    }
}

token_enum! {
    /// The typeface a font role names.
    ///
    /// The tokens and their order are `koru_draw::theme::FontChoice`'s; the
    /// order is load-bearing there (role globals store an index) and is kept
    /// identical here so the two cannot drift into disagreeing about it.
    FontFamily, default = Inter, fold_case = false,
    {
        Inter => "inter";
        SourceSans3 => "sourcesans3";
        PlusJakartaSans => "plusjakartasans";
        IbmPlexSans => "ibmplexsans";
        Poppins => "poppins";
        Manrope => "manrope";
        SourceSerif4 => "sourceserif4";
        Lora => "lora";
        Nunito => "nunito";
        JetBrainsMono => "jetbrainsmono";
        SourceCodePro => "sourcecodepro";
        Mono => "mono";
        System => "system";
    }
}

impl FontFamily {
    /// Whether this family is one a terminal may use.
    ///
    /// The terminal font is deliberately its own setting rather than Global's
    /// `code_font`, and it is restricted to this class: a proportional face in
    /// a grid renderer is not a preference, it is a broken window.
    pub fn is_mono(self) -> bool {
        matches!(self, Self::JetBrainsMono | Self::SourceCodePro | Self::Mono)
    }
}

/// The longest family name Koru stores or puts into a font stack.
const MAX_FAMILY_NAME: usize = 64;

/// Whether `name` is safe to store in the settings file and to splice into a
/// font stack: no separator of either (`,` `;` `|` `=`), no quote, no
/// backslash, no `@` (Windows' vertical-text aliases) and no control
/// character, and at most 64 characters.
///
/// **The one rule.** The terminal's per-agent font, the app's font roles and
/// the drawing layer's installed-family scan all ask this function; there is
/// no second copy to drift (DOCFONTSIZE Q-H, 2026-10-30).
pub fn is_storable_family_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= MAX_FAMILY_NAME
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, ',' | ';' | '|' | '=' | '"' | '\'' | '`' | '\\' | '@')
        })
}

/// An installed font family, by the name the operating system gives it.
///
/// Valid by construction ([`is_storable_family_name`]), on the wire too: a
/// name with a separator or a quote fails to deserialize rather than reaching
/// a stack.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FamilyName(String);

impl FamilyName {
    pub fn new(name: &str) -> Option<Self> {
        is_storable_family_name(name).then(|| Self(name.trim().to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FamilyName {
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(&name).ok_or_else(|| {
            "a family name is 1 to 64 characters with no separator, quote or control character"
                .to_owned()
        })
    }
}

impl From<FamilyName> for String {
    fn from(name: FamilyName) -> Self {
        name.0
    }
}

impl schemars::JsonSchema for FamilyName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("FamilyName")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": MAX_FAMILY_NAME,
        })
    }
}

/// What a font role is set to.
///
/// **"Follow the app default" is a state, not a value.** The alternative —
/// writing today's default into the file — froze the default forever: v1 files
/// wrote every font key on every save, and nobody saw a new default font until
/// their file was deleted. So `FollowDefault` persists as *no key at all*, and
/// a chosen family is always written even when it happens to equal the current
/// default. That last part is a deliberate change from the pre-SP-6 writer,
/// which omitted a key whose value matched the default and so could not tell
/// a choice from a habit either.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FontSetting {
    /// Use whatever this build's default for the role is, now and later.
    #[default]
    FollowDefault,
    /// Use this family, whatever the build's default becomes.
    Chosen(FontFamily),
    /// Use this installed family, by name. On a computer without it the role
    /// draws its default and the choice is kept for one that has it.
    Installed(FamilyName),
}

impl FontSetting {
    /// The bundled family, or `None` when the role follows the app or names
    /// an installed family.
    pub fn family(&self) -> Option<FontFamily> {
        match self {
            Self::Chosen(family) => Some(*family),
            Self::FollowDefault | Self::Installed(_) => None,
        }
    }

    /// The installed family this names, if it names one.
    pub fn installed(&self) -> Option<&FamilyName> {
        match self {
            Self::Installed(name) => Some(name),
            Self::FollowDefault | Self::Chosen(_) => None,
        }
    }

    /// Read a font role's `preferences.conf` value: `family:<name>` names an
    /// installed family, anything else a bundled one. A name that is not
    /// storable follows the default.
    pub fn parse_file_value(value: &str) -> Self {
        match value.strip_prefix("family:") {
            Some(name) => FamilyName::new(name).map_or(Self::FollowDefault, Self::Installed),
            None => Self::Chosen(FontFamily::parse(value)),
        }
    }

    /// The `preferences.conf` value, or `None` while the role follows the app.
    pub fn file_value(&self) -> Option<String> {
        match self {
            Self::FollowDefault => None,
            Self::Chosen(family) => Some(family.token().to_owned()),
            Self::Installed(name) => Some(format!("family:{}", name.as_str())),
        }
    }
}

/// A document's body text size, in px: one of the sizes Settings offers.
///
/// **A closed set, enforced where the value is read.** `Zoom` is a range
/// checked in the patch; this is a list, so a JSON `19` fails to deserialize
/// rather than slipping in, and the generated schema names the same seven
/// values the runtime accepts. The file codec stays tolerant on its own: an
/// unknown `doc_text_size=` reads as following the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct DocTextSize(u8);

impl DocTextSize {
    /// Every size offered, smallest first. Above 22 is what zoom is for.
    pub const ALL: [u8; 7] = [14, 15, 16, 17, 18, 20, 22];
    /// The size the type ramp is drawn at, and what follows-the-default means
    /// in this build.
    pub const DEFAULT: Self = Self(16);

    pub fn new(px: u8) -> Option<Self> {
        Self::ALL.contains(&px).then_some(Self(px))
    }

    pub fn px(self) -> u8 {
        self.0
    }

    /// Read a `doc_text_size=` value, or `None` for anything not offered.
    pub fn parse(text: &str) -> Option<Self> {
        text.trim().parse::<u8>().ok().and_then(Self::new)
    }
}

impl TryFrom<u8> for DocTextSize {
    type Error = String;

    fn try_from(px: u8) -> Result<Self, Self::Error> {
        Self::new(px).ok_or_else(|| format!("a document text size is one of {:?}", Self::ALL))
    }
}

impl From<DocTextSize> for u8 {
    fn from(size: DocTextSize) -> Self {
        size.0
    }
}

impl schemars::JsonSchema for DocTextSize {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DocTextSize")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "enum": Self::ALL,
        })
    }
}

/// What the document text size is set to.
///
/// The same shape as [`FontSetting`], for the same reason: following the
/// default is a state, persisted as no key, and a chosen size is always
/// written, even when it is today's default, so a person who picked 16 keeps
/// 16 if the default ever moves.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DocTextSizeSetting {
    /// This build's default size, now and later.
    #[default]
    FollowDefault,
    /// This size, whatever the build's default becomes.
    Chosen(DocTextSize),
}

impl DocTextSizeSetting {
    /// The chosen size, or `None` while the default applies.
    pub fn chosen(self) -> Option<DocTextSize> {
        match self {
            Self::FollowDefault => None,
            Self::Chosen(size) => Some(size),
        }
    }
}

/// A zoom factor, valid only within the range the app has always accepted.
///
/// Out of range is *ignored* rather than clamped — the guarded-geometry
/// stance: a file carrying nonsense keeps the default rather than snapping to
/// a surprising value the person never chose.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Zoom(f32);

impl Zoom {
    pub const MIN: f32 = 0.5;
    pub const MAX: f32 = 2.0;

    pub fn new(value: f32) -> Option<Self> {
        (value.is_finite() && (Self::MIN..=Self::MAX).contains(&value)).then_some(Self(value))
    }

    pub fn get(self) -> f32 {
        self.0
    }

    /// Read a `zoom=` value, keeping the default when it is out of range.
    pub fn parse_or_default(text: &str) -> Self {
        text.trim()
            .parse::<f32>()
            .ok()
            .and_then(Self::new)
            .unwrap_or_default()
    }
}

impl Default for Zoom {
    fn default() -> Self {
        Self(1.0)
    }
}

/// The canvas background a new document opens with.
///
/// `Auto` persists as the literal `auto`; a color persists as `#rrggbb` in
/// lowercase. Alpha is not stored and is always opaque, exactly as today.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CanvasBackground {
    #[default]
    Auto,
    Color([u8; 3]),
}

impl CanvasBackground {
    pub fn token(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Color([r, g, b]) => format!("#{r:02x}{g:02x}{b:02x}"),
        }
    }

    pub fn parse(text: &str) -> Self {
        let Some(hex) = text.trim().strip_prefix('#') else {
            return Self::Auto;
        };
        if hex.len() != 6 {
            return Self::Auto;
        }
        let component = |range: std::ops::Range<usize>| {
            hex.get(range)
                .and_then(|part| u8::from_str_radix(part, 16).ok())
        };
        match (component(0..2), component(2..4), component(4..6)) {
            (Some(r), Some(g), Some(b)) => Self::Color([r, g, b]),
            _ => Self::Auto,
        }
    }
}

/// An opaque color, persisted as `#rrggbb` in lowercase — today the window's
/// base color for one theme.
///
/// **Any color, not only the swatches Settings offers.** The list is what a
/// person can pick today; the file and the wire take every `#rrggbb`, so a
/// free-form picker later needs no format change. Read in either case and
/// written in lowercase. Anything that is not `#` and six hex digits is not a
/// color: the file reads it as absent, and the wire refuses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RgbColor(u32);

impl RgbColor {
    /// An `0xRRGGBB` value, or `None` for one with bits above the 24.
    pub fn new(rgb: u32) -> Option<Self> {
        (rgb <= 0xFF_FFFF).then_some(Self(rgb))
    }

    /// The color as `0xRRGGBB`.
    pub fn rgb(self) -> u32 {
        self.0
    }

    /// The exact spelling written: `#rrggbb`, lowercase.
    pub fn token(self) -> String {
        format!("#{:06x}", self.0)
    }

    /// Read `#rrggbb`, or `None` for anything else.
    ///
    /// Every digit is checked as a hex digit before parsing, because
    /// `from_str_radix` takes a leading `+` and `#+12345` is not a color.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.trim().strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(hex, 16).ok().and_then(Self::new)
    }
}

impl TryFrom<String> for RgbColor {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text).ok_or_else(|| "a color is written #rrggbb".to_owned())
    }
}

impl From<RgbColor> for String {
    fn from(color: RgbColor) -> Self {
        color.token()
    }
}

impl schemars::JsonSchema for RgbColor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("RgbColor")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^#[0-9A-Fa-f]{6}$",
        })
    }
}

/// A language preference: automatic, or one BCP 47 tag.
///
/// The authority validates the *shape* of a tag and never resolves it: the
/// closed set of languages lives with each consumer (`koru-i18n` for the UI,
/// the speech engine for dictation, the spell engine for spelling), and
/// pinning a copy of any of them here would be a second list to keep true.
/// An unrecognized tag resolves to automatic at the consumer.
///
/// Grew out of the dictation preference when spell checking needed exactly
/// the same shape; [`DictationLanguage`] remains as an alias so nothing
/// renames on the wire or at a call site.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LanguageChoice {
    #[default]
    Automatic,
    Tag(String),
}

/// The original name, kept so call sites and derived schemas do not churn.
pub type DictationLanguage = LanguageChoice;

impl LanguageChoice {
    pub const AUTO: &'static str = "auto";

    pub fn token(&self) -> &str {
        match self {
            Self::Automatic => Self::AUTO,
            Self::Tag(tag) => tag,
        }
    }

    pub fn parse(text: &str) -> Self {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(Self::AUTO) {
            return Self::Automatic;
        }
        Self::Tag(trimmed.to_owned())
    }

    /// A BCP 47 tag is letters, digits and hyphens. Anything else would put a
    /// line break or a `=` into a `key=value` file.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Self::Tag(tag) = self else {
            return Ok(());
        };
        if tag.is_empty() || tag.len() > 35 {
            return Err("a language tag is between 1 and 35 characters");
        }
        if !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err("a language tag is ASCII letters, digits and hyphens");
        }
        Ok(())
    }
}

/// Which microphone dictation opens.
///
/// The device *name* roams with the person rather than staying on the machine
/// — ADR-252's recorded reasoning, kept deliberately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DictationInput {
    #[default]
    SystemDefault,
    Named(String),
}

impl DictationInput {
    pub const SYSTEM_DEFAULT: &'static str = "system-default";

    pub fn token(&self) -> &str {
        match self {
            Self::SystemDefault => Self::SYSTEM_DEFAULT,
            Self::Named(name) => name,
        }
    }

    pub fn parse(text: &str) -> Self {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(Self::SYSTEM_DEFAULT) {
            return Self::SystemDefault;
        }
        Self::Named(trimmed.to_owned())
    }

    /// A device name is one line of text. A newline would end the record and a
    /// `=` would be read back as a second key.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Self::Named(name) = self else {
            return Ok(());
        };
        if name.is_empty() || name.chars().count() > 256 {
            return Err("a device name is between 1 and 256 characters");
        }
        if name.chars().any(|c| c.is_control() || c == '=') {
            return Err("a device name carries no control characters and no `=`");
        }
        Ok(())
    }
}

/// The chord that raises global search.
///
/// Currently writerless — no surface changes it — but the registry defines it
/// anyway, because a setting the app reads and nothing can write is exactly
/// the hole this authority exists to close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct GlobalShortcut(String);

impl GlobalShortcut {
    pub const DEFAULT: &'static str = "Ctrl+Shift+K";

    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.0.trim().is_empty() {
            return Err("a global shortcut is not empty");
        }
        if self.0.chars().any(char::is_control) {
            return Err("a global shortcut carries no control characters");
        }
        Ok(())
    }
}

impl Default for GlobalShortcut {
    fn default() -> Self {
        Self(Self::DEFAULT.to_owned())
    }
}

/// A global Dictation chord accepted by Agent's single hotkey pump.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct DictationShortcut(String);

impl DictationShortcut {
    pub const DEFAULT: &'static str = "Ctrl+Shift+D";
    pub const WINDOWS_MODIFIERS: &'static [&'static str] =
        &["Ctrl+Shift", "Ctrl+Alt", "Ctrl+Win", "Shift+Win"];
    pub const MACOS_MODIFIERS: &'static [&'static str] =
        &["Ctrl+Shift", "Ctrl+Option", "Shift+Option"];
    /// Physical keys the Voice picker and Windows registrar accept.
    ///
    /// The stored label names the key cap rather than text produced by the
    /// current keyboard layout. That keeps Ctrl+Alt from turning a valid key
    /// into an AltGr character before Agent can register it.
    pub const KEYS: &'static [&'static str] = &[
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
        "Space", "[", "]", "\\", "/",
    ];

    pub fn try_new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = Self(value.into());
        value.validate()?;
        Ok(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Read a tolerant device-local value. Invalid text becomes the shipped
    /// chord before it can reach Agent's platform registrar.
    pub fn parse_or_default(value: &str) -> Self {
        Self::try_new(value).unwrap_or_default()
    }

    /// Keep capture keyboard-only and total over Agent's platform parser.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Some((modifiers, key)) = self.0.rsplit_once('+') else {
            return Err("a dictation shortcut uses a supported modifier pair and key");
        };
        if !Self::WINDOWS_MODIFIERS.contains(&modifiers)
            && !Self::MACOS_MODIFIERS.contains(&modifiers)
        {
            return Err("a dictation shortcut uses a supported modifier pair and key");
        }
        if Self::KEYS.contains(&key) {
            Ok(())
        } else {
            Err(
                "a dictation shortcut uses a supported modifier pair with A-Z, 0-9, Space, [, ], \\, or /",
            )
        }
    }
}

impl TryFrom<String> for DictationShortcut {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<DictationShortcut> for String {
    fn from(value: DictationShortcut) -> Self {
        value.0
    }
}

impl Default for DictationShortcut {
    fn default() -> Self {
        Self(Self::DEFAULT.to_owned())
    }
}

/// An Ollama endpoint. The same shape `ai.db`'s repository validates, checked
/// here so the authority refuses before the worker has to.
pub fn validate_endpoint(endpoint: &str) -> Result<(), &'static str> {
    let rest = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .ok_or("an endpoint is an http or https URL")?;
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err("an endpoint names a host");
    }
    if host.contains('@') {
        return Err("an endpoint carries no userinfo");
    }
    if endpoint.contains('?') || endpoint.contains('#') {
        return Err("an endpoint carries no query or fragment");
    }
    if endpoint.chars().any(char::is_control) {
        return Err("an endpoint carries no control characters");
    }
    Ok(())
}

/// A provider model name, as `ai.db` bounds it.
pub fn validate_model(model: &str) -> Result<(), &'static str> {
    if model.is_empty() || model.len() > 500 {
        return Err("a model name is between 1 and 500 characters");
    }
    if model.chars().any(char::is_control) {
        return Err("a model name carries no control characters");
    }
    Ok(())
}

/// An emoji skin-tone selector, stored as the raw modifier text.
pub fn validate_skin_tone(tone: &str) -> Result<(), &'static str> {
    if tone.chars().count() > 8 {
        return Err("a skin tone is at most 8 characters");
    }
    if tone.chars().any(|c| c.is_control() || c == '=') {
        return Err("a skin tone carries no control characters and no `=`");
    }
    Ok(())
}
