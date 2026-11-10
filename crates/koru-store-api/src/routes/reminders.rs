// SPDX-License-Identifier: MPL-2.0

//! Public reminder operations over the synchronized profile root.
//!
//! Projection reads are synchronous SQLite work and may wait for a bounded
//! workspace sweep. The handlers copy profile state while holding the Store
//! mutex, release it, then use `spawn_blocking`; neither the Tokio worker nor
//! the Store serialization point is held through that wait.

use std::sync::Arc;

pub mod scoped;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use koru_store_core::errors::{Code, Error};
use koru_store_core::ids::{ProfileId, ReminderId};
use koru_store_core::model::Scope;
use koru_store_core::personal_attributes::EntityRef;
use koru_store_core::reminders::{
    ActionKind, OneTimeSchedule, ReminderRule, RuleStatus, StoredReminder,
};
use koru_store_wire::reminders::{
    ActionRequest, ClearHistoryQuery, ClearedHistory, ReminderQuery, ReminderQueryResponse,
    ReminderSnapshot, RevisionRequest, RuleRequest, SettingsPatch,
};
use serde::Deserialize;

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct EntityQuery {
    entity: Option<String>,
}

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::ReadDocuments)?;
    let (profile, reminder) = ids(&profile, &reminder)?;
    Ok(Json(lock(&state)?.reminder(profile, reminder)?))
}

pub async fn list_for_entity(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
    Query(query): Query<EntityQuery>,
) -> ApiResult<Json<Vec<StoredReminder>>> {
    caller.require(Scope::ReadDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    let target = query
        .entity
        .as_deref()
        .and_then(EntityRef::parse)
        .ok_or_else(invalid_entity)?;
    Ok(Json(lock(&state)?.reminders_for(profile, &target)?))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
    Json(body): Json<RuleRequest>,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::EditDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    validate_authored_rule(&body.rule)?;
    let answer = lock(&state)?.create_reminder(profile, &body.rule)?;
    state.notify_replication();
    Ok(Json(answer))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
    Json(body): Json<RuleRequest>,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::EditDocuments)?;
    let (profile, reminder) = ids(&profile, &reminder)?;
    validate_rule_path(reminder, &body.rule)?;
    validate_authored_rule(&body.rule)?;
    let answer = lock(&state)?.update_reminder(profile, &body.rule)?;
    state.notify_replication();
    Ok(Json(answer))
}

async fn status(
    state: Arc<AppState>,
    caller: Caller,
    profile: String,
    reminder: String,
    body: RevisionRequest,
    status: RuleStatus,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::EditDocuments)?;
    let (profile, reminder) = ids(&profile, &reminder)?;
    let answer = lock(&state)?.set_reminder_status(profile, reminder, body.revision, status)?;
    state.notify_replication();
    Ok(Json(answer))
}

pub async fn pause(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
    Json(body): Json<RevisionRequest>,
) -> ApiResult<Json<StoredReminder>> {
    status(state, caller, profile, reminder, body, RuleStatus::Paused).await
}

pub async fn resume(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
    Json(body): Json<RevisionRequest>,
) -> ApiResult<Json<StoredReminder>> {
    status(state, caller, profile, reminder, body, RuleStatus::Enabled).await
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::EditDocuments)?;
    let (profile, reminder) = ids(&profile, &reminder)?;
    let answer = lock(&state)?.delete_reminder_rule(profile, reminder)?;
    state.notify_replication();
    Ok(Json(answer))
}

async fn action(
    state: Arc<AppState>,
    caller: Caller,
    profile: String,
    reminder: String,
    body: ActionRequest,
    snooze: bool,
) -> ApiResult<Json<StoredReminder>> {
    caller.require(Scope::EditDocuments)?;
    let (profile, reminder) = ids(&profile, &reminder)?;
    validate_action_route(&body.action.kind, snooze)?;
    let answer = lock(&state)?.append_reminder_action(profile, reminder, &body.action)?;
    state.notify_replication();
    Ok(Json(answer))
}

pub async fn snooze(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
    Json(body): Json<ActionRequest>,
) -> ApiResult<Json<StoredReminder>> {
    action(state, caller, profile, reminder, body, true).await
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, reminder)): Path<(String, String)>,
    Json(body): Json<ActionRequest>,
) -> ApiResult<Json<StoredReminder>> {
    action(state, caller, profile, reminder, body, false).await
}

pub async fn clear_history(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
    Query(query): Query<ClearHistoryQuery>,
) -> ApiResult<Json<ClearedHistory>> {
    caller.require(Scope::EditDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    let target = query
        .entity
        .as_deref()
        .map(|value| EntityRef::parse(value).ok_or_else(invalid_entity))
        .transpose()?;
    let cleared = match target {
        Some(target) => lock(&state)?.clear_reminder_history_for_target(profile, &target)?,
        None => lock(&state)?.clear_reminder_history(profile)?,
    };
    if cleared > 0 {
        state.notify_replication();
    }
    Ok(Json(ClearedHistory { cleared }))
}

pub async fn settings(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
) -> ApiResult<Json<koru_store_core::reminders::ReminderSettings>> {
    caller.require(Scope::ReadDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    Ok(Json(lock(&state)?.reminder_settings(profile)?))
}

pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
    Json(body): Json<SettingsPatch>,
) -> ApiResult<Json<koru_store_core::reminders::ReminderSettings>> {
    caller.require(Scope::EditDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    let answer = lock(&state)?.update_reminder_settings(
        profile,
        body.end_of_day,
        body.default_tone,
        body.week_start,
    )?;
    state.notify_replication();
    Ok(Json(answer))
}

pub async fn reconciliation(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
) -> ApiResult<Json<ReminderSnapshot>> {
    caller.require(Scope::ReadDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    let (reminders, settings) = indexed_reminder_inputs(Arc::clone(&state), profile).await?;
    let service = state
        .reminder_projection_service()
        .ok_or_else(projection_unavailable)?;
    blocking_projection(move || service.snapshot(&reminders, &settings)).await
}

pub async fn query(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(profile): Path<String>,
    Json(query): Json<ReminderQuery>,
) -> ApiResult<Json<ReminderQueryResponse>> {
    caller.require(Scope::ReadDocuments)?;
    let profile = ProfileId::parse(&profile).ok_or_else(invalid_id)?;
    let (reminders, settings) = indexed_reminder_inputs(Arc::clone(&state), profile).await?;
    let service = state
        .reminder_projection_service()
        .ok_or_else(projection_unavailable)?;
    blocking_projection(move || service.query(&reminders, &settings, &query)).await
}

/// A missed post-commit wake is repaired by this same path on reconnect.
/// Capture opaque bytes briefly, then decode/reduce outside the Store lock.
async fn indexed_reminder_inputs(
    state: Arc<AppState>,
    profile: ProfileId,
) -> ApiResult<(
    Vec<koru_store_core::ReminderReductionInput>,
    koru_store_core::ReminderSettings,
)> {
    tokio::task::spawn_blocking(move || {
        let (candidates, existing_rows) = {
            let store = lock(&state)?;
            (
                store.reminder_index_candidates(profile)?,
                store.reminder_index_rows(profile)?,
            )
        };
        let mut prepared = Vec::new();
        let mut reminders = Vec::new();
        let mut expected_rows = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for candidate in candidates {
            let (version, rows) = koru_store_core::service::build_reminder_index(&candidate)?;
            let inputs = koru_store_core::service::read_personal_reminder_inputs(&candidate)?;
            if inputs
                .iter()
                .any(|input| !seen.insert(input.stored().rule.id))
            {
                return Err(ApiError::new(
                    Code::ProfileUnitRequired,
                    "reminder id occurs in multiple units",
                ));
            }
            let workspace = candidate.unit.workspace();
            let indexed: std::collections::BTreeSet<_> = existing_rows
                .iter()
                .filter(|row| Some(row.workspace) == workspace)
                .map(|row| (row.reminder, row.due_at_ms))
                .collect();
            let expected: std::collections::BTreeSet<_> = rows
                .iter()
                .map(|row| (row.reminder, row.due_at_ms))
                .collect();
            // The stamp is a *version*, compared decoded — two encodings of one
            // version differ in peer order, and reading that as a change
            // republishes an index that was already current
            // (`indexed_version_is_current`).
            let complete = koru_store_core::service::indexed_version_is_current(
                candidate.indexed_version.as_deref(),
                &version,
            ) && indexed == expected;
            if !complete
                && !lock(&state)?.publish_reminder_index(&candidate, &version.encode(), &rows)?
            {
                return Err(ApiError::new(
                    Code::ReminderIndexUpdating,
                    "personal unit changed during index repair",
                ));
            }
            prepared.push((candidate, version));
            reminders.extend(inputs);
            expected_rows.extend(rows);
        }
        let mut store = lock(&state)?;
        let mut current_rows = store.reminder_index_rows(profile)?;
        current_rows.sort_by_key(|row| (row.workspace, row.reminder));
        expected_rows.sort_by_key(|row| (row.workspace, row.reminder));
        // Re-read the authority: the same units, the same bytes, and a stamp
        // that still names the version each index was built from. The stamp is
        // the one field that cannot be compared as bytes, so it is compared
        // apart from the rest rather than through `ProfileUnitState`'s
        // derived equality — a unit left alone because it was already current
        // still holds the *earlier* encoding of that version, and reading that
        // as a concurrent change failed a read that nothing had disturbed.
        let found = store.reminder_index_candidates(profile)?;
        let undisturbed = found.len() == prepared.len()
            && std::iter::zip(&found, &prepared).all(|(found, (indexed, version))| {
                found.unit == indexed.unit
                    && found.snapshot == indexed.snapshot
                    && koru_store_core::service::indexed_version_is_current(
                        found.indexed_version.as_deref(),
                        version,
                    )
            });
        if !undisturbed || current_rows != expected_rows {
            return Err(ApiError::new(
                Code::ReminderIndexUpdating,
                "personal authority changed during index read",
            ));
        }
        Ok((reminders, store.reminder_settings(profile)?))
    })
    .await
    .map_err(|_| ApiError::new(Code::Internal, "reminder index worker did not finish"))?
}

async fn blocking_projection<T>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> ApiResult<Json<T>>
where
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => Ok(Json(result?)),
        Err(join) => Err(ApiError::new(
            Code::Internal,
            format!("the reminder projection thread did not finish: {join}"),
        )),
    }
}

fn ids(profile: &str, reminder: &str) -> ApiResult<(ProfileId, ReminderId)> {
    Ok((
        ProfileId::parse(profile).ok_or_else(invalid_id)?,
        ReminderId::parse(reminder).ok_or_else(invalid_id)?,
    ))
}

fn validate_authored_rule(rule: &ReminderRule) -> ApiResult<()> {
    let OneTimeSchedule::Zoned { value } = &rule.schedule else {
        return Ok(());
    };
    let rules =
        koru_time_zone::IanaZoneRules::parse(value.zone()).map_err(|_| invalid_reminder_zone())?;
    if rules.canonical_name().as_str() != value.zone() {
        return Err(invalid_reminder_zone());
    }
    Ok(())
}

fn validate_rule_path(reminder: ReminderId, rule: &koru_store_core::ReminderRule) -> ApiResult<()> {
    if rule.id != reminder {
        return Err(ApiError::new(
            Code::InvalidRequest,
            "path and rule ids differ",
        ));
    }
    Ok(())
}

fn validate_action_route(kind: &ActionKind, snooze: bool) -> ApiResult<()> {
    let is_snooze = matches!(kind, ActionKind::SnoozedUntil { .. });
    if is_snooze != snooze {
        return Err(ApiError::new(
            Code::InvalidRequest,
            "action kind does not match route",
        ));
    }
    Ok(())
}

fn invalid_id() -> ApiError {
    ApiError::new(Code::InvalidId, "not a canonical id")
}
fn invalid_entity() -> ApiError {
    ApiError::new(Code::InvalidRequest, "not a canonical entity reference")
}
fn invalid_reminder_zone() -> ApiError {
    ApiError::new(
        Code::InvalidRequest,
        "a zoned reminder requires a canonical IANA zone",
    )
}
fn projection_unavailable() -> ApiError {
    ApiError::new(
        Code::ProjectionUnavailable,
        "reminder projection is unavailable",
    )
}
fn lock(state: &Arc<AppState>) -> ApiResult<std::sync::MutexGuard<'_, crate::state::KoruStore>> {
    state
        .store
        .lock()
        .map_err(|_| ApiError::from(Error::new(Code::Internal, "the store lock is poisoned")))
}

#[cfg(test)]
mod tests;
