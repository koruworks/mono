// SPDX-License-Identifier: MPL-2.0

//! **The read model's baselines, on the fixture the roadmap names** — plan
//! P-Base (roadmap rev6 §5, cases §6 (a), (b-walk) and (e)).
//!
//! ```sh
//! cargo test --release -p koru-projection --test perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every test here is `#[ignore]`d. That is the gate, and it is deliberate: the
//! roadmap corpus is 55,000 rows written into SQLite twice over, and a suite
//! that paid for it on every `cargo test --workspace` would be a tax on work
//! that has nothing to do with performance. The *structural* claims — a warm
//! query opens no document, a repeat answers identically and writes nothing, a
//! page is its limit — already run unignored beside the code, in
//! `crates/koru-projection/src/store/tests.rs` and
//! `apps/koru-store-server/tests/tasks_query.rs`. What is here is the
//! **arithmetic**, and arithmetic is what a person asks for on purpose.
//!
//! ## Ratios, not milliseconds
//!
//! `apps/koru/src/perf.rs` set the rule and `apps/koru-store-server/tests/perf.rs`
//! repeated it: **assert that something does not get worse as the corpus grows,
//! never that it takes fewer than N milliseconds.** A wall clock is a promise
//! about the machine. So every assertion below is a ratio between two sizes of
//! the same fixture, and every raw number is *printed* rather than asserted —
//! the printed table is what `docs/features/tasks/PERF_BASELINE.md` is built from.
//!
//! ## The ratios are gates now
//!
//! **Approved by the owner on 2026-09-01**, with P-Base's proposed numbers taken
//! as proposed (`PERF_BASELINE.md` §7). Every ratio this file asserts is one of
//! them, at the value the report argued for, and the looser "shape" ceilings that
//! shipped with the harness are gone — they were the line between *this machine
//! is busy* and *the answer changed shape*, which is a weaker question than the
//! one a release now has to answer. Each assertion names its case, its observed
//! range and its margin, so the next person can see what the number was bought
//! with rather than having to trust it.
//!
//! **Loosening one is a decision, not a fix.** A gate that fails is either a
//! regression or evidence the margin was wrong, and the second needs the same
//! two-run protocol the first numbers were taken under.
//!
//! ## The protocol, stated once
//!
//! Three warmups, then the median of nine runs, per roadmap §6. The warmups are
//! not politeness: the first query against a fresh connection pays SQLite's
//! statement preparation and the page cache's first read of every b-tree page it
//! will touch, and including that in a median measures the cache rather than the
//! query. [`Bench::spread`] is printed beside every median so the report can say
//! how much of a margin a threshold needs.

use std::time::{Duration, Instant};

use koru_projection::fixtures::{SEED, TaskCorpus};
use koru_projection::store::{
    AttributeRowQuery, Bucket, BucketWindows, DateWindow, DueFilter, InstantWindow,
    LifecycleFilter, Projections, TaskRowQuery,
};
use koru_store_core::ids::{DocumentId, WorkspaceId};

// ------------------------------------------------------------------ protocol

/// How many runs are thrown away before anything is recorded.
const WARMUPS: usize = 3;
/// How many runs the median is taken over.
const RUNS: usize = 9;

/// What one measurement answers: the median, and enough around it to judge a
/// threshold.
struct Bench {
    median: Duration,
    min: Duration,
    max: Duration,
}

impl Bench {
    /// Three warmups, nine runs, median. `f` is run for its side effect and its
    /// answer is dropped, so a query that got optimised away would show as zero
    /// rather than as a pass — which is why each caller returns a count out of
    /// its closure and this one asserts the counts never move.
    fn of(mut f: impl FnMut() -> usize) -> (Self, usize) {
        for _ in 0..WARMUPS {
            f();
        }
        let mut samples = Vec::with_capacity(RUNS);
        let mut answered = None;
        for _ in 0..RUNS {
            let at = Instant::now();
            let count = f();
            samples.push(at.elapsed());
            match answered {
                None => answered = Some(count),
                Some(first) => assert_eq!(
                    first, count,
                    "the same query answered a different number of rows between runs — \
                     this fixture is not deterministic and no number off it means anything"
                ),
            }
        }
        samples.sort();
        (
            Self {
                median: samples[samples.len() / 2],
                min: samples[0],
                max: samples[samples.len() - 1],
            },
            answered.unwrap_or(0),
        )
    }

    /// How far the slowest run sat above the fastest, as a fraction of the
    /// median. **The number a threshold's margin is argued from**: a case whose
    /// spread is 40% cannot carry a 10% budget however much one would like it
    /// to.
    fn spread(&self) -> f64 {
        (self.max.as_secs_f64() - self.min.as_secs_f64()) / self.median.as_secs_f64().max(1e-9)
    }

    fn ms(&self) -> f64 {
        self.median.as_secs_f64() * 1000.0
    }
}

/// One line of the table the report is built from.
fn report(what: &str, bench: &Bench, rows: usize) {
    println!(
        "  {what:<44} {:>9.3} ms   (min {:>8.3}  max {:>8.3}  spread {:>5.1}%)   rows={rows}",
        bench.ms(),
        bench.min.as_secs_f64() * 1000.0,
        bench.max.as_secs_f64() * 1000.0,
        bench.spread() * 100.0,
    );
}

// ------------------------------------------------------------------- corpora

/// The roadmap corpus: 3 workspaces, 250 documents, 5,000 tasks, 50,000 rows
/// that are not tasks, one workspace cold.
fn roadmap() -> TaskCorpus {
    TaskCorpus::roadmap()
}

/// **The same corpus, scaled by `n`** — the roadmap shape divided down, dealt by
/// the same generator from the same seed.
///
/// Every dimension at once, deliberately: shrinking the tasks alone would leave
/// the same 50,000 non-task rows in every scan and measure the wrong ratio. What
/// a pair of these asks is "does a hundred times the workspace cost a hundred
/// times the answer", and only a proportional pair asks it.
fn scaled(divisor: usize) -> TaskCorpus {
    TaskCorpus {
        seed: SEED,
        workspaces: 3,
        cold_workspaces: 1,
        documents: (250 / divisor).max(3),
        tasks: 5_000 / divisor,
        other_rows: 50_000 / divisor,
    }
}

/// A read model holding `shape`'s warm workspaces.
fn written(shape: TaskCorpus) -> (Projections, koru_projection::fixtures::Corpus) {
    let mut p = Projections::in_memory().expect("an in-memory read model");
    let corpus = shape.plan();
    corpus.write_into(&mut p, 1).expect("write the corpus");
    (p, corpus)
}

// ------------------------------------------------------------------- queries

/// The page a Tasks tab asks for: the default limit, the default sort, the
/// default lifecycle.
///
/// **Unscoped**, which is not what the view sends — `tasks_view::query::page_query`
/// always names one workspace. The pair is measured deliberately: unscoped is
/// the widest question the surface can be asked, and the workspace filter is one
/// of the levers the report has to be able to price.
fn a_page() -> TaskRowQuery {
    TaskRowQuery {
        limit: 100,
        ..TaskRowQuery::default()
    }
}

/// The same page, scoped to one workspace — **the query the Tasks tab actually
/// sends**.
fn a_scoped_page(workspace: WorkspaceId) -> TaskRowQuery {
    TaskRowQuery {
        attributes: AttributeRowQuery {
            workspaces: vec![workspace],
            ..AttributeRowQuery::default()
        },
        ..a_page()
    }
}

/// The day everything here is measured on. Inside the fixture's calendar year,
/// so all five buckets are populated and both horizons land on real content.
const TODAY: &str = "2026-06-15";

/// The reader's zone, as a fixed offset. Zurich in summer, +02:00 — a fixed
/// offset rather than a database because what `BucketWindows` wants is already
/// *resolved* edges, and resolving them is the caller's job by design.
const OFFSET_SECONDS: i32 = 2 * 3_600;

fn a_day(s: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("a fixture day is a day")
}

/// The instant a calendar day *ends* at in the reader's zone, epoch-ms UTC.
fn day_ends_ms(day: chrono::NaiveDate) -> i64 {
    use chrono::TimeZone;
    chrono::FixedOffset::east_opt(OFFSET_SECONDS)
        .expect("a real offset")
        .from_local_datetime(&(day + chrono::Duration::days(1)).and_time(chrono::NaiveTime::MIN))
        .single()
        .expect("no fold at this offset")
        .timestamp_millis()
}

/// The windows a reader in that zone sends on [`TODAY`], under SPEC §7's shipped
/// horizons — seven days and thirty.
///
/// Computed here rather than in the store, which is the whole point of the
/// shape: this crate carries no timezone database and never invents one.
fn windows() -> BucketWindows {
    use chrono::TimeZone;
    let today = a_day(TODAY);
    let horizon = today + chrono::Duration::days(7);
    let upcoming = today + chrono::Duration::days(30);
    BucketWindows {
        today: today.format("%Y-%m-%d").to_string(),
        horizon: horizon.format("%Y-%m-%d").to_string(),
        upcoming_horizon: upcoming.format("%Y-%m-%d").to_string(),
        today_ends_ms: day_ends_ms(today),
        horizon_ends_ms: day_ends_ms(horizon),
        upcoming_ends_ms: day_ends_ms(upcoming),
        // Mid-morning, so "before now" is strictly smaller than "before the end
        // of today" and the two edges cannot pass by coincidence.
        now_ms: chrono::FixedOffset::east_opt(OFFSET_SECONDS)
            .expect("a real offset")
            .from_local_datetime(&today.and_hms_opt(10, 0, 0).expect("a real time"))
            .single()
            .expect("no fold at this offset")
            .timestamp_millis(),
        min_next_priority: koru_core::attributes::Priority::High,
    }
}

/// The overdue definition the rail's tint reads — everything due on or before
/// today, in both coordinate systems.
fn overdue() -> DueFilter {
    let w = windows();
    DueFilter {
        dates: Some(DateWindow {
            after: None,
            before: Some(
                (a_day(TODAY) + chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string(),
            ),
        }),
        instants: Some(InstantWindow {
            after_ms: None,
            before_ms: Some(w.now_ms),
        }),
    }
}

// ------------------------------------------------------- (a) the warm query

/// **What a warm page costs as the corpus grows** — roadmap §6 case (a).
///
/// The roadmap called this one *sub-linearity*, and the honest finding of this
/// plan is that it is not sub-linear: the page is bounded (a hundred rows at
/// every size) and the *statement behind it* is not, because the derivation
/// predicate and the lifecycle test are correlated subqueries over `attrs`
/// evaluated per candidate row, and the candidate set is the workspace. So a
/// hundred times the corpus costs more than a hundred times the page, not less.
/// That is R-1b's per-row attribution, seen from the other end, and the lever it
/// named — a covering index over `attrs` — is sized in
/// `docs/features/tasks/PERF_BASELINE.md` rather than built here.
///
/// Four sizes, so the report can say what the curve *is* rather than quoting one
/// ratio: the exponent is fitted across the whole range and printed.
///
/// **The assertion is the shape, not the number**, and since 2026-09-01 it is
/// the *approved* shape: gate (a.1), exponent below 1.45. It was measured at
/// 1.32–1.34 when the correlated `attrs` probes each ended in a random table row
/// read, and at **0.75–0.85** once `attrs_probe` covered them — genuinely
/// sub-linear, which is what the roadmap had hoped for and what the pre-index
/// baseline had to report it was not. The ceiling stayed at the number approved
/// against the *worse* measurement rather than being tightened to the better one:
/// 1.45 is what an added join or a lost index has to clear, and the whole margin
/// between 0.85 and 1.45 is now the room the index bought.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn what_a_warm_page_costs_as_the_corpus_grows() {
    println!("\n(a) warm task page of 100, against corpus size");
    let mut measured: Vec<(f64, f64, f64)> = Vec::new();
    for divisor in [100usize, 20, 5, 1] {
        let shape = scaled(divisor);
        let (p, corpus) = written(shape);
        let workspace = corpus.warm().next().expect("a warm workspace").id;

        let (unscoped, unscoped_rows) =
            Bench::of(|| p.task_rows(&a_page()).expect("page").rows.len());
        let (scoped, scoped_rows) = Bench::of(|| {
            p.task_rows(&a_scoped_page(workspace))
                .expect("page")
                .rows
                .len()
        });
        report(
            &format!("unscoped page @ {:>5} tasks", shape.tasks),
            &unscoped,
            unscoped_rows,
        );
        report(
            &format!("one-workspace page @ {:>5} tasks", shape.tasks),
            &scoped,
            scoped_rows,
        );
        measured.push((shape.tasks as f64, unscoped.ms(), scoped.ms()));
    }

    let (small_tasks, small_unscoped, small_scoped) = measured[0];
    let (big_tasks, big_unscoped, big_scoped) = measured[measured.len() - 1];
    let content = big_tasks / small_tasks;
    let unscoped_grew = big_unscoped / small_unscoped.max(1e-9);
    let scoped_grew = big_scoped / small_scoped.max(1e-9);
    // log(time ratio) / log(content ratio): 1.0 is linear in the corpus, below
    // 1.0 is the sub-linearity the roadmap hoped for, above 1.0 is worse.
    let exponent = unscoped_grew.ln() / content.ln();
    println!(
        "  content grew {content:.0}x; unscoped page grew {unscoped_grew:.1}x \
         (exponent {exponent:.2}), one-workspace page grew {scoped_grew:.1}x"
    );
    assert!(
        exponent < 1.45,
        "a warm page grew as corpus^{exponent:.2} — gate (a.1) is 1.45, against 0.75-0.85 \
         measured with `attrs_probe` in place and 1.32-1.34 without it. Anything at or over \
         the ceiling is an added join or a lost index, not a busy machine: this is a ratio \
         of ratios, so the machine's state cancels"
    );
}

/// **Asking again is not more work** — roadmap §6 case (a), the second half.
///
/// Nine identical queries in a row, each timed on its own, and the ninth
/// compared against the *second*. The second rather than the first because the
/// first pays SQLite's statement preparation, which is a real cost paid once and
/// not an accumulation; what this is looking for is the shape where every query
/// leaves something behind — a cursor kept, a cache grown, a temp table not
/// dropped — and that shows as the ninth costing more than the second.
///
/// The structural twin of this claim, which is the one that runs in the ordinary
/// suite, is `repeated_queries_answer_identically_and_change_nothing` in
/// `src/store/tests.rs` (no row written) and
/// `repeated_warm_queries_open_no_document_and_accumulate_no_work` in
/// `apps/koru-store-server/tests/tasks_query.rs` (no document walked). This one
/// adds the only thing those cannot say: that the *time* is flat too.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn nine_repeated_warm_queries_do_not_get_slower() {
    println!("\n(a) nine repeated warm queries @ 5,000 tasks");
    let (p, _) = written(roadmap());
    let query = a_page();

    // Seven passes of nine, so each position has seven samples and one
    // scheduling hiccup at position nine does not decide the answer.
    const PASSES: usize = 7;
    let mut per_position: Vec<Vec<Duration>> = vec![Vec::with_capacity(PASSES); RUNS];
    for _ in 0..WARMUPS {
        let _ = p.task_rows(&query).expect("warmup");
    }
    for _ in 0..PASSES {
        for position in 0..RUNS {
            let at = Instant::now();
            let rows = p.task_rows(&query).expect("page").rows.len();
            per_position[position].push(at.elapsed());
            assert_eq!(rows, 100);
        }
    }
    let median = |mut s: Vec<Duration>| {
        s.sort();
        s[s.len() / 2].as_secs_f64() * 1000.0
    };
    let mut ms = Vec::with_capacity(RUNS);
    for (i, samples) in per_position.into_iter().enumerate() {
        let m = median(samples);
        println!("  run {:>2}: {m:>8.3} ms", i + 1);
        ms.push(m);
    }
    let second = ms[1];
    let ninth = ms[8];
    let grew = ninth / second.max(1e-9);
    println!("  run 9 / run 2 = {grew:.3}");
    assert!(
        grew < 1.25,
        "the ninth identical query cost {grew:.2}x the second — gate (a.2) is 1.25, against \
         0.97-1.02 observed over four runs with a per-position spread under 6%. The margin is \
         generous because the failure this catches is a *slope* — a cursor kept, a cache \
         grown — and a slope over nine queries is not a 20% step"
    );
}

/// **A cold workspace is a counter, never a wall clock** — roadmap §6 case (a),
/// the third part.
///
/// What a cold workspace costs is *nothing*, and the honest way to say so is to
/// count rather than to time: before its sweep lands it has no listing digest
/// (which is the fact the port turns into `updating_workspaces`) and answers no
/// rows, and the first answer over it is the same statement as any other — so a
/// timing here would be measuring the warm half of the same query.
///
/// The number this prints is the one the report quotes: what the *first* answer
/// after the sweep costs against the steady state, which is where a
/// convergence that re-read something would show.
///
/// The end-to-end counter evidence is not here and cannot be: it belongs to the
/// daemon, and it is
/// `a_cold_workspace_reports_updating_then_converges_without_opening_a_document`
/// in `apps/koru-store-server/tests/tasks.rs`, which asserts `projected`
/// unmoved across five warm queries and `+1` across one edit.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn a_cold_workspace_costs_nothing_until_its_sweep_lands() {
    println!("\n(a) cold workspace convergence @ 5,000 tasks");
    let mut p = Projections::in_memory().expect("read model");
    let corpus = roadmap().plan();
    corpus.write_into(&mut p, 1).expect("write the warm half");
    let cold = corpus.cold().next().expect("one cold workspace").id;

    let scoped = |workspace: WorkspaceId| TaskRowQuery {
        attributes: AttributeRowQuery {
            workspaces: vec![workspace],
            ..AttributeRowQuery::default()
        },
        limit: 100,
        ..TaskRowQuery::default()
    };

    assert!(
        p.listing_digest(cold).expect("digest").is_none(),
        "nothing has swept it, and that is the fact a port reports"
    );
    let (before, cold_rows) =
        Bench::of(|| p.task_rows(&scoped(cold)).expect("cold page").rows.len());
    report("cold workspace, before its sweep", &before, cold_rows);
    assert_eq!(cold_rows, 0, "it answers nothing rather than failing");

    let at = Instant::now();
    corpus.sweep_in(&mut p, cold, 2).expect("the sweep lands");
    println!(
        "  the sweep itself: {:>8.1} ms",
        at.elapsed().as_secs_f64() * 1000.0
    );

    // The *first* answer after convergence, timed on its own — this is the one
    // a person sees when a Tasks view opens on a workspace nobody has touched.
    let at = Instant::now();
    let first = p
        .task_rows(&scoped(cold))
        .expect("first warm page")
        .rows
        .len();
    let first_ms = at.elapsed().as_secs_f64() * 1000.0;
    let (steady, steady_rows) = Bench::of(|| p.task_rows(&scoped(cold)).expect("page").rows.len());
    println!("  first answer after the sweep: {first_ms:>8.3} ms");
    report("steady state on the same workspace", &steady, steady_rows);
    assert_eq!(first, steady_rows, "the first answer is the whole answer");
    assert!(first > 0, "it converged with no edit anywhere");
}

// ------------------------------------- documents carrying a changed reading

/// **What turning changed readings into documents costs, and the plan it
/// uses**, on a projection of the size `task_kind_sql`'s note names: about
/// 170,000 nodes, a quarter of the tasks pictures or files with a blob each.
///
/// `Projections::documents_carrying` is the search index's targeted re-index
/// when a reading lands (OCR TECHNICAL §7 "The index"). `nodes.blob` has no
/// index, so the plan has to start from the workspace; the unit test
/// `documents_carrying_starts_from_the_workspace_and_never_scans_nodes` pins
/// that shape on the real statement, and this prints it on a full corpus,
/// times it, and then times the same question with the partial index on
/// `nodes(blob)` that would make it proportional to the matches instead — so
/// whoever decides whether that index is worth a schema change has both
/// numbers. The index is created on this test's own database only.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn documents_carrying_a_changed_reading_at_170k_nodes() {
    println!("\n documents_carrying @ ~170,000 nodes");
    let name = koru_sqlite::MemoryName::fresh("documents-carrying");
    let mut p = Projections::in_named_memory(&name).expect("an in-memory read model");
    let corpus = TaskCorpus {
        seed: SEED,
        workspaces: 3,
        cold_workspaces: 0,
        documents: 300,
        tasks: 20_000,
        other_rows: 150_000,
    }
    .plan();
    corpus.write_into(&mut p, 1).expect("write the corpus");
    let side = koru_sqlite::open_memory(&name).expect("a second connection");
    let count = |sql: &str| -> i64 { side.query_row(sql, [], |r| r.get(0)).expect("count") };
    println!(
        "  nodes={} with a blob={} documents={}",
        count("SELECT COUNT(*) FROM nodes"),
        count("SELECT COUNT(*) FROM nodes WHERE blob IS NOT NULL"),
        count("SELECT COUNT(*) FROM documents"),
    );

    let workspace = corpus.warm().next().expect("a workspace");
    let hashes_of = |n: usize| -> Vec<String> {
        corpus
            .tasks()
            .filter(|t| t.workspace == workspace.id && matches!(t.kind, "image" | "file"))
            .take(n)
            .map(|t| format!("blob-{}", t.node))
            .collect()
    };

    // The statement as `documents_carrying_sql` writes it, for one hash.
    let shipped = "SELECT DISTINCT n.document_id
                     FROM documents d
                     JOIN nodes n ON n.document_id = d.document_id
                    WHERE d.workspace_id = ?1
                      AND +n.blob IN (?2)
                      AND n.in_trash = 0";
    // The statement for `n` hashes, and its plan with `n` placeholders bound.
    let widen =
        |sql: &str, n: usize| sql.replace("IN (?2)", &format!("IN ({})", vec!["?"; n].join(", ")));
    let plan = |sql: &str, n: usize| -> String {
        let parameters =
            std::iter::once(workspace.id.to_storage()).chain((0..n).map(|i| format!("blob-{i}")));
        side.prepare(&format!("EXPLAIN QUERY PLAN {}", widen(sql, n)))
            .expect("explain")
            .query_map(rusqlite::params_from_iter(parameters), |r| {
                r.get::<_, String>(3)
            })
            .expect("plan")
            .collect::<Result<Vec<_>, _>>()
            .expect("plan rows")
            .join(" | ")
    };

    for n in [1usize, 50, 500] {
        let shipped_plan = plan(shipped, n);
        println!("  plan, {n} hash(es): {shipped_plan}");
        assert!(
            !shipped_plan.contains("SCAN n"),
            "the shipped statement scans nodes: {shipped_plan}"
        );
        let hashes = hashes_of(n);
        let (bench, documents) = Bench::of(|| {
            p.documents_carrying(workspace.id, &hashes)
                .expect("documents")
                .len()
        });
        report(
            &format!("documents_carrying, {n} hash(es)"),
            &bench,
            documents,
        );
    }

    // **What an index would buy**, measured rather than argued. Partial, so it
    // holds only the nodes that have a blob. No `ANALYZE`: the shipped
    // databases carry no statistics, and neither does this comparison.
    side.execute_batch("CREATE INDEX nodes_blob ON nodes(blob) WHERE blob IS NOT NULL;")
        .expect("the candidate index");
    let indexed = "SELECT DISTINCT n.document_id
                     FROM nodes n
                     JOIN documents d ON d.document_id = n.document_id
                    WHERE d.workspace_id = ?1
                      AND n.blob IN (?2)
                      AND n.in_trash = 0";
    for n in [1usize, 50, 500] {
        println!(
            "  with nodes_blob, plan, {n} hash(es): {}",
            plan(indexed, n)
        );
        let hashes = hashes_of(n);
        let sql = widen(indexed, hashes.len());
        let (bench, documents) = Bench::of(|| {
            let mut statement = side.prepare_cached(&sql).expect("prepare");
            let parameters = std::iter::once(workspace.id.to_storage()).chain(hashes.clone());
            statement
                .query_map(rusqlite::params_from_iter(parameters), |r| {
                    r.get::<_, String>(0)
                })
                .expect("query")
                .count()
        });
        report(&format!("with nodes_blob, {n} hash(es)"), &bench, documents);
    }
}

// ------------------------------------------------- (b) the projection walk

/// **The walk is what one edit costs**, and it is linear in the document.
///
/// Roadmap §6 case (b), the second half. The first half — *one edit projects
/// exactly one document* — is a counter and already asserted: `projected` on
/// `/v1/debug/projection` moves by exactly one across one publish, in
/// `a_cold_workspace_reports_updating_then_converges_without_opening_a_document`
/// (`apps/koru-store-server/tests/tasks.rs`). Given that, the whole of an
/// edit's projection cost is one `walk::project` over the document that was
/// edited, and *that* is what this sizes.
///
/// Linear and not better: every node is read once, and there is nothing to
/// skip — a projection is a whole document by definition, which is why the
/// counter above is the load-bearing claim and this is the ruler beside it.
/// What it catches is the walk becoming O(nodes x depth), which is one
/// `in_trash` call per node away and is the exact mistake `walk.rs`'s comment
/// says was avoided.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn the_projection_walk_is_linear_in_the_document() {
    use koru_core::node::Node;
    use koru_core::schema::{Parent, WorkspaceDoc};

    println!("\n(b) walk::project, cost against document size");
    let build = |rows: usize| {
        let mut ws =
            WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Perf").expect("workspace");
        let page = ws
            .insert(Parent::Root, Node::document("Doc").expect("document"))
            .expect("insert");
        let mut section = page;
        for i in 0..rows {
            // Every tenth row opens a level, so the tree has depth rather than
            // being one flat list — depth is what an ancestors-per-node walk
            // would be quadratic in.
            if i % 10 == 0 {
                section = ws
                    .insert(
                        Parent::Node(page),
                        Node::paragraph(&format!("Section {i}")).expect("paragraph"),
                    )
                    .expect("insert");
            }
            let node = ws
                .insert(
                    Parent::Node(section),
                    Node::task(&format!("Task {i} with a reasonable amount of text on it"))
                        .expect("task"),
                )
                .expect("insert");
            if i % 3 == 0 {
                ws.set_attribute(
                    node,
                    koru_core::attributes::KEY_PRIORITY,
                    koru_core::node::AttrValue::Text("high".into()),
                )
                .expect("priority");
            }
        }
        ws
    };

    let mut measured = Vec::new();
    for rows in [100usize, 1_000, 5_000] {
        let ws = build(rows);
        let workspace = WorkspaceId::new();
        let document = DocumentId::new();
        let (bench, nodes) = Bench::of(|| {
            koru_projection::walk::project(&ws, workspace, document, vec![1])
                .expect("project")
                .nodes
                .len()
        });
        report(&format!("walk::project @ {rows} task rows"), &bench, nodes);
        measured.push((rows as f64, bench.ms()));
    }
    let (small_rows, small) = measured[0];
    let (big_rows, big) = measured[measured.len() - 1];
    let rows_grew = big_rows / small_rows;
    let time_grew = big / small.max(1e-9);
    let exponent = time_grew.ln() / rows_grew.ln();
    println!(
        "  {rows_grew:.0}x the document cost {time_grew:.1}x the walk (exponent {exponent:.2})"
    );
    // The exponent rather than a multiple of the row growth: at 50x, a "3x
    // linear" ceiling and a "1.28 exponent" are the same line, and only one of
    // them keeps meaning the same thing if somebody changes the sizes measured.
    assert!(
        exponent < 1.45,
        "the walk grew as document^{exponent:.2} — gate (b.2) is 1.45, against 1.18-1.22 over \
         five runs agreeing within 4%. What it catches is the walk becoming O(nodes x depth), \
         which is one `WorkspaceDoc::in_trash` call per node away and is exactly what \
         `walk.rs` carries the trash depth down the walk to avoid"
    );
}

// -------------------------------------------- (e) R-1b's three statements

/// **R-1b's cost note, re-measured under one protocol.**
///
/// Roadmap §6 case (e). The three statements a Tasks surface asks on open — the
/// rail's five bucket counts, the lists' counts, and one page of rows — measured
/// on the same corpus, in the same process, with the same warmups and the same
/// median, so the report carries one table rather than three numbers from three
/// occasions.
///
/// The interesting shape is the *order*: the counts walk the corpus and the page
/// does not, so the counts are where a covering index over `attrs` pays most and
/// the page is where it shows least.
///
/// **The index exists now** (`attrs_probe`, schema 8), and this test is what
/// priced it: it fell out at 4–8× rather than the 1.6–2× the report had sized,
/// because the plan was worse than the sizing assumed — two of the rail's probes
/// were reaching `attrs_value (role=?, value=?)` and scanning *every* completed
/// row in the corpus per candidate, not merely paying a row read each. The
/// decomposition at the bottom of this test is now the evidence that it worked:
/// turning both `attrs` predicates off saves nothing measurable, where before it
/// saved three quarters of the page.
///
/// So the last line prices the *other* sort key. What is left of a page is the
/// `ORDER BY`, and `TaskSort::Due` folds three temporal types onto one number
/// with a `strftime` per dated row — which no index over `attrs` reaches.
/// `TaskSort::Modified` is `d.modified_at` and asks for none of it, so the pair
/// says how much of what remains is the fold. It is a measurement rather than a
/// proposal: nothing here suggests changing a default sort.
#[test]
#[ignore = "a baseline run: cargo test --release -p koru-projection --test perf -- --ignored"]
fn the_three_statements_a_tasks_surface_asks_on_open() {
    println!("\n(e) the three statements @ 5,000 tasks / 55,000 rows");
    let (p, corpus) = written(roadmap());
    let workspace = corpus.warm().next().expect("a warm workspace").id;
    let w = windows();
    let od = overdue();

    let (counts, buckets) = Bench::of(|| {
        p.task_bucket_counts(&a_page(), &w)
            .expect("bucket counts")
            .len()
    });
    report("task_bucket_counts (the rail)", &counts, buckets);

    let (lists, listed) = Bench::of(|| {
        p.task_list_counts(&a_page(), Some(&od))
            .expect("list counts")
            .len()
    });
    report("task_list_counts (the lists)", &lists, listed);

    let (page, rows) = Bench::of(|| p.task_rows(&a_page()).expect("page").rows.len());
    report("task_rows (one page of 100)", &page, rows);

    let (bucket_page, bucket_rows) = Bench::of(|| {
        p.task_rows(&TaskRowQuery {
            bucket: Some(Bucket::Today),
            windows: Some(w.clone()),
            ..a_page()
        })
        .expect("bucket page")
        .rows
        .len()
    });
    report("task_rows (one bucket page)", &bucket_page, bucket_rows);

    let (all, all_rows) = Bench::of(|| {
        p.task_rows(&TaskRowQuery {
            lifecycle: LifecycleFilter::All,
            include_trashed: true,
            ..a_page()
        })
        .expect("page")
        .rows
        .len()
    });
    report("task_rows (nothing filtered out)", &all, all_rows);

    // The same three, scoped to one workspace — **what the Tasks tab sends**, so
    // the report can price the difference between the widest question the
    // surface can be asked and the one it actually asks.
    println!("  --- scoped to one workspace, which is what the view sends ---");
    let scope = |q: TaskRowQuery| TaskRowQuery {
        attributes: AttributeRowQuery {
            workspaces: vec![workspace],
            ..q.attributes
        },
        ..q
    };
    let (s_counts, s_buckets) = Bench::of(|| {
        p.task_bucket_counts(&scope(a_page()), &w)
            .expect("bucket counts")
            .len()
    });
    report("task_bucket_counts, one workspace", &s_counts, s_buckets);

    let (s_lists, s_listed) = Bench::of(|| {
        p.task_list_counts(&scope(a_page()), Some(&od))
            .expect("list counts")
            .len()
    });
    report("task_list_counts, one workspace", &s_lists, s_listed);

    let (s_page, s_rows) = Bench::of(|| p.task_rows(&scope(a_page())).expect("page").rows.len());
    report("task_rows, one workspace", &s_page, s_rows);

    let (s_bucket, s_bucket_rows) = Bench::of(|| {
        p.task_rows(&scope(TaskRowQuery {
            bucket: Some(Bucket::Today),
            windows: Some(w.clone()),
            ..a_page()
        }))
        .expect("bucket page")
        .rows
        .len()
    });
    report(
        "task_rows bucket page, one workspace",
        &s_bucket,
        s_bucket_rows,
    );

    println!(
        "  what a Tasks tab pays on open (rail + lists + page): {:.1} ms",
        s_counts.ms() + s_lists.ms() + s_page.ms()
    );

    // **Which predicate the time is in.** R-1b attributed the cost to the
    // per-row subqueries over `attrs`; two of them are switchable from the
    // outside, so the attribution can be *measured* rather than reasoned about.
    // Turning each off is not a proposal to turn it off — both are semantics —
    // it is how the report sizes what a covering index over `attrs` could
    // reach.
    println!("  --- which predicate the time is in (one workspace) ---");
    let (no_lifecycle, _) = Bench::of(|| {
        p.task_rows(&scope(TaskRowQuery {
            lifecycle: LifecycleFilter::All,
            ..a_page()
        }))
        .expect("page")
        .rows
        .len()
    });
    report("without the lifecycle predicate", &no_lifecycle, 100);

    let (with_trash, _) = Bench::of(|| {
        p.task_rows(&scope(TaskRowQuery {
            include_trashed: true,
            ..a_page()
        }))
        .expect("page")
        .rows
        .len()
    });
    report("without the node-bin predicate", &with_trash, 100);

    let (neither, _) = Bench::of(|| {
        p.task_rows(&scope(TaskRowQuery {
            lifecycle: LifecycleFilter::All,
            include_trashed: true,
            ..a_page()
        }))
        .expect("page")
        .rows
        .len()
    });
    report("without either", &neither, 100);

    // What is left once the `attrs` probes are covered: the sort key. The
    // default `Due` folds three temporal types onto one number with a
    // `strftime` per dated row; `Modified` reads a column.
    let (by_modified, _) = Bench::of(|| {
        p.task_rows(&scope(TaskRowQuery {
            sort: koru_projection::store::TaskSort::Modified,
            ..a_page()
        }))
        .expect("page")
        .rows
        .len()
    });
    report("sorted by modified, not by due", &by_modified, 100);

    println!(
        "  the two attrs predicates are {:.0}% of a scoped page ({:.1} ms of {:.1} ms)",
        (1.0 - neither.ms() / s_page.ms().max(1e-9)) * 100.0,
        s_page.ms() - neither.ms(),
        s_page.ms(),
    );

    // The one claim worth asserting rather than only printing: the counts walk
    // the corpus and the page does not, so a page that ever costs *more* than
    // the counts means the page has started walking it too.
    assert!(
        page.ms() < counts.ms() * 2.0,
        "a page of 100 cost {:.1} ms against {:.1} ms for a statement that counts the whole \
         corpus — the page is no longer paging",
        page.ms(),
        counts.ms(),
    );
}
