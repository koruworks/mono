// SPDX-License-Identifier: MPL-2.0

//! Finding, ranking and explaining.
//!
//! The shape is the old app search index's, re-keyed. Three things changed and each one
//! is a simplification rather than a port:
//!
//! - **Scope joins key on a document, not a partition.** Workspace scope is the
//!   fragment with no document; document scope is that document's `title`
//!   fragment.
//! - **Lifecycle is a join, not a column.** It lives on the mirrored listing,
//!   one row per document, so a hit and its own title can no longer disagree
//!   about whether they are in the trash.
//! - **Breadcrumbs are assembled afterwards**, in Rust, from the listing rows
//!   the page touched. Nothing stores a path, so a rename needs no repair.
//!
//! ## The evidence table
//!
//! Every term, and every fuzzy alternative for it, is resolved to fragment ids
//! in a temp table first. A hit then has to be *explained*: which terms matched
//! it directly, which matched only its workspace or document title, and at what
//! edit distance. That is what makes the snippet highlight the right bytes and
//! the corrected spelling honest.
//!
//! ## A `#tag` is a filter, and it lives in `candidates` alone
//!
//! [`split_tag_filters`] lifts every `#word` out of the text before a term is
//! ever formed, so a tag is never a term: it does not have to be *covered*, it
//! earns no tier, it is not spelled back in `corrected_query`, and it never
//! becomes an FTS lookup. What it becomes is one correlated `EXISTS` per tag in
//! the `candidates` CTE, keyed on `(document_id, node_id)` — which is what makes
//! it **node-scoped**: `Hello #fun` finds the line that both reads *Hello* and
//! carries the tag, not every line in a document that has one somewhere
//! ([ADR-207](../../../docs/decisions/ADR-201-225.md#adr-207-tagfilter--a-tag-in-a-query-is-a-node-scoped-filter-not-a-term-and-it-lives-in-candidates-alone)).
//!
//! Everything below `candidates` therefore sees exactly what it saw before, so
//! ranking, dedup and `next_offset` are unchanged for a query with no tag in it.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use rusqlite::{Connection, params, params_from_iter, types::Value};

use crate::model::{
    AttrFilter, AttrOp, Lifecycle, LifecycleFilter, SearchHit, SearchPage, SearchQuery,
    SearchResult, SearchSort, field,
};
use crate::normalize::{EvidenceTerm, matching_evidence_spans, normalize, query_terms, snippet};
use crate::storage::{Listed, SearchIndex};
use crate::tags::TAG_ROLE;

/// The field token a tag fragment carries, spelled from the two constants that
/// declare it rather than a third time here.
static TAG_FIELD: LazyLock<String> = LazyLock::new(|| field::attr(TAG_ROLE));

/// What a `#tag` in the query text meant, separated from what is left to search
/// for.
///
/// Both halves are in **search form** — the caller normalizes once and this
/// splits the result, so a tag folds case and diacritics exactly the way the
/// `normalized` column it is compared against does.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Facets {
    /// The query with every `#tag` removed, whitespace collapsed.
    pub text: String,
    /// Tag names in the order they were typed, without their `#`, deduplicated.
    pub tags: Vec<String>,
}

/// What may follow the `#` — alphanumerics, plus the three joiners a tag name
/// is spelled with: `/` for the hierarchy (which is what makes `#fun` an
/// ancestor of `#fun/home`), and `-`/`_` because a two-word tag is usually
/// written with one of them.
///
/// **The definition lives in `koru-tagname`**, because the search box completes
/// a half-typed `#tag` against this exact rule. A box that offered a completion
/// this parser then declined to read as a filter would be worse than one that
/// offered nothing, and the only way to be sure is for both to ask the same
/// function.
use koru_tagname::is_tag_char;

/// Lift every `#tag` out of a normalized query.
///
/// **A `#` counts only where a word starts** — the beginning of the query or
/// after whitespace. So `a#b` is the text it has always been, and `##a` is
/// literal text too: the first `#` names nothing and the second does not start
/// a word. A `#` with no name after it is dropped rather than searched for,
/// because it is a filter halfway through being typed and nobody is looking for
/// the character.
///
/// **A bare word is untouched.** `amazing` with no `#` still searches for the
/// word and still finds the block whose tag is spelled that way
/// ([ADR-205](../../../docs/decisions/ADR-201-225.md#adr-205-tagsearch--a-tag-is-indexed-by-its-name-resolved-at-index-time-and-a-rename-re-indexes-what-carries-it)); nothing here
/// narrows it.
pub(crate) fn split_tag_filters(normalized: &str) -> Facets {
    let chars: Vec<char> = normalized.chars().collect();
    let mut kept = String::with_capacity(normalized.len());
    let mut tags: Vec<String> = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let starts_word = at == 0 || chars[at - 1].is_whitespace();
        if chars[at] == '#' && starts_word {
            let mut end = at + 1;
            while end < chars.len() && is_tag_char(chars[end]) {
                end += 1;
            }
            let name: String = chars[at + 1..end].iter().collect();
            if !name.is_empty() && !tags.contains(&name) {
                tags.push(name);
            }
            at = end;
            continue;
        }
        kept.push(chars[at]);
        at += 1;
    }
    Facets {
        text: kept.split_whitespace().collect::<Vec<_>>().join(" "),
        tags,
    }
}

/// One kind's half of an [`AttrFilter`], as SQL over `attr_values`.
///
/// `numeric` decides the column, and therefore the *comparison*: a `number` or
/// a `datetime_zoned` is compared as a number, everything else as folded text.
/// That is why a bound is bound twice over — once per kind — rather than once
/// for the filter: the same `2026-09-10` is a string to a `date` row and
/// nothing at all to an instant, which needs the caller's epoch bound instead.
///
/// **A malformed operand answers `None`**, and the caller turns a filter with
/// no satisfiable kind into `AND 0`. Refusing to compare is right: a bound
/// that will not parse as a number is not "zero", which is 1970.
///
/// **A numeric kind reads `numeric_values` when the filter carries them**, and
/// `values` when it does not. That is what lets one filter name `date` and
/// `datetime_zoned` at once: the two do not share a column, so the same bound
/// arrives twice — `2026-09-10` for the calendar day, epoch milliseconds for
/// the instant — computed by the layer that knows the zone. Without it the
/// instant half drops out here (an ISO date does not parse as a number) and
/// the window silently answers for all-day deadlines only.
///
/// `Missing` builds the same existence test as `Exists`; the caller negates it.
fn predicate(filter: &AttrFilter, numeric: bool, values: &mut Vec<Value>) -> Option<String> {
    let operands: &[String] = if numeric && !filter.numeric_values.is_empty() {
        &filter.numeric_values
    } else {
        &filter.values
    };
    let mut bind = |raw: &str| -> Option<usize> {
        if numeric {
            values.push(Value::Real(raw.trim().parse::<f64>().ok()?));
        } else {
            values.push(Value::Text(normalize(raw).text));
        }
        Some(values.len())
    };
    let column = if numeric { "v.num" } else { "v.value" };
    match filter.op {
        AttrOp::Exists | AttrOp::Missing => Some("1".to_owned()),
        AttrOp::Is => {
            let v = bind(operands.first()?)?;
            Some(format!("{column}=?{v}"))
        }
        AttrOp::OneOf => {
            let mut any = Vec::new();
            for value in operands {
                if let Some(v) = bind(value) {
                    any.push(format!("{column}=?{v}"));
                }
            }
            (!any.is_empty()).then(|| any.join(" OR "))
        }
        AttrOp::Before => {
            let v = bind(operands.first()?)?;
            Some(format!("{column}<?{v}"))
        }
        AttrOp::After => {
            let v = bind(operands.first()?)?;
            Some(format!("{column}>=?{v}"))
        }
        AttrOp::Between => {
            let lo = bind(operands.first()?)?;
            let hi = bind(operands.get(1)?)?;
            Some(format!("{column}>=?{lo} AND {column}<?{hi}"))
        }
    }
}

/// A tag name as a `LIKE` pattern matching its descendants — `fun` → `fun/%`.
///
/// **Escaped, because a name is user content.** `LIKE` reads `_` as *any
/// character* and `%` as *any run*, so a tag spelled `q_1` would otherwise
/// filter in `qx1/…` as well. The `ESCAPE '\'` clause at the call site is what
/// makes the backslashes here mean a literal.
fn like_descendants(normalized_tag: &str) -> String {
    let mut pattern = String::with_capacity(normalized_tag.len() + 2);
    for ch in normalized_tag.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push_str("/%");
    pattern
}

/// How a filter is spelled in the SQL below.
///
/// **Here rather than on the type.** `LifecycleFilter` lives in
/// `koru-store-wire`, which three crates link and none of which should learn a
/// sort code from a `CASE` expression in this file.
pub(crate) fn lifecycle_code(filter: LifecycleFilter) -> i64 {
    match filter {
        LifecycleFilter::Live => 0,
        LifecycleFilter::Trash => 1,
        LifecycleFilter::All => 2,
    }
}

/// The same, for the ordering. See [`lifecycle_code`].
pub(crate) fn sort_code(sort: SearchSort) -> i64 {
    match sort {
        SearchSort::Relevance => 0,
        SearchSort::ObservedModified => 1,
        SearchSort::LastUsedWorkspace => 2,
    }
}

/// Whether a document in this state is wanted under this filter.
pub(crate) fn wanted(filter: LifecycleFilter, lifecycle: Lifecycle) -> bool {
    match filter {
        LifecycleFilter::All => true,
        LifecycleFilter::Live => lifecycle == Lifecycle::Live,
        LifecycleFilter::Trash => lifecycle == Lifecycle::Trashed,
    }
}

/// Workspace, then the document's title path, root first.
///
/// **Assembled, never stored.** The alternative is a breadcrumb column on every
/// fragment and a rename that rewrites every descendant — the write
/// amplification the whole design is arranged to avoid.
pub(crate) fn breadcrumb(
    workspace_name: &str,
    listed: Option<&Listed>,
    listing: &HashMap<String, Listed>,
) -> String {
    let mut names = vec![workspace_name.to_owned()];
    let mut path = Vec::new();
    let mut at = listed;
    // A parent chain is a tree, but a corrupt one could be a cycle. Bounded by
    // the listing's own size, because a hung query is worse than a short path.
    for _ in 0..listing.len().max(1) {
        let Some(current) = at else { break };
        path.push(current.title.clone());
        at = current.parent.as_ref().and_then(|p| listing.get(p));
    }
    path.reverse();
    names.extend(path);
    names.join(" / ")
}

/// One statement, and the sort is a parameter rather than three of them.
///
/// **A workspace-name hit belongs to no document**, so under
/// `SearchSort::ObservedModified` it had nothing to order by and
/// `COALESCE(…,0)` put it below every document in the store — invisible to
/// anyone searching for a workspace by name. It orders by the freshest change
/// in that workspace instead, which is what "when was this workspace last
/// modified" means. The reported `observed_modified_at_ms` is left alone: it
/// documents the *containing document*, and a workspace has none.
///
/// This is load-bearing now that the surface opens on that sort
/// ([ADR-110](../../../docs/decisions/README.md)).
pub(crate) fn search(
    index: &SearchIndex,
    query: &SearchQuery,
    updating: &[String],
) -> SearchResult<SearchPage> {
    let connection = &index.connection;
    let facets = split_tag_filters(&normalize(query.text.trim()).text);
    let normalized_query = facets.text;
    // A surface may also filter without anybody typing a `#`. Same form, same
    // list, and the typed ones come first because that is the order they were
    // read in.
    let mut tag_filters = facets.tags;
    for tag in &query.tags {
        let normalized = normalize(tag.trim()).text;
        if !normalized.is_empty() && !tag_filters.contains(&normalized) {
            tag_filters.push(normalized);
        }
    }
    let terms = query_terms(&normalized_query);
    // **Nothing to select by at all** — no words, no tag, no typed filter — so
    // there is no question to answer and the whole corpus is not it.
    //
    // `attributes` belongs in this test and was missing from it, which is the
    // difference between `/due:after 2026-08-29` answering and answering
    // *nothing*. The statement below is written for exactly this shape — with
    // no terms it reads `fragments` directly because "the filters *are* the
    // query" — and this guard returned before it could. The tests did not
    // catch it because every one of them passes a word beside the filter, so
    // the filter-only query, which is the ordinary way to ask *what is due
    // this month*, was the one shape nothing tried.
    if terms.is_empty() && tag_filters.is_empty() && query.attributes.is_empty() {
        return Ok(SearchPage {
            hits: Vec::new(),
            next_offset: None,
            updating_workspaces: updating.to_vec(),
            corrected_query: None,
            request_seq: query.request_seq,
        });
    }

    // Hold the read guard through the statement. The writer takes the write
    // side immediately before its commit and releases it after, so evidence and
    // fragment rows are one coherent generation even across two WAL
    // connections.
    let vocabulary = index.vocabulary.read();
    prepare_evidence(connection, &vocabulary, &terms)?;

    let mut values = vec![
        Value::Integer(lifecycle_code(query.lifecycle)),
        Value::Integer(terms.len() as i64),
        Value::Text(normalized_query.clone()),
        Value::Integer(sort_code(query.sort)),
    ];
    let mut scope = String::new();
    if !query.workspaces.is_empty() {
        scope.push_str(" AND f.workspace_id IN (");
        for (i, workspace) in query.workspaces.iter().enumerate() {
            if i > 0 {
                scope.push(',');
            }
            values.push(Value::Text(workspace.to_storage()));
            scope.push_str(&format!("?{}", values.len()));
        }
        scope.push(')');
    }
    for workspace in &query.excluded_workspaces {
        values.push(Value::Text(workspace.to_storage()));
        scope.push_str(&format!(" AND f.workspace_id<>?{}", values.len()));
    }
    let mut kinds = String::new();
    if !query.kinds.is_empty() {
        kinds.push_str(" AND f.kind IN (");
        for (i, kind) in query.kinds.iter().enumerate() {
            if i > 0 {
                kinds.push(',');
            }
            values.push(Value::Text(kind.clone()));
            kinds.push_str(&format!("?{}", values.len()));
        }
        kinds.push(')');
    }
    if query.entity_picker {
        // The typed task model is marker OR due. Primary fragments carry the
        // authored task label; attribute fragments would show a token instead.
        // A title remains a document/file choice. This reads existing tables.
        kinds.push_str(
            " AND ((f.field='title' AND f.node_id IS NULL) OR \
            (f.node_id IS NOT NULL AND f.stable_key LIKE 'node:%:primary' AND EXISTS (\
                SELECT 1 FROM attr_values task WHERE task.document_id=f.document_id \
                AND task.node_id=f.node_id AND task.role IN ('koru.task','koru.due'))))",
        );
    }
    // **The semi-join, and the whole of the feature.** Correlated on the node
    // rather than the document, so a tag qualifies the line that carries it;
    // `LIKE …/%` is the hierarchy, so `#fun` admits `#fun/home`; and the
    // comparison is on `normalized` because that is the folded column the tag's
    // own fragment was written into, which is what makes `#FUN` find `#fun`.
    let mut tags = String::new();
    for tag in &tag_filters {
        values.push(Value::Text(TAG_FIELD.clone()));
        let field = values.len();
        values.push(Value::Text(tag.clone()));
        let exact = values.len();
        values.push(Value::Text(like_descendants(tag)));
        let descendants = values.len();
        tags.push_str(&format!(
            " AND EXISTS (
                 SELECT 1 FROM fragments t
                 WHERE t.document_id=f.document_id AND t.node_id=f.node_id
                   AND t.field=?{field}
                   AND (t.normalized=?{exact}
                        OR t.normalized LIKE ?{descendants} ESCAPE '\\')
               )"
        ));
    }
    // **The typed half of the same idea**, and the same shape: correlated on
    // the node, in `candidates` alone, so coverage and scoring are untouched
    // and no hit is duplicated.
    //
    // Every predicate is guarded by the row's own `kind`, so a stored value is
    // only ever compared against a filter that named its kind — which is how
    // the three temporal types stay three (ADR-189). `number` and
    // `datetime_zoned` compare on `num`; everything else on the folded `value`,
    // whose ISO forms order correctly within one kind.
    let mut attrs = String::new();
    for filter in &query.attributes {
        values.push(Value::Text(filter.key.clone()));
        let key = values.len();
        // An empty `kinds` matches nothing rather than everything: a filter
        // that forgot to say which kinds it meant has asked no question, and
        // answering "all of them" would compare a date against an instant.
        let mut per_kind: Vec<String> = Vec::new();
        for kind in &filter.kinds {
            let numeric = matches!(kind.as_str(), "number" | "datetime_zoned");
            values.push(Value::Text(kind.clone()));
            let k = values.len();
            let Some(test) = predicate(filter, numeric, &mut values) else {
                continue;
            };
            per_kind.push(format!("(v.kind=?{k} AND {test})"));
        }
        if per_kind.is_empty() {
            // Nothing can satisfy it. Say so in SQL rather than dropping the
            // filter, which would widen the answer instead of emptying it.
            attrs.push_str(" AND 0");
            continue;
        }
        let body = per_kind.join(" OR ");
        let exists = format!(
            "EXISTS (
                 SELECT 1 FROM attr_values v
                 WHERE v.document_id=f.document_id AND v.node_id=f.node_id
                   AND v.role=?{key} AND ({body})
               )"
        );
        if filter.op == AttrOp::Missing {
            attrs.push_str(&format!(" AND NOT {exists}"));
        } else {
            attrs.push_str(&format!(" AND {exists}"));
        }
    }

    let page_size = query.page_size.clamp(1, 200);
    // One more than the page, so `next_offset` is knowable without a count.
    values.push(Value::Integer(page_size as i64 + 1));
    let limit = values.len();
    values.push(Value::Integer(query.offset as i64));
    let offset = values.len();

    // With no terms there is no evidence to join and nothing to cover: the
    // filters *are* the query, and every fragment of a node that passes them is
    // a hit. The scoring below still runs, and every one of its term-shaped
    // tests reads `0 = ?2` — uniform across the page, so the field tier and the
    // ordering decide it, which is the same ranking with the term half removed.
    let source = if terms.is_empty() {
        "fragments f"
    } else {
        "search_query_evidence direct
           JOIN fragments f ON f.id=direct.fragment_id"
    };
    let qualified = if terms.is_empty() {
        UNCOVERED
    } else {
        COVERAGE
    };

    let sql = format!(
        "WITH candidates AS (
           SELECT DISTINCT f.id
           FROM {source}
           LEFT JOIN listing l ON l.document_id=f.document_id
           WHERE ({LIFECYCLE})
             {scope}
             {kinds}
             {tags}
             {attrs}
         ),
         {qualified}
         SELECT f.workspace_id, f.document_id, f.node_id, f.kind, f.field, f.text,
                f.stable_key, d.observed_modified_at_ms, recent.last_used_at_ms,
                (CASE
                   WHEN q.max_distance=0 AND q.direct_count=?2 THEN 4000
                   WHEN q.max_distance=0 THEN 3000
                   WHEN q.direct_count=?2 THEN 2000
                   ELSE 1000
                 END)
                + (CASE
                   WHEN f.field IN ('workspace','title') THEN 700
                   WHEN f.field='heading-1' THEN 600
                   WHEN f.field='heading-2' THEN 580
                   WHEN f.field='heading-3' THEN 560
                   WHEN f.field='heading-4' THEN 540
                   WHEN f.field IN ('heading-5','heading-6') THEN 520
                   WHEN f.field IN ('name','list-name','contact','level-name') THEN 400
                   WHEN f.field='task' THEN 300
                   WHEN f.field='body' THEN 200
                   ELSE 100
                 END)
                + (CASE
                   WHEN f.normalized=?3 THEN 30
                   WHEN instr(f.normalized,?3)=1 THEN 20
                   WHEN instr(f.normalized,?3)>0 THEN 10
                   WHEN q.direct_count=?2 THEN 5
                   ELSE 0
                 END)
                - q.total_distance*10 AS score
         FROM qualified q
         JOIN fragments f ON f.id=q.candidate_id
         LEFT JOIN documents d ON d.document_id=f.document_id
         LEFT JOIN (
           SELECT workspace_id, MAX(opened_at_ms) AS last_used_at_ms
           FROM recent_targets GROUP BY workspace_id
         ) recent ON recent.workspace_id=f.workspace_id
         LEFT JOIN (
           SELECT workspace_id, MAX(observed_modified_at_ms) AS observed_modified_at_ms
           FROM documents GROUP BY workspace_id
         ) ws_change ON ws_change.workspace_id=f.workspace_id
         ORDER BY
           CASE WHEN ?4=1
             THEN COALESCE(d.observed_modified_at_ms,
                           ws_change.observed_modified_at_ms, 0) END DESC,
           CASE WHEN ?4=2 THEN COALESCE(recent.last_used_at_ms,0) END DESC,
           score DESC, f.text COLLATE NOCASE, f.stable_key
         LIMIT ?{limit} OFFSET ?{offset}"
    );

    let mut statement = connection.prepare(&sql)?;
    let raw = statement
        .query_map(params_from_iter(values), |r| {
            Ok(RawHit {
                workspace: r.get(0)?,
                document: r.get(1)?,
                node: r.get(2)?,
                kind: r.get(3)?,
                field: r.get(4)?,
                text: r.get(5)?,
                observed_modified_at_ms: r.get(7)?,
                workspace_last_used_at_ms: r.get(8)?,
                score: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let workspaces: Vec<String> = {
        let mut all: Vec<String> = raw.iter().map(|r| r.workspace.clone()).collect();
        all.sort();
        all.dedup();
        all
    };
    let listing = index.listing_of(&workspaces)?;
    let names = index.workspace_names()?;

    let mut hits = Vec::with_capacity(raw.len());
    let mut corrected = terms.clone();
    let mut has_correction = false;
    for row in raw {
        let evidence = evidence_for(connection, &row)?;
        if hits.is_empty() {
            for term in evidence.direct.iter().chain(evidence.scope.iter()) {
                if term.fuzzy
                    && let Some(original) = corrected.get_mut(term.ordinal as usize)
                    && *original != term.text
                {
                    *original = term.text.clone();
                    has_correction = true;
                }
            }
        }
        let listed = row.document.as_ref().and_then(|d| listing.get(d));
        let workspace_name = names.get(&row.workspace).cloned().unwrap_or_default();
        let path = breadcrumb(&workspace_name, listed, &listing);
        let direct = matching_evidence_spans(
            &row.text,
            evidence.direct.iter().map(|t| EvidenceTerm {
                text: &t.text,
                whole_token: t.fuzzy,
            }),
        );
        let (snippet_text, spans) = snippet(&row.text, &direct, 70);
        let breadcrumb_spans = matching_evidence_spans(
            &path,
            evidence.scope.iter().map(|t| EvidenceTerm {
                text: &t.text,
                whole_token: t.fuzzy,
            }),
        );
        hits.push(SearchHit {
            title: if query.entity_picker && row.node.is_some() {
                row.text.clone()
            } else {
                listed
                    .map(|l| l.title.clone())
                    .unwrap_or_else(|| workspace_name.clone())
            },
            workspace_name,
            workspace: row.workspace,
            document: row.document,
            node: row.node,
            document_kind: listed.map(|l| l.kind.clone()).unwrap_or_default(),
            mime: listed.and_then(|l| l.mime.clone()),
            kind: row.kind,
            field: row.field,
            breadcrumb: path,
            breadcrumb_spans,
            snippet: snippet_text,
            spans,
            lifecycle: listed.map(Listed::lifecycle).unwrap_or_default(),
            trashed_at: listed.and_then(|l| l.trashed_at),
            trashed_by: listed.and_then(|l| l.trashed_by.clone()),
            score: row.score,
            observed_modified_at_ms: row.observed_modified_at_ms,
            workspace_last_used_at_ms: row.workspace_last_used_at_ms,
        });
    }
    let has_more = hits.len() > page_size;
    hits.truncate(page_size);
    Ok(SearchPage {
        hits,
        next_offset: has_more.then_some(query.offset + page_size),
        updating_workspaces: updating.to_vec(),
        corrected_query: has_correction.then(|| corrected.join(" ")),
        request_seq: query.request_seq,
    })
}

/// A workspace fragment has no document and is therefore never in the trash;
/// everything else follows its document's listing row.
const LIFECYCLE: &str = "?1=2
             OR (?1=0 AND (f.document_id IS NULL OR l.trashed_at IS NULL))
             OR (?1=1 AND l.trashed_at IS NOT NULL)";

/// What every term explaining a candidate was, and how far off it was.
///
/// A candidate qualifies only by covering **all** of them — `?2` is the count —
/// from its own text, its workspace's name, or its document's title, in that
/// order of preference.
const COVERAGE: &str = "coverage_raw AS (
           SELECT c.id AS candidate_id, e.term_ordinal, e.distance, 0 AS scope
           FROM candidates c
           JOIN search_query_evidence e ON e.fragment_id=c.id
           UNION ALL
           SELECT c.id, e.term_ordinal, e.distance, 1
           FROM candidates c
           JOIN fragments target ON target.id=c.id
           JOIN fragments ws
             ON ws.workspace_id=target.workspace_id AND ws.document_id IS NULL
           JOIN search_query_evidence e ON e.fragment_id=ws.id
           UNION ALL
           SELECT c.id, e.term_ordinal, e.distance, 2
           FROM candidates c
           JOIN fragments target ON target.id=c.id
           JOIN fragments doc
             ON doc.document_id=target.document_id AND doc.field='title'
           JOIN search_query_evidence e ON e.fragment_id=doc.id
         ),
         term_coverage AS (
           SELECT candidate_id, term_ordinal, MIN(distance) AS distance,
                  MAX(CASE WHEN scope=0 THEN 1 ELSE 0 END) AS direct
           FROM coverage_raw GROUP BY candidate_id, term_ordinal
         ),
         qualified AS (
           SELECT candidate_id, COUNT(*) AS covered, SUM(direct) AS direct_count,
                  MAX(distance) AS max_distance, SUM(distance) AS total_distance
           FROM term_coverage GROUP BY candidate_id HAVING covered=?2
         )";

/// The same shape for a query that is only filters — `#fun` on its own.
///
/// Nothing to cover, so `candidates` is already the answer. The zeros are what
/// the scoring reads, and they are the values a fully, exactly covered
/// candidate would have carried: nothing is fuzzy, nothing is missing.
const UNCOVERED: &str = "qualified AS (
           SELECT c.id AS candidate_id, 0 AS covered, 0 AS direct_count,
                  0 AS max_distance, 0 AS total_distance
           FROM candidates c
         )";

fn prepare_evidence(
    connection: &Connection,
    vocabulary: &crate::fuzzy::Vocabulary,
    terms: &[String],
) -> SearchResult<()> {
    connection.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS search_query_evidence (
           term_ordinal INTEGER NOT NULL,
           fragment_id  INTEGER NOT NULL,
           matched_term TEXT NOT NULL,
           distance     INTEGER NOT NULL,
           PRIMARY KEY(term_ordinal, fragment_id, matched_term)
         ) WITHOUT ROWID;
         DELETE FROM search_query_evidence;",
    )?;
    let mut insert = connection.prepare_cached(
        "INSERT OR IGNORE INTO search_query_evidence(
           term_ordinal, fragment_id, matched_term, distance
         ) SELECT ?1, rowid, ?2, ?3 FROM fragment_fts WHERE fragment_fts MATCH ?4",
    )?;
    for (ordinal, term) in terms.iter().enumerate() {
        insert.execute(params![ordinal as i64, term, 0_i64, fts_term(term, true)])?;
        for candidate in vocabulary.candidates(term) {
            insert.execute(params![
                ordinal as i64,
                candidate.term,
                candidate.distance as i64,
                fts_term(&candidate.term, false)
            ])?;
        }
    }
    Ok(())
}

fn fts_term(term: &str, prefix: bool) -> String {
    let escaped = term.replace('"', "\"\"");
    if prefix {
        format!("\"{escaped}\"*")
    } else {
        format!("\"{escaped}\"")
    }
}

#[derive(Default)]
struct HitEvidence {
    direct: Vec<SelectedTerm>,
    scope: Vec<SelectedTerm>,
}

#[derive(Clone)]
struct SelectedTerm {
    ordinal: i64,
    text: String,
    fuzzy: bool,
}

fn evidence_for(connection: &Connection, hit: &RawHit) -> SearchResult<HitEvidence> {
    let mut evidence = HitEvidence::default();
    let mut direct = connection.prepare(
        "SELECT e.term_ordinal, e.matched_term, e.distance
         FROM search_query_evidence e
         JOIN fragments f ON f.id=e.fragment_id
         WHERE f.workspace_id=?1
           AND ((?2 IS NULL AND f.document_id IS NULL) OR f.document_id=?2)
           AND ((?3 IS NULL AND f.node_id IS NULL) OR f.node_id=?3)
           AND f.field=?4
         ORDER BY e.term_ordinal, e.distance, e.matched_term",
    )?;
    evidence.direct = select_best_terms(direct.query_map(
        params![hit.workspace, hit.document, hit.node, hit.field],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)?;

    let mut scope = connection.prepare(
        "SELECT e.term_ordinal, e.matched_term, e.distance
         FROM search_query_evidence e
         JOIN fragments f ON f.id=e.fragment_id
         WHERE f.workspace_id=?1
           AND (f.document_id IS NULL OR (f.document_id=?2 AND f.field='title'))
         ORDER BY e.term_ordinal, e.distance, e.matched_term",
    )?;
    evidence.scope =
        select_best_terms(scope.query_map(params![hit.workspace, hit.document], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?)?;
    // A term explained directly is not also explained by the path.
    evidence
        .scope
        .retain(|s| !evidence.direct.iter().any(|d| d.ordinal == s.ordinal));
    Ok(evidence)
}

fn select_best_terms(
    rows: impl Iterator<Item = rusqlite::Result<(i64, String, i64)>>,
) -> SearchResult<Vec<SelectedTerm>> {
    let mut selected = BTreeMap::new();
    for row in rows {
        let (ordinal, text, distance) = row?;
        selected.entry(ordinal).or_insert(SelectedTerm {
            ordinal,
            text,
            fuzzy: distance > 0,
        });
    }
    Ok(selected.into_values().collect())
}

struct RawHit {
    workspace: String,
    document: Option<String>,
    node: Option<String>,
    kind: String,
    field: String,
    text: String,
    observed_modified_at_ms: Option<i64>,
    workspace_last_used_at_ms: Option<i64>,
    score: f64,
}

/// Keeps the tokens in [`field`] and the boosts above from drifting apart.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Fragment;
    use koru_projection::model::ListedDocument;
    use koru_store_core::ids::{DocumentId, WorkspaceId};

    // ------------------------------------------------------- the parse

    fn split(text: &str) -> (String, Vec<String>) {
        let facets = split_tag_filters(&normalize(text.trim()).text);
        (facets.text, facets.tags)
    }

    #[test]
    fn a_hash_word_becomes_a_filter_and_leaves_the_text() {
        assert_eq!(
            split("Hello #amazing"),
            ("hello".to_owned(), vec!["amazing".to_owned()])
        );
        assert_eq!(
            split("#amazing Hello"),
            ("hello".to_owned(), vec!["amazing".to_owned()])
        );
    }

    /// The whole of ADR-182 still has to work: a word with no `#` is a word.
    #[test]
    fn a_bare_word_is_never_a_filter() {
        assert_eq!(split("amazing"), ("amazing".to_owned(), Vec::new()));
    }

    #[test]
    fn a_filter_folds_case_and_carries_its_hierarchy() {
        assert_eq!(
            split("#FUN/Home"),
            (String::new(), vec!["fun/home".to_owned()])
        );
    }

    /// Each of these was a decision, and each is cheap to get wrong later.
    #[test]
    fn a_hash_that_names_nothing_is_not_a_filter() {
        // Nothing after it: a filter halfway through being typed, dropped
        // rather than searched for as a character.
        assert_eq!(split("#"), (String::new(), Vec::new()));
        assert_eq!(split("hello #"), ("hello".to_owned(), Vec::new()));
        // The first `#` names nothing; the second does not start a word.
        assert_eq!(split("##a"), ("#a".to_owned(), Vec::new()));
        // Mid-word, so it is the text it has always been.
        assert_eq!(split("a#b"), ("a#b".to_owned(), Vec::new()));
    }

    #[test]
    fn two_filters_are_both_kept_and_a_repeat_is_not() {
        assert_eq!(
            split("#fun #home #fun"),
            (String::new(), vec!["fun".to_owned(), "home".to_owned()])
        );
    }

    /// `LIKE` reads `_` and `%` as wildcards, and a tag name is user content.
    #[test]
    fn a_descendant_pattern_escapes_what_like_would_read() {
        assert_eq!(like_descendants("fun"), "fun/%");
        assert_eq!(like_descendants("q_1"), "q\\_1/%");
        assert_eq!(like_descendants("100%"), "100\\%/%");
    }

    // ------------------------------------------------- the filter, executed

    const WORKSPACE: &str = "w";

    /// One document whose nodes are `(text, tags)`.
    fn indexed(nodes: &[(&str, &[&str])]) -> SearchIndex {
        let mut index = SearchIndex::in_memory().unwrap();
        let document = DocumentId::new();
        let id = document.to_storage();
        index
            .set_listing(
                WORKSPACE,
                "Acme",
                b"d1",
                &[ListedDocument {
                    document,
                    parent: None,
                    ordinal: 0,
                    title: "Roadmap".to_owned(),
                    kind: "page".to_owned(),
                    mime: None,
                    trashed_at: None,
                    trashed_by: None,
                }],
            )
            .unwrap();
        let mut fragments = Vec::new();
        for (seq, (text, tags)) in nodes.iter().enumerate() {
            let node = format!("n{seq}");
            fragments.push(Fragment {
                key: format!("node:{id}:{node}:primary"),
                workspace: WORKSPACE.to_owned(),
                document: Some(id.clone()),
                node: Some(node.clone()),
                seq: seq as i64 + 1,
                kind: "paragraph".to_owned(),
                field: field::BODY.to_owned(),
                text: (*text).to_owned(),
            });
            for (i, tag) in tags.iter().enumerate() {
                fragments.push(Fragment {
                    key: format!("node:{id}:{node}:attr:a{i}"),
                    workspace: WORKSPACE.to_owned(),
                    document: Some(id.clone()),
                    node: Some(node.clone()),
                    seq: seq as i64 + 1,
                    kind: "paragraph".to_owned(),
                    field: field::attr(TAG_ROLE),
                    text: (*tag).to_owned(),
                });
            }
        }
        index
            .replace_document(WORKSPACE, &id, &fragments, &[], b"r1", 10)
            .unwrap();
        index.warm_fuzzy_vocabulary().unwrap();
        index
    }

    /// The snippets of every hit whose field is the node's own text, in order.
    fn bodies(index: &SearchIndex, text: &str) -> Vec<String> {
        index
            .query(
                &SearchQuery {
                    text: text.to_owned(),
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap()
            .hits
            .into_iter()
            .filter(|hit| hit.field == field::BODY)
            .map(|hit| hit.snippet)
            .collect()
    }

    /// The headline case, and the three halves of it that must each hold.
    /// Nodes with typed attribute rows: `(text, [(role, kind, value)])`.
    fn with_values(nodes: &[(&str, &[(&str, &str, &str)])]) -> SearchIndex {
        let mut index = SearchIndex::in_memory().unwrap();
        let document = DocumentId::new();
        let id = document.to_storage();
        index
            .set_listing(
                WORKSPACE,
                "Acme",
                b"d1",
                &[ListedDocument {
                    document,
                    parent: None,
                    ordinal: 0,
                    title: "Roadmap".to_owned(),
                    kind: "page".to_owned(),
                    mime: None,
                    trashed_at: None,
                    trashed_by: None,
                }],
            )
            .unwrap();
        let mut fragments = Vec::new();
        let mut values = Vec::new();
        for (seq, (text, attrs)) in nodes.iter().enumerate() {
            let node = format!("n{seq}");
            fragments.push(Fragment {
                key: format!("node:{id}:{node}:primary"),
                workspace: WORKSPACE.to_owned(),
                document: Some(id.clone()),
                node: Some(node.clone()),
                seq: seq as i64 + 1,
                kind: "paragraph".to_owned(),
                field: field::BODY.to_owned(),
                text: (*text).to_owned(),
            });
            for (role, kind, value) in attrs.iter() {
                values.push(crate::model::AttrRow {
                    workspace: WORKSPACE.to_owned(),
                    document: id.clone(),
                    node: node.clone(),
                    role: (*role).to_owned(),
                    kind: (*kind).to_owned(),
                    value: (*value).to_owned(),
                    num: matches!(*kind, "number" | "datetime_zoned")
                        .then(|| value.parse::<f64>().ok())
                        .flatten(),
                    zone: None,
                });
            }
        }
        index
            .replace_document(WORKSPACE, &id, &fragments, &values, b"r1", 10)
            .unwrap();
        index
    }

    fn filtered(index: &SearchIndex, text: &str, filters: Vec<AttrFilter>) -> Vec<String> {
        index
            .query(
                &SearchQuery {
                    text: text.to_owned(),
                    attributes: filters,
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap()
            .hits
            .into_iter()
            .map(|hit| hit.snippet)
            .collect()
    }

    fn a_filter(key: &str, op: AttrOp, values: &[&str], kinds: &[&str]) -> AttrFilter {
        AttrFilter {
            key: key.to_owned(),
            op,
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            numeric_values: Vec::new(),
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
        }
    }

    /// **A typed filter qualifies the line, exactly as a tag does** — same
    /// place, same shape, and the text still ranks on its own.
    #[test]
    fn a_typed_filter_qualifies_the_line_that_carries_it() {
        let index = with_values(&[
            ("Ship it", &[("koru.priority", "text", "high")]),
            ("Ship it", &[("koru.priority", "text", "low")]),
            ("Ship it", &[]),
        ]);
        assert_eq!(
            filtered(
                &index,
                "Ship",
                vec![a_filter("koru.priority", AttrOp::Is, &["high"], &["text"])]
            )
            .len(),
            1
        );
        assert_eq!(filtered(&index, "Ship", Vec::new()).len(), 3, "unfiltered");
    }

    #[test]
    fn entity_picker_finds_marked_and_due_tasks_with_node_identity() {
        let index = with_values(&[
            ("Ship café", &[("koru.task", "text", "explicit")]),
            ("Ship café", &[("koru.due", "date", "2026-11-07")]),
            ("Ship café", &[]),
        ]);
        let page = index
            .query(
                &SearchQuery {
                    text: "cafe".into(),
                    entity_picker: true,
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(page.hits.len(), 2);
        assert_eq!(
            page.hits
                .iter()
                .map(|h| h.node.as_deref())
                .collect::<Vec<_>>(),
            [Some("n0"), Some("n1")]
        );
        assert!(page.hits.iter().all(|hit| hit.title == "Ship café"));
    }

    #[test]
    fn task_edit_and_marker_removal_update_the_entity_picker_index() {
        let mut index = with_values(&[("Ship café", &[("koru.task", "text", "explicit")])]);
        let query = SearchQuery {
            text: "cafe".into(),
            entity_picker: true,
            ..SearchQuery::default()
        };
        let first = index.query(&query, &[]).unwrap().hits;
        assert_eq!(first.len(), 1);
        let document = first[0].document.as_ref().unwrap();
        let mut fragment = Fragment {
            key: format!("node:{document}:n0:primary"),
            workspace: WORKSPACE.into(),
            document: Some(document.clone()),
            node: Some("n0".into()),
            seq: 1,
            kind: "paragraph".into(),
            field: field::BODY.into(),
            text: "Ship café".into(),
        };
        fragment.text = "Launch runway".into();
        let marker = crate::model::AttrRow {
            workspace: WORKSPACE.into(),
            document: document.clone(),
            node: "n0".into(),
            role: "koru.task".into(),
            kind: "text".into(),
            value: "explicit".into(),
            num: None,
            zone: None,
        };
        index
            .replace_document(
                WORKSPACE,
                document,
                &[fragment.clone()],
                &[marker],
                b"r2",
                20,
            )
            .unwrap();
        assert!(index.query(&query, &[]).unwrap().hits.is_empty());
        let renamed = SearchQuery {
            text: "launch".into(),
            ..query.clone()
        };
        assert_eq!(index.query(&renamed, &[]).unwrap().hits.len(), 1);
        index
            .replace_document(WORKSPACE, document, &[fragment], &[], b"r3", 30)
            .unwrap();
        assert!(index.query(&renamed, &[]).unwrap().hits.is_empty());

        index
            .replace_document(WORKSPACE, document, &[], &[], b"r4", 40)
            .unwrap();
        assert!(index.query(&renamed, &[]).unwrap().hits.is_empty());
    }

    #[test]
    fn task_entity_query_obeys_current_and_widened_workspace_scope() {
        let mut index = SearchIndex::in_memory().unwrap();
        let first = WorkspaceId::new();
        let second = WorkspaceId::new();
        for (workspace, label) in [(first, "First"), (second, "Second")] {
            let workspace_id = workspace.to_storage();
            let document = DocumentId::new();
            let document_id = document.to_storage();
            index
                .set_listing(
                    &workspace_id,
                    label,
                    b"r1",
                    &[ListedDocument {
                        document,
                        parent: None,
                        ordinal: 0,
                        title: "Plan".into(),
                        kind: "page".into(),
                        mime: None,
                        trashed_at: None,
                        trashed_by: None,
                    }],
                )
                .unwrap();
            let fragment = Fragment {
                key: format!("node:{document_id}:task:primary"),
                workspace: workspace_id.clone(),
                document: Some(document_id.clone()),
                node: Some("task".into()),
                seq: 1,
                kind: "paragraph".into(),
                field: field::BODY.into(),
                text: "Ship release".into(),
            };
            let marker = crate::model::AttrRow {
                workspace: workspace_id.clone(),
                document: document_id.clone(),
                node: "task".into(),
                role: "koru.task".into(),
                kind: "text".into(),
                value: "explicit".into(),
                num: None,
                zone: None,
            };
            index
                .replace_document(
                    &workspace_id,
                    &document_id,
                    &[fragment],
                    &[marker],
                    b"r1",
                    10,
                )
                .unwrap();
        }
        let mut query = SearchQuery {
            text: "ship".into(),
            entity_picker: true,
            workspaces: vec![first],
            ..SearchQuery::default()
        };
        let current = index.query(&query, &[]).unwrap().hits;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].workspace, first.to_storage());
        query.workspaces.push(second);
        assert_eq!(index.query(&query, &[]).unwrap().hits.len(), 2);
    }

    /// Equality folds the way the text search folds, so the filter and the box
    /// agree about which spellings are one word.
    #[test]
    fn equality_folds_case() {
        let index = with_values(&[("Ship it", &[("koru.priority", "text", "High")])]);
        assert_eq!(
            filtered(
                &index,
                "Ship",
                vec![a_filter("koru.priority", AttrOp::Is, &["HIGH"], &["text"])]
            )
            .len(),
            1
        );
    }

    /// **The rule the whole design rests on**: a filter compares only against
    /// the kinds it names. A window over `date` does not answer for an instant,
    /// and naming both is how a person asks about deadlines however they were
    /// set (ADR-189).
    #[test]
    fn a_filter_compares_only_within_the_kinds_it_names() {
        let index = with_values(&[
            ("Plan", &[("koru.due", "date", "2026-09-10")]),
            ("Plan", &[("koru.due", "datetime_zoned", "1789000000000")]),
        ]);
        let dates_only = filtered(
            &index,
            "Plan",
            vec![a_filter(
                "koru.due",
                AttrOp::Before,
                &["2026-09-11"],
                &["date"],
            )],
        );
        assert_eq!(dates_only.len(), 1, "the instant was not asked about");

        let instants_only = filtered(
            &index,
            "Plan",
            vec![a_filter(
                "koru.due",
                AttrOp::Before,
                &["1789900000000"],
                &["datetime_zoned"],
            )],
        );
        assert_eq!(instants_only.len(), 1, "and now only the instant answers");
    }

    /// **A window over both temporal kinds carries both bounds**, because the
    /// two do not share a column: the calendar day compares against ISO text
    /// and the instant against a number, so the same edge has to arrive twice.
    ///
    /// Without `numeric_values` the instant half drops out here — `2026-09-11`
    /// does not parse as a number — and the window answers only for all-day
    /// deadlines while looking exactly as though it answered for both. That is
    /// the failure ADR-194 names, met at the layer that can commit it.
    #[test]
    fn a_window_over_both_kinds_carries_a_bound_for_each_column() {
        let index = with_values(&[
            ("Plan", &[("koru.due", "date", "2026-09-10")]),
            ("Plan", &[("koru.due", "datetime_zoned", "1789000000000")]),
        ]);
        let both = AttrFilter {
            key: "koru.due".to_owned(),
            op: AttrOp::Before,
            values: vec!["2026-09-11".to_owned()],
            numeric_values: vec!["1789900000000".to_owned()],
            kinds: vec!["date".to_owned(), "datetime_zoned".to_owned()],
        };
        assert_eq!(
            filtered(&index, "Plan", vec![both.clone()]).len(),
            2,
            "each kind was compared against the bound written for its column"
        );

        let iso_only = AttrFilter {
            numeric_values: Vec::new(),
            ..both
        };
        assert_eq!(
            filtered(&index, "Plan", vec![iso_only]).len(),
            1,
            "and with only the ISO bound the instant is silently unanswered"
        );
    }

    /// **The edge belongs to exactly one side.** A due sitting on the bound is
    /// answered by `after` and not by `before`, which is what makes the two
    /// halves a partition rather than two windows that both claim the day —
    /// the rule the search box's before/after step depends on (ADR-197).
    #[test]
    fn a_due_on_the_bound_is_after_it_and_not_before_it() {
        let index = with_values(&[("Plan", &[("koru.due", "date", "2026-09-10")])]);
        assert_eq!(
            filtered(
                &index,
                "Plan",
                vec![a_filter(
                    "koru.due",
                    AttrOp::After,
                    &["2026-09-10"],
                    &["date"]
                )]
            )
            .len(),
            1,
            "after is >=, so the day itself is in"
        );
        assert_eq!(
            filtered(
                &index,
                "Plan",
                vec![a_filter(
                    "koru.due",
                    AttrOp::Before,
                    &["2026-09-10"],
                    &["date"]
                )]
            )
            .len(),
            0,
            "and before is <, so it is out"
        );
        assert_eq!(
            filtered(
                &index,
                "Plan",
                vec![a_filter(
                    "koru.due",
                    AttrOp::Between,
                    &["2026-09-10", "2026-09-11"],
                    &["date"]
                )]
            )
            .len(),
            1,
            "the twenty-four hours from the bound take the bound with them"
        );
    }

    /// One filter naming **both** kinds is how a person asks about deadlines
    /// however they were set — the OR is inside the filter, not between two.
    #[test]
    fn one_filter_naming_both_kinds_answers_for_both() {
        let index = with_values(&[
            ("Plan", &[("koru.due", "date", "2026-09-10")]),
            ("Plan", &[("koru.due", "datetime_zoned", "1789000000000")]),
        ]);
        assert_eq!(
            filtered(
                &index,
                "Plan",
                vec![AttrFilter {
                    key: "koru.due".to_owned(),
                    op: AttrOp::Exists,
                    values: Vec::new(),
                    numeric_values: Vec::new(),
                    kinds: vec!["date".to_owned(), "datetime_zoned".to_owned()],
                }]
            )
            .len(),
            2
        );
    }

    /// A number compares as a number — not as the text `10` sorting before `9`
    /// — and a range is half-open.
    #[test]
    fn a_number_compares_numerically_and_a_range_is_half_open() {
        let index = with_values(&[
            ("Task", &[("user.effort", "number", "9")]),
            ("Task", &[("user.effort", "number", "10")]),
        ]);
        assert_eq!(
            filtered(
                &index,
                "Task",
                vec![a_filter("user.effort", AttrOp::After, &["10"], &["number"])]
            )
            .len(),
            1,
            "After is >=, so 10 is in and 9 is out"
        );
        assert_eq!(
            filtered(
                &index,
                "Task",
                vec![a_filter(
                    "user.effort",
                    AttrOp::Before,
                    &["10"],
                    &["number"]
                )]
            )
            .len(),
            1,
            "Before is <, so 10 is out and 9 is in"
        );
    }

    /// A **custom** attribute filters with no new code, which is the point of a
    /// general filter rather than a field per attribute.
    #[test]
    fn a_workspace_defined_attribute_needs_no_new_code() {
        let index = with_values(&[
            ("Item", &[("user.effort", "select", "large")]),
            ("Item", &[("user.effort", "select", "small")]),
        ]);
        assert_eq!(
            filtered(
                &index,
                "Item",
                vec![a_filter(
                    "user.effort",
                    AttrOp::OneOf,
                    &["large", "huge"],
                    &["select"]
                )]
            )
            .len(),
            1
        );
    }

    /// `Exists` asks about the key and not the value; `Missing` is its
    /// negation, and both ignore the operand list.
    #[test]
    fn exists_and_missing_ask_about_the_key_alone() {
        let index = with_values(&[("Row", &[("koru.due", "date", "2026-09-10")]), ("Row", &[])]);
        assert_eq!(
            filtered(
                &index,
                "Row",
                vec![a_filter("koru.due", AttrOp::Exists, &[], &["date"])]
            )
            .len(),
            1
        );
        assert_eq!(
            filtered(
                &index,
                "Row",
                vec![a_filter("koru.due", AttrOp::Missing, &[], &["date"])]
            )
            .len(),
            1,
            "the undated row, and only it"
        );
    }

    /// **A filter naming no kind matches nothing, and does not widen.**
    /// Dropping it would answer a question nobody asked; `AND 0` answers the
    /// one that was asked, emptily.
    #[test]
    fn a_filter_with_no_kinds_matches_nothing() {
        let index = with_values(&[("Row", &[("koru.due", "date", "2026-09-10")])]);
        assert!(
            filtered(
                &index,
                "Row",
                vec![a_filter("koru.due", AttrOp::Exists, &[], &[])]
            )
            .is_empty()
        );
    }

    /// An operand that will not parse as a number cannot be compared, and the
    /// filter empties rather than widening or comparing against zero.
    #[test]
    fn an_unparsable_numeric_bound_matches_nothing() {
        let index = with_values(&[("Row", &[("user.effort", "number", "3")])]);
        assert!(
            filtered(
                &index,
                "Row",
                vec![a_filter(
                    "user.effort",
                    AttrOp::After,
                    &["soon"],
                    &["number"]
                )]
            )
            .is_empty()
        );
    }

    /// **A filter on its own is a whole query.** *What is due this month* names
    /// no word, and until the guard above learned about `attributes` it
    /// answered nothing at all — the page came back empty before the statement
    /// ran, and typing any letter beside the filter "fixed" it.
    ///
    /// Every other test in this file passes a word beside its filter, which is
    /// precisely why none of them saw it.
    #[test]
    fn a_filter_alone_is_a_query() {
        let index = with_values(&[
            ("Ship it", &[("koru.due", "date", "2026-12-04")]),
            ("Later", &[("koru.due", "date", "2027-03-01")]),
            ("Untimed", &[]),
        ]);
        let hits = filtered(
            &index,
            "",
            vec![a_filter(
                "koru.due",
                AttrOp::Between,
                &["2026-12-03", "2026-12-05"],
                &["date"],
            )],
        );
        assert_eq!(hits, ["Ship it"], "the window's one due, and no word typed");

        // And the empty question is still empty: no words, no tag, no filter.
        assert!(filtered(&index, "", Vec::new()).is_empty());
    }

    /// The half-open window, asked without a word: `after` includes its bound
    /// and `before` excludes it, so two of them are the ordinary way to say
    /// *this month* and must not need a letter typed beside them.
    #[test]
    fn two_bounds_alone_answer_a_range() {
        let index = with_values(&[
            ("Ship it", &[("koru.due", "date", "2026-12-04")]),
            ("Too early", &[("koru.due", "date", "2026-08-30")]),
            ("Too late", &[("koru.due", "date", "2027-01-01")]),
        ]);
        let mut hits = filtered(
            &index,
            "",
            vec![
                a_filter("koru.due", AttrOp::After, &["2026-08-31"], &["date"]),
                a_filter("koru.due", AttrOp::Before, &["2027-01-01"], &["date"]),
            ],
        );
        hits.sort();
        assert_eq!(hits, ["Ship it"]);
    }

    /// Two filters are an **AND**: a node must satisfy every one.
    #[test]
    fn two_typed_filters_both_have_to_hold() {
        let index = with_values(&[
            (
                "Ship it",
                &[
                    ("koru.priority", "text", "high"),
                    ("user.effort", "number", "3"),
                ],
            ),
            ("Ship it", &[("koru.priority", "text", "high")]),
        ]);
        assert_eq!(
            filtered(
                &index,
                "Ship",
                vec![
                    a_filter("koru.priority", AttrOp::Is, &["high"], &["text"]),
                    a_filter("user.effort", AttrOp::Exists, &[], &["number"]),
                ]
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_tag_filter_qualifies_the_line_that_carries_it() {
        let index = indexed(&[
            ("Hello", &["amazing"]),
            ("Hello", &[]),
            ("Goodbye", &["amazing"]),
        ]);
        let hits = index
            .query(
                &SearchQuery {
                    text: "Hello #amazing".to_owned(),
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 1, "one line reads Hello and carries the tag");
        assert_eq!(hits[0].snippet, "Hello");
        assert_eq!(
            hits[0]
                .spans
                .iter()
                .map(|s| &hits[0].snippet[s.start..s.end])
                .collect::<Vec<_>>(),
            ["Hello"],
            "the word is still highlighted; the tag was never a term"
        );
    }

    /// Node-scoped, not document-scoped: the untagged line in the same document
    /// as a tagged one is not a hit.
    #[test]
    fn a_sibling_line_in_the_same_document_is_not_admitted() {
        let index = indexed(&[("Hello", &[]), ("Hello", &["amazing"])]);
        assert_eq!(bodies(&index, "Hello #amazing"), ["Hello"]);
        assert_eq!(bodies(&index, "Hello").len(), 2, "unfiltered, both");
    }

    #[test]
    fn a_filter_admits_the_descendants_of_the_tag_it_names() {
        let index = indexed(&[
            ("alpha", &["fun"]),
            ("beta", &["fun/home"]),
            ("gamma", &["funny"]),
            ("delta", &[]),
        ]);
        let mut found = bodies(&index, "#fun");
        found.sort();
        assert_eq!(found, ["alpha", "beta"], "`funny` is not under `fun`");
    }

    #[test]
    fn a_filter_folds_the_case_of_the_tag_it_names() {
        let index = indexed(&[("alpha", &["fun"])]);
        assert_eq!(bodies(&index, "#FUN"), ["alpha"]);
    }

    #[test]
    fn two_filters_both_have_to_hold() {
        let index = indexed(&[
            ("alpha", &["fun", "home"]),
            ("beta", &["fun"]),
            ("gamma", &["home"]),
        ]);
        assert_eq!(bodies(&index, "#fun #home"), ["alpha"]);
    }

    /// A surface may filter without anybody typing a `#`.
    #[test]
    fn a_filter_may_arrive_on_the_wire_instead_of_in_the_text() {
        let index = indexed(&[("Hello", &["amazing"]), ("Hello", &[])]);
        let hits = index
            .query(
                &SearchQuery {
                    text: "Hello".to_owned(),
                    tags: vec!["Amazing".to_owned()],
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 1);
    }

    /// **The pin.** A query with no tag in it must rank exactly as it did before
    /// tag filtering existed — same hits, same order, same page.
    #[test]
    fn a_query_with_no_filter_ranks_exactly_as_it_did() {
        let index = indexed(&[
            ("the quarterly plan", &[]),
            ("quarterly", &["fun"]),
            ("a quarterly review of the plan", &[]),
        ]);
        assert_eq!(
            bodies(&index, "quarterly"),
            [
                // Exact field match (+30), then prefix, then substring — the
                // ordering the score's third tier produces.
                "quarterly",
                "a quarterly review of the plan",
                "the quarterly plan",
            ]
        );
        let page = index
            .query(
                &SearchQuery {
                    text: "quarterly".to_owned(),
                    page_size: 2,
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.next_offset, Some(2), "paging is untouched");
    }

    /// A filtered query pages the way an unfiltered one does: the filter is
    /// applied before the page is cut, never after.
    #[test]
    fn a_filtered_page_reports_what_is_left_of_the_filtered_set() {
        let index = indexed(&[
            ("alpha", &["fun"]),
            ("beta", &["fun"]),
            ("gamma", &[]),
            ("delta", &[]),
        ]);
        let page = index
            .query(
                &SearchQuery {
                    text: "#fun".to_owned(),
                    page_size: 1,
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(page.hits.len(), 1);
        assert_eq!(page.next_offset, Some(1));
        let rest = index
            .query(
                &SearchQuery {
                    text: "#fun".to_owned(),
                    page_size: 1,
                    offset: 1,
                    ..SearchQuery::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(rest.hits.len(), 1);
        assert_ne!(rest.hits[0].snippet, page.hits[0].snippet);
    }

    #[test]
    fn every_field_the_ranking_names_is_a_token_something_produces() {
        for token in [
            field::TITLE,
            field::BODY,
            field::TASK,
            field::NAME,
            field::LIST_NAME,
            field::LEVEL_NAME,
            field::CONTACT,
            field::WORKSPACE,
        ] {
            assert!(
                !token.contains(' '),
                "{token} reads like a label, not a token"
            );
        }
        assert_eq!(field::heading(2), "heading-2");
        assert_eq!(field::heading(9), "heading-6", "clamped, not invented");
        assert_eq!(field::attr("due"), "attr:due");
    }

    #[test]
    fn a_breadcrumb_walks_the_title_path_root_first() {
        let mut listing = HashMap::new();
        listing.insert(
            "a".to_owned(),
            Listed {
                document: "a".into(),
                workspace: "w".into(),
                parent: None,
                title: "Roadmap".into(),
                kind: "page".into(),
                mime: None,
                trashed_at: None,
                trashed_by: None,
            },
        );
        listing.insert(
            "b".to_owned(),
            Listed {
                document: "b".into(),
                workspace: "w".into(),
                parent: Some("a".into()),
                title: "Q3".into(),
                kind: "page".into(),
                mime: None,
                trashed_at: None,
                trashed_by: None,
            },
        );
        assert_eq!(
            breadcrumb("Acme", listing.get("b"), &listing),
            "Acme / Roadmap / Q3"
        );
        assert_eq!(breadcrumb("Acme", None, &listing), "Acme");
    }

    /// A listing that somehow names itself as its own ancestor must not hang a
    /// query. Bounded rather than trusted.
    #[test]
    fn a_cycle_in_the_listing_ends_the_walk() {
        let mut listing = HashMap::new();
        for (id, parent) in [("a", "b"), ("b", "a")] {
            listing.insert(
                id.to_owned(),
                Listed {
                    document: id.into(),
                    workspace: "w".into(),
                    parent: Some(parent.into()),
                    title: id.into(),
                    kind: "page".into(),
                    mime: None,
                    trashed_at: None,
                    trashed_by: None,
                },
            );
        }
        let path = breadcrumb("Acme", listing.get("a"), &listing);
        assert!(path.starts_with("Acme"));
    }
}
