// SPDX-License-Identifier: MPL-2.0

//! The daemon's implementation of the tasks query service ([ADR-287]).
//!
//! **The same read model, asked a second question**, so this is `attributes.rs`
//! with a different projection call and a different cursor — deliberately, and
//! not by accident: the two share `attribute_conditions` underneath, so a
//! filter that means one thing to an attribute page must mean the same thing
//! to a task page, and two ports that had drifted in how they *reach* that
//! query would be the first place the equality broke.
//!
//! Same lock discipline as `projection.rs` and `attributes.rs`, which is where
//! it is written down: derived rows come from a read-only `projection.db`
//! connection WAL keeps independent of the worker's writer, and the workspace
//! **system documents** — the custom definitions and the tag table, which live
//! in root containers the projection deliberately does not walk — are read by
//! forking under the Store lock and decoding *outside* it. In, fork, out.
//!
//! The port asks for a sweep before it reads, the `ReadPort` rule: a Tasks
//! surface can be the first consumer of a cold workspace after start-up, and
//! the sweep is what makes it converge. **The answer never waits** —
//! `updating_workspaces` names what it may be behind on instead, and a client
//! draws what it has and asks again.
//!
//! Two things differ from the attribute port and both are the tasks query's own
//! rules rather than taste:
//!
//! - **The page is a keyset, not an offset.** `next_after` is the last row's
//!   `task:` reference, which is also its identity and its sort tie-break —
//!   one string, so a page boundary cannot mean one thing to the cursor and
//!   another to the ordering. `restarted` comes straight back from the
//!   projection: an `after` naming a row the filter no longer matches
//!   (completed, removed or binned between pages) is answered from the top,
//!   and a client that could not tell that from a continuation would silently
//!   draw page one twice.
//! - **The limit is not clamped here.** `routes::tasks::query` already did it,
//!   at the boundary, so a second clamp would be a second answer to one
//!   question.
//!
//! [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use koru_core::ids::ListId;
use koru_projection::Projections;
use koru_projection::store::{
    Bucket, BucketWindows, LifecycleFilter, Requirement, TaskGrouping, TaskGroups, TaskRowQuery,
    TaskSort,
};
use koru_projection::worker::Sweeps;
use koru_store_api::routes::tasks::{TaskService, TaskServiceError};
use koru_store_api::state::AppState;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::attributes::{AttributeDefinition, AttributeRowValue, AttributeTag};
use koru_store_wire::tasks as wire;

use crate::systemdoc;

pub struct TaskPort {
    projections: Mutex<Projections>,
    sweeps: Sweeps,
    /// `Weak`, because this port is stored *in* `AppState`'s service slot — an
    /// `Arc` here would be a cycle that keeps both alive forever. The daemon
    /// holds the strong reference for the life of the process, so an upgrade
    /// failing means shutdown, and the port degrades rather than fails.
    state: Weak<AppState>,
    // Content-free counters, published to the debug surface after every query.
    // Counts and milliseconds only — never a title, a breadcrumb, a due date or
    // a task reference (LOGGING.md). A task reference names a document and a
    // node, which is exactly the shape of thing a bug report must not carry.
    queries: AtomicU64,
    rows_answered: AtomicU64,
    sweeps_requested: AtomicU64,
    restarts: AtomicU64,
    total_millis: AtomicU64,
}

/// Open the port behind `POST /v1/tasks/query`. Its own read-only connection,
/// per the `read_ports` note: WAL lets it answer without the worker's mutex or
/// the Store's authoritative lock.
pub fn task_port(
    root: &std::path::Path,
    sweeps: Sweeps,
    state: &Arc<AppState>,
) -> Option<Arc<TaskPort>> {
    match Projections::open(root) {
        Ok(projections) => Some(Arc::new(TaskPort {
            projections: Mutex::new(projections),
            sweeps,
            state: Arc::downgrade(state),
            queries: AtomicU64::new(0),
            rows_answered: AtomicU64::new(0),
            sweeps_requested: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            total_millis: AtomicU64::new(0),
        })),
        Err(error) => {
            tracing::error!("the tasks query projection did not open: {error}");
            None
        }
    }
}

impl TaskService for TaskPort {
    fn query(&self, query: &wire::TaskQuery) -> Result<wire::TaskQueryResponse, TaskServiceError> {
        let started = std::time::Instant::now();
        let row_query = translate(query)?;
        let grouping = group_by(query.group_by.as_deref())?;
        windows_are_present(&row_query, grouping)?;
        // **The overdue windows are the caller's, and they only split counts.**
        // Translated up here so a malformed window is refused before any work,
        // by the same translator the `due` filter goes through — overdue is a
        // due question asked in the reader's zone, not a second notion of time.
        let overdue = query
            .overdue
            .as_ref()
            .map(crate::attributes::due_filter)
            .transpose()
            .map_err(invalid)?;
        // **A malformed requirement refuses the request**, before any work: it
        // is a client bug, and answering it quietly as *uncovered* would hide
        // one behind a wait that never ends.
        let covering = requirements(&query.covering)?;

        // The scope is what the caller named, or every workspace the store
        // knows. Resolved from the authoritative catalog rather than from the
        // projection, so a workspace nothing has swept yet is still *named* in
        // `updating_workspaces` instead of silently absent — which for a task
        // list is the difference between "still counting" and "you have
        // nothing to do".
        let scope: Vec<WorkspaceId> = if query.workspaces.is_empty() {
            match self.state.upgrade() {
                Some(state) => state
                    .store
                    .lock()
                    .map_err(|_| TaskServiceError::Failed("the store lock is poisoned".into()))?
                    .workspaces()
                    .map_err(|e| TaskServiceError::Failed(e.to_string()))?
                    .into_iter()
                    .map(|w| w.id)
                    .collect(),
                None => Vec::new(),
            }
        } else {
            query.workspaces.clone()
        };
        // Sweep-first, the ReadPort rule: a cold workspace converges because
        // somebody asked, not because somebody edited.
        for workspace in &scope {
            self.sweeps.request(*workspace);
        }
        self.sweeps_requested
            .fetch_add(scope.len() as u64, Ordering::Relaxed);
        // **Coverage describes these rows**, so a requirement on a workspace the
        // query never asked about is refused rather than answered — the answer
        // could not describe rows it did not fetch. Answered against the scope,
        // which needs no projection row: a document the projector has never
        // seen is *uncovered*, not invalid.
        for requirement in &query.covering {
            if !scope.contains(&requirement.workspace) {
                return Err(TaskServiceError::Invalid(
                    "a covering requirement names a workspace outside the query's scope".into(),
                ));
            }
        }

        let (page, groups, covered, updating, system_documents) = {
            let projections = self.projections.lock().map_err(|_| {
                TaskServiceError::Failed("the tasks projection lock was poisoned".into())
            })?;
            let mut updating = Vec::new();
            for workspace in &scope {
                if projections
                    .listing_digest(*workspace)
                    .map_err(|e| TaskServiceError::Failed(e.to_string()))?
                    .is_none()
                {
                    updating.push(*workspace);
                }
            }
            // **One read, at the projection's seam.** Rows, counts and the
            // coverage evidence come out of one read transaction there, so a
            // projection committing through the writer's connection between
            // the steps cannot put a rail's number out of step with the list
            // under it, or certify a page it was not read with. The windows
            // are present when a bucket grouping asks for them — the refusal
            // above is what makes that true — and they carry the overdue
            // edges too, so the rail's tint and its numbers come from one
            // object.
            let grouping = match grouping {
                None => TaskGrouping::Rows,
                Some(GroupBy::List) => TaskGrouping::Lists {
                    overdue: overdue.as_ref(),
                },
                Some(GroupBy::Bucket) => TaskGrouping::Buckets,
            };
            let answer = projections
                .task_answer(&row_query, grouping, &covering)
                .map_err(|e| TaskServiceError::Failed(e.to_string()))?;
            let groups = match answer.groups {
                TaskGroups::None => Vec::new(),
                TaskGroups::Lists(lists) => lists.into_iter().map(group_to_wire).collect(),
                TaskGroups::Buckets(buckets) => buckets.into_iter().map(bucket_to_wire).collect(),
            };
            let page = answer.page;
            // Which documents hold each workspace's definitions and tags. From
            // the projection's listing rather than by opening every workspace:
            // it converges with the same sweep the rows do, and a workspace it
            // has not seen yet is already named in `updating`.
            let mut system_documents: Vec<(WorkspaceId, Vec<DocumentId>)> = Vec::new();
            for workspace in &scope {
                let ids = systemdoc::system_documents(&projections, *workspace);
                if !ids.is_empty() {
                    system_documents.push((*workspace, ids));
                }
            }
            (page, groups, answer.covered, updating, system_documents)
        };

        // System definitions always: their semantics are this build's, and a
        // task row carries `koru.due`, `koru.priority` and `koru.task` values a
        // surface has to be able to label before any workspace has defined
        // anything of its own.
        let mut definitions: Vec<AttributeDefinition> = koru_core::attributes::system_definitions()
            .iter()
            .map(definition_to_wire)
            .collect();
        let mut tags = Vec::new();
        if let Some(state) = self.state.upgrade() {
            for (workspace, ids) in system_documents {
                let Some(system) = systemdoc::open_system_document(&state, workspace, &ids) else {
                    continue;
                };
                definitions.extend(system.definitions().iter().map(definition_to_wire));
                tags.extend(system.tags().into_iter().map(|tag| AttributeTag {
                    id: tag.id.to_storage(),
                    name: tag.name,
                    color: tag.color,
                    workspace,
                }));
            }
        }

        // **The cursor is the last row's own identity.** Only when the page
        // filled: a short page is the end of the list, and handing back a
        // cursor there would make a client ask one more question to be told
        // nothing.
        let next_after = (page.rows.len() >= row_query.limit)
            .then(|| page.rows.last().map(|row| row.task.clone()))
            .flatten();
        if page.restarted {
            self.restarts.fetch_add(1, Ordering::Relaxed);
        }

        let response = wire::TaskQueryResponse {
            rows: page.rows.into_iter().map(row_to_wire).collect(),
            definitions,
            tags,
            groups,
            projected: updating.is_empty(),
            updating_workspaces: updating,
            limit: row_query.limit,
            next_after,
            restarted: page.restarted,
            // Always `Some` from a server that read the requirement — `true`
            // when nothing was required. `None` is what an older server says by
            // not saying anything, and a client reads it as *unknown*.
            covered: Some(covered),
        };

        let millis = started.elapsed().as_millis() as u64;
        let queries = self.queries.fetch_add(1, Ordering::Relaxed) + 1;
        let rows_answered = self
            .rows_answered
            .fetch_add(response.rows.len() as u64, Ordering::Relaxed)
            + response.rows.len() as u64;
        let total_millis = self.total_millis.fetch_add(millis, Ordering::Relaxed) + millis;
        tracing::debug!(
            target: "koru_tasks",
            workspaces = scope.len(),
            rows = response.rows.len(),
            restarted = response.restarted,
            millis,
            "tasks query answered"
        );
        if let Some(state) = self.state.upgrade()
            && let Ok(mut slot) = state.tasks.lock()
        {
            *slot = serde_json::json!({
                "queries": queries,
                "rows_answered": rows_answered,
                "sweeps_requested": self.sweeps_requested.load(Ordering::Relaxed),
                "restarts": self.restarts.load(Ordering::Relaxed),
                "total_millis": total_millis,
                "last_millis": millis,
            });
        }
        Ok(response)
    }
}

/// Wire filters to the typed projection query.
///
/// The six shared filters are translated by the attribute port's own
/// translator, over an `AttributeQuery` carrying exactly them — **one
/// translation, not two**, so `min_priority` cannot come to mean a different
/// floor on this route than on the other one. Its `sort`, `limit` and `offset`
/// are left at their defaults and ignored: a task page orders and paginates by
/// its own rules.
fn translate(query: &wire::TaskQuery) -> Result<TaskRowQuery, TaskServiceError> {
    let shared = crate::attributes::translate(&koru_store_wire::attributes::AttributeQuery {
        workspaces: query.workspaces.clone(),
        has: query.has.clone(),
        missing: query.missing.clone(),
        tags: query.tags.clone(),
        priority: query.priority.clone(),
        min_priority: query.min_priority.clone(),
        due: query.due.clone(),
        ..Default::default()
    })
    .map_err(invalid)?;

    Ok(TaskRowQuery {
        attributes: shared,
        lifecycle: match query.lifecycle {
            wire::LifecycleFilter::Open => LifecycleFilter::Open,
            wire::LifecycleFilter::Completed => LifecycleFilter::Completed,
            wire::LifecycleFilter::All => LifecycleFilter::All,
        },
        // **A list filter is an id, and an unparseable one is the caller's
        // mistake** — the `tags` rule. Answering "no tasks" to a name would
        // look exactly like an empty list.
        list: match &query.list {
            None => None,
            Some(list) => Some(
                ListId::parse(list)
                    .ok_or_else(|| {
                        TaskServiceError::Invalid("a list filter must be a list id".into())
                    })?
                    .to_storage(),
            ),
        },
        include_trashed: query.include_trashed,
        completed_on_or_after: query.completed_on_or_after.clone(),
        recently_completed_since: query.recently_completed_since.clone(),
        // **The windows are translated whether or not a bucket filter names
        // one**, because `group_by: "bucket"` needs them too and a caller may
        // page one bucket while drawing the rail's five numbers. Refusing a
        // malformed window here rather than at the grouping means one refusal
        // for one mistake.
        bucket: bucket_token(query.bucket.as_deref())?,
        windows: match &query.bucket_windows {
            None => None,
            Some(windows) => Some(bucket_windows(windows)?),
        },
        sort: match query.sort {
            wire::TaskSort::Due => TaskSort::Due,
            wire::TaskSort::Priority => TaskSort::Priority,
            wire::TaskSort::Modified => TaskSort::Modified,
            wire::TaskSort::Manual => TaskSort::Manual,
        },
        // Already clamped at the boundary (`routes::tasks::query`); clamping it
        // again here would be a second answer to one question.
        limit: query.limit,
        after: query.after.clone(),
    })
}

/// The writes a query wants included, decoded at the boundary.
///
/// Refused, never guessed: a requirement is a client naming its own write, so
/// one whose peer is not an id or whose counter is negative is a client bug,
/// and `InvalidRequest` is the answer that surfaces one. The
/// workspace is checked against the scope by the caller, once the scope is
/// known; nothing here needs the projection.
///
/// Duplicates need no folding — the answer is a conjunction, and a repeated
/// requirement changes nothing.
fn requirements(
    covering: &[wire::RevisionRequirement],
) -> Result<Vec<Requirement>, TaskServiceError> {
    if covering.len() > wire::MAX_COVERING {
        return Err(TaskServiceError::Invalid(format!(
            "a query may wait on at most {} writes",
            wire::MAX_COVERING
        )));
    }
    covering
        .iter()
        .map(|requirement| {
            // **No length rule any more, because there is no length.** A
            // receipt is a peer and a counter; it cannot grow with the
            // document's history, which is what the rule it replaces was
            // failing to contain. What is still refused is a receipt that is
            // not a receipt.
            let peer: u64 = requirement.peer.parse().map_err(|_| {
                TaskServiceError::Invalid("a covering receipt's peer is not an id".into())
            })?;
            if requirement.counter < 0 {
                return Err(TaskServiceError::Invalid(
                    "a covering receipt's counter is negative".into(),
                ));
            }
            Ok(Requirement::new(
                requirement.document,
                peer,
                requirement.counter,
            ))
        })
        .collect()
}

/// Parse a `bucket` token into the **domain's** enum — the same one
/// `koru_core::tasks::bucket` answers with, so the vocabulary a caller sends
/// and the vocabulary the rule is written in cannot drift apart.
///
/// An unknown token is refused rather than answered as *every bucket*: a Tasks
/// view that asked for Today and got the whole corpus would look like the
/// bucketing had silently stopped working.
fn bucket_token(token: Option<&str>) -> Result<Option<Bucket>, TaskServiceError> {
    let Some(token) = token else { return Ok(None) };
    Bucket::ALL
        .iter()
        .find(|bucket| bucket.as_str() == token)
        .copied()
        .map(Some)
        .ok_or_else(|| {
            TaskServiceError::Invalid(format!(
                "bucket must be one of {}",
                koru_store_wire::tasks::BUCKETS.join(", ")
            ))
        })
}

/// The reader's zone, as the projection takes it.
///
/// **Everything is the caller's number**, including the priority floor: SPEC §7
/// is the caller's product rule, and a store that knew it would be a second
/// place to change it. What this adds is the refusal — a malformed day, a
/// transposed pair of horizons or an unknown priority token is a 400 with a
/// reason, because each of them would otherwise answer a wrong question
/// silently and plausibly.
fn bucket_windows(
    windows: &koru_store_wire::tasks::BucketWindows,
) -> Result<BucketWindows, TaskServiceError> {
    let floor =
        koru_core::attributes::Priority::parse(&windows.min_next_priority).ok_or_else(|| {
            TaskServiceError::Invalid(
                "min_next_priority must be one of low, medium, high, urgent".to_owned(),
            )
        })?;
    let translated = BucketWindows {
        today: windows.today.clone(),
        horizon: windows.horizon.clone(),
        upcoming_horizon: windows.upcoming_horizon.clone(),
        today_ends_ms: windows.today_ends_ms,
        horizon_ends_ms: windows.horizon_ends_ms,
        upcoming_ends_ms: windows.upcoming_ends_ms,
        now_ms: windows.now_ms,
        min_next_priority: floor,
    };
    translated
        .check()
        .map_err(|e| TaskServiceError::Invalid(e.to_string()))?;
    Ok(translated)
}

/// **A bucket without windows is refused, never guessed.**
///
/// SPEC §7 makes a bucket a function of the reader's zone and of today's date,
/// and this store has neither: answering with a date of its own would be wrong
/// by morning and wrong in another country, which is exactly what [ADR-287]
/// refused to persist. Both doors are checked here — the one that pages a
/// bucket and the one that counts all five — so the refusal has one wording.
///
/// [ADR-287]: ../../../docs/decisions/ADR-276-300.md#adr-287-taskread--the-tasks-query-is-a-second-question-asked-of-the-one-read-model
fn windows_are_present(
    query: &TaskRowQuery,
    grouping: Option<GroupBy>,
) -> Result<(), TaskServiceError> {
    if query.windows.is_none() && (query.bucket.is_some() || grouping == Some(GroupBy::Bucket)) {
        return Err(TaskServiceError::Invalid(
            "a bucket needs bucket_windows: the reader's today, horizons and instants".to_owned(),
        ));
    }
    Ok(())
}

/// What a `group_by` token asks for. An enum rather than a bool so a third one
/// — a priority, a kind — is a `match` arm instead of a second flag. The
/// attribute port's shape, deliberately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GroupBy {
    List,
    Bucket,
}

/// Parse `group_by`. Absent means no counts; an unknown token is refused with
/// a reason rather than silently answered ungrouped — a rail that asked for
/// numbers and drew none would look like a store bug.
fn group_by(token: Option<&str>) -> Result<Option<GroupBy>, TaskServiceError> {
    match token {
        None => Ok(None),
        Some("list") => Ok(Some(GroupBy::List)),
        Some("bucket") => Ok(Some(GroupBy::Bucket)),
        Some(_) => Err(TaskServiceError::Invalid(
            "group_by must be list or bucket".to_owned(),
        )),
    }
}

fn group_to_wire(count: koru_projection::store::TaskListCount) -> wire::TaskGroup {
    wire::TaskGroup {
        workspace: count.workspace,
        list: count.list,
        count: count.count,
        overdue: count.overdue,
        trashed: count.trashed,
    }
}

/// A bucket count as a group row. `list` is the **group's key** here, carrying
/// the bucket token — the field's name is the list grouping's history rather
/// than a second meaning, and [`wire::TaskGroup`] says so at length. The token
/// itself comes from the domain enum, so this route's vocabulary and the rule's
/// are one list.
fn bucket_to_wire(count: koru_projection::store::TaskBucketCount) -> wire::TaskGroup {
    wire::TaskGroup {
        workspace: count.workspace,
        list: count.bucket.as_str().to_owned(),
        count: count.count,
        overdue: count.overdue,
        trashed: count.trashed,
    }
}

/// The attribute port's refusal, in this route's vocabulary. The two enums are
/// the same two cases — the caller's mistake, or the read model's — and mapping
/// rather than sharing keeps each route's error type its own.
fn invalid(error: koru_store_api::routes::attributes::AttributeServiceError) -> TaskServiceError {
    use koru_store_api::routes::attributes::AttributeServiceError as Attr;
    match error {
        Attr::Invalid(why) => TaskServiceError::Invalid(why),
        Attr::Failed(why) => TaskServiceError::Failed(why),
    }
}

fn definition_to_wire(def: &koru_core::attributes::AttributeDefinition) -> AttributeDefinition {
    AttributeDefinition {
        id: def.id.clone(),
        key: def.key.clone(),
        label: def.label.clone(),
        value_type: def.value_type.as_str().to_owned(),
        cardinality: def.cardinality.as_str().to_owned(),
        scope: def.scope.as_str().to_owned(),
        system: def.system,
        options: def.options.clone(),
        filterable: def.filterable,
        sortable: def.sortable,
        groupable: def.groupable,
    }
}

fn row_to_wire(row: koru_projection::store::TaskQueryRow) -> wire::TaskRow {
    wire::TaskRow {
        workspace: row.workspace,
        document: row.document,
        node: row.node,
        task: row.task,
        node_kind: row.node_kind,
        lifecycle: row.lifecycle,
        document_trashed: row.document_trashed,
        in_trash: row.in_trash,
        breadcrumb: row.breadcrumb,
        title: row.title,
        attributes: row
            .attrs
            .into_iter()
            .map(|attr| AttributeRowValue {
                id: attr.id,
                role: attr.role,
                kind: attr.kind,
                value: attr.value,
                // The zone rides with the instant, per ADR-183: a client given
                // epoch-ms alone would be holding an unreadable value.
                zone: attr.zone,
                label: attr.label,
            })
            .collect(),
    }
}

// --------------------------------------------------------------- the console

/// `koru-store-server tasks [<filter>]` — look at the tasks in the read model.
///
/// Reads `projection.db` **directly**, like `koru-store-server projection` and
/// `search`, and for the same reason: a diagnostic that needs the daemon to be
/// healthy is a diagnostic that is missing exactly when it is wanted. **It
/// cannot ask a running store to sweep**, so what it prints is what is on the
/// disk now, which may be behind a store that is still catching up — the one
/// thing the HTTP route does that this cannot.
///
/// The optional argument is a *lifecycle* filter — `open`, `completed`, `all`
/// — because the question asked of a task list at a console is almost always
/// "what is still outstanding" or "did that actually tick".
pub fn report_to_console(
    root: &std::path::Path,
    filter: Option<&str>,
) -> koru_store_core::errors::Result<()> {
    use koru_store_core::errors::{Code, Error};

    let lifecycle = match filter {
        None | Some("open") => LifecycleFilter::Open,
        Some("completed") => LifecycleFilter::Completed,
        Some("all") => LifecycleFilter::All,
        Some(other) => {
            return Err(Error::new(
                Code::InvalidRequest,
                format!("{other} is not a lifecycle — say open, completed or all"),
            ));
        }
    };

    let path = root.join("projection.db");
    if !path.exists() {
        println!("No projection at {}.", path.display());
        println!("Nothing has used a workspace yet, or it has been deleted — either way it");
        println!("is rebuilt the next time a workspace is written to.");
        return Ok(());
    }
    let projections = Projections::open(root)
        .map_err(|e| Error::new(Code::StorageFailed, format!("tasks: {e}")))?;

    let page = projections
        .task_rows(&TaskRowQuery {
            lifecycle,
            // The bin included, and each row says so. At a console the
            // interesting question is usually *where did it go*, and hiding
            // the answer is what makes somebody reach for sqlite3.
            include_trashed: true,
            limit: 500,
            ..Default::default()
        })
        .map_err(|e| Error::new(Code::StorageFailed, format!("tasks: {e}")))?;

    println!("{}", path.display());
    println!("{} task(s), lifecycle {:?}", page.rows.len(), lifecycle);
    println!();
    for row in &page.rows {
        let mut marks = vec![row.lifecycle.clone(), row.node_kind.clone()];
        if row.in_trash {
            marks.push("node binned".into());
        }
        if row.document_trashed {
            marks.push("document binned".into());
        }
        for attr in &row.attrs {
            marks.push(format!("{}={}", attr.role, attr.value));
        }
        println!("{}", row.task);
        println!("  {}", row.title);
        println!("  {}   [{}]", row.breadcrumb, marks.join(", "));
    }
    if page.rows.is_empty() {
        println!("Nothing carries a task marker or a due date.");
    }
    println!();
    println!("A lifecycle — open, completed or all — filters the list.");
    Ok(())
}

#[cfg(test)]
mod tests;
