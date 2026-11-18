// SPDX-License-Identifier: MPL-2.0

//! **What the tasks milestone costs on the desktop side** — plan P-Base
//! (roadmap rev6 §5), cases §6 (c) and (d).
//!
//! ```sh
//! cargo test --release -p koru --bin koru tasks_view::perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `#[ignore]`d, on the same argument the other two baselines in this plan
//! carry: the runs below build pages of five thousand rows and type twelve
//! hundred characters, which is a minute nobody working on something else should
//! pay. The **structural** claims already run unignored — `apps/koru/src/perf.rs`
//! for the edit path, `tasks_view/tests.rs` for a page that lays out only its
//! visible band, `mainview/tests.rs` for the rollover — and this adds only the
//! arithmetic they deliberately refuse to carry.
//!
//! ## What "5,000" can and cannot mean in this process
//!
//! It matters, because the roadmap's phrasing and this window's architecture do
//! not line up, and quietly measuring the wrong thing would be worse than saying
//! so. **A page is a page**: `koru_store_wire::tasks::DEFAULT_LIMIT` is 100 and
//! `MAX_LIMIT` is 500, so no `TasksView` in a shipped build ever holds five
//! thousand rows — the store answers a page and the corpus stays in the store.
//! And after [ADR-90] one `WorkspaceDoc` is one *document*, so five thousand
//! tasks scattered over two hundred and fifty documents cannot be in the
//! document a person is typing in either.
//!
//! So each case below is measured at the size it can actually reach, and one
//! synthetic oversize run is included to show the *shape*:
//!
//! - **(d) the view** at 100 and 500 rows — the two page sizes that ship — and at
//!   5,000, which is not a page any store will send but is what says the view's
//!   own layout is linear rather than accidentally quadratic.
//! - **(c) typing** in two models, because the honest one is not the obvious one.
//!   *Model A* is the container's real per-keystroke work: a Tasks tab holding a
//!   full page, `adopt`ed on every press exactly as `App::adopt_others` does it,
//!   against no such tab. *Model B* is the harshest local version: typing in a
//!   document that itself holds many task rows, which is where E-4's due chip and
//!   T-0b's edit noting are actually on the edit path.
//!
//! ## The ratios are gates now
//!
//! **Approved by the owner on 2026-09-01**, as proposed (`PERF_BASELINE.md` §7).
//! Two of the four numbers here were already asserted at the value the report
//! went on to propose — the frame's exponent and the open path's — because their
//! margins were wide enough that a second, looser ceiling above them would have
//! meant nothing. The other two were loose and are now the approved figures: the
//! paired typing ratio at 1.30 and the task-premium exponent at 0.5. One is new:
//! opening at 500 rows against opening at 100, which is the only shipped-size
//! comparison in the file and had no assertion at all.
//!
//! **Nothing here was touched by `attrs_probe`.** These cases feed the view a
//! synthetic `TaskQueryResponse` and never reach a store, which is what makes
//! them the client's numbers rather than the sum of both sides. The store's half
//! of a Tasks-tab open fell 830 ms -> 43 ms with the index; this half was 1.6 ms
//! before and is 1.6 ms after, and that is the point of measuring them apart.
//!
//! [ADR-90]: ../../../../docs/decisions/ADR-076-100.md

use std::time::{Duration, Instant};

use crate::panes::explore::rail::{Bucket, RailTarget};
use chrono::NaiveDate;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{NodeId, WorkspaceId};
use koru_store_wire::attributes::AttributeRowValue;
use koru_store_wire::tasks::{TaskQueryResponse, TaskRow};
// **Named, and every call through it written out in full.** `Leaf` implements
// both `Editing` and `koru_workspace_host::View`, and the two name
// `set_viewport`, `key` and six others each; which one a bare `leaf.key(..)`
// resolves to depends on what is imported, and choosing wrong is silent
// (CLAUDE.md, "two traits that overlap"). So nothing here is a method call that
// could have gone either way — neither trait is in scope for method syntax.
use koru_editor::Editing;
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState};

use super::query::Filters;
use super::view::TasksView;
use crate::leaf::Leaf;

// ------------------------------------------------------------------ protocol

/// Three warmups, median of nine — roadmap §6, and the same protocol
/// `crates/koru-projection/tests/perf.rs` and
/// `apps/koru-store-server/tests/tasks_perf.rs` use, so the three tables in
/// `docs/features/tasks/PERF_BASELINE.md` are comparable.
const WARMUPS: usize = 3;
const RUNS: usize = 9;

struct Bench {
    median: Duration,
    min: Duration,
    max: Duration,
}

impl Bench {
    /// `f` builds whatever state it needs *outside* the timer and returns the
    /// elapsed time of the part being measured, because half of these cases are
    /// a step taken on state that the step itself consumes — a filter change
    /// only costs anything the first time it is applied.
    fn of(mut f: impl FnMut() -> Duration) -> Self {
        for _ in 0..WARMUPS {
            f();
        }
        let mut samples: Vec<Duration> = (0..RUNS).map(|_| f()).collect();
        samples.sort();
        Self {
            median: samples[samples.len() / 2],
            min: samples[0],
            max: samples[samples.len() - 1],
        }
    }

    fn ms(&self) -> f64 {
        self.median.as_secs_f64() * 1000.0
    }

    fn spread(&self) -> f64 {
        (self.max.as_secs_f64() - self.min.as_secs_f64()) / self.median.as_secs_f64().max(1e-9)
    }
}

fn report(what: &str, bench: &Bench) {
    println!(
        "  {what:<46} {:>9.3} ms   (min {:>8.3}  max {:>8.3}  spread {:>5.1}%)",
        bench.ms(),
        bench.min.as_secs_f64() * 1000.0,
        bench.max.as_secs_f64() * 1000.0,
        bench.spread() * 100.0,
    );
}

/// log(time ratio) / log(size ratio). 1.0 is linear, below is better, above is
/// the shape a baseline exists to catch.
fn exponent(time_grew: f64, size_grew: f64) -> f64 {
    time_grew.ln() / size_grew.ln()
}

// -------------------------------------------------------------- the fixtures

/// A fixed day, so nothing here reads a clock — the fixture rule
/// `crates/koru-projection/src/fixtures.rs` states and this file obeys for the
/// same reason: a page whose rule labels moved with the morning would measure
/// differently every morning.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 6, 15).expect("a real day")
}

fn now_ms() -> i64 {
    today()
        .and_hms_opt(10, 0, 0)
        .expect("a real time")
        .and_utc()
        .timestamp_millis()
}

fn attr(role: &str, kind: &str, value: &str) -> AttributeRowValue {
    AttributeRowValue {
        id: format!("{role}-1"),
        role: role.to_owned(),
        kind: kind.to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

/// One wire row, at the shape the store sends: a due date on seven in ten and a
/// priority on four in ten, which is the roadmap corpus's own mix.
fn wire_row(i: usize) -> TaskRow {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    let node = NodeId::new();
    let mut attributes = Vec::new();
    if i % 10 < 7 {
        // A year of consecutive days, wrapped, so a date-sorted page spans every
        // rule label the Upcoming grouping can print.
        attributes.push(attr(
            koru_core::attributes::KEY_DUE,
            "date",
            &format!("2026-{:02}-{:02}", 1 + (i / 28) % 12, 1 + i % 28),
        ));
    }
    if i % 10 < 4 {
        attributes.push(attr(koru_core::attributes::KEY_PRIORITY, "text", "high"));
    }
    TaskRow {
        workspace,
        document,
        node: node.to_storage(),
        task: format!(
            "task:{}:{}:{}",
            workspace.to_storage(),
            document.to_storage(),
            node.to_storage()
        ),
        node_kind: "paragraph".to_owned(),
        lifecycle: "open".to_owned(),
        document_trashed: false,
        in_trash: false,
        breadcrumb: "Acme / Projects / Q3".to_owned(),
        title: format!("Task {:05} with a reasonable amount of text on it", i + 1),
        attributes,
        bucket: None,
    }
}

fn an_answer(rows: usize) -> TaskQueryResponse {
    TaskQueryResponse {
        rows: (0..rows).map(wire_row).collect(),
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: Vec::new(),
        limit: rows.max(1),
        next_after: None,
        restarted: false,
        covered: None,
    }
}

/// An empty in-memory document — what `catch_up` needs to answer the residue
/// question, and nothing more.
fn a_document() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "Perf").expect("an in-memory document")
}

/// A Tasks view holding one answer, laid out, ready to draw.
fn view_holding(rows: usize) -> (WorkspaceDoc, TasksView) {
    let doc = a_document();
    let mut view = TasksView::new(RailTarget::Bucket(Bucket::Upcoming));
    koru_workspace_host::View::set_viewport(&mut view, 1000.0, 700.0);
    view.panel_mut()
        .accept_for_test(&an_answer(rows), today(), now_ms());
    koru_workspace_host::View::adopt(&mut view, &doc);
    view.catch_up(&doc);
    (doc, view)
}

/// Everything `paint` needs and nothing it does not: a fresh scene at the
/// origin, caret off.
fn paint(view: &mut TasksView) {
    let mut scene = Scene::new();
    view.paint(&mut scene, Affine::IDENTITY, false);
}

// ---------------------------------------------------- (d) the Tasks view

/// **Opening a Tasks tab** — roadmap §6 case (d), first of three.
///
/// The whole of what the view does when an answer lands: fold it into rows
/// (`accept_for_test` is the worker's own `apply`), take them (`catch_up`, which
/// also runs the residue walk and the relayout), and prepare a frame (`paint`,
/// which is every measurement and every scene command up to the GPU).
///
/// Three sizes. **100 and 500 are the two a shipped build can reach** — the wire
/// default and the wire maximum; 5,000 is not a page any store will send and is
/// here only to say that the view's own cost is linear in what it holds. If that
/// exponent ever climbs, a page of 500 is where it will be felt.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn opening_a_tasks_tab() {
    println!("\n(d) opening a Tasks tab");
    let mut measured = Vec::new();
    for rows in [100usize, 500, 5_000] {
        let answer = an_answer(rows);

        let fold = Bench::of(|| {
            let mut view = TasksView::new(RailTarget::Bucket(Bucket::Upcoming));
            koru_workspace_host::View::set_viewport(&mut view, 1000.0, 700.0);
            let at = Instant::now();
            view.panel_mut().accept_for_test(&answer, today(), now_ms());
            at.elapsed()
        });
        report(&format!("fold the answer   @ {rows:>5} rows"), &fold);

        let take = Bench::of(|| {
            let doc = a_document();
            let mut view = TasksView::new(RailTarget::Bucket(Bucket::Upcoming));
            koru_workspace_host::View::set_viewport(&mut view, 1000.0, 700.0);
            view.panel_mut().accept_for_test(&answer, today(), now_ms());
            koru_workspace_host::View::adopt(&mut view, &doc);
            let at = Instant::now();
            view.catch_up(&doc);
            at.elapsed()
        });
        report(&format!("catch_up + relayout @ {rows:>5} rows"), &take);

        let (_doc, mut view) = view_holding(rows);
        let frame = Bench::of(|| {
            let at = Instant::now();
            paint(&mut view);
            at.elapsed()
        });
        report(&format!("paint one frame   @ {rows:>5} rows"), &frame);

        let total = fold.ms() + take.ms() + frame.ms();
        println!("    open, end to end @ {rows:>5} rows: {total:>8.3} ms");
        measured.push((rows as f64, total, frame.ms()));
    }

    // **Gate (d), the shipped-size one**: 500 rows is the wire maximum and 100 is
    // the wire default, so this is the only comparison in the file between two
    // page sizes a store will actually send. Observed 1.26x; the ceiling is 4x
    // and the margin is enormous on purpose, because both numbers are around a
    // millisecond and a millisecond of scheduler noise is a 60% swing at that
    // scale. What it catches is the fold or the relayout becoming per-row work
    // that is *not* linear — at 5x the rows, 4x is already generous for linear.
    let (_, hundred, _) = measured[0];
    let (_, five_hundred, _) = measured[1];
    let shipped = five_hundred / hundred.max(1e-9);
    println!("  opening at 500 rows / opening at 100 rows = {shipped:.2}x");
    assert!(
        shipped < 4.0,
        "opening a page of 500 cost {shipped:.2}x opening a page of 100 — gate (d) is 4x, \
         against 1.26x observed. Both are the sizes a shipped build reaches, so this is the \
         one that a person would feel"
    );

    let (small_rows, small_total, small_frame) = measured[0];
    let (big_rows, big_total, big_frame) = measured[measured.len() - 1];
    let size_grew = big_rows / small_rows;
    let open_exp = exponent(big_total / small_total.max(1e-9), size_grew);
    let frame_exp = exponent(big_frame / small_frame.max(1e-9), size_grew);
    println!(
        "  {size_grew:.0}x the page: open exponent {open_exp:.2}, frame exponent {frame_exp:.2}"
    );

    // **The frame is the one that must not scale**, and it is the one the
    // container's rules are about: a page draws only its visible band, so fifty
    // times the rows must not be fifty times the frame. The open path may grow —
    // folding and laying out are per-row by definition — but not faster than the
    // rows do.
    assert!(
        frame_exp < 0.6,
        "a frame grew as page^{frame_exp:.2} over {size_grew:.0}x the rows — gate (d) is 0.6, \
         against 0.40-0.41 observed with a 1-5% spread. This is the tightest number in the \
         whole baseline and it is defensible because the culling either happens or it does \
         not: the view is drawing rows it cannot show, which is what `visible_range` exists \
         for"
    );
    assert!(
        open_exp < 1.5,
        "opening grew as page^{open_exp:.2} — gate (d) is 1.5, against 0.52-0.53 observed. \
         The fold and the relayout are per-row by definition, so linear is the target and \
         this only catches worse"
    );
}

/// **Changing a filter** — roadmap §6 case (d), second of three.
///
/// Two halves, and the split is the product claim rather than an implementation
/// detail. Pressing a chip must feel instant, and what it costs *then* is
/// `set_filters`: the page empties, re-lays out and re-measures its chip row,
/// and the store is asked again. What arrives later is another answer, which is
/// the open path above. So the press is measured on its own — that is the number
/// a person feels — and the settle is measured beside it.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn changing_a_filter() {
    println!("\n(d) changing a filter");
    let mut measured = Vec::new();
    for rows in [100usize, 500, 5_000] {
        let answer = an_answer(rows);

        // The press. Built fresh each run because a filter change is only a
        // change once — applying the same `Filters` twice is a no-op by design.
        let press = Bench::of(|| {
            let (_doc, mut view) = view_holding(rows);
            let flipped = Filters {
                min_high: !view.filters().min_high,
                ..view.filters()
            };
            let at = Instant::now();
            view.set_filters(flipped);
            at.elapsed()
        });
        report(&format!("the press (set_filters) @ {rows:>5} rows"), &press);

        // The settle: the next answer arrives and the page comes back.
        let settle = Bench::of(|| {
            let (doc, mut view) = view_holding(rows);
            let flipped = Filters {
                min_high: !view.filters().min_high,
                ..view.filters()
            };
            view.set_filters(flipped);
            let at = Instant::now();
            view.panel_mut().accept_for_test(&answer, today(), now_ms());
            koru_workspace_host::View::adopt(&mut view, &doc);
            view.catch_up(&doc);
            paint(&mut view);
            at.elapsed()
        });
        report(
            &format!("the settle (answer + frame) @ {rows:>5} rows"),
            &settle,
        );
        measured.push((rows as f64, press.ms()));
    }

    let (small_rows, small) = measured[0];
    let (big_rows, big) = measured[measured.len() - 1];
    let exp = exponent(big / small.max(1e-9), big_rows / small_rows);
    println!("  the press grew as page^{exp:.2}");
    assert!(
        exp < 1.5,
        "the press grew as page^{exp:.2} — a chip is doing per-row work beyond the relayout \
         it is allowed"
    );
}

/// **The midnight rebucket** — roadmap §6 case (d), third of three.
///
/// What the rollover costs the *view*. The container's half is
/// `MainView::day_rolled` (`apps/koru/src/mainview/tasks.rs`), which is a date
/// comparison and three invalidations, and is pinned unignored in
/// `mainview/tests.rs`; what follows from it here is one more answer computed on
/// the new day, taken, and drawn — every rule label above every row is a
/// function of `today`, so the whole page's labels move.
///
/// **This is the same shape as the open path and that is the finding**, not a
/// gap in the measurement: a rollover is not a special path, it is one more
/// answer. The number is reported so the report can say what waking a laptop on
/// the other side of midnight costs, which is a real question a person asks.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn the_midnight_rebucket() {
    println!("\n(d) the midnight rebucket");
    let tomorrow = today() + chrono::Duration::days(1);
    let tomorrow_ms = now_ms() + 86_400_000;
    for rows in [100usize, 500, 5_000] {
        let answer = an_answer(rows);
        let rebucket = Bench::of(|| {
            let (doc, mut view) = view_holding(rows);
            // Everything the container does to a Tasks tab when the date moves.
            view.panel_mut().invalidate();
            view.mark_stale();
            let at = Instant::now();
            view.panel_mut()
                .accept_for_test(&answer, tomorrow, tomorrow_ms);
            view.catch_up(&doc);
            paint(&mut view);
            at.elapsed()
        });
        report(&format!("rollover, end to end @ {rows:>5} rows"), &rebucket);

        // What the rule labels alone cost, which is the part that is genuinely
        // about the date rather than about the answer.
        let (_doc, view) = view_holding(rows);
        let labels = Bench::of(|| {
            let at = Instant::now();
            let n = view.rules_for_test().len();
            std::hint::black_box(n);
            at.elapsed()
        });
        report(
            &format!("  of which the rule labels @ {rows:>5} rows"),
            &labels,
        );
    }
}

/// **A page draws its visible band and nothing else** — the deterministic claim
/// §6 gives the V-lane, measured here as the number it implies.
///
/// Unlike everything else in this file this one is cheap and structural, so it
/// is the one case that could have gone in the ordinary suite; it lives here
/// because it is the sentence the frame measurement above is an argument about,
/// and separating them would leave each of them half a claim.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn a_page_lays_out_only_its_visible_band() {
    println!("\n(d) the visible band");
    for rows in [100usize, 500, 5_000] {
        let (_doc, view) = view_holding(rows);
        let visible = view.visible_for_test().len();
        println!("  {rows:>5} rows in the page, {visible:>3} lines in the band");
        assert!(
            visible < 60,
            "a 700px viewport drew {visible} lines — the band is not being culled"
        );
    }
}

// ------------------------------------------------------------- (c) typing

const PRESSES: usize = 100;
const NONE: ModifiersState = ModifiersState::empty();

/// A non-flat, title-less document view rooted at `node` — the twin of the one
/// in `crate::perf` and in `tests.rs`, duplicated for the reason stated there:
/// a helper reachable from all three would have to be `pub(crate)` on the
/// non-test build too.
fn list_block(ws: &WorkspaceDoc, node: NodeId) -> Leaf {
    Leaf::Document(koru_document::BuiltInDocument::open_list(ws, node))
}

/// A document of `rows` rows, `tasks` of them tasks carrying a due date, with
/// the caret in the last one — the furthest point down the reflow, so anything
/// that walks the rows per keystroke shows up here at its worst.
fn typing_into(rows: usize, tasks: usize) -> (WorkspaceDoc, Leaf) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "Perf").expect("workspace");
    let doc = ws
        .insert(Parent::Root, Node::document("Doc").expect("document"))
        .expect("insert");
    for i in 0..rows {
        let text = format!("Row number {i} with a reasonable amount of text on it");
        let node = ws
            .insert(Parent::Node(doc), Node::task(&text).expect("task"))
            .expect("insert");
        if i < tasks {
            // A marked task with a due date: E-4's chip is drawn for it and
            // T-0b's edit noting applies to it, which is the whole of what a
            // task adds to the edit path.
            koru_core::tasks::set_due(
                &mut ws,
                node,
                koru_core::time::Temporal::Date(
                    koru_core::time::DateOnly::parse(&format!(
                        "2026-{:02}-{:02}",
                        1 + (i / 28) % 12,
                        1 + i % 28
                    ))
                    .expect("a real day"),
                ),
            )
            .expect("due");
        }
    }
    ws.enable_undo();
    let mut leaf = list_block(&ws, doc);
    Editing::set_viewport(&mut leaf, 900.0, 560.0);
    if rows > 0 {
        leaf.body_mut().focus = rows - 1;
    }
    leaf.view_mut().active = true;
    (ws, leaf)
}

/// One hundred keystrokes, mean per press. Every seventh is a space, because a
/// word boundary is where `checkpoint` closes an undo group and a run of them is
/// the realistic mix rather than the cheapest.
fn type_a_hundred(ws: &mut WorkspaceDoc, leaf: &mut Leaf, mut between: impl FnMut()) -> Duration {
    let start = Instant::now();
    for i in 0..PRESSES {
        let c = if i % 7 == 6 { " " } else { "x" };
        Editing::key(leaf, ws, &Key::Character(c.into()), Some(c), NONE);
        between();
    }
    start.elapsed() / PRESSES as u32
}

/// **Model A: typing while a Tasks tab is open** — roadmap §6 case (c), and the
/// one that answers the question as asked.
///
/// A hundred keystrokes in an ordinary document while a Tasks tab holds a full
/// page, with the container's own per-press work done on it: `App::adopt_others`
/// calls `View::adopt` on every other pane, and for a `TasksView` that is
/// `self.stale = true` and nothing else. The claim §6 makes — *typing with many
/// unrelated tasks triggers no task-dependent document walk or reflow* — is
/// exactly the claim that this ratio is one.
///
/// The structural half is not measured here and does not need to be:
/// `TasksView::adopt` is three tokens long and a background tab is never in the
/// scene, both of which the ordinary suite pins. What a number adds is that
/// nothing *else* crept onto the press path.
///
/// **The ratio is paired, and that is not fussiness.** A keystroke is eighty
/// microseconds, the two arms of this comparison differ by about twenty, and the
/// spread of either arm measured on its own is a third of its own median — so
/// two independent medians divided by each other is mostly a measurement of what
/// the machine was doing between them. Measuring both arms inside one run and
/// taking the **median of the per-run ratios** cancels that drift exactly, and it
/// is the statistic the claim is about anyway.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn typing_while_a_tasks_tab_is_open() {
    println!("\n(c) model A: typing with a Tasks tab open");
    for rows in [100usize, 500] {
        let mut alone_ms = Vec::new();
        let mut with_ms = Vec::new();
        let mut ratios = Vec::new();
        for run in 0..(WARMUPS + RUNS) {
            let alone = {
                let (mut ws, mut leaf) = typing_into(200, 0);
                type_a_hundred(&mut ws, &mut leaf, || {})
            };
            let with = {
                let (mut ws, mut leaf) = typing_into(200, 0);
                let (tasks_doc, mut tasks) = view_holding(rows);
                type_a_hundred(&mut ws, &mut leaf, || {
                    // Exactly what `App::adopt_others` does to every other pane.
                    koru_workspace_host::View::adopt(&mut tasks, &tasks_doc);
                })
            };
            if run < WARMUPS {
                continue;
            }
            alone_ms.push(alone.as_secs_f64() * 1000.0);
            with_ms.push(with.as_secs_f64() * 1000.0);
            ratios.push(with.as_secs_f64() / alone.as_secs_f64().max(1e-12));
        }
        let median = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
            v[v.len() / 2]
        };
        let a = median(alone_ms);
        let w = median(with_ms);
        let r = median(ratios.clone());
        let worst = ratios
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, |m, x| m.max(x));
        println!(
            "  a Tasks tab of {rows:>3}: alone {a:.4} ms/press, with {w:.4} ms/press, \
             paired ratio {r:.3} (worst run {worst:.3})"
        );
        // **Gate (c/A)**, approved 2026-09-01. The roadmap proposed 1.2 and the
        // report relaxed it to 1.30 on the evidence: 1.2 sits inside the
        // single-run noise band (worst single paired run 1.37) and would flake.
        // 1.30 sits between the observed median — 1.00-1.02 — and that worst
        // sample, which is tight enough that real work appearing on the press
        // path fails it and loose enough that one descheduled run does not.
        //
        // The statistic is the **median of the per-run paired ratios**, not two
        // medians divided: a keystroke is 70-95 µs and either arm's own spread
        // is a third of its median, so the divided form gave 1.26-1.28 and was
        // measuring machine drift.
        assert!(
            r < 1.30,
            "a keystroke cost {r:.2}x with a Tasks tab open — gate (c/A) is 1.30, against \
             1.00-1.02 observed. The tab is doing work on the press path, which is what \
             `adopt` being a flag exists to prevent"
        );
    }
}

/// **Model B: typing in a document that is itself full of tasks** — the harshest
/// local reading of §6 case (c).
///
/// Five thousand tasks cannot be in one document in a shipped build either
/// (they are dealt over two hundred and fifty), but this is where the task
/// machinery genuinely sits on the edit path: E-4 draws a due chip on every task
/// row the viewport shows, and T-0b notes the edited row on every
/// `insert_text`. So the ratio between a document of two hundred plain rows and
/// one of two hundred dated tasks is the real price of the feature per
/// keystroke, and the 2,000-row pair says whether it is flat in the document.
///
/// **Three arms, not two**, because the two-arm version cannot tell the answer
/// apart from its explanation. `none` is the control; `one` gives a due date to
/// the row the caret is in and to nothing else, which is what T-0b's noting and
/// the edited row's own chip cost; `all` gives one to every row, which adds
/// whatever the *visible band* pays for the chips it draws. The difference
/// between the second and the third is the per-visible-row half, and it is not
/// derivable from a single ratio.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru --bin koru tasks_view::perf -- --ignored"]
fn typing_in_a_document_full_of_tasks() {
    println!("\n(c) model B: typing among tasks in the same document");
    let mut measured = Vec::new();
    for rows in [200usize, 2_000] {
        let plain = Bench::of(|| {
            let (mut ws, mut leaf) = typing_into(rows, 0);
            type_a_hundred(&mut ws, &mut leaf, || {})
        });
        report(&format!("{rows:>5} rows, none a task"), &plain);

        // Only the row the caret is in. `typing_into` dates the *first* `tasks`
        // rows and focuses the last, so this arm dates a row the caret is not
        // in — which is exactly wrong for what it is trying to isolate, and is
        // why it is built here rather than through the shared helper.
        let one = Bench::of(|| {
            let (mut ws, mut leaf) = typing_into(rows, 0);
            let focused = leaf.body().rows[rows - 1].id;
            koru_core::tasks::set_due(
                &mut ws,
                focused,
                koru_core::time::Temporal::Date(
                    koru_core::time::DateOnly::parse("2026-06-15").expect("a real day"),
                ),
            )
            .expect("due");
            Editing::adopt(&mut leaf, &ws);
            type_a_hundred(&mut ws, &mut leaf, || {})
        });
        report(&format!("{rows:>5} rows, only the caret's a task"), &one);

        let tasks = Bench::of(|| {
            let (mut ws, mut leaf) = typing_into(rows, rows);
            type_a_hundred(&mut ws, &mut leaf, || {})
        });
        report(&format!("{rows:>5} rows, every one a dated task"), &tasks);

        let ratio = tasks.ms() / plain.ms().max(1e-9);
        println!(
            "    ratio: {ratio:.3}   (the caret's row alone: {:.3}x; the rest of the band \
             adds {:.3} ms/press)",
            one.ms() / plain.ms().max(1e-9),
            tasks.ms() - one.ms(),
        );
        measured.push((rows as f64, plain.ms(), tasks.ms(), ratio));
    }

    let (small_rows, _, small_tasks, small_ratio) = measured[0];
    let (big_rows, _, big_tasks, big_ratio) = measured[1];
    println!(
        "  the task premium is {small_ratio:.2}x at {small_rows:.0} rows and {big_ratio:.2}x at \
         {big_rows:.0}"
    );
    let exp = exponent(big_tasks / small_tasks.max(1e-9), big_rows / small_rows);
    println!("  typing among tasks grew as document^{exp:.2}");
    // **Gate (c/B)**, approved 2026-09-01. The property `apps/koru/src/perf.rs`
    // established for plain rows, asked of task rows: a keystroke costs what it
    // costs whatever else is in the document, because only the edited row is
    // re-shaped.
    //
    // **The exponent is the gate and the 3.8x constant deliberately is not.**
    // That constant — a document whose visible rows are all dated tasks costs
    // ~250 µs a keystroke against ~66 µs — is a *finding*, written up in
    // `PERF_BASELINE.md` §4 and owed a decision. Gating it would freeze it in
    // place, and freezing a number nobody has decided to accept is how a
    // regression becomes a requirement.
    assert!(
        exp < 0.5,
        "typing among tasks grew as document^{exp:.2} — gate (c/B) is 0.5, against 0.15-0.24 \
         observed over 10x the rows. A keystroke is walking the body, which is the one thing \
         the tasks work was not allowed to add"
    );
}
