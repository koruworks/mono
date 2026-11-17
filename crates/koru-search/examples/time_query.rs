// SPDX-License-Identifier: MPL-2.0

//! Time search queries against an index file, with and without the two
//! opt-in behaviours, so a slow query is a number rather than a feeling.
//!
//! ```text
//! cargo run -p koru-search --example time_query -- <search.db> [--workspace <id>] <words> [<words> ...]
//! ```
//!
//! `--workspace <id>` scopes every query to one workspace, as a search opened
//! inside one is.
//!
//! Point it at a **copy** of an index: opening one runs its schema statements.
//! It prints counts and milliseconds only, never a word of anybody's text.

use std::time::Instant;

use koru_search::SearchIndex;
use koru_search::model::{SearchQuery, SearchSort};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("the path of a search.db");
    let mut workspace = None;
    let mut queries: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--workspace" {
            workspace = args
                .next()
                .and_then(|id| koru_store_core::ids::WorkspaceId::parse(&id));
        } else {
            queries.push(arg);
        }
    }
    let started = Instant::now();
    let index = SearchIndex::at(std::path::Path::new(&path)).expect("the index opens");
    println!("open: {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
    let started = Instant::now();
    index.warm_fuzzy_vocabulary().expect("the vocabulary warms");
    println!(
        "warm vocabulary: {:.1} ms, {} fragments",
        started.elapsed().as_secs_f64() * 1e3,
        index.fragment_count().unwrap_or(0)
    );
    for (n, text) in queries.iter().enumerate() {
        for sort in [SearchSort::Relevance, SearchSort::LastUsedWorkspace] {
            for (lists, tasks) in [(false, false), (true, false), (false, true), (true, true)] {
                let query = SearchQuery {
                    text: text.clone(),
                    include_lists: lists,
                    task_results: tasks,
                    sort,
                    workspaces: workspace.into_iter().collect(),
                    ..SearchQuery::default()
                };
                // Twice: the second is the warm number a person typing sees.
                let mut took = [0.0; 2];
                let mut hits = 0;
                for slot in &mut took {
                    let started = Instant::now();
                    hits = index.query(&query, &[]).map_or(0, |page| page.hits.len());
                    *slot = started.elapsed().as_secs_f64() * 1e3;
                }
                println!(
                    "query {n} ({} chars) sort={sort:?} lists={lists} tasks={tasks}: \
                     {:.1} ms then {:.1} ms, {hits} hits",
                    text.chars().count(),
                    took[0],
                    took[1],
                );
                if let Some(t) = index.last_query_timing() {
                    println!(
                        "    words {:.1} ms, page {:.1} ms, hits {:.1} ms, {} word-matches",
                        t.evidence_ms, t.statement_ms, t.hits_ms, t.evidence_rows
                    );
                }
            }
        }
    }
}
