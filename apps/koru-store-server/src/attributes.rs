// SPDX-License-Identifier: MPL-2.0

//! The daemon's implementation of the attribute query service (roadmap D9).
//!
//! Same lock discipline as `projection.rs`, which is where it is written down:
//! the derived rows come from a read-only `projection.db` connection that WAL
//! keeps independent of the worker's writer, and the workspace **system
//! documents** — custom attribute definitions and the tag table, which live in
//! root containers the projection deliberately does not walk — are read by
//! forking under the Store lock and decoding *outside* it. In, fork, out.
//!
//! The port asks for a sweep before it reads, the `ReadPort` pattern: an
//! attribute view can be the first consumer of a cold workspace after
//! start-up, and the sweep is what makes it converge. The answer never waits —
//! `updating_workspaces` names what it may be behind on instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use koru_core::attributes::Priority;
use koru_core::ids::TagId;
use koru_projection::Projections;
use koru_projection::store::{AttrSort, AttributeRowQuery, DateWindow, DueFilter, InstantWindow};
use koru_projection::worker::Sweeps;
use koru_store_api::routes::attributes::{AttributeService, AttributeServiceError};
use koru_store_api::state::AppState;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::attributes as wire;

use crate::systemdoc;

/// The page size a query that names none gets, and the cap on one that does.
/// A cap rather than a refusal: a caller asking for ten thousand rows gets a
/// full page and pages on, instead of an error to special-case.
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

pub struct AttributePort {
    projections: Mutex<Projections>,
    sweeps: Sweeps,
    /// `Weak`, because this port is stored *in* `AppState`'s service slot — an
    /// `Arc` here would be a cycle that keeps both alive forever. The daemon
    /// holds the strong reference for the life of the process, so an upgrade
    /// failing means shutdown, and the port degrades rather than fails.
    state: Weak<AppState>,
    // Content-free counters, published to the debug surface after every
    // query. Counts and milliseconds only — never a key, a value or a tag
    // name (LOGGING.md).
    queries: AtomicU64,
    rows_answered: AtomicU64,
    total_millis: AtomicU64,
}

/// Open the port behind `POST /v1/attributes/query`. Its own read-only
/// connection, per the `read_ports` note: WAL lets it answer without the
/// worker's mutex or the Store's authoritative lock.
pub fn attribute_port(
    derived: &crate::derived::Derived,
    sweeps: Sweeps,
    state: &Arc<AppState>,
) -> Option<Arc<AttributePort>> {
    match derived.projections() {
        Ok(projections) => Some(Arc::new(AttributePort {
            projections: Mutex::new(projections),
            sweeps,
            state: Arc::downgrade(state),
            queries: AtomicU64::new(0),
            rows_answered: AtomicU64::new(0),
            total_millis: AtomicU64::new(0),
        })),
        Err(error) => {
            tracing::error!("the attribute query projection did not open: {error}");
            None
        }
    }
}

impl AttributeService for AttributePort {
    fn query(
        &self,
        query: &wire::AttributeQuery,
    ) -> Result<wire::AttributeQueryResponse, AttributeServiceError> {
        let started = std::time::Instant::now();
        let row_query = translate(query)?;
        let grouping = group_by(query.group_by.as_deref())?;

        // The scope is what the caller named, or every workspace the store
        // knows. Resolved from the authoritative catalog rather than from the
        // projection, so a workspace nothing has swept yet is still *named*
        // in `updating_workspaces` instead of silently absent.
        let scope: Vec<WorkspaceId> = if query.workspaces.is_empty() {
            match self.state.upgrade() {
                Some(state) => state
                    .store
                    .lock()
                    .map_err(|_| {
                        AttributeServiceError::Failed("the store lock is poisoned".into())
                    })?
                    .workspaces()
                    .map_err(|e| AttributeServiceError::Failed(e.to_string()))?
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

        let (rows, updating, system_documents) = {
            let projections = self.projections.lock().map_err(|_| {
                AttributeServiceError::Failed("the attribute projection lock was poisoned".into())
            })?;
            let mut updating = Vec::new();
            for workspace in &scope {
                if projections
                    .listing_digest(*workspace)
                    .map_err(|e| AttributeServiceError::Failed(e.to_string()))?
                    .is_none()
                {
                    updating.push(*workspace);
                }
            }
            let rows = projections
                .attribute_rows(&row_query)
                .map_err(|e| AttributeServiceError::Failed(e.to_string()))?;
            // Which documents hold each workspace's definitions and tags. From
            // the projection's listing rather than by opening every workspace:
            // it converges with the same sweep the rows do, and a workspace it
            // has not seen yet is already named in `updating`.
            let mut system_documents: Vec<(WorkspaceId, Vec<DocumentId>)> = Vec::new();
            for workspace in &scope {
                // The canonical one first — lowest id, every peer's pick — and
                // the rest absorbed into it, the same union `koru-workspace`
                // performs when it reconciles.
                let ids = systemdoc::system_documents(&projections, *workspace);
                if !ids.is_empty() {
                    system_documents.push((*workspace, ids));
                }
            }
            (rows, updating, system_documents)
        };

        // System definitions always: their semantics are this build's, and a
        // filter UI must be able to offer `koru.due` before any workspace has
        // defined anything.
        let mut definitions: Vec<wire::AttributeDefinition> =
            koru_core::attributes::system_definitions()
                .iter()
                .map(definition_to_wire)
                .collect();
        let mut tags = Vec::new();
        // Flat rather than per workspace: a `TagId` is a v4 UUID, so an alias
        // pair means the same thing whichever table minted it, and the fold is
        // one `CASE` over one column.
        let mut aliases: Vec<(String, String)> = Vec::new();
        if let Some(state) = self.state.upgrade() {
            for (workspace, ids) in system_documents {
                let Some(system) = systemdoc::open_system_document(&state, workspace, &ids) else {
                    continue;
                };
                definitions.extend(system.definitions().iter().map(definition_to_wire));
                tags.extend(system.tags().into_iter().map(|tag| wire::AttributeTag {
                    id: tag.id.to_storage(),
                    name: tag.name,
                    color: tag.color,
                    workspace,
                }));
                if grouping.is_some() {
                    aliases.extend(systemdoc::tag_aliases(&system));
                }
            }
        }

        // **After the tag tables, because grouping needs their aliases.** A
        // merged tag re-points no node, so grouping on the raw stored value
        // would put a merged-away id in a bucket the `tags` array above cannot
        // name — a merge, visible as a group splitting in two.
        let groups = match grouping {
            None => Vec::new(),
            Some(GroupBy::Tag) => {
                let projections = self.projections.lock().map_err(|_| {
                    AttributeServiceError::Failed(
                        "the attribute projection lock was poisoned".into(),
                    )
                })?;
                projections
                    .attribute_groups(&row_query, &aliases)
                    .map_err(|e| AttributeServiceError::Failed(e.to_string()))?
                    .into_iter()
                    .map(group_to_wire)
                    .collect()
            }
        };

        let response = wire::AttributeQueryResponse {
            rows: rows.into_iter().map(row_to_wire).collect(),
            definitions,
            tags,
            groups,
            projected: updating.is_empty(),
            updating_workspaces: updating,
            limit: row_query.limit,
            offset: row_query.offset,
        };

        let millis = started.elapsed().as_millis() as u64;
        let queries = self.queries.fetch_add(1, Ordering::Relaxed) + 1;
        let rows_answered = self
            .rows_answered
            .fetch_add(response.rows.len() as u64, Ordering::Relaxed)
            + response.rows.len() as u64;
        let total_millis = self.total_millis.fetch_add(millis, Ordering::Relaxed) + millis;
        tracing::debug!(
            target: "koru_attr",
            workspaces = scope.len(),
            rows = response.rows.len(),
            groups = response.groups.len(),
            millis,
            "attribute query answered"
        );
        if let Some(state) = self.state.upgrade()
            && let Ok(mut slot) = state.attributes.lock()
        {
            *slot = serde_json::json!({
                "queries": queries,
                "rows_answered": rows_answered,
                "total_millis": total_millis,
                "last_millis": millis,
            });
        }
        Ok(response)
    }
}

/// What a `group_by` token asks for. One arm today, and an enum rather than a
/// bool so the second one — priority, or a custom select — is a `match` arm
/// instead of a second flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GroupBy {
    Tag,
}

/// Parse `group_by`. Absent means no groups; an unknown token is the caller's
/// mistake, refused with a reason rather than silently answering ungrouped —
/// a surface that asked for counts and drew none would look like a store bug.
fn group_by(token: Option<&str>) -> Result<Option<GroupBy>, AttributeServiceError> {
    match token {
        None => Ok(None),
        Some("tag") => Ok(Some(GroupBy::Tag)),
        Some(_) => Err(AttributeServiceError::Invalid(
            "group_by must be tag".into(),
        )),
    }
}

/// Wire filters to the typed projection query. `Invalid` for anything a
/// caller could fix — an unknown token, a bound naming both domains — so it
/// comes back as a 400 with a reason rather than a storage failure.
/// `pub(crate)` for one caller: the tasks port translates the **same six
/// shared filters** and must not carry a second copy of them. Two translations
/// that had drifted would make `min_priority` a different floor on one route
/// than on the other, over one SQL builder that cannot tell them apart.
pub(crate) fn translate(
    query: &wire::AttributeQuery,
) -> Result<AttributeRowQuery, AttributeServiceError> {
    let priority = |token: &Option<String>, which: &str| match token {
        None => Ok(None),
        Some(token) => Priority::parse(token).map(Some).ok_or_else(|| {
            AttributeServiceError::Invalid(format!(
                "{which} must be one of low, medium, high, urgent"
            ))
        }),
    };
    let mut tags = Vec::with_capacity(query.tags.len());
    for tag in &query.tags {
        let id = TagId::parse(tag).ok_or_else(|| {
            AttributeServiceError::Invalid("a tag filter must be a tag id".into())
        })?;
        tags.push(id.to_storage());
    }
    let sort = match query.sort.as_deref() {
        None | Some("modified") => AttrSort::Modified,
        Some("due") => AttrSort::Due,
        Some("priority") => AttrSort::Priority,
        Some(_) => {
            return Err(AttributeServiceError::Invalid(
                "sort must be one of due, priority, modified".into(),
            ));
        }
    };
    Ok(AttributeRowQuery {
        workspaces: query.workspaces.clone(),
        has: query.has.clone(),
        missing: query.missing.clone(),
        tags,
        priority: priority(&query.priority, "priority")?,
        min_priority: priority(&query.min_priority, "min_priority")?,
        due: query.due.as_ref().map(due_filter).transpose()?,
        sort,
        limit: query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        offset: query.offset.unwrap_or(0),
    })
}

/// **One window per temporal type, and each compared in its own terms**
/// ([ADR-183], rule 5).
///
/// A caller asking about days sends `dates`; a caller asking about moments
/// sends `instants`; a caller who means both sends both, and the windows
/// compose with OR. Nothing here converts one into the other — the shape this
/// replaced cut instants at the UTC day boundary, which answers a different
/// question from the one a reader outside UTC asked.
///
/// A filter naming neither window is refused rather than treated as "no
/// constraint": it is a question nobody asked, and a caller can fix it.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
pub(crate) fn due_filter(filter: &wire::DueFilter) -> Result<DueFilter, AttributeServiceError> {
    if filter.dates.is_none() && filter.instants.is_none() {
        return Err(AttributeServiceError::Invalid(
            "a due filter names a calendar window (dates), an instant window \
             (instants), or both"
                .into(),
        ));
    }
    let dates = match &filter.dates {
        None => None,
        Some(window) => {
            for bound in [&window.after, &window.before] {
                if let Some(day) = bound
                    && !is_calendar_date(day)
                {
                    return Err(AttributeServiceError::Invalid(
                        "a due window's calendar bound must be YYYY-MM-DD".into(),
                    ));
                }
            }
            Some(DateWindow {
                after: window.after.clone(),
                before: window.before.clone(),
            })
        }
    };
    Ok(DueFilter {
        dates,
        instants: filter.instants.map(|window| InstantWindow {
            after_ms: window.after_ms,
            before_ms: window.before_ms,
        }),
    })
}

/// The same strict shape the projection enforces, checked here so a caller's
/// typo is a 400 rather than a 500 from the read model.
fn is_calendar_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit())
}

fn definition_to_wire(
    def: &koru_core::attributes::AttributeDefinition,
) -> wire::AttributeDefinition {
    wire::AttributeDefinition {
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

fn group_to_wire(group: koru_projection::store::AttributeGroup) -> wire::AttributeGroup {
    wire::AttributeGroup {
        workspace: group.workspace,
        tag: group.tag,
        count: group.count,
        trashed: group.trashed,
    }
}

fn row_to_wire(row: koru_projection::store::AttributeQueryRow) -> wire::AttributeRow {
    wire::AttributeRow {
        workspace: row.workspace,
        document: row.document,
        node: row.node,
        node_kind: row.node_kind,
        excerpt: row.excerpt,
        document_title: row.title,
        trashed: row.trashed,
        attributes: row
            .attrs
            .into_iter()
            .map(|attr| wire::AttributeRowValue {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> wire::AttributeQuery {
        wire::AttributeQuery::default()
    }

    #[test]
    fn an_unknown_priority_token_is_the_callers_mistake_not_storage_trouble() {
        let result = translate(&wire::AttributeQuery {
            min_priority: Some("critical".into()),
            ..base()
        });
        assert!(matches!(result, Err(AttributeServiceError::Invalid(_))));
    }

    /// A due filter has to say **which temporal type it is asking about**.
    /// Naming no window at all is a question nobody asked, and a malformed
    /// calendar bound is the caller's typo — both come back as a 400 with a
    /// reason rather than as an empty page or a storage failure.
    #[test]
    fn a_due_filter_names_at_least_one_window_and_spells_its_dates_properly() {
        for filter in [
            wire::DueFilter {
                dates: None,
                instants: None,
            },
            wire::DueFilter {
                dates: Some(wire::DateWindow {
                    after: Some("september first".into()),
                    before: None,
                }),
                instants: None,
            },
            wire::DueFilter {
                dates: Some(wire::DateWindow {
                    after: None,
                    before: Some("2026-9-1".into()),
                }),
                instants: None,
            },
        ] {
            let result = translate(&wire::AttributeQuery {
                due: Some(filter),
                ..base()
            });
            assert!(matches!(result, Err(AttributeServiceError::Invalid(_))));
        }
    }

    /// **The two windows are separate questions and both survive translation.**
    /// Naming both is how a caller asks "this day, however it was set" — which
    /// is the only honest way to ask it, since folding a calendar bound onto
    /// instants would answer in whatever zone this layer happened to pick.
    #[test]
    fn a_due_filter_carries_each_temporal_window_through_untouched() {
        let translated = translate(&wire::AttributeQuery {
            due: Some(wire::DueFilter {
                dates: Some(wire::DateWindow {
                    after: Some("2026-09-01".into()),
                    before: Some("2026-09-02".into()),
                }),
                instants: Some(wire::InstantWindow {
                    after_ms: Some(1_000),
                    before_ms: Some(2_000),
                }),
            }),
            ..base()
        })
        .unwrap();
        let due = translated.due.expect("the filter survived");
        let dates = due.dates.expect("the calendar window survived");
        assert_eq!(dates.after.as_deref(), Some("2026-09-01"));
        assert_eq!(dates.before.as_deref(), Some("2026-09-02"));
        let instants = due.instants.expect("the instant window survived");
        assert_eq!(instants.after_ms, Some(1_000));
        assert_eq!(instants.before_ms, Some(2_000));
    }

    #[test]
    fn the_limit_is_defaulted_and_capped_rather_than_refused() {
        assert_eq!(translate(&base()).unwrap().limit, DEFAULT_LIMIT);
        let huge = translate(&wire::AttributeQuery {
            limit: Some(1_000_000),
            ..base()
        })
        .unwrap();
        assert_eq!(huge.limit, MAX_LIMIT);
        let zero = translate(&wire::AttributeQuery {
            limit: Some(0),
            ..base()
        })
        .unwrap();
        assert_eq!(zero.limit, 1);
    }

    #[test]
    fn sort_tokens_are_the_three_documented_ones() {
        assert_eq!(translate(&base()).unwrap().sort, AttrSort::Modified);
        for (token, sort) in [
            ("due", AttrSort::Due),
            ("priority", AttrSort::Priority),
            ("modified", AttrSort::Modified),
        ] {
            let translated = translate(&wire::AttributeQuery {
                sort: Some(token.into()),
                ..base()
            })
            .unwrap();
            assert_eq!(translated.sort, sort);
        }
        assert!(matches!(
            translate(&wire::AttributeQuery {
                sort: Some("alphabetic".into()),
                ..base()
            }),
            Err(AttributeServiceError::Invalid(_))
        ));
    }

    /// **An unknown `group_by` is refused, not ignored.** A surface that asked
    /// for counts and got an ungrouped answer would draw no headings and read
    /// as a broken store rather than as its own typo.
    #[test]
    fn group_by_is_the_one_token_or_a_refusal() {
        assert_eq!(group_by(None).unwrap(), None);
        assert_eq!(group_by(Some("tag")).unwrap(), Some(GroupBy::Tag));
        for bad in ["tags", "priority", ""] {
            assert!(matches!(
                group_by(Some(bad)),
                Err(AttributeServiceError::Invalid(_))
            ));
        }
    }

    #[test]
    fn a_tag_filter_must_hold_tag_ids() {
        assert!(matches!(
            translate(&wire::AttributeQuery {
                tags: vec!["#mvp".into()],
                ..base()
            }),
            Err(AttributeServiceError::Invalid(_))
        ));
        let id = TagId::new();
        let ok = translate(&wire::AttributeQuery {
            tags: vec![id.to_storage()],
            ..base()
        })
        .unwrap();
        assert_eq!(ok.tags, vec![id.to_storage()]);
    }
}
