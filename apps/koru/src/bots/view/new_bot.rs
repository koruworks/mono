// SPDX-License-Identifier: MPL-2.0

//! **The *New bot* dialog** (QUESTIONS O155, the first-bot experience): a
//! small modal over the Bots view that asks for a name, a face and where
//! the Bot runs, and creates the Bot.
//!
//! ```text
//! col  New bot dialog                    the panel (the window draws the scrim and the floating panel)
//!   row  New bot header                  the title; Close
//!   scroll_y  New bot body               everything between, scrolled when the window is short
//!     col  New bot content
//!       row  New bot preview row         the large face: the name's initial, the face picked
//!       New bot name                     the shared TextInput, focused on open
//!       col  New bot faces               two rows of six tiles; the picked one ringed
//!       row  New bot others row          Show me others: the other twelve
//!       Divider
//!       col  New bot runs                the track: This device (selected), Cloud (not selectable);
//!                                        where it runs in words; why Cloud cannot be chosen
//!       col  New bot agents              runtime pills, readiness and Manage agent providers
//!       col  New bot folder              one-line path and folder chooser; Open the full form
//!       New bot ended / stage / notice / problem   what a press came to, in the form's own words
//!   row  New bot footer                  Creating…; Retry; Start over; Cancel; Create
//! ```
//!
//! **It is a second presentation of the create form, not a second way to
//! create.** The dialog holds a Bot tab in create mode that is not in the
//! strip yet (its *host*): the draft is [`CreateForm`]'s, the commands are
//! `CreateBotWithRole`, `ReadBotCreation` and `RetryBotCreation` through
//! the host's own link, by the draft's one request and one BotId, and what
//! a refusal says is the form's own sentence. When the owner answers that
//! the Bot is made the host has become that Bot's tab
//! (`BotView::become_created`) and the window puts it in the strip: the
//! dialog closes and the new Bot's chat is in front.
//!
//! **What it decides that the form does not** is in `bots::first_bot`: the
//! first agent of the person's list, the agent's automatic mode, a folder
//! of its own under Koru's local data. A face is always picked, the default
//! one (`koru_ai_core::bots::DEFAULT_AVATAR`, the first of the table) until
//! the person picks another.
//!
//! **The picked face moves; no other does.** The preview and the picked
//! tile sway and blink together, in one pose read from the copy of the
//! window's one face clock the window hands in ([`NewBotDialog::set_face_clock`],
//! `MainView::sync_bot_faces`). The dialog owns no clock, asks for no frame
//! and has no deadline for it: the window's clock runs while the dialog
//! stands and stops when it goes, and under reduced motion every face here
//! is still. A tile the pointer is over stays still, and so does the tile
//! the keyboard rests on while the picked face is in the other twelve: only
//! the picked one moves.
//!
//! **Nothing is lost by closing it.** With nothing sent, Cancel, Escape and
//! Close drop the draft and nothing was created. Once the owner may hold
//! the creation, closing the dialog opens the same draft as the create tab,
//! where Retry and Abandon are: a creation is never left with no surface.
//! *Open the full form* does the same on purpose, before anything is sent.
//!
//! **Keys.** Tab and Shift+Tab walk the controls in the order they are
//! drawn, the face grid as one stop; the arrows, Home and End move in the
//! grid and pick as they move; Enter creates when Create is enabled, except
//! on a button, which it presses; Space presses the control it is on;
//! Escape closes. Every key is the dialog's while it stands.
//!
//! No disk, network or folder work happens here: the folder the dialog
//! shows is path arithmetic (`first_bot::default_folder`), and the owner
//! makes it with the Bot.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use koru_ai_core::bots::{BotId, DEFAULT_AVATAR};
use koru_ai_core::{AiProviderId, ModelSelection, ReasoningChoice};
use koru_draw::el::{
    Action, El, ElState, Frame, FrameCx, Interaction, Key, Leaf, Look, Ring, Role, SemNode,
    StateRecipe, TextCache, col, divider, icon, leaf, row, scroll_y, spacer, text,
};
use koru_draw::face::{FACE_COUNT, Face, FaceClock, FacePose};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::scene;
use koru_draw::text::TextEngine;
use koru_draw::theme::{IconSize, Ink, Line, Metric, Radius, Space, Surface, metrics, roles};
use koru_i18n::{Key as Words, Lang, fill, t};
use koru_store_core::ids::WorkspaceId;
use koru_ui::button::ActionStyle;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::BotView;
use super::controls::{ButtonLeaf, FieldLeaf, IconLeaf};
use super::create::{CREATE_NOTICE, ENDED_LINES, STAGE_LINES};
use super::region::{Act, NAME_FIELD, focus_order};
use crate::bots::catalog::AgentSettings;
use crate::bots::creation::{CreateForm, Sent};
use crate::bots::faces::{self, FaceLeaf};
use crate::bots::first_bot;
use crate::bots::settings::ModelDraft;

mod folder;
mod runtime;
use runtime::agent_key;

/// The surface every key below belongs to.
pub(crate) const SURFACE: &str = "new-bot";
pub(crate) const ROOT: Key = Key::surface(SURFACE, "root");
const HEADER: Key = Key::surface(SURFACE, "header");
pub(crate) const TITLE: Key = Key::surface(SURFACE, "title");
pub(crate) const CLOSE: Key = Key::surface(SURFACE, "close");
const BODY: Key = Key::surface(SURFACE, "body");
pub(crate) const PREVIEW: Key = Key::surface(SURFACE, "preview");
pub(crate) const NAME: Key = Key::surface(SURFACE, "name");
pub(crate) const GRID: Key = Key::surface(SURFACE, "faces");
pub(crate) const OTHERS: Key = Key::surface(SURFACE, "others");
pub(crate) const RUNS: Key = Key::surface(SURFACE, "runs");
pub(crate) const THIS_DEVICE: Key = Key::surface(SURFACE, "this-device");
pub(crate) const CLOUD: Key = Key::surface(SURFACE, "cloud");
pub(crate) const LOCAL_NOTE: Key = Key::surface(SURFACE, "local-note");
pub(crate) const CLOUD_NOTE: Key = Key::surface(SURFACE, "cloud-note");
pub(crate) const AGENTS: Key = Key::surface(SURFACE, "agents");
pub(crate) const MANAGE_AGENTS: Key = Key::surface(SURFACE, "manage-agents");
pub(crate) const AGENT_SETTINGS: Key = Key::surface(SURFACE, "agent-settings");
pub(crate) const AGENT_READINESS: Key = Key::surface(SURFACE, "agent-readiness");
pub(crate) const FOLDER: Key = Key::surface(SURFACE, "folder");
pub(crate) const CHANGE_FOLDER: Key = Key::surface(SURFACE, "change-folder");
pub(crate) const MANAGED_FOLDER: Key = Key::surface(SURFACE, "managed-folder");
pub(crate) const FULL_FORM: Key = Key::surface(SURFACE, "full-form");
pub(crate) const NOTICE: Key = Key::surface(SURFACE, "notice");
pub(crate) const PROBLEM: Key = Key::surface(SURFACE, "problem");
const FOOTER: Key = Key::surface(SURFACE, "footer");
pub(crate) const CREATING: Key = Key::surface(SURFACE, "creating");
pub(crate) const RETRY: Key = Key::surface(SURFACE, "retry");
pub(crate) const START_OVER: Key = Key::surface(SURFACE, "start-over");
pub(crate) const CANCEL: Key = Key::surface(SURFACE, "cancel");
pub(crate) const CREATE: Key = Key::surface(SURFACE, "create");

/// The grid's tiles, one part per face of the table: a tile is its face,
/// never its place in the grid.
const FACE_PARTS: [&str; FACE_COUNT as usize] = [
    "face-1", "face-2", "face-3", "face-4", "face-5", "face-6", "face-7", "face-8", "face-9",
    "face-10", "face-11", "face-12", "face-13", "face-14", "face-15", "face-16", "face-17",
    "face-18", "face-19", "face-20", "face-21", "face-22", "face-23", "face-24",
];
/// The form's own sentences, as the dialog keys them: what a creation the
/// owner ended left, then where one it holds stands.
const ENDED_PARTS: [Key; 3] = [
    Key::surface(SURFACE, "ended"),
    Key::surface(SURFACE, "ended-more"),
    Key::surface(SURFACE, "ended-last"),
];
const STAGE_PARTS: [Key; 2] = [
    Key::surface(SURFACE, "stage"),
    Key::surface(SURFACE, "stage-more"),
];

/// How many faces the grid shows at once, and how many in a row.
pub(crate) const SET: u8 = 12;
const COLUMNS: u8 = 6;

/// The dialog's width, as in the design; a narrower window gets what fits.
const DIALOG_W: Metric = Metric::new("NEW_BOT_DIALOG_W", 440.0);
/// The least the panel is tall when the window is very short: the header,
/// one row of the body and the footer.
const DIALOG_MIN_H: Metric = Metric::new("NEW_BOT_DIALOG_MIN_H", 160.0);
/// A layout tall enough to read the body's natural height from.
const MEASURE_H: f32 = 100_000.0;

/// A tile's key.
pub(crate) fn face_key(face: u8) -> Key {
    Key::surface(SURFACE, FACE_PARTS[usize::from(face % FACE_COUNT)])
}

/// The face a key names, when it is a tile's.
pub(crate) fn face_of(key: Key) -> Option<u8> {
    if key.object != koru_draw::el::Object::Surface(SURFACE) {
        return None;
    }
    FACE_PARTS
        .iter()
        .position(|part| *part == key.part)
        .map(|index| index as u8)
}

/// The close button: lit under the pointer, ringed under the keyboard.
static QUIET_STATES: StateRecipe = StateRecipe {
    name: "new bot quiet action",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// A segment of the *where it runs* track: the selected one on the raised
/// plate, the track's own fill behind the other.
static SEGMENT_STATES: StateRecipe = StateRecipe {
    name: "new bot segment",
    base: Look::NONE,
    selected: Look::bg(Surface::Raised),
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Raised),
    selected_pressed: Look::bg(Surface::Raised),
    disabled: Look::NONE,
    focus: Some(Ring::Line(Line::Focus)),
};

/// **One tile of the grid**: the face, and round it the ring that says it
/// is picked (the reading ink, as the design draws it), the pointer's (the
/// border) or the keyboard's (the focus line). A picked face is also the
/// preview and is published as pressed, so the ring is never the only sign.
/// The picked tile is also the only one that moves: `pose` is the window
/// clock's for it and [`FacePose::STILL`] for every other.
struct TileLeaf {
    face: u8,
    name: String,
    pose: FacePose,
}

impl Leaf for TileLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (faces::TILE.value, faces::TILE.value)
    }

    fn hit_region(&self, at: Rect) -> Rect {
        faces::centred(at, faces::TILE.value)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let palette = cx.palette();
        let plate = faces::centred(at, faces::TILE.value);
        let inset = (faces::TILE.value - faces::TILE_FACE.value) * 0.5;
        let face = faces::centred(at, (plate.w - inset * 2.0).max(0.0));
        Face::new(self.face, &self.name)
            .posed(self.pose)
            .draw_in(scene, root, engine, face, palette);
        let ring = if state.focused {
            Some(Line::Focus.of(palette))
        } else if state.selected {
            Some(Ink::Primary.of(palette))
        } else if state.hovered && !state.disabled {
            Some(Line::Border.of(palette))
        } else {
            None
        };
        let Some(color) = ring else {
            return;
        };
        // The face's own corner, grown by the gap the ring stands off at.
        // The tile itself never moves (a pose turns the head inside it), so
        // the ring is the resting one whatever the pose.
        let width = koru_draw::theme::BORDER_W * 2.0;
        let gap = inset * 0.5;
        let side = face.w + gap * 2.0;
        let corner = koru_draw::face::geometry(self.face, side, FacePose::STILL)
            .tile
            .radii()
            .top_left as f32;
        scene::stroke(
            scene,
            root,
            color,
            width,
            &scene::rrect(face.x - gap, face.y - gap, side, side, corner),
        );
    }
}

/// What a gesture asks of the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing changed.
    Ignored,
    /// The dialog changed: repaint.
    Changed,
    /// Take the dialog down. Nothing was sent, so nothing was created.
    Cancel,
    /// Take the dialog down and put its draft in the strip as the create
    /// tab: the person asked for the full form, or closed the dialog while
    /// the owner may hold the creation.
    OpenForm,
    /// Preserve the draft as a tab and open the existing runtime settings.
    ManageAgents,
}

/// What the dialog publishes to an assistive technology: its controls as
/// the frame holds them, where the panel is, the name typed so far and the
/// control the keyboard is on.
pub(crate) struct Published {
    pub(crate) panel: Rect,
    pub(crate) nodes: Vec<SemNode>,
    pub(crate) name: String,
    pub(crate) focus: Key,
}

pub(crate) struct NewBotDialog {
    /// The create tab behind the dialog: the draft, its link and its agent
    /// catalog. Not in the strip until the Bot is made or the full form is
    /// asked for.
    host: BotView,
    /// This window's channel root, read once when the dialog opened: what
    /// the folder it shows is worked out from. `None` when the environment
    /// names none, and then the dialog says a managed folder in words.
    channel_root: Option<PathBuf>,
    /// Which twelve faces the grid shows: 0 or 1.
    set: u8,
    focus: Key,
    hot: Option<Key>,
    pressed: Option<Key>,
    scroll: Cell<f32>,
    /// The keyboard moved: the next layout brings its control into view.
    reveal: Cell<bool>,
    viewport: (f32, f32),
    cache: RefCell<TextCache>,
    lang: Lang,
    /// A pass has run: the catalog was asked for the agents at least once.
    passed: bool,
    /// **A copy of the window's one face clock**, handed in by the window
    /// (`set_face_clock`): what the picked face is posed from. The dialog
    /// owns no clock and asks for no frame. Until the window hands one in
    /// it is a clock that never started, and every face is still.
    face_clock: FaceClock,
}

impl NewBotDialog {
    /// **A new dialog for `home`**: a fresh draft, the default face picked,
    /// the keyboard in the name field.
    pub(crate) fn open(
        home: WorkspaceId,
        home_name: Option<String>,
        channel_root: Option<PathBuf>,
        viewport: (f32, f32),
    ) -> Self {
        Self::over(BotView::creating(home, home_name), channel_root, viewport)
    }

    /// The dialog over `host`, a create tab nothing was typed into.
    pub(crate) fn over(
        mut host: BotView,
        channel_root: Option<PathBuf>,
        viewport: (f32, f32),
    ) -> Self {
        if let Some(create) = host.create.as_deref_mut() {
            create.first_bot = true;
            create.form.choose_avatar(Some(DEFAULT_AVATAR));
        }
        let lang = host.lang;
        Self {
            host,
            channel_root,
            set: 0,
            focus: NAME,
            hot: None,
            pressed: None,
            scroll: Cell::new(0.0),
            reveal: Cell::new(false),
            viewport,
            cache: RefCell::new(TextCache::default()),
            lang,
            passed: false,
            face_clock: FaceClock::new(),
        }
    }

    fn form(&self) -> Option<&CreateForm> {
        Some(&self.host.create.as_deref()?.form)
    }

    fn form_mut(&mut self) -> Option<&mut CreateForm> {
        Some(&mut self.host.create.as_deref_mut()?.form)
    }

    /// The BotId the draft reserves: what the window's folder dialog
    /// answers by.
    pub(crate) fn draft_bot(&self) -> Option<BotId> {
        self.host.create_bot()
    }

    /// The name typed so far.
    pub(crate) fn name(&self) -> String {
        self.form()
            .map(|form| form.draft().name.clone())
            .unwrap_or_default()
    }

    /// The face picked.
    pub(crate) fn face(&self) -> u8 {
        self.form()
            .and_then(|form| form.draft().avatar)
            .unwrap_or(DEFAULT_AVATAR)
    }

    /// **The window's face clock**, as it stands this pass
    /// (`MainView::sync_bot_faces`): what the picked face moves by.
    pub(crate) fn set_face_clock(&mut self, clock: &FaceClock) {
        self.face_clock = clock.clone();
    }

    /// **The pose `face` is drawn in at `now`**: the window clock's for the
    /// picked face, so the preview and its tile move as one, and
    /// [`FacePose::STILL`] for every other. Still for all of them while the
    /// clock is stopped or the person asked for reduced motion.
    pub(crate) fn pose_of(&self, face: u8, now: std::time::Instant) -> FacePose {
        self.face_clock.pose(face, face == self.face(), now)
    }

    /// **What the preview draws**: the face picked, wearing the initial of
    /// the name typed so far.
    pub(crate) fn preview(&self) -> (u8, String) {
        (self.face(), koru_draw::face::initial(&self.name()))
    }

    /// The pose of every face of the table at `now`, by its index. Test
    /// only.
    #[cfg(test)]
    pub(crate) fn poses_at(&self, now: std::time::Instant) -> Vec<FacePose> {
        (0..FACE_COUNT)
            .map(|face| self.pose_of(face, now))
            .collect()
    }

    /// Which twelve faces the grid shows.
    #[cfg(test)]
    pub(crate) fn set(&self) -> u8 {
        self.set
    }

    pub(crate) fn focus(&self) -> Key {
        self.focus
    }

    pub(crate) fn set_viewport(&mut self, viewport: (f32, f32)) {
        self.viewport = viewport;
    }

    /// **The owner may hold this draft's creation**: a command is with the
    /// link, or the owner said it holds one, or Koru could not confirm
    /// whether it took the Create. The inputs are then locked, and closing
    /// the dialog keeps the draft as the create tab.
    pub(crate) fn held(&self) -> bool {
        self.form().is_some_and(|form| !form.editable())
    }

    /// **The Bot is made**: the host has become its tab, on its main
    /// conversation. The window puts it in the strip.
    pub(crate) fn created(&self) -> bool {
        !self.host.is_creating()
    }

    /// The tab the dialog leaves: the new Bot's, or the create form on the
    /// same draft. The full form applies its own rules from here on: a
    /// draft not sent yet starts in the agent's asking mode there.
    pub(crate) fn into_tab(mut self) -> BotView {
        if let Some(create) = self.host.create.as_deref_mut() {
            create.first_bot = false;
        }
        self.into_setup_tab()
    }

    /// Visiting runtime configuration keeps the dialog's draft and defaults.
    pub(crate) fn into_setup_tab(mut self) -> BotView {
        self.host.focus = None;
        self.host
    }

    // ------------------------------------------------------------ the pass

    /// **The pass a standing dialog runs**, on the window's wake path: the
    /// host's own (its link and catalog the first time, the agent for the
    /// draft, the read a creation owes, what came back). `true` when a
    /// repaint is owed.
    pub(crate) fn pass(&mut self, root: Option<PathBuf>, settings: &AgentSettings) -> bool {
        self.passed = true;
        let mut changed = self.host.set_agent_settings(settings);
        changed |= self.host.shown_pass(root);
        if changed {
            self.keep_focus();
        }
        changed
    }

    /// When the dialog next owes a pass: the creation's clock or tick, and
    /// the catalog's while an answer is owed. `None` for a dialog nobody
    /// has pressed Create in and whose agents are listed.
    pub(crate) fn deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        [self.host.create_deadline(now), self.host.catalog.deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Where the creation stands, when one of the host's answers moved it:
    /// for the roster's Unfinished group. Taken once.
    pub(crate) fn take_creation_news(&mut self) -> Option<koru_ai_core::bots::BotCreation> {
        self.host.take_creation_news()
    }

    /// *Change…* was pressed: where the window's folder dialog starts.
    /// Taken once.
    pub(crate) fn take_folder_request(&mut self) -> Option<Option<PathBuf>> {
        self.host.take_folder_request()
    }

    /// The folder dialog answered: the folder joins the draft. Nothing is
    /// sent, and the owner checks it when the Bot is created.
    pub(crate) fn folder_chosen(&mut self, folder: &std::path::Path) -> bool {
        self.host.create_folder_chosen(folder)
    }

    // ------------------------------------------------------------- words

    fn words(&self, key: Words) -> String {
        t(self.lang, key).to_owned()
    }

    /// The agents the person may choose from, in their order: every agent
    /// they have not turned off, by id and name.
    fn agents(&self) -> Vec<(AiProviderId, String)> {
        self.host
            .catalog
            .control_agents(&self.host.agent_settings)
            .into_iter()
            .filter(|agent| !agent.hidden)
            .map(|agent| (agent.id.clone(), agent.label.to_owned()))
            .collect()
    }

    /// The folder the Bot will work in, as a path: the one the person
    /// chose, else the managed one the owner will make. `None` when the
    /// managed one cannot be named here.
    pub(crate) fn folder_path(&self) -> Option<String> {
        let form = self.form()?;
        if let Some(chosen) = &form.draft().folder {
            return Some(chosen.clone());
        }
        let root = self.channel_root.as_deref()?;
        Some(
            first_bot::default_folder(root, form.bot())
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// The complete folder description, retained for accessibility.
    pub(crate) fn summary(&self) -> String {
        match self.folder_path() {
            Some(path) => fill(t(self.lang, Words::BotsNewBotWorksIn), &[path.as_str()]),
            None => self.words(Words::BotsFolderManaged),
        }
    }

    /// What the form refuses before anything is sent, in words, where the
    /// dialog says it: never the empty name (Create is simply not offered
    /// yet), and no agent only once the agents were asked for and answered.
    pub(crate) fn problem(&self) -> Option<String> {
        let form = self.form()?;
        if !form.editable() {
            return None;
        }
        match self.host.create_problem()? {
            Words::BotsNameRequired => None,
            Words::BotsAgentRequired if !self.passed || self.host.catalog.waiting() => None,
            words => Some(self.words(words)),
        }
    }

    /// Whether Create would send now.
    pub(crate) fn can_create(&self) -> bool {
        self.host.can_create()
    }

    // -------------------------------------------------------------- frame

    fn button(
        &self,
        engine: &mut TextEngine,
        name: &'static str,
        key: Key,
        label: String,
        style: ActionStyle,
        disabled: bool,
    ) -> El {
        leaf(
            name,
            ButtonLeaf::new(engine, label.clone(), style, disabled, self.focus == key),
        )
        .semantic()
        .id(key)
        .hit()
        .focusable()
        .disabled(disabled)
        .action(Action::Click)
        .action(Action::Focus)
        .semantics(Role::Button)
        .name_text(label)
    }

    /// A sentence of the dialog: meta, wrapped, and published as words.
    fn sentence(name: &'static str, key: Key, words: String, ink: Ink) -> El {
        text(words.clone())
            .named(name)
            .id(key)
            .role(roles::META)
            .ink(ink)
            .wrap()
            .semantics(Role::Group)
            .name_text(words)
    }

    fn grid(&self, locked: bool, now: std::time::Instant) -> El {
        let name = self.name();
        let picked = self.face();
        let first = self.set * SET;
        let mut grid = col("New bot faces")
            .id(GRID)
            .gap(Space::S1)
            .semantics(Role::Group)
            .name(Words::BotsNewBotAvatar);
        for line in 0..(SET / COLUMNS) {
            let mut tiles = row("New bot face row").gap(Space::S1);
            for column in 0..COLUMNS {
                let face = first + line * COLUMNS + column;
                let number = koru_i18n::number::decimal(self.lang, u64::from(face) + 1);
                tiles = tiles.child(
                    leaf(
                        "New bot face",
                        TileLeaf {
                            face,
                            name: name.clone(),
                            pose: self.pose_of(face, now),
                        },
                    )
                    .semantic()
                    .fill()
                    .id(face_key(face))
                    .hit()
                    .focusable()
                    .disabled(locked)
                    .selected(face == picked)
                    .action(Action::Click)
                    .action(Action::Focus)
                    .semantics(Role::Button)
                    .name_text(fill(
                        t(self.lang, Words::BotsNewBotFace),
                        &[number.as_str()],
                    )),
                );
            }
            grid = grid.child(tiles);
        }
        grid
    }

    /// **Where it runs**: a two-segment track built from the element
    /// vocabulary, because the shared `Segmented` draws text labels only and
    /// cannot hold one segment inert. *Cloud* is drawn and published but
    /// takes no press, no focus and no selection: remote Bots are not
    /// built. It wears the struck cloud and the hint ink, and the sentence
    /// under the track says so in words.
    fn runs(&self) -> El {
        let segment = |name: &'static str, key: Key, glyph: char, words: Words, ink: Ink| {
            row(name)
                .id(key)
                .fill()
                .min_h(metrics::BUTTON_MIN)
                .gap(Space::S1_5)
                .items_center()
                .radius(Radius::M)
                .states(&SEGMENT_STATES)
                .child(spacer())
                .child(icon(glyph, IconSize::S).decorative().ink(ink))
                .child(
                    text(self.words(words))
                        .named("New bot segment label")
                        .ink(ink),
                )
                .child(spacer())
        };
        col("New bot runs")
            .gap(Space::S2)
            .child(
                row("New bot runs track")
                    .id(RUNS)
                    .bg(Surface::Chip)
                    .radius(Radius::L)
                    .pad(Space::S0_5)
                    .gap(Space::S0_5)
                    .semantics(Role::Group)
                    .name(Words::BotsNewBotRunsWhere)
                    .child(
                        segment(
                            "New bot this device",
                            THIS_DEVICE,
                            icons::DESKTOP,
                            Words::BotsNewBotThisDevice,
                            Ink::Primary,
                        )
                        .hit()
                        .focusable()
                        .selected(true)
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::Button)
                        .name(Words::BotsNewBotThisDevice),
                    )
                    .child(
                        segment(
                            "New bot cloud",
                            CLOUD,
                            icons::CLOUD_SLASH,
                            Words::BotsNewBotCloud,
                            Ink::Hint,
                        )
                        .disabled(true)
                        .selected(false)
                        .semantics(Role::Button)
                        .name(Words::BotsNewBotCloudUnavailable),
                    ),
            )
            .child(Self::sentence(
                "New bot local note",
                LOCAL_NOTE,
                self.words(Words::BotsNewBotLocalNote),
                Ink::Secondary,
            ))
            .child(Self::sentence(
                "New bot cloud note",
                CLOUD_NOTE,
                self.words(Words::BotsNewBotCloudNote),
                Ink::Hint,
            ))
    }

    /// The dialog as elements.
    pub(crate) fn tree(&self, engine: &mut TextEngine) -> El {
        let Some(form) = self.form() else {
            return col("New bot dialog").id(ROOT);
        };
        let locked = !form.editable();
        // One instant for the whole frame: the preview and the picked tile
        // are the same face in the same pose.
        let now = std::time::Instant::now();
        let header = row("New bot header")
            .id(HEADER)
            .pad_start(Space::S5)
            .pad_end(Space::S3)
            .pad_top(Space::S3)
            .pad_bottom(Space::S1)
            .gap(Space::S2)
            .items_center()
            .child(
                text(self.words(Words::BotsNewBot))
                    .named("New bot title")
                    .id(TITLE)
                    .role(roles::POPOVER_HEADING)
                    .emphasis(true)
                    .fill()
                    .truncate()
                    .semantics(Role::Heading)
                    .name(Words::BotsNewBot),
            )
            .child(
                leaf(
                    "New bot close",
                    IconLeaf {
                        glyph: icons::WINDOW_CLOSE,
                        disabled: false,
                    },
                )
                .semantic()
                .id(CLOSE)
                .radius(Radius::M)
                .states(&QUIET_STATES)
                .hit()
                .focusable()
                .action(Action::Click)
                .action(Action::Focus)
                .semantics(Role::Button)
                .name(Words::BotsNewBotClose),
            );

        let mut content = col("New bot content")
            .pad_x(Space::S6)
            .pad_top(Space::S1)
            .pad_bottom(Space::S2)
            .gap(Space::S4)
            .child(
                row("New bot preview row")
                    .child(spacer())
                    .child(
                        leaf("New bot preview", {
                            let (face, initial) = self.preview();
                            FaceLeaf {
                                face,
                                name: initial,
                                pose: self.pose_of(face, now),
                                size: faces::PREVIEW_FACE,
                            }
                        })
                        .semantic()
                        .id(PREVIEW)
                        .semantics(Role::Image)
                        .name(Words::BotsNewBotPreview),
                    )
                    .child(spacer()),
            )
            .child(
                leaf(
                    "New bot name",
                    FieldLeaf {
                        input: self.host.name_input_with(engine),
                        placeholder: self.words(Words::BotsNewBotNamePlaceholder),
                        focused: self.focus == NAME,
                    },
                )
                .semantic()
                .id(NAME)
                .hit()
                .focusable()
                .disabled(locked)
                .action(Action::Click)
                .action(Action::Focus)
                .semantics(Role::TextInput)
                .name(Words::BotsNewBotNamePlaceholder),
            )
            .child(
                col("New bot faces block")
                    .gap(Space::S2)
                    .child(self.grid(locked, now))
                    .child(
                        row("New bot others row")
                            .child(spacer())
                            .child(self.button(
                                engine,
                                "New bot others",
                                OTHERS,
                                self.words(Words::BotsNewBotOthers),
                                ActionStyle::Ghost,
                                false,
                            ))
                            .child(spacer()),
                    ),
            )
            .child(divider(Line::Divider))
            .child(self.runs())
            .child(self.runtime_choices(engine, locked))
            .child(self.folder_controls(locked));

        // **What the last press came to**, in the form's own sentences: a
        // creation the owner ended and what it left, where one it holds
        // stands, a refusal by its code, and what the form refuses before
        // anything is sent.
        for (line, key) in ENDED_LINES.into_iter().zip(ENDED_PARTS) {
            if let Some(words) = self.host.create_line_words(line) {
                content =
                    content.child(Self::sentence("New bot ended", key, words, Ink::Secondary));
            }
        }
        for (line, key) in STAGE_LINES.into_iter().zip(STAGE_PARTS) {
            if let Some(words) = self.host.create_line_words(line) {
                content =
                    content.child(Self::sentence("New bot stage", key, words, Ink::Secondary));
            }
        }
        if let Some(words) = self.host.create_line_words(CREATE_NOTICE)
            && !form
                .notice()
                .is_some_and(crate::bots::creation::names_the_agent)
        {
            content = content.child(Self::sentence("New bot notice", NOTICE, words, Ink::Accent));
        }
        if let Some(words) = self.problem() {
            content = content.child(Self::sentence(
                "New bot problem",
                PROBLEM,
                words,
                Ink::Secondary,
            ));
        }

        let mut footer = row("New bot footer")
            .id(FOOTER)
            .pad_x(Space::S5)
            .pad_top(Space::S3)
            .pad_bottom(Space::S4)
            .gap(Space::S2)
            .items_center();
        let making = form.in_flight() == Some(Sent::Create)
            || form
                .live()
                .is_some_and(|live| live.stage == koru_ai_core::bots::BotCreationStage::Pending);
        footer = if making {
            footer.child(
                Self::sentence(
                    "New bot creating",
                    CREATING,
                    self.words(Words::BotsCreating),
                    Ink::Hint,
                )
                .fill(),
            )
        } else {
            footer.child(spacer())
        };
        if form.in_flight().is_none() && form.live().is_some_and(|live| live.retryable) {
            footer = footer.child(self.button(
                engine,
                "New bot retry",
                RETRY,
                self.words(Words::BotsRetry),
                ActionStyle::Secondary,
                false,
            ));
        }
        if form.can_start_over() {
            footer = footer.child(self.button(
                engine,
                "New bot start over",
                START_OVER,
                self.words(Words::BotsCreateStartOver),
                ActionStyle::Secondary,
                false,
            ));
        }
        // Closing a dialog whose creation the owner may hold cancels
        // nothing, and the button says so.
        footer = footer.child(self.button(
            engine,
            "New bot cancel",
            CANCEL,
            self.words(if locked {
                Words::BotsNewBotClose
            } else {
                Words::BotsDialogCancel
            }),
            ActionStyle::Secondary,
            false,
        ));
        if form.live().is_none() {
            footer = footer.child(self.button(
                engine,
                "New bot create",
                CREATE,
                self.words(Words::BotsNewBotCreate),
                ActionStyle::Primary,
                !self.can_create(),
            ));
        }

        col("New bot dialog")
            .id(ROOT)
            .semantics(Role::Group)
            .name(Words::BotsNewBot)
            .child(header)
            .child(
                scroll_y("New bot body")
                    .id(BODY)
                    .fill()
                    .offset(self.scroll.get())
                    .scrollbar()
                    .child(content),
            )
            .child(footer)
    }

    fn interaction(&self) -> Interaction {
        Interaction {
            hot: self.hot,
            pressed: self.pressed,
            focused: Some(self.focus),
        }
    }

    /// **The panel's place**: the design's width or what the window leaves,
    /// as tall as its content or what the window leaves, in the middle.
    fn panel(&self, tree: &El, engine: &mut TextEngine) -> Rect {
        let margin = Space::S4.px(koru_draw::theme::Profile::Desktop);
        let w = self.panel_width();
        let room = (self.viewport.1 - margin * 2.0).max(DIALOG_MIN_H.value.min(self.viewport.1));
        let probe = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            Rect::new(0.0, 0.0, w, MEASURE_H),
            FrameCx::root(),
        );
        let part = |key| probe.node(key).map_or(0.0, |node| node.rect.h);
        let body = probe
            .node(BODY)
            .and_then(|node| node.scroll)
            .map_or(0.0, |(extent, _)| extent);
        let h = (part(HEADER) + body + part(FOOTER)).min(room);
        Rect::new(
            ((self.viewport.0 - w) * 0.5).max(0.0),
            ((self.viewport.1 - h) * 0.5).max(0.0),
            w,
            h,
        )
    }

    fn layout(&self, engine: &mut TextEngine) -> (El, Frame, Rect) {
        let lay = |engine: &mut TextEngine| {
            let tree = self.tree(engine);
            let panel = self.panel(&tree, engine);
            let frame = tree.layout(
                engine,
                &mut self.cache.borrow_mut(),
                panel,
                FrameCx::root().with_interaction(self.interaction()),
            );
            (tree, frame, panel)
        };
        let mut laid = lay(engine);
        // The keyboard moved: its control comes into view.
        if self.reveal.replace(false)
            && let Some((_, offset)) = laid.1.reveal(self.focus)
            && offset != self.scroll.get()
        {
            self.scroll.set(offset);
            laid = lay(engine);
        }
        if let Some((_, offset)) = laid.1.node(BODY).and_then(|node| node.scroll) {
            self.scroll.set(offset);
        }
        laid
    }

    /// The frame as input, accessibility and a test read it.
    pub(crate) fn with_frame<R>(&self, read: impl FnOnce(&Frame, Rect) -> R) -> R {
        let (_, frame, panel) = crate::input_engine::with(|engine| self.layout(engine));
        read(&frame, panel)
    }

    /// Paint the dialog: the shared scrim, the floating panel every modal
    /// is drawn on, and the frame.
    pub(crate) fn draw(&self, scene_out: &mut Scene, root: Affine, engine: &mut TextEngine) {
        let (tree, frame, panel) = self.layout(engine);
        let radius = Radius::XL.px(koru_draw::theme::Profile::Desktop, panel.w, panel.h);
        koru_ui::modal::draw(scene_out, root, self.viewport, |canvas| {
            let _panel = scene::floating_panel_scope(
                canvas,
                root,
                panel.x,
                panel.y,
                panel.w,
                panel.h,
                &scene::Panel::popover(radius),
            );
            frame.paint(&tree, canvas, root, engine);
        });
    }

    /// What an assistive technology is told.
    pub(crate) fn published(&self) -> Published {
        self.with_frame(|frame, panel| Published {
            panel,
            nodes: frame.semantics(),
            name: self.name(),
            focus: self.focus,
        })
    }

    // ---------------------------------------------------------- the keyboard

    /// **Where Tab stops**, in the order the controls are drawn: every
    /// focusable, enabled control, the grid as one stop (the picked face
    /// when it is among the twelve shown, else the first of them) and the
    /// agents as one (the chosen one).
    pub(crate) fn tab_stops(&self) -> Vec<Key> {
        let order = self.with_frame(|frame, _| focus_order(frame));
        let picked = self.face();
        let first = self.set * SET;
        let face_stop = if (first..first + SET).contains(&picked) {
            picked
        } else {
            first
        };
        let chosen = self.host.agent_provider();
        let agent_stop = self
            .agents()
            .iter()
            .position(|(id, _)| chosen.as_ref() == Some(id))
            .unwrap_or(0);
        order
            .into_iter()
            .filter(|key| match (face_of(*key), self.agent_of(*key)) {
                (Some(face), _) => face == face_stop,
                (_, Some(agent)) => agent == agent_stop,
                _ => true,
            })
            .collect()
    }

    /// **The keyboard stays on a control that is drawn.** One that went
    /// inert under it keeps it (the name field while a Create is on its
    /// way: a second Enter there sends nothing and closes nothing). When
    /// the control itself left (Create, once the owner holds the creation)
    /// the keyboard goes to what took its place: Retry, Start over, else
    /// Cancel.
    fn keep_focus(&mut self) {
        if self.with_frame(|frame, _| frame.contains(self.focus)) {
            return;
        }
        let stops = self.tab_stops();
        self.focus = [RETRY, START_OVER, CANCEL]
            .into_iter()
            .find(|key| stops.contains(key))
            .or_else(|| stops.first().copied())
            .unwrap_or(CANCEL);
        self.reveal.set(true);
    }

    fn set_focus(&mut self, key: Key) {
        if self.focus != key {
            self.focus = key;
            self.reveal.set(true);
        }
        if key != NAME {
            let input = self.host.name_input();
            crate::input_engine::with(|engine| input.borrow_mut().blur(engine));
        }
    }

    fn step_focus(&mut self, back: bool) {
        let stops = self.tab_stops();
        if stops.is_empty() {
            return;
        }
        let next = match stops.iter().position(|key| *key == self.focus) {
            Some(at) if back => (at + stops.len() - 1) % stops.len(),
            Some(at) => (at + 1) % stops.len(),
            None if back => stops.len() - 1,
            None => 0,
        };
        self.set_focus(stops[next]);
    }

    /// Pick `face`, when the inputs may change: the preview and the Create
    /// request follow.
    pub(crate) fn pick(&mut self, face: u8) -> bool {
        self.form_mut()
            .is_some_and(|form| form.choose_avatar(Some(face)))
    }

    /// The agent at `index` of the offered list, for the draft: its
    /// *Default* model at its own level, mode and Fast, as the form seeds
    /// one.
    pub(crate) fn choose_agent(&mut self, index: usize) -> bool {
        let Some((provider, _)) = self.agents().into_iter().nth(index) else {
            return false;
        };
        if !self.form().is_some_and(CreateForm::editable) {
            return false;
        }
        if self.host.agent_provider().as_ref() == Some(&provider) {
            return false;
        }
        self.host.catalog.want(&provider);
        let draft = ModelDraft {
            model: ModelSelection {
                provider,
                requested_model: koru_ai_ui::agent_catalog::config::picker::DEFAULT_MODEL
                    .to_owned(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        };
        if let Some(form) = self.form_mut() {
            form.edit_model(draft);
        }
        true
    }

    /// **Do what pressing `key` asks.**
    pub(crate) fn activate(&mut self, key: Key) -> Outcome {
        let inert = self.with_frame(|frame, _| frame.node(key).is_none_or(|node| node.disabled));
        if inert {
            return Outcome::Ignored;
        }
        if let Some(face) = face_of(key) {
            return if self.pick(face) {
                Outcome::Changed
            } else {
                Outcome::Ignored
            };
        }
        if let Some(agent) = self.agent_of(key) {
            return if self.choose_agent(agent) {
                Outcome::Changed
            } else {
                Outcome::Ignored
            };
        }
        let outcome = match key {
            k if k == CLOSE || k == CANCEL => return self.dismiss(),
            k if k == FULL_FORM => return Outcome::OpenForm,
            k if k == MANAGE_AGENTS || k == AGENT_SETTINGS => return Outcome::ManageAgents,
            k if k == OTHERS => {
                self.set = 1 - self.set.min(1);
                true
            }
            k if k == CHANGE_FOLDER => self.host.perform_create(Act::ChooseFolder),
            k if k == MANAGED_FOLDER => self.host.perform_create(Act::UseManagedFolder),
            k if k == RETRY => self.host.perform_create(Act::Retry),
            k if k == START_OVER => self.host.perform_create(Act::StartOver),
            k if k == CREATE => self.host.perform_create(Act::Create),
            // *This device* is already the choice; *Cloud* is inert above.
            _ => false,
        };
        if outcome {
            self.keep_focus();
            Outcome::Changed
        } else {
            Outcome::Ignored
        }
    }

    /// **Cancel, Escape and Close.** With nothing sent the draft is dropped
    /// and nothing was created; once the owner may hold the creation the
    /// draft goes to the strip as the create tab instead.
    pub(crate) fn dismiss(&mut self) -> Outcome {
        if self.held() {
            Outcome::OpenForm
        } else {
            Outcome::Cancel
        }
    }

    /// Move in the grid from the tile the keyboard is on, picking the tile
    /// it lands on: the arrows by one tile or one row, Home and End to
    /// either end of the twelve shown.
    fn grid_key(&mut self, face: u8, key: &WinitKey) -> bool {
        let first = self.set * SET;
        let last = first + SET - 1;
        let to = match key {
            WinitKey::Named(NamedKey::ArrowLeft) => face.saturating_sub(1).max(first),
            WinitKey::Named(NamedKey::ArrowRight) => (face + 1).min(last),
            // At an edge the keyboard stays where it is, and the key is
            // still the grid's.
            WinitKey::Named(NamedKey::ArrowUp) if face >= first + COLUMNS => face - COLUMNS,
            WinitKey::Named(NamedKey::ArrowDown) if face + COLUMNS <= last => face + COLUMNS,
            WinitKey::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => face,
            WinitKey::Named(NamedKey::Home) => first,
            WinitKey::Named(NamedKey::End) => last,
            _ => return false,
        };
        if to != face {
            self.set_focus(face_key(to));
            self.pick(to);
        }
        true
    }

    /// Move among the agents from the one the keyboard is on, choosing the
    /// one it lands on.
    fn agents_key(&mut self, agent: usize, key: &WinitKey) -> bool {
        let count = self.agents().len();
        if count == 0 {
            return false;
        }
        let to = match key {
            WinitKey::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => agent.saturating_sub(1),
            WinitKey::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                (agent + 1).min(count - 1)
            }
            WinitKey::Named(NamedKey::Home) => 0,
            WinitKey::Named(NamedKey::End) => count - 1,
            _ => return false,
        };
        if to != agent {
            self.set_focus(agent_key(&self.agents()[to].0));
            self.choose_agent(to);
        }
        true
    }

    /// Whether Enter on `key` presses it: a button of the dialog. On the
    /// name field, a face, a segment or an agent, Enter creates.
    fn enter_presses(key: Key) -> bool {
        [
            CLOSE,
            OTHERS,
            MANAGE_AGENTS,
            AGENT_SETTINGS,
            CHANGE_FOLDER,
            MANAGED_FOLDER,
            FULL_FORM,
            RETRY,
            START_OVER,
            CANCEL,
            CREATE,
        ]
        .contains(&key)
    }

    /// **A key while the dialog stands.** Every key is the dialog's: one it
    /// has no use for goes no further.
    pub(crate) fn key(
        &mut self,
        key: &WinitKey,
        typed: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let chord = mods.control_key() || mods.alt_key() || mods.super_key();
        match key {
            WinitKey::Named(NamedKey::Escape) => return self.dismiss(),
            WinitKey::Named(NamedKey::Tab) if !chord => {
                self.step_focus(mods.shift_key());
                return Outcome::Changed;
            }
            WinitKey::Named(NamedKey::Enter) if !chord => {
                let target = if Self::enter_presses(self.focus) {
                    self.focus
                } else {
                    CREATE
                };
                return self.activate(target);
            }
            _ => {}
        }
        if self.focus == NAME {
            // Typing, the caret, the clipboard and undo are the shared
            // field's; the draft takes the name as it stands after each key.
            return match self.host.create_field_key(NAME_FIELD, key, typed, mods) {
                Some(true) => Outcome::Changed,
                _ => Outcome::Ignored,
            };
        }
        if chord {
            return Outcome::Ignored;
        }
        if let Some(face) = face_of(self.focus)
            && self.grid_key(face, key)
        {
            return Outcome::Changed;
        }
        if let Some(agent) = self.agent_of(self.focus)
            && self.agents_key(agent, key)
        {
            return Outcome::Changed;
        }
        let space = matches!(key, WinitKey::Named(NamedKey::Space))
            || matches!(key, WinitKey::Character(text) if text == " ");
        if space {
            return self.activate(self.focus);
        }
        Outcome::Ignored
    }

    // ------------------------------------------------------------ the pointer

    /// A press. On a control it takes the keyboard there and presses it; in
    /// the name field it places the caret. A press anywhere else, the scrim
    /// included, dismisses nothing: a name somebody typed is not lost to a
    /// stray click.
    pub(crate) fn mouse_down(&mut self, x: f32, y: f32) -> Outcome {
        let hit = self.with_frame(|frame, _| {
            let key = frame.hit_at(x, y)?;
            let node = frame.node(key)?;
            Some((key, node.rect, node.focusable && !node.disabled))
        });
        let Some((key, rect, focusable)) = hit else {
            return Outcome::Ignored;
        };
        if key == BODY {
            return Outcome::Ignored;
        }
        if key == NAME {
            if !focusable {
                return Outcome::Ignored;
            }
            self.set_focus(NAME);
            self.pressed = Some(NAME);
            self.host.create_field_press(NAME_FIELD, Some(rect), x, y);
            return Outcome::Changed;
        }
        if focusable {
            self.set_focus(key);
        }
        match self.activate(key) {
            Outcome::Ignored if focusable => Outcome::Changed,
            outcome => outcome,
        }
    }

    pub(crate) fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.pressed == Some(NAME) {
            let rect = self.with_frame(|frame, _| frame.node(NAME).map(|node| node.rect));
            if let Some(rect) = rect {
                let input = self.host.name_input();
                crate::input_engine::with(|engine| input.borrow_mut().drag(engine, rect, x));
            }
            return true;
        }
        let hot = self
            .with_frame(|frame, _| frame.hit_at(x, y))
            .filter(|key| *key != BODY);
        let changed = self.hot != hot;
        self.hot = hot;
        changed
    }

    pub(crate) fn mouse_up(&mut self) -> bool {
        if self.pressed.take().is_some() {
            self.host.name_input().borrow_mut().release();
            return true;
        }
        false
    }

    /// The wheel: the body scrolls when the window is too short for it.
    pub(crate) fn scroll_by(&mut self, dy: f32) -> bool {
        let before = self.scroll.get();
        self.scroll.set((before + dy).max(0.0));
        let after = self.with_frame(|_, _| self.scroll.get());
        after != before
    }

    pub(crate) fn cursor(&self, x: f32, y: f32) -> CursorIcon {
        match self.with_frame(|frame, _| frame.hit_at(x, y)) {
            Some(key) if key == NAME => CursorIcon::Text,
            Some(key) if key != BODY => koru_ui::cursor::PRESS,
            _ => CursorIcon::Default,
        }
    }

    // ------------------------------------------------- assistive technology

    /// A client's click or focus on one of the dialog's controls.
    pub(crate) fn accessibility_action(&mut self, key: Key, click: bool) -> Outcome {
        let focusable = self.with_frame(|frame, _| {
            frame
                .node(key)
                .is_some_and(|node| node.focusable && !node.disabled)
        });
        if !focusable {
            return Outcome::Ignored;
        }
        self.set_focus(key);
        if click && key != NAME {
            return match self.activate(key) {
                Outcome::Ignored => Outcome::Changed,
                outcome => outcome,
            };
        }
        Outcome::Changed
    }

    /// A client set the name, as typing it would.
    pub(crate) fn accessibility_set_name(&mut self, name: &str) -> bool {
        if !self.form().is_some_and(CreateForm::editable) {
            return false;
        }
        let input = self.host.name_input();
        crate::input_engine::with(|engine| input.borrow_mut().set_text(engine, name));
        if let Some(form) = self.form_mut() {
            form.edit_name(name);
        }
        true
    }

    /// Type `text` into the name field, as the keyboard would. Test only.
    #[cfg(test)]
    pub(crate) fn type_name(&mut self, text: &str) {
        self.set_focus(NAME);
        for ch in text.chars() {
            let s = ch.to_string();
            let key = if ch == ' ' {
                WinitKey::Named(NamedKey::Space)
            } else {
                WinitKey::Character(s.as_str().into())
            };
            self.key(&key, Some(&s), ModifiersState::empty());
        }
    }
}

#[cfg(test)]
mod tests;
