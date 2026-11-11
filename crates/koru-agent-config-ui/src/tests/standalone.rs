// SPDX-License-Identifier: MPL-2.0

//! **The control with no API behind it** (QUESTIONS O42): a host that is
//! plain data — two agents, a draft, a list of favorites — drives the model
//! picker and the approval-mode list through their whole public surface:
//! opening, searching, the keyboard, the pointer, every kind of choice, a
//! ☆, the agent settings, and the locked states; then draws both and reads
//! what they declare. This crate cannot link a reader, a client or the AI
//! stack (`dependency.rs`), so passing is the proof that the control works
//! standalone. No window, no GPU, no disk.

use koru_draw::brand::ProfileMark;
use koru_draw::el::Role;
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::inspect;
use koru_draw::text::TextEngine;
use koru_i18n::Lang;
use koru_ui::tooltip::Tooltip;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::effort::{Effort, Level, Levels};
use crate::modes::{self, ModeList, ModeOffer, NewChatPermission};
use crate::picker::elements;
use crate::picker::{
    Agent, Applies, Context, Current, DEFAULT_MODEL, FavoriteModel, FooterOffer, Inputs, Listing,
    ModelPicker, ModelRecord, Outcome, Picker, PickerState, Place, RailEntry, Readiness, Row,
};

fn words(key: koru_i18n::Key) -> &'static str {
    koru_i18n::t(Lang::En, key)
}

fn model(id: &'static str, group: &'static str, name: &'static str) -> ModelRecord<'static> {
    ModelRecord {
        id,
        group,
        name: Some(name),
        description: None,
        group_name: None,
        legacy: false,
        context: None,
    }
}

/// The catalog a host would have read from somewhere: an agent with
/// grouped models — one with a longer window, one older — and an agent that
/// is not installed.
fn catalog() -> Vec<ModelRecord<'static>> {
    let mut opus = model("opus", "anthropic", "Opus");
    opus.context = Some("opus[1m]");
    let mut old = model("claude-2", "anthropic", "Claude 2");
    old.legacy = true;
    vec![
        model(DEFAULT_MODEL, "anthropic", "Default (recommended)"),
        opus,
        model("sonnet", "anthropic", "Sonnet"),
        model("haiku", "small", "Haiku"),
        old,
    ]
}

/// **A host that is data**: what is chosen, the favorites, what it was
/// asked for, and what the choosing applies to.
struct Host {
    models: Vec<ModelRecord<'static>>,
    provider: String,
    model: String,
    effort: Effort,
    fast: bool,
    long: bool,
    favorites: Vec<FavoriteModel>,
    applies: Applies,
    asked: Vec<String>,
}

impl Host {
    fn new() -> Self {
        Self {
            models: catalog(),
            provider: "claude".into(),
            model: "opus".into(),
            effort: Effort::Default,
            fast: false,
            long: false,
            favorites: vec![FavoriteModel::new("claude", "sonnet")],
            applies: Applies::NextTask,
            asked: Vec::new(),
        }
    }

    fn inputs(&self) -> Inputs<'_, String> {
        let offers_footer = self.model == "opus";
        Inputs {
            lang: Lang::En,
            agents: vec![
                Agent {
                    id: "claude".to_owned(),
                    key: Some("claude"),
                    label: "Claude",
                    mark: ProfileMark::Glyph(icons::ROBOT),
                    readiness: Some(Readiness::Ready),
                    hidden: false,
                    listing: Listing::Listed(self.models.clone()),
                },
                Agent {
                    id: "missing".to_owned(),
                    key: Some("missing"),
                    label: "Missing",
                    mark: ProfileMark::Glyph(icons::ROBOT),
                    readiness: Some(Readiness::NotInstalled),
                    hidden: false,
                    listing: Listing::Loading,
                },
            ],
            builtin: None,
            current: Some(Current {
                provider: &self.provider,
                model: &self.model,
                effort: self.effort,
            }),
            applies: self.applies,
            allowed: None,
            preferred: None,
            favorites: &self.favorites,
            manage: true,
            footer: FooterOffer {
                levels: offers_footer.then(|| Levels::Levels {
                    values: vec![Level::Low, Level::Medium, Level::High],
                    default: Some(Level::Medium),
                    supports_off: false,
                }),
                fast: offers_footer.then_some(self.fast),
                context: offers_footer.then_some(Context {
                    tokens: 1_000_000,
                    long: self.long,
                }),
            },
            place: Place {
                anchor: Rect::new(600.0, 640.0, 120.0, 28.0),
                viewport: Rect::new(0.0, 0.0, 900.0, 700.0),
                width: 360.0,
            },
        }
    }

    /// Carry out what the person chose, as a Bot's settings would on its
    /// draft.
    fn apply(&mut self, outcome: Outcome<String>) {
        match outcome {
            Outcome::Model { provider, model } => {
                self.provider = provider;
                self.long = model.ends_with("[1m]");
                self.model = model.trim_end_matches("[1m]").to_owned();
            }
            Outcome::Level(level) => self.effort = level,
            Outcome::Fast(on) => self.fast = on,
            Outcome::Context(long) => self.long = long,
            Outcome::Favorites(next) => self.favorites = next,
            Outcome::Catalog(agent) | Outcome::Refresh(agent) => self.asked.push(agent),
            Outcome::BuiltinModel(_) | Outcome::EffortLocked | Outcome::Setup => {}
        }
    }
}

/// The picker's lent parts, kept by the host beside its data.
struct Lent {
    state: PickerState<String>,
    open: bool,
    tooltip: Tooltip,
    engine: TextEngine,
    pointer: (f32, f32),
}

impl Lent {
    fn new() -> Self {
        let mut engine = TextEngine::new();
        Self {
            state: PickerState::new(&mut engine),
            open: false,
            tooltip: Tooltip::default(),
            engine,
            pointer: (0.0, 0.0),
        }
    }
}

/// One call, its outcomes carried out, and what it still owed finished:
/// the loop every host runs. Returns the call's answer and its outcomes.
fn run<R>(
    host: &mut Host,
    lent: &mut Lent,
    f: impl FnOnce(&mut ModelPicker<'_, '_, String>) -> R,
) -> (R, Vec<Outcome<String>>) {
    let result = {
        let inputs = host.inputs();
        let mut view = ModelPicker::new(
            &inputs,
            &mut lent.state,
            &mut lent.open,
            &mut lent.tooltip,
            &mut lent.engine,
            lent.pointer,
        );
        f(&mut view)
    };
    let mut seen = Vec::new();
    loop {
        for outcome in lent.state.take_outcomes() {
            seen.push(outcome.clone());
            host.apply(outcome);
        }
        if !lent.state.pending() {
            break;
        }
        let inputs = host.inputs();
        ModelPicker::new(
            &inputs,
            &mut lent.state,
            &mut lent.open,
            &mut lent.tooltip,
            &mut lent.engine,
            lent.pointer,
        )
        .resume();
    }
    (result, seen)
}

fn read<R>(host: &Host, lent: &Lent, f: impl FnOnce(Picker<'_, '_, String>) -> R) -> R {
    let inputs = host.inputs();
    f(Picker::new(&inputs, &lent.state, lent.open))
}

fn key(host: &mut Host, lent: &mut Lent, key: Key) -> (Option<bool>, Vec<Outcome<String>>) {
    run(host, lent, |view| {
        view.key(&key, None, ModifiersState::empty())
    })
}

fn typed(host: &mut Host, lent: &mut Lent, text: &str) {
    for ch in text.chars() {
        let text = ch.to_string();
        run(host, lent, |view| {
            view.key(
                &Key::Character(text.clone().into()),
                Some(&text),
                ModifiersState::empty(),
            )
        });
    }
}

/// A press at the centre of the element keyed `target` in the frame a
/// press reads.
fn press(
    host: &mut Host,
    lent: &mut Lent,
    target: koru_draw::el::Key,
) -> (Option<bool>, Vec<Outcome<String>>) {
    let at = read(host, lent, |picker| {
        let (_, _, frame) = picker.frame().expect("open");
        let rect = frame.node(target).expect("drawn").rect;
        (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
    });
    run(host, lent, |view| view.mouse_down(at.0, at.1))
}

fn open(host: &mut Host, lent: &mut Lent) {
    lent.open = true;
    run(host, lent, |view| view.reset());
}

#[test]
fn a_host_of_plain_data_opens_searches_and_chooses_with_the_keyboard() {
    let (mut host, mut lent) = (Host::new(), Lent::new());
    open(&mut host, &mut lent);
    // Favorites lead the rail and the picker opens on them.
    let rail = read(&host, &lent, |picker| picker.rail());
    assert_eq!(
        rail,
        [
            RailEntry::Favorites,
            RailEntry::Agent("claude".to_owned()),
            RailEntry::Agent("missing".to_owned()),
        ]
    );
    assert_eq!(
        read(&host, &lent, |picker| picker.entry()),
        Some(RailEntry::Favorites)
    );

    // A query spans every agent, under a heading each.
    typed(&mut host, &mut lent, "haik");
    let rows = read(&host, &lent, |picker| picker.rows());
    let claude = RailEntry::Agent("claude".to_owned());
    let missing = RailEntry::Agent("missing".to_owned());
    // An agent that cannot run keeps its *Manage agent providers* in a
    // search, whatever the query: what the chat's picker has always done
    // (P1.8.PICKER.notes.md §8.1), pinned here as it is.
    assert_eq!(
        rows,
        [
            Row::EntryHeading(claude.clone()),
            Row::Found(claude.clone(), Box::new(Row::Model("haiku".into()))),
            Row::EntryHeading(missing.clone()),
            Row::Found(missing, Box::new(Row::ManageAgents)),
        ]
    );
    // Enter on the found row chooses it, and the picker stays open.
    let (handled, outcomes) = key(&mut host, &mut lent, Key::Named(NamedKey::Enter));
    assert_eq!(handled, Some(true));
    assert_eq!(
        outcomes,
        [Outcome::Model {
            provider: "claude".into(),
            model: "haiku".into()
        }]
    );
    assert_eq!(host.model, "haiku");
    assert!(lent.open, "the picker stays open (D66)");

    // Esc clears the query, then closes.
    key(&mut host, &mut lent, Key::Named(NamedKey::Escape));
    assert!(lent.open);
    assert!(read(&host, &lent, |picker| picker.query().is_empty()));
    key(&mut host, &mut lent, Key::Named(NamedKey::Escape));
    assert!(!lent.open);
}

#[test]
fn every_kind_of_choice_comes_back_as_an_outcome() {
    let (mut host, mut lent) = (Host::new(), Lent::new());
    open(&mut host, &mut lent);
    // The agent's mark shows its list; its models are listed, so nothing
    // is asked for.
    let (_, outcomes) = press(
        &mut host,
        &mut lent,
        elements::mark_key(&RailEntry::Agent("claude".to_owned())),
    );
    assert!(outcomes.is_empty(), "{outcomes:?}");
    assert_eq!(
        read(&host, &lent, |picker| picker.entry()),
        Some(RailEntry::Agent("claude".to_owned()))
    );
    // Opus, chosen by the pointer: its footer offers levels, Fast and a
    // longer window.
    let opus = read(&host, &lent, |picker| {
        let rows = picker.rows();
        let index = rows
            .iter()
            .position(|row| *row == Row::Model("opus".into()))
            .unwrap();
        let spec = picker.spec();
        spec.list[index].key.unwrap()
    });
    host.model = "sonnet".into();
    let (_, outcomes) = press(&mut host, &mut lent, opus);
    assert_eq!(
        outcomes,
        [Outcome::Model {
            provider: "claude".into(),
            model: "opus".into()
        }]
    );
    let (_, outcomes) = press(
        &mut host,
        &mut lent,
        elements::segment_key(Effort::Level(Level::High)),
    );
    assert_eq!(outcomes, [Outcome::Level(Effort::Level(Level::High))]);
    let (_, outcomes) = press(&mut host, &mut lent, elements::FAST_KEY);
    assert_eq!(outcomes, [Outcome::Fast(true)]);
    let (_, outcomes) = press(&mut host, &mut lent, elements::CONTEXT_KEY);
    assert_eq!(outcomes, [Outcome::Context(true)]);
    let (_, outcomes) = press(&mut host, &mut lent, elements::REFRESH_KEY);
    assert_eq!(outcomes, [Outcome::Refresh("claude".into())]);
    lent.state.refresh_started("claude".into());
    assert_eq!(
        read(&host, &lent, |picker| picker.rows()),
        [Row::Note(words(koru_i18n::Key::TerminalChecking).into())]
    );
    lent.state.refresh_answered(&"claude".to_owned());

    // A ☆ stars without choosing; the host stores the whole list.
    let star = read(&host, &lent, |picker| {
        picker
            .spec()
            .list
            .iter()
            .find(|row| row.label == "Haiku")
            .and_then(|row| row.star.as_ref().map(|star| star.key))
            .unwrap()
    });
    let (_, outcomes) = press(&mut host, &mut lent, star);
    assert_eq!(
        outcomes,
        [Outcome::Favorites(vec![
            FavoriteModel::new("claude", "sonnet"),
            FavoriteModel::new("claude", "haiku"),
        ])]
    );
    assert_eq!(host.favorites.len(), 2);

    // An agent that cannot run says so and offers the host's settings.
    press(
        &mut host,
        &mut lent,
        elements::mark_key(&RailEntry::Agent("missing".to_owned())),
    );
    assert_eq!(
        host.asked,
        ["claude", "missing"],
        "its models were asked for"
    );
    let rows = read(&host, &lent, |picker| picker.rows());
    assert_eq!(
        rows,
        [
            Row::Note(words(koru_i18n::Key::TerminalNotInstalled).into()),
            Row::ManageAgents
        ]
    );
    let (_, outcomes) = run(&mut host, &mut lent, |view| view.choose_row(1));
    assert_eq!(outcomes, [Outcome::Setup]);
    assert!(!lent.open, "the picker closes for the settings");
}

#[test]
fn a_begun_conversation_locks_what_its_host_says_and_a_next_task_never_does() {
    let (mut host, mut lent) = (Host::new(), Lent::new());
    host.applies = Applies::Conversation {
        begun: true,
        agent_begun: true,
        model_locked: true,
        effort_locked: true,
    };
    open(&mut host, &mut lent);
    let picker_rail = read(&host, &lent, |picker| {
        (picker.rail_hidden(), picker.marks())
    });
    assert!(picker_rail.0 && picker_rail.1.is_empty(), "no rail");
    let (rows, choosable) = read(&host, &lent, |picker| {
        let rows = picker.rows();
        let choosable = picker.choosable(&rows);
        (rows, choosable)
    });
    assert_eq!(rows[0], Row::Default);
    assert!(
        rows.iter()
            .zip(&choosable)
            .all(|(row, can)| !can || *row == Row::LegacyToggle),
        "no model to move to; only the older ones can be shown"
    );
    let (_, outcomes) = press(
        &mut host,
        &mut lent,
        elements::segment_key(Effort::Level(Level::High)),
    );
    assert_eq!(outcomes, [Outcome::EffortLocked]);
    let locked = read(&host, &lent, |picker| {
        let geometry = picker.geometry().unwrap();
        let strip = geometry.strip.unwrap();
        picker.tooltip_at(&geometry, strip.x + 4.0, strip.y + 4.0)
    });
    assert_eq!(
        locked.map(|tip| tip.label).as_deref(),
        Some(words(koru_i18n::Key::AiEffortLockedAfterStart))
    );

    // A Bot's next task: the same picker, every row of a ready agent free.
    host.applies = Applies::NextTask;
    let (rows, choosable) = read(&host, &lent, |picker| {
        let rows = picker.rows();
        let choosable = picker.choosable(&rows);
        (rows, choosable)
    });
    assert!(!rows.is_empty() && choosable.iter().any(|can| *can));
}

#[test]
fn both_controls_draw_inside_their_shells_and_declare_their_buttons() {
    let (mut host, mut lent) = (Host::new(), Lent::new());
    open(&mut host, &mut lent);
    press(
        &mut host,
        &mut lent,
        elements::mark_key(&RailEntry::Agent("claude".to_owned())),
    );
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    run(&mut host, &mut lent, |view| {
        view.draw(&mut scene, Affine::IDENTITY)
    });
    let snapshot = capture.finish();
    let records = &snapshot.records;
    let shell = records
        .iter()
        .position(|record| record.name == "Popover surface")
        .expect("the popover");
    let under = |mut index: usize| loop {
        match records[index].parent {
            Some(parent) if parent == shell => return true,
            Some(parent) => index = parent,
            None => return index == shell,
        }
    };
    assert!(records.len() > 50);
    assert!(
        (0..records.len()).all(under),
        "everything inside the popover"
    );
    let (diagnostics, buttons) = read(&host, &lent, |picker| {
        let (_, _, frame) = picker.frame().unwrap();
        fn count(nodes: &[koru_draw::el::SemNode]) -> usize {
            nodes
                .iter()
                .map(|node| usize::from(node.role == Role::Button) + count(&node.children))
                .sum()
        }
        (frame.diagnostics.len(), count(&frame.semantics()))
    });
    assert_eq!(diagnostics, 0);
    // Favorites, two agents, the +, ↻, three levels, Fast and the window.
    assert_eq!(buttons, 3 + 1 + 1 + 3 + 2);

    // The approval-mode list, from the agent's own mode ids.
    let offers = [
        ModeOffer {
            id: "default",
            name: Some("Manual"),
            description: None,
        },
        ModeOffer {
            id: "auto",
            name: None,
            description: None,
        },
        ModeOffer {
            id: "bypassPermissions",
            name: None,
            description: None,
        },
    ];
    let rows = modes::rows(Lang::En, "agent-claude", &offers);
    assert_eq!(
        modes::default_mode(
            "agent-claude",
            &offers,
            Some("default"),
            NewChatPermission::Manual,
            NewChatPermission::Manual,
        )
        .as_deref(),
        Some("default")
    );
    assert_eq!(
        modes::next_mode(&rows, Some("auto")).map(|row| row.id.as_str()),
        Some("default"),
        "Shift+Tab skips Full access"
    );
    let list = ModeList {
        lang: Lang::En,
        rows: &rows,
        current: Some("auto"),
        focus: Some(rows.len()),
        anchor: Rect::new(780.0, 640.0, 40.0, 28.0),
        viewport: Rect::new(0.0, 0.0, 900.0, 700.0),
        width: 360.0,
        pointer: (0.0, 0.0),
    };
    let second = list.row_rect(1).unwrap();
    assert_eq!(
        list.row_at(second.x + 20.0, second.y + 10.0),
        Some(1),
        "the row the frame draws is the one a press finds"
    );
    let capture = inspect::Frame::begin(
        2,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0),
    );
    let mut scene = Scene::new();
    list.draw(&mut scene, Affine::IDENTITY, &mut lent.engine);
    let snapshot = capture.finish();
    let panel = snapshot
        .records
        .iter()
        .position(|record| record.name == "FloatingPanel")
        .expect("the panel");
    assert!(
        snapshot
            .records
            .iter()
            .enumerate()
            .all(|(index, record)| index == panel || record.parent.is_some()),
        "everything inside the panel"
    );
}
