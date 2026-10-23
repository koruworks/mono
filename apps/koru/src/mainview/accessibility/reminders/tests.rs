// SPDX-License-Identifier: MPL-2.0

//! What must stay true about the reminder surface while a save is in flight,
//! after it lands, and after it fails.
//!
//! Nothing here touches disk, the Store or a network: the reminder controller
//! is seeded with the two things a projection reads — the Store's own rows, and
//! the submissions that have not turned up among them yet — and the tree is a
//! pure function of that. The one thing these cannot establish, that Narrator
//! and NVDA agree with any of it, is the interactive lane's and is written down
//! in `docs/features/accessibility/README.md`.
//!
//! ## Time is pinned, three ways
//!
//! Every test that involves a clock states **the reference instant, the zone
//! and the locale**, because a test that leaves any of the three to the machine
//! passes in one timezone and fails in another, and passes in the morning and
//! fails at 23:50. [`REFERENCE_MS`] is the instant, [`ZURICH`] and [`AUCKLAND`]
//! are the zones, and [`en_us`]/[`de_de`] are the locales. Resolution reads no
//! clock of its own — `ResolutionContext` carries `now_ms` — so *in 20 minutes*
//! is a deterministic assertion here, and would be a flake with a schedule in a
//! live regression, which is why `ACCEPTANCE.md`'s live checks use explicit
//! future dates instead.

use accesskit::{Action, Live, Role};
use chrono::NaiveDate;
use koru_i18n::{Key, Lang, t, time::LocaleFormat};

use koru_core::time::TimeZoneName;
use koru_core::{DocumentId, NodeId};
use koru_editor::reminders::{
    Creation, Draft, Failure, GlobalItem, GlobalItemState, OptimisticCreation, OptimisticState,
    Request, RequestIdentity, RequestToken, Tone,
};
use koru_editor::utils::when::{DateOrder, Language, ReminderDraft, ResolutionContext, WeekStart};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use koru_workspace_host::accessibility::{
    ControlPart, Occurrence, Scope, SemanticNode, ViewSlot, WindowSlot,
};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

use super::*;
use crate::mainview::harness::*;

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(1);

/// **The reference instant every clock-dependent assertion is written
/// against**: 2026-09-02 17:00:00 UTC. In `Europe/Zurich` that is 19:00 CEST;
/// in `Pacific/Auckland` it is 05:00 the following morning. Both are asserted
/// below, which is the point of stating it rather than sampling one.
const REFERENCE_MS: i64 = 1_788_368_400_000;

const ZURICH: &str = "Europe/Zurich";
const AUCKLAND: &str = "Pacific/Auckland";

fn zone(name: &str) -> TimeZoneName {
    TimeZoneName::parse(name).expect("a canonical IANA name")
}

fn en_us() -> LocaleFormat {
    LocaleFormat::for_system(Some("en-US"), Some(koru_i18n::time::HourCycle::H12))
}

fn de_de() -> LocaleFormat {
    LocaleFormat::for_system(Some("de-DE"), Some(koru_i18n::time::HourCycle::H24))
}

/// The whole of what resolution is allowed to read. Nothing here is sampled
/// from the machine.
fn context(zone_name: &str) -> ResolutionContext {
    ResolutionContext {
        now_ms: REFERENCE_MS,
        language: Language::English,
        date_order: DateOrder::MonthFirst,
        current_zone: zone(zone_name),
        selection: None,
        end_of_day: None,
        due_anchor: None,
        system_week_start: WeekStart::Monday,
        week_start_override: None,
    }
}

/// A container over an in-memory workspace, seated in window 1, showing a
/// document whose caret is in a row.
fn seated() -> (MainView, Workspace) {
    let (mut v, w) = on_document();
    v.seat_accessibility(WINDOW);
    // A store-backed tab carries a `DocumentId`; the in-memory harness does
    // not, and `reminder_target` refuses without one. Stamping it is what
    // `Workspace::open_document` would have done.
    let document = DocumentId::new();
    v.active_document = Some(document);
    let index = v.tabs().active();
    if let Some(tab) = v.tabs_mut().get_mut(index) {
        tab.document = Some(document);
    }
    (v, w)
}

/// The node the caret is in — the item a reminder is about.
fn subject(v: &MainView) -> NodeId {
    v.reminder_subject().expect("the caret is in a row")
}

fn target(v: &MainView) -> crate::mainview::reminders::Target {
    v.reminder_target(subject(v)).expect("a reminder target")
}

/// A schedule the Store holds, on the item the caret is in.
fn stored(v: &MainView, id: &str, wall_hour: u32, state: GlobalItemState) -> GlobalItem {
    GlobalItem {
        target: target(v).entity.to_storage(),
        title: String::new(),
        source_location: String::new(),
        reminder_id: id.to_owned(),
        revision: "r1".to_owned(),
        occurrence: None,
        sort_at_ms: None,
        draft: floating(wall_hour),
        state,
        snoozed: false,
        rule_paused: false,
    }
}

/// A submission the Store has not answered yet.
fn in_flight(v: &MainView, id: &str, wall_hour: u32, state: OptimisticState) -> OptimisticCreation {
    OptimisticCreation {
        request: request(NodeId::new()),
        target: target(v).entity.to_storage(),
        reminder_id: id.to_owned(),
        draft: floating(wall_hour),
        state,
    }
}

fn floating(hour: u32) -> Draft {
    Draft::new(ReminderDraft::Floating(
        koru_core::time::FloatingDateTime::new(
            NaiveDate::from_ymd_opt(2026, 9, 3)
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
        ),
    ))
}

fn request(node: NodeId) -> Request {
    Request {
        identity: RequestIdentity {
            token: RequestToken::new(),
            node,
        },
        phrase: String::new(),
        due_anchor: None,
    }
}

/// Open the creation popover over the caret's row, with `phrase` typed into it
/// and resolved against `zone_name`'s clock.
///
/// The `Creation` is built the way the controller builds one — `loading`, then
/// `resolve` against an explicit context — rather than by reaching for a Store.
fn open_creation(v: &mut MainView, phrase: &str, zone_name: &str, locale: LocaleFormat) {
    let node = subject(v);
    let mut creation = Creation::loading(Request {
        identity: RequestIdentity {
            token: RequestToken::new(),
            node,
        },
        phrase: phrase.to_owned(),
        due_anchor: None,
    });
    creation.resolve(&context(zone_name), Tone::System);
    seed_surface(v, Some(&creation), zone_name, locale);
}

/// Hand the surface a creation (or none) and let maintenance build its
/// geometry, the way `sync_reminder_creation_surface` does every pass.
fn seed_surface(
    v: &mut MainView,
    creation: Option<&Creation>,
    zone_name: &str,
    locale: LocaleFormat,
) {
    v.reminder_environment.resolution = context(zone_name);
    v.reminder_environment.locale = locale.clone();
    v.reminder_creation
        .update_context(context(zone_name), locale, Tone::System);
    let anchor = koru_draw::geometry::Rect::new(100.0, 200.0, 1.0, 1.0);
    v.reminder_creation
        .sync_creation(&mut v.engine, creation, Some(anchor));
    let viewport = v.viewport;
    v.reminder_creation
        .maintain_projection(&mut v.engine, viewport, Lang::En, creation, None, &[]);
}

/// How a locator spells a reminder id: `Uuid::simple`, so the hyphens a
/// `ReminderId` is written with are not in it.
fn locator_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .expect("a uuid")
        .simple()
        .to_string()
}

/// The tree this window would publish right now, flattened for searching.
fn nodes(v: &MainView) -> Vec<SemanticNode> {
    let mut out = Vec::new();
    for node in v.semantics(VIEW, Scope::Whole) {
        flatten(&node, &mut out);
    }
    out
}

fn flatten(node: &SemanticNode, out: &mut Vec<SemanticNode>) {
    out.push(node.clone());
    for child in &node.children {
        flatten(child, out);
    }
}

/// A node whose locator ends in `suffix`. A locator carries a window, a view
/// and an object, none of which a test should have to spell out to find one
/// control.
fn ending(v: &MainView, suffix: &str) -> Option<SemanticNode> {
    nodes(v)
        .into_iter()
        .find(|node| node.occurrence.locator().ends_with(suffix))
}

fn label_ending(v: &MainView, suffix: &str) -> String {
    ending(v, suffix)
        .unwrap_or_else(|| panic!("no node whose locator ends in `{suffix}`"))
        .label
        .unwrap_or_default()
}

// ------------------------------------------------------------------ the panel

#[test]
fn the_popover_publishes_every_control_it_draws_where_it_draws_it() {
    let (mut v, _w) = seated();
    open_creation(&mut v, "", ZURICH, en_us());
    assert!(
        v.reminder_creation.is_creation(),
        "the fixture did not open the creation popover"
    );

    let published = nodes(&v);
    let panel = published
        .iter()
        .find(|node| node.role == Role::Dialog)
        .expect("the popover publishes a dialog");
    assert!(panel.modal, "a popover that takes the keyboard is modal");
    assert_eq!(panel.bounds, v.reminder_creation.panel());

    // Every control the surface offers, at the rectangle drawing and
    // hit-testing already share. A projection that computed its own would be a
    // second copy of the layout, which is what ADR-338 forbids.
    for (hit, rect, enabled) in v.reminder_creation.controls() {
        let label = v.reminder_creation.hit_label(Lang::En, hit);
        let node = published
            .iter()
            .find(|node| {
                node.bounds == Some(rect)
                    && node
                        .label
                        .as_deref()
                        .is_some_and(|drawn| drawn.starts_with(label.as_str()))
            })
            .unwrap_or_else(|| panic!("{hit:?} (`{label}`) is not in the tree at {rect:?}"));
        assert_eq!(
            node.disabled, !enabled,
            "{hit:?} disagrees with the keyboard about being usable"
        );
    }

    // The chrome underneath is gone: a blocker publishes alone.
    assert!(
        !published
            .iter()
            .any(|node| node.occurrence.locator().contains("s:tabs")),
        "the tab strip is still reachable under a modal popover"
    );
}

#[test]
fn the_phrase_field_carries_what_is_on_the_screen_and_offers_no_write() {
    let (mut v, _w) = seated();
    open_creation(&mut v, "  in 20 minutes", ZURICH, en_us());
    let field = ending(&v, "#phrase").expect("the phrase field");
    assert_eq!(field.role, Role::TextInput);
    // `raw_text`, not the trimmed value the form acts on: telling somebody
    // their leading spaces are not there while the caret sits after them is the
    // small lie that costs an hour.
    assert_eq!(field.value.as_deref(), Some("  in 20 minutes"));
    assert_eq!(
        field.label.as_deref(),
        Some(t(Lang::En, Key::ReminderWhenPlaceholder))
    );
    // No `SetValue`: nothing performs it. The phrase is parsed a keystroke at a
    // time, and an action a client can see that does nothing is worse than one
    // that is absent.
    assert!(!field.actions.contains(&Action::SetValue));
    assert!(!field.actions.contains(&Action::Click));
}

// -------------------------------------------------------------------- the time

#[test]
fn a_relative_phrase_is_read_in_the_pinned_locale_zone_and_reference_instant() {
    let (mut v, _w) = seated();
    // 2026-09-02 17:00 UTC is 19:00 in Zurich, so twenty minutes on is 19:20 —
    // 7:20 PM to an en-US reader on a twelve-hour clock.
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    assert_eq!(
        label_ending(&v, "#message"),
        t(Lang::En, Key::ReminderScheduledFor).replace("{}", "09/02/2026 7:20 PM"),
    );
}

#[test]
fn the_same_phrase_in_another_zone_and_locale_resolves_and_reads_differently() {
    let (mut v, _w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let zurich = label_ending(&v, "#message");

    let (mut v, _w) = seated();
    // The same instant, read where the day has already turned over: 17:00 UTC
    // on the 2nd is 05:00 on the 3rd in Auckland, and twenty minutes on is
    // 05:20 — written `03.09.2026 05:20` by a German reader on a 24-hour clock.
    open_creation(&mut v, "in 20 minutes", AUCKLAND, de_de());
    let auckland = label_ending(&v, "#message");

    assert_ne!(
        zurich, auckland,
        "a rendered time that does not move with the reader is not localized"
    );
    assert!(
        auckland.contains("03.09.2026 05:20"),
        "expected the Auckland wall clock, got `{auckland}`"
    );
}

#[test]
fn changing_the_locale_moves_every_word_and_no_identity() {
    let (mut v, _w) = seated();
    // One opening, read twice. A second *opening* would legitimately hold a
    // different instance number, which is the property
    // `a_request_queued_against_one_opening_cannot_land_in_the_next` is about;
    // what this asks is the other one — that a rendering is never an identity.
    let node = subject(&v);
    let mut creation = Creation::loading(Request {
        identity: RequestIdentity {
            token: RequestToken::new(),
            node,
        },
        phrase: "in 20 minutes".to_owned(),
        due_anchor: None,
    });
    creation.resolve(&context(ZURICH), Tone::System);

    seed_surface(&mut v, Some(&creation), ZURICH, en_us());
    let instance = v.reminder_creation.instance();
    let english = label_ending(&v, "#message");
    let locators: Vec<String> = nodes(&v)
        .iter()
        .map(|node| node.occurrence.locator())
        .collect();

    seed_surface(&mut v, Some(&creation), ZURICH, de_de());
    assert_eq!(
        v.reminder_creation.instance(),
        instance,
        "the fixture reopened the popover instead of redrawing it"
    );
    let german = label_ending(&v, "#message");
    // **A floating reminder prints its own reading, unconverted** — 19:20 is
    // 19:20 wherever it is read (docs/architecture/TIME.md) — so what moves here is the
    // *clock convention*, not the moment.
    assert!(english.contains("7:20 PM"), "got `{english}`");
    assert!(german.contains("19:20"), "got `{german}`");
    assert_eq!(
        locators,
        nodes(&v)
            .iter()
            .map(|node| node.occurrence.locator())
            .collect::<Vec<_>>(),
        "a locator moved when the way a time is written did"
    );
}

// -------------------------------------------------------------- what is stored

#[test]
fn a_saved_schedule_is_read_from_the_store_and_a_pill_that_is_not_says_so() {
    let (mut v, _w) = seated();
    let entity = target(&v);
    // One reminder the Store has answered for, and one that has only been
    // submitted. A sighted person sees the same pill for both.
    let confirmed = stored(
        &v,
        "11111111-1111-4111-8111-111111111111",
        9,
        GlobalItemState::ComingUp,
    );
    let pending = in_flight(
        &v,
        "22222222-2222-4222-8222-222222222222",
        18,
        OptimisticState::Pending,
    );
    v.reminders
        .seed_for_test(entity, vec![confirmed], vec![pending]);
    seed_surface(&mut v, None, ZURICH, en_us());

    let saved = label_ending(
        &v,
        &format!(
            "{}#schedule",
            locator_id("11111111-1111-4111-8111-111111111111")
        ),
    );
    let unsaved = label_ending(
        &v,
        &format!(
            "{}#schedule",
            locator_id("22222222-2222-4222-8222-222222222222")
        ),
    );
    assert_eq!(
        saved,
        t(Lang::En, Key::ReminderScheduledFor).replace("{}", "09/03/2026 9:00 AM"),
        "a confirmed schedule reads as the Store holds it, and says nothing else"
    );
    assert!(
        unsaved.contains(t(Lang::En, Key::A11yReminderUnconfirmed)),
        "a submission the Store has not answered must not read as a saved schedule: `{unsaved}`"
    );
}

#[test]
fn a_confirmed_row_replaces_the_submission_it_answers_rather_than_doubling_it() {
    let (mut v, _w) = seated();
    let entity = target(&v);
    let id = "33333333-3333-4333-8333-333333333333";
    // The same client-minted id in both halves — which is what a save that has
    // landed looks like for the moment before the optimistic record is retired.
    let confirmed = stored(&v, id, 9, GlobalItemState::ComingUp);
    let pending = in_flight(&v, id, 18, OptimisticState::Succeeded);
    v.reminders
        .seed_for_test(entity, vec![confirmed], vec![pending]);
    seed_surface(&mut v, None, ZURICH, en_us());

    let published: Vec<_> = nodes(&v)
        .into_iter()
        .filter(|node| node.occurrence.locator().ends_with("#schedule"))
        .collect();
    assert_eq!(published.len(), 1, "one reminder, published once");
    let label = published[0].label.clone().unwrap_or_default();
    assert!(
        !label.contains(t(Lang::En, Key::A11yReminderUnconfirmed)),
        "the Store has answered for this id; the pill's own guess must not win"
    );
    assert!(
        label.contains("9:00 AM"),
        "the authoritative schedule must win over the submitted one: `{label}`"
    );
}

#[test]
fn a_revisited_reminder_reads_the_stored_schedule_and_not_the_phrase_that_made_it() {
    let (mut v, w) = seated();
    let entity = target(&v);
    let node = subject(&v);
    let id = "88888888-8888-4888-8888-888888888888";
    // What the Store came back with. Deliberately **not** what was typed: a
    // person asked for one thing, storage holds another, and the whole of this
    // plan is that reopening reads the second.
    v.reminders.seed_for_test(
        entity,
        vec![stored(&v, id, 9, GlobalItemState::ComingUp)],
        Vec::new(),
    );
    // The pill the document draws, so the reopened editor has an anchor —
    // exactly what the shell does after a save.
    v.push_reminder_chip();
    v.reminder_environment.resolution = context(ZURICH);
    v.reminder_environment.locale = en_us();

    assert!(
        v.open_reminders_here(&w.ws),
        "the shortcut found nothing to reopen"
    );
    assert_eq!(v.reminder_creation.node(), Some(node));
    let creation = None;
    seed_surface_keeping_open(&mut v, creation);

    // **The confirmed schedule, in the reopened panel.** Not the phrase, which
    // is gone; not the pill, which is a rendering; the row the Store answered
    // with, resolved back into an editable draft by `open_reminder_edit`.
    let message = label_ending(&v, "#message");
    assert!(
        message.contains("09/03/2026 9:00 AM"),
        "the reopened popover does not read the stored schedule: `{message}`"
    );
}

/// Redraw the popover that is already open, without handing it a controller
/// creation — which is what a maintenance pass does for a surface-owned edit.
fn seed_surface_keeping_open(v: &mut MainView, creation: Option<&Creation>) {
    let viewport = v.viewport;
    v.reminder_creation
        .maintain_projection(&mut v.engine, viewport, Lang::En, creation, None, &[]);
}

#[test]
fn a_saved_row_is_addressed_by_its_reminder_id_rather_than_its_position() {
    let (mut v, mut w) = seated();
    let first = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let second = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let anchor = koru_draw::geometry::Rect::new(100.0, 200.0, 1.0, 1.0);
    v.reminder_creation.open_management(subject(&v), anchor);
    v.reminder_creation
        .update_context(context(ZURICH), en_us(), Tone::System);

    let mut management = koru_editor::reminders::Management::loading(String::new());
    management.replace(vec![item(first, 9), item(second, 18)], Vec::new());
    let viewport = v.viewport;
    v.reminder_creation.maintain_projection(
        &mut v.engine,
        viewport,
        Lang::En,
        None,
        Some(&management),
        &[],
    );

    // Two rows, and the *second* one's Edit control.
    let edit = ending(&v, &format!("{}#edit", locator_id(second)))
        .expect("the second row's Edit control")
        .occurrence;

    // The rows reorder — which is what an asynchronous refresh does — and the
    // held id must still name the same reminder rather than the one that moved
    // into its place.
    management.replace(vec![item(second, 18), item(first, 9)], Vec::new());
    v.reminder_creation.maintain_projection(
        &mut v.engine,
        viewport,
        Lang::En,
        None,
        Some(&management),
        &[],
    );
    assert!(super::action(&mut v, &mut w.ws, edit, Action::Focus));
    assert_eq!(
        v.reminder_creation.focus(),
        Some(crate::mainview::reminder_creation::Hit::Row(
            0,
            crate::mainview::reminder_creation::RowAction::Edit
        )),
        "the request landed on a position rather than on its reminder"
    );

    // A reminder that has left the list resolves to nothing at all.
    management.replace(vec![item(first, 9)], Vec::new());
    v.reminder_creation.maintain_projection(
        &mut v.engine,
        viewport,
        Lang::En,
        None,
        Some(&management),
        &[],
    );
    assert!(!super::action(&mut v, &mut w.ws, edit, Action::Focus));
}

fn item(id: &str, wall_hour: u32) -> koru_editor::reminders::Item {
    koru_editor::reminders::Item {
        id: id.to_owned(),
        revision: "r1".to_owned(),
        occurrence: None,
        draft: floating(wall_hour),
        state: koru_editor::reminders::ItemState::Active,
    }
}

#[test]
fn a_reminder_the_store_has_not_confirmed_cannot_be_revisited() {
    let (mut v, w) = seated();
    let entity = target(&v);
    let pending = in_flight(
        &v,
        "44444444-4444-4444-8444-444444444444",
        18,
        OptimisticState::Pending,
    );
    v.reminders.seed_for_test(entity, Vec::new(), vec![pending]);

    // The keyboard route reads the authoritative rows and finds none, so it
    // does not reopen a schedule nothing has stored. (It cannot get as far as a
    // populated popover here — there is no client to load settings from — but
    // it must not have tried to edit one.)
    v.open_reminders_here(&w.ws);
    assert!(
        v.reminder_creation.management().is_none(),
        "an optimistic pill was reopened as though it were saved"
    );
}

// -------------------------------------------------------- validation and news

#[test]
fn an_unresolved_phrase_is_said_when_asked_and_never_announced() {
    let (mut v, _w) = seated();
    // A phrase that resolves to nothing leaves `Set reminder` unusable — which
    // is the validation a person needs, and which no live region should read
    // aloud on every keystroke.
    open_creation(&mut v, "qqqqqq", ZURICH, en_us());
    let set = ending(&v, "#set").expect("the Set reminder control");
    assert!(
        set.disabled,
        "Set reminder is offered for a phrase that resolved to nothing"
    );
    let message = ending(&v, "#message").expect("the status line");
    assert_eq!(
        message.live, None,
        "the status moves with every keystroke and must never be a live region"
    );
    assert!(
        ending(&v, "#news").is_none(),
        "typing something unresolvable is not news"
    );
}

#[test]
fn a_failed_save_is_announced_once_and_the_same_failure_again_says_nothing_new() {
    let (mut v, _w) = seated();
    let entity = target(&v);
    let failed = in_flight(
        &v,
        "55555555-5555-4555-8555-555555555555",
        18,
        OptimisticState::Failed {
            reason: Failure::Rejected,
        },
    );

    // The first observation *seeds*: a client attaching into a save in flight
    // is told by the status when it asks, not as though it had just happened.
    v.reminder_news.observe(&[OptimisticState::Pending]);
    assert!(v.reminder_news.news.is_none());

    v.reminder_news.observe(&[failed.state]);
    assert_eq!(v.reminder_news.news, Some(Key::ReminderErrorRejected));
    let after_first = v.reminder_news.observations();

    // Observed again, unchanged. A live region speaks on a *name* change, so a
    // republished identical label raises nothing — and the record must not
    // churn either.
    v.reminder_news.observe(&[failed.state]);
    assert_eq!(v.reminder_news.news, Some(Key::ReminderErrorRejected));
    assert_eq!(v.reminder_news.observations(), after_first + 1);

    v.reminders.seed_for_test(entity, Vec::new(), vec![failed]);
    seed_surface(&mut v, None, ZURICH, en_us());
    let news = ending(&v, "#news").expect("the announcement");
    assert_eq!(news.role, Role::Alert);
    assert_eq!(
        news.live,
        Some(Live::Assertive),
        "a refusal is what somebody is waiting on"
    );
    assert_eq!(
        news.label.as_deref(),
        Some(t(Lang::En, Key::ReminderErrorRejected))
    );
    // **On the leaf, never on the group.** `live` is inherited, so a live
    // container would make every schedule beside it speak.
    let group = ending(&v, "s:remind#root").expect("the group");
    assert_eq!(group.live, None);
}

#[test]
fn a_save_that_lands_is_announced_politely_and_only_on_the_transition() {
    let (mut v, _w) = seated();
    v.reminder_news.observe(&[OptimisticState::Pending]);
    v.reminder_news.observe(&[OptimisticState::Pending]);
    assert!(
        v.reminder_news.news.is_none(),
        "waiting is not news; it is the ordinary half second"
    );
    v.reminder_news.observe(&[OptimisticState::Succeeded]);
    assert_eq!(v.reminder_news.news, Some(Key::A11yReminderNewsSaved));

    let entity = target(&v);
    v.reminders.seed_for_test(entity, Vec::new(), Vec::new());
    seed_surface(&mut v, None, ZURICH, en_us());
    let news = ending(&v, "#news").expect("the announcement");
    assert_eq!(news.live, Some(Live::Polite));
}

#[test]
fn submitting_a_second_reminder_while_one_landed_says_nothing() {
    let (mut v, _w) = seated();
    v.reminder_news.observe(&[OptimisticState::Pending]);
    v.reminder_news.observe(&[OptimisticState::Succeeded]);
    assert_eq!(v.reminder_news.news, Some(Key::A11yReminderNewsSaved));
    // A second submission moves the situation back to *in flight*. That is a
    // transition, and it is still not news: "Saving reminder…" after every
    // reminder is the chatter this surface exists to avoid.
    v.reminder_news.observe(&[OptimisticState::Pending]);
    assert_eq!(
        v.reminder_news.news,
        Some(Key::A11yReminderNewsSaved),
        "arriving at *in flight* was announced"
    );
    // …and the transition was still recorded, so the next landing is news
    // again rather than being swallowed as "no change".
    v.reminder_news.observe(&[OptimisticState::Succeeded]);
    assert_eq!(v.reminder_news.news, Some(Key::A11yReminderNewsSaved));
}

/// A refused submission left standing as a recovery pill, with **no popover
/// open** — which is the only way this pill is ever seen, and the state that
/// makes it interesting: nothing in the window owns the keyboard at that
/// moment.
fn stand_a_recovery_pill(v: &mut MainView) -> RequestToken {
    let entity = target(v);
    let failed = in_flight(
        v,
        "99999999-9999-4999-8999-999999999999",
        18,
        OptimisticState::Failed {
            reason: Failure::Rejected,
        },
    );
    let token = failed.request.identity.token;
    v.reminders
        .seed_for_test(entity, Vec::new(), vec![failed.clone()]);
    let viewport = v.viewport;
    v.reminder_creation
        .update_context(context(ZURICH), en_us(), Tone::System);
    v.reminder_creation.maintain_projection(
        &mut v.engine,
        viewport,
        Lang::En,
        None,
        None,
        &[failed],
    );
    assert!(
        !v.reminder_creation.is_open(),
        "the recovery pill stands with no popover, which is the case that matters"
    );
    token
}

#[test]
fn a_failed_saves_recovery_is_reachable_from_a_client_with_no_popover_open() {
    let (mut v, mut w) = seated();
    let token = stand_a_recovery_pill(&mut v);

    // The pill is a real control at a real rectangle — it is drawn, unlike the
    // status sentences — and its three actions are scoped by the request token
    // so three standing failures each recover their own.
    let retry = ending(
        &v,
        &format!("{}#pill-retry", locator_id(&token.as_uuid().to_string())),
    )
    .expect("the pill's Try again control");
    assert_eq!(retry.role, Role::Button);
    assert!(
        retry.bounds.is_some(),
        "a control that is drawn publishes where"
    );
    assert!(retry.actions.contains(&Action::Click));

    // And invoking it reaches the same place pressing it reaches, and the same
    // place `F6` and Enter now reach — three doors, one call.
    assert!(super::action(
        &mut v,
        &mut w.ws,
        retry.occurrence,
        Action::Click
    ));

    // A token that never failed performs nothing, however well-formed the
    // request looks.
    let stale = Occurrence::model(
        WINDOW,
        VIEW,
        RequestToken::new().as_uuid(),
        ControlPart::new("pill-retry"),
    );
    assert!(!super::action(&mut v, &mut w.ws, stale, Action::Click));
}

/// **`F6` reaches the recovery pill, and the keyboard's focus is the published
/// one.**
///
/// This is Q-R1. The pill floats outside every panel with no popover open, so
/// `blockers()` answers `None` and nothing in the window owns the keyboard —
/// which is why it was pointer-and-screen-reader-only. `F6` already meant *move
/// focus into window chrome* and already had the reminder bell as its one stop;
/// the pill joins that rotation rather than getting a chord of its own.
///
/// The two halves asserted together on purpose: the ring is painted from
/// `Surface::focus` and `accessibility_focus` is a function *over that same
/// field* (§7.1), so a test that checked only one could pass while a screen
/// reader and a sighted person disagreed about where the keyboard was.
#[test]
fn f6_lands_on_a_standing_recovery_pill_and_the_published_focus_follows() {
    let (mut v, mut w) = seated();
    let token = stand_a_recovery_pill(&mut v);
    let pill_retry = format!("{}#pill-retry", locator_id(&token.as_uuid().to_string()));

    let before = v.accessibility_focus(VIEW).expect("focus somewhere real");
    assert!(
        !before.locator().ends_with(&pill_retry),
        "nothing has reached the pill yet"
    );

    let f6 = |v: &mut MainView, w: &mut Workspace| {
        View::key(
            v,
            &mut w.ws,
            &WKey::Named(NamedKey::F6),
            None,
            ModifiersState::empty(),
        )
    };
    assert!(f6(&mut v, &mut w), "F6 moved focus into chrome");
    assert!(v.reminder_creation.pill_focused());
    assert_eq!(
        v.accessibility_focus(VIEW).map(|at| at.locator()),
        ending(&v, &pill_retry).map(|node| node.occurrence.locator()),
        "the published focus is the control the ring is on"
    );

    // Tab inside the pill reaches Edit and Dismiss — the same three actions a
    // client is offered, so neither route is richer than the other.
    assert!(View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    ));
    let pill_edit = format!("{}#pill-edit", locator_id(&token.as_uuid().to_string()));
    assert_eq!(
        v.accessibility_focus(VIEW).map(|at| at.locator()),
        ending(&v, &pill_edit).map(|node| node.occurrence.locator()),
    );

    // `F6` again moves on to the bell, which is the stop it always had — and
    // the pill lets go, so two rings are never painted at once.
    assert!(f6(&mut v, &mut w));
    assert!(!v.reminder_creation.pill_focused());
    assert!(v.reminder_shell.bell_is_focused());

    // …then on to the sidebar's own stops — the lane, and the apps row and
    // floor while the pane is open — and round again to the pill, because it
    // is still standing (`chrome_focus.rs`).
    assert!(f6(&mut v, &mut w));
    assert!(!v.reminder_shell.bell_is_focused(), "the bell let go");
    assert!(v.chrome_focus.is_some(), "the lane took the keyboard");
    let mut rounds = 0;
    while !v.reminder_creation.pill_focused() {
        assert!(f6(&mut v, &mut w));
        rounds += 1;
        assert!(rounds <= 4, "the rotation must come round to the pill");
    }
    assert!(v.chrome_focus.is_none(), "and the lane let go");
}

/// **Escape leaves the pill and focus lands somewhere valid**, rather than on a
/// control nothing is drawn for. Nothing is recovered by leaving.
#[test]
fn escape_gives_the_keyboard_back_from_a_recovery_pill() {
    let (mut v, mut w) = seated();
    stand_a_recovery_pill(&mut v);
    View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::F6),
        None,
        ModifiersState::empty(),
    );
    assert!(v.reminder_creation.pill_focused());

    assert!(View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    ));
    assert!(!v.reminder_creation.pill_focused());
    let focus = v.accessibility_focus(VIEW).expect("focus somewhere real");
    assert!(
        !focus.locator().contains("#pill-"),
        "focus was left on a pill nobody is standing on: {}",
        focus.locator()
    );
    // The pill is still there — leaving it is not dismissing it.
    assert!(v.reminder_creation.has_failed_pill());
}

/// **With nothing failed, `F6` is exactly what it always was.** A stop that is
/// not standing is not a dead press: the key goes straight to the bell.
#[test]
fn f6_still_lands_on_the_bell_when_no_save_has_failed() {
    let (mut v, mut w) = seated();
    assert!(!v.reminder_creation.has_failed_pill());
    assert!(View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::F6),
        None,
        ModifiersState::empty(),
    ));
    assert!(v.reminder_shell.bell_is_focused());
    assert!(!v.reminder_creation.pill_focused());
}

/// **A pill that has gone gives the keyboard back on its own.** Try again, Edit
/// and Dismiss all end the pill they sit on, so the published focus must not be
/// left naming a control that has left the tree — `Counters::unresolved_focus`
/// is what that would show up as.
#[test]
fn recovering_from_the_keyboard_does_not_leave_focus_on_a_pill_that_has_gone() {
    let (mut v, mut w) = seated();
    let token = stand_a_recovery_pill(&mut v);
    View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::F6),
        None,
        ModifiersState::empty(),
    );
    assert!(v.reminder_creation.pill_focused());

    // Enter on *Try again* performs the same request a press performs.
    assert!(View::key(
        &mut v,
        &mut w.ws,
        &WKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    ));

    // The controller took the failure back; the next projection pass finds no
    // pill and the focus goes with the rectangle it named.
    let viewport = v.viewport;
    v.reminder_creation
        .maintain_projection(&mut v.engine, viewport, Lang::En, None, None, &[]);
    assert!(!v.reminder_creation.has_failed_pill());
    assert!(!v.reminder_creation.pill_focused());
    let focus = v.accessibility_focus(VIEW).expect("focus somewhere real");
    let gone = format!("{}#pill-retry", locator_id(&token.as_uuid().to_string()));
    assert!(!focus.locator().ends_with(&gone));
    assert!(
        nodes(&v).iter().any(|node| node.occurrence == focus),
        "the published focus names a node that is in the tree"
    );
}

#[test]
fn no_two_controls_of_the_reminder_surface_share_an_identity() {
    let (mut v, _w) = seated();
    // The one pair that could collide by construction: the red "not valid" line
    // under the structured fields and the panel's own retry control, which can
    // stand at the same time.
    open_creation(&mut v, "qqqqqq", ZURICH, en_us());
    let mut seen = std::collections::BTreeSet::new();
    for node in nodes(&v) {
        assert!(
            seen.insert(node.occurrence),
            "{} is published twice",
            node.occurrence.locator()
        );
    }
}

#[test]
fn nothing_submitted_leaves_the_last_thing_said_standing() {
    let (mut v, _w) = seated();
    v.reminder_news.observe(&[OptimisticState::Pending]);
    v.reminder_news.observe(&[OptimisticState::Succeeded]);
    assert_eq!(v.reminder_news.news, Some(Key::A11yReminderNewsSaved));
    // The record is retired once the projection catches up. That is bookkeeping
    // rather than an event, and announcing "saved" again for it would be Koru
    // talking about itself.
    let before = v.reminder_news.news;
    v.reminder_news.observe(&[]);
    assert_eq!(v.reminder_news.news, before);
}

// -------------------------------------------------- cancelling and retirement

#[test]
fn cancelling_retires_the_popover_and_gives_the_keyboard_back() {
    let (mut v, mut w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let set = ending(&v, "#set")
        .expect("the Set reminder control")
        .occurrence;
    let inside = v
        .accessibility_focus(VIEW)
        .expect("focus inside the popover");
    assert!(inside.locator().contains("remind:"));

    // Escape, which is what the popover's own key handler does with it.
    let handled = v.reminder_creation_key(
        &w.ws,
        &WKey::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert!(handled);
    assert!(!v.reminder_creation.is_open());

    // The controls have left the tree, so a request naming one resolves to
    // nothing — which is the refusal §5 asks for rather than a coincidence of
    // the surface's own bookkeeping.
    assert!(
        !nodes(&v).iter().any(|node| node.occurrence == set),
        "a closed popover still publishes its controls"
    );
    assert!(!super::action(&mut v, &mut w.ws, set, Action::Click));

    // And the keyboard is somewhere real again.
    let focus = v.accessibility_focus(VIEW).expect("focus somewhere real");
    assert!(
        !focus.locator().contains("remind:"),
        "focus was left inside a popover that has closed: {}",
        focus.locator()
    );
    // Nothing was said about it: a person who pressed Escape knows.
    assert!(ending(&v, "#news").is_none());
}

#[test]
fn a_request_queued_against_one_opening_cannot_land_in_the_next() {
    let (mut v, mut w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let first = ending(&v, "#cancel")
        .expect("the Cancel control")
        .occurrence;
    let first_instance = v.reminder_creation.instance();

    v.reminder_creation.close_context();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    assert_ne!(
        v.reminder_creation.instance(),
        first_instance,
        "a second opening reused its predecessor's number"
    );
    let second = ending(&v, "#cancel")
        .expect("the Cancel control")
        .occurrence;
    assert_ne!(first, second);
    // The stale one performs nothing, even though a control with the same
    // *part* is on screen.
    assert!(super::is_popover_target(&v, second));
    assert!(!super::is_popover_target(&v, first));
    assert!(!super::action(&mut v, &mut w.ws, first, Action::Click));
    assert!(v.reminder_creation.is_open());
}

// ------------------------------------------------------- actions and the keys

#[test]
fn an_invoke_reaches_the_same_place_the_key_does() {
    let (mut v, mut w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let cancel = ending(&v, "#cancel")
        .expect("the Cancel control")
        .occurrence;
    assert!(super::action(&mut v, &mut w.ws, cancel, Action::Click));
    assert!(
        !v.reminder_creation.is_open(),
        "invoking Cancel did not do what pressing it does"
    );
}

#[test]
fn a_focus_request_moves_the_ring_and_nothing_else() {
    let (mut v, mut w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let cancel = ending(&v, "#cancel")
        .expect("the Cancel control")
        .occurrence;
    assert!(super::action(&mut v, &mut w.ws, cancel, Action::Focus));
    assert!(
        v.reminder_creation.is_open(),
        "a focus request performed the control"
    );
    // Visual focus and accessible focus are one state (§7.1): what a client is
    // told is read out of the field the ring is painted from.
    assert_eq!(v.accessibility_focus(VIEW), Some(cancel));
}

#[test]
fn a_control_the_keyboard_cannot_reach_is_refused_however_it_is_advertised() {
    let (mut v, mut w) = seated();
    // An empty phrase leaves `Set reminder` disabled and out of the Tab order.
    open_creation(&mut v, "", ZURICH, en_us());
    let set = ending(&v, "#set").expect("the Set reminder control");
    assert!(set.disabled);
    // An advertised action set is a hint rather than a filter, so the refusal
    // has to live in the owner. `activate` is never reached.
    assert!(!super::action(
        &mut v,
        &mut w.ws,
        set.occurrence,
        Action::Click
    ));
    assert!(v.reminder_creation.is_open());
}

// --------------------------------------------------------------- the shortcut

#[test]
fn the_shortcut_is_the_registrys_and_asks_for_the_reminders_here() {
    use crate::mainview::keys::{Chord, resolve};
    use koru_keys::{Layer, On, ShortcutId, registry};
    let entry = registry::of_layer(Layer::Container)
        .find(|entry| entry.id == ShortcutId::Reminders)
        .expect("the registry records the chord");
    let key = match entry.chord.on {
        On::Letter(c) => WKey::Character(c.to_string().into()),
        On::Named(n) => WKey::Named(n),
    };
    let text = match entry.chord.on {
        On::Letter(c) => Some(c.to_string()),
        On::Named(_) => None,
    };
    assert_eq!(
        resolve(&key, text.as_deref(), entry.chord.mods),
        Some(Chord::Reminders),
        "the registry and the container disagree about the reminders chord"
    );
}

#[test]
fn the_shortcut_answers_nothing_where_there_is_no_item() {
    let (mut v, w) = crate::mainview::harness::container();
    // A welcome tab has no caret and no item, so the chord does nothing rather
    // than reaching for a neighbour.
    assert!(!v.open_reminders_here(&w.ws));
}

// -------------------------------------------------------------- cost and words

#[test]
fn the_fingerprint_moves_when_a_stored_schedule_does_and_not_when_it_does_not() {
    let (mut v, _w) = seated();
    let entity = target(&v);
    let id = "66666666-6666-4666-8666-666666666666";
    let nine = stored(&v, id, 9, GlobalItemState::ComingUp);
    v.reminders
        .seed_for_test(entity.clone(), vec![nine], Vec::new());
    seed_surface(&mut v, None, ZURICH, en_us());
    let before = v.semantics_revision();
    assert_eq!(
        before,
        v.semantics_revision(),
        "asking the same question twice changed the answer"
    );

    let six = stored(&v, id, 18, GlobalItemState::ComingUp);
    v.reminders.seed_for_test(entity, vec![six], Vec::new());
    seed_surface(&mut v, None, ZURICH, en_us());
    assert_ne!(
        before,
        v.semantics_revision(),
        "the same reminder rescheduled published the same fingerprint"
    );
}

#[test]
fn every_control_is_named_in_all_eight_languages_and_no_locator_moves() {
    let (mut v, _w) = seated();
    open_creation(&mut v, "in 20 minutes", ZURICH, en_us());
    let hits: Vec<_> = v
        .reminder_creation
        .controls()
        .map(|(hit, ..)| hit)
        .collect();
    for lang in Lang::ALL {
        for hit in &hits {
            let label = v.reminder_creation.hit_label(lang, *hit);
            assert!(
                !label.is_empty(),
                "{hit:?} has no name in {lang:?} — a missing translation is a compile error, \
                 so an empty one is a row that was written blank"
            );
        }
    }
    // …and no identity is derived from any of them. A locator is ASCII, and
    // carries neither a translated word nor anything a person wrote.
    for node in nodes(&v) {
        let locator = node.occurrence.locator();
        assert!(
            locator.is_ascii(),
            "{locator} is not ASCII, so something a person wrote reached it"
        );
        for hit in &hits {
            let label = v.reminder_creation.hit_label(Lang::En, *hit);
            assert!(
                !locator.contains(&label),
                "{locator} carries the label `{label}`"
            );
        }
    }
}

#[test]
fn a_reminder_phrase_never_reaches_a_log() {
    // §7.4: a reminder's phrase and its subject's title are the person's own
    // content. The one diagnostic line this module writes carries a keyword and
    // a count, and it is the only thing in it that prints at all.
    let source = include_str!("../reminders.rs");
    let printing: Vec<&str> = source
        .lines()
        .filter(|line| line.contains("eprintln!"))
        .collect();
    assert_eq!(
        printing.len(),
        1,
        "a second diagnostic appeared; check it carries no content: {printing:?}"
    );
    assert!(
        source.contains("reminder from={} to={} observations={}"),
        "the diagnostic line changed shape"
    );
}

fn project_typed_reminder(v: &mut MainView) {
    v.reminder_creation
        .maintain_projection(&mut v.engine, v.viewport, Lang::En, None, None, &[]);
}
fn click_reminder_field(v: &mut MainView, hit: Hit) {
    let rect = v
        .reminder_creation
        .controls()
        .into_iter()
        .find(|(candidate, _, enabled)| *candidate == hit && *enabled)
        .unwrap()
        .1;
    assert!(
        v.reminder_creation
            .mouse_down(&mut v.engine, rect.x + rect.w / 2.0, rect.y + rect.h / 2.0,)
            .handled
    );
    v.reminder_creation.mouse_up();
    project_typed_reminder(v);
}
fn type_reminder_character(v: &mut MainView, text: &str, mods: ModifiersState) {
    assert!(
        v.reminder_creation
            .key(
                &mut v.engine,
                &WKey::Character(text.into()),
                Some(text),
                mods,
            )
            .handled
    );
    project_typed_reminder(v);
}

#[test]
fn field_edits_refresh_accessibility_values_without_a_focus_or_validation_change() {
    for (hit, suffix) in [
        (Hit::Field, "#phrase"),
        (Hit::Date, "#date"),
        (Hit::Time, "#time"),
    ] {
        let (mut v, _w) = seated();
        open_creation(&mut v, "", ZURICH, en_us());
        click_reminder_field(&mut v, Hit::More);
        click_reminder_field(&mut v, hit);
        type_reminder_character(&mut v, "x", ModifiersState::empty());
        let before = v.semantics_revision();
        let focus = v.accessibility_focus(VIEW);
        let invalid = v.reminder_creation.structured_is_invalid();
        let disabled = ending(&v, "#set").unwrap().disabled;
        type_reminder_character(&mut v, "y", ModifiersState::empty());
        assert_eq!(ending(&v, suffix).unwrap().value.as_deref(), Some("xy"));
        assert_eq!(v.accessibility_focus(VIEW), focus);
        assert_eq!(v.reminder_creation.structured_is_invalid(), invalid);
        assert_eq!(ending(&v, "#set").unwrap().disabled, disabled);
        assert_ne!(
            v.semantics_revision(),
            before,
            "edited {suffix} stayed stale in UIA"
        );
        let after = v.semantics_revision();
        project_typed_reminder(&mut v);
        assert_eq!(
            v.semantics_revision(),
            after,
            "unchanged fields must not republish"
        );
    }
}

#[test]
fn exact_fixture_time_is_valid_but_a_literal_select_all_letter_is_not() {
    let (mut v, _w) = seated();
    open_creation(&mut v, "", ZURICH, en_us());
    click_reminder_field(&mut v, Hit::More);
    click_reminder_field(&mut v, Hit::Date);
    for c in "04/12/2099".chars() {
        type_reminder_character(&mut v, &c.to_string(), ModifiersState::empty());
    }
    click_reminder_field(&mut v, Hit::Time);
    for c in "a3:30 PM".chars() {
        type_reminder_character(&mut v, &c.to_string(), ModifiersState::empty());
    }
    assert_eq!(
        ending(&v, "#time").unwrap().value.as_deref(),
        Some("a3:30 PM")
    );
    assert!(ending(&v, "#set").unwrap().disabled);
    assert!(v.reminder_creation.structured_is_invalid());
    type_reminder_character(&mut v, "a", koru_keys::PRIMARY);
    for c in "3:30 PM".chars() {
        type_reminder_character(&mut v, &c.to_string(), ModifiersState::empty());
    }
    assert_eq!(
        ending(&v, "#date").unwrap().value.as_deref(),
        Some("04/12/2099")
    );
    assert_eq!(
        ending(&v, "#time").unwrap().value.as_deref(),
        Some("3:30 PM")
    );
    assert!(!v.reminder_creation.structured_is_invalid());
    assert!(!ending(&v, "#set").unwrap().disabled);
}
