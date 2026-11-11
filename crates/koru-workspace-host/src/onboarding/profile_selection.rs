// SPDX-License-Identifier: MPL-2.0

//! Account-form selection only. The shared worker remains the set authority.
use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct SelectionDraft {
    pub checked: BTreeSet<String>,
    pub submitted: Option<Vec<String>>,
    pub submission: Option<crate::account_ui::CommandSubmissionId>,
    pub waiting: bool,
    pub uncertain: bool,
    pub generation: Option<u64>,
    pub floor: u64,
    pub notice: Option<koru_i18n::Key>,
    pub early_results: std::collections::BTreeMap<u64, koru_agent_wire::AccountOperationResult>,
}

impl SelectionDraft {
    pub fn reconcile(&mut self, authorized: &BTreeSet<String>) {
        self.checked.retain(|id| authorized.contains(id));
        // The admitted set is immutable even if grants change while it waits.
    }

    pub fn toggle(&mut self, id: &str) {
        if self.locked() {
            return;
        }
        self.submitted = None;
        if !self.checked.remove(id) {
            self.checked.insert(id.to_owned());
        }
    }

    pub fn locked(&self) -> bool {
        self.waiting || self.uncertain
    }

    pub fn choice(
        &self,
        authorized: &BTreeSet<String>,
        completed: &BTreeSet<String>,
    ) -> Vec<String> {
        self.checked
            .intersection(authorized)
            .filter(|id| !completed.contains(*id))
            .cloned()
            .collect()
    }

    pub fn begin(&mut self, ids: Vec<String>) -> bool {
        if self.locked() || ids.is_empty() {
            return false;
        }
        self.checked = ids.iter().cloned().collect();
        self.submitted = Some(ids);
        self.submission = Some(crate::account_ui::CommandSubmissionId::next());
        self.generation = None;
        self.early_results.clear();
        self.waiting = true;
        self.notice = None;
        true
    }

    pub fn failed(&mut self, failure: crate::account_ui::CommandFailure) -> bool {
        if self.submission != Some(failure.submission) || !self.waiting {
            return false;
        }
        self.waiting = false;
        self.uncertain = failure.uncertain;
        if !failure.uncertain {
            self.submission = None;
        }
        true
    }

    pub fn settled(&mut self) {
        self.submission = None;
        self.generation = None;
        self.waiting = false;
        self.uncertain = false;
        self.notice = None;
    }
}

pub(super) struct RowLayout {
    pub rect: Rect,
    source: String,
    source_height: f32,
    status: String,
    action: Option<(Hit, Rect, &'static str)>,
}
pub(super) struct ChooserLayout {
    pub viewport: Rect,
    pub rows: Vec<RowLayout>,
    pub content_height: f32,
    title: Rect,
    body: Rect,
    count: Rect,
    pub(super) footer: Vec<(Hit, Rect, &'static str, bool)>,
}

impl OnboardingView {
    pub fn observe_account_projection(&mut self, state: &AccountState) {
        self.account_state = state.clone();
        self.prune_choice_grants();
    }
    pub(super) fn prune_choice_grants(&mut self) {
        let selected = self.selected_account_profile_id().map(str::to_owned);
        let profiles = std::mem::take(&mut self.account_profiles);
        let sources = std::mem::take(&mut self.account_profile_sources);
        let (profiles, sources) = profiles
            .into_iter()
            .zip(sources)
            .filter(|(row, _)| self.choice_authorized(row))
            .unzip();
        self.account_profiles = profiles;
        self.account_profile_sources = sources;
        self.selected_existing = selected.and_then(|id| {
            self.account_profiles
                .iter()
                .position(|row| row.profile_id == id)
        });
        self.selection.reconcile(&self.authorized_selection_ids());
    }
    pub(super) fn choice_authorized(&self, row: &AccountProfileRow) -> bool {
        // ProfileChoices is already a targeted authorized response. When the
        // matching broadcast owner is present, additionally enforce its exact
        // account/Auth Identity; a missing broadcast is not invented authority.
        let held = self.account_state.held_accounts.iter().find(|held| {
            held.scope_id == self.account_state.active_scope
                && self
                    .account_state
                    .active_account_id
                    .as_ref()
                    .is_none_or(|id| id == &held.account_id)
        });
        self.account_state
            .active_account_id
            .as_ref()
            .is_none_or(|id| id == &row.koru_identity_id)
            && held
                .and_then(|held| held.auth_identity_id.as_ref())
                .is_none_or(|id| {
                    row.auth_identities
                        .iter()
                        .any(|auth| &auth.auth_identity_id == id)
                })
            && uuid::Uuid::parse_str(&row.profile_id).is_ok_and(|id| {
                id.get_version_num() == 4
                    && id.get_variant() == uuid::Variant::RFC4122
                    && id.hyphenated().to_string() == row.profile_id
            })
    }
    pub fn command_submission(
        &self,
        command: &koru_agent_wire::AccountCommand,
    ) -> Option<crate::account_ui::CommandSubmissionId> {
        use koru_agent_wire::AccountCommand as C;
        match command {
            C::StartGoogle { .. }
            | C::StartApple { .. }
            | C::ContinueWithEmail { .. }
            | C::Cancel { .. } => self.browser_submission(command),
            C::SelectProfiles { .. } | C::CancelEnrollment => self.selection.submission,
            C::ConfirmAccountCreation | C::CancelAccountCreation | C::Retry => {
                self.creation_submission
            }
            _ => None,
        }
    }
    pub fn account_command_admitted(
        &mut self,
        admission: crate::account_ui::CommandAdmission,
    ) -> bool {
        if self.browser_command_admitted(admission) {
            return true;
        }
        if admission.owner_scope == self.account_scope()
            && self.selection.submission == Some(admission.submission)
        {
            self.selection.generation = Some(admission.generation);
            if let Some(result) = self.selection.early_results.remove(&admission.generation) {
                self.selection_result(admission.owner_scope, admission.generation, &result);
            }
            self.selection.early_results.clear();
            return true;
        }
        self.account_forms
            .values_mut()
            .fold(false, |changed, view| {
                view.account_command_admitted(admission) || changed
            })
    }
    pub fn selection_result(
        &mut self,
        owner: u64,
        generation: u64,
        result: &koru_agent_wire::AccountOperationResult,
    ) -> bool {
        if owner != self.account_scope() {
            return self
                .account_forms
                .get_mut(&owner)
                .is_some_and(|view| view.selection_result(owner, generation, result));
        }
        if self.selection.submission.is_none() {
            return false;
        }
        use koru_agent_wire::AccountOperationResult as R;
        if !matches!(
            result,
            R::ProfilesSelected { .. } | R::Cancelled | R::Failed { .. }
        ) {
            return false;
        }
        if !self
            .selection
            .generation
            .map_or(generation > self.selection.floor, |expected| {
                generation == expected
            })
        {
            return true;
        }
        if self.selection.generation.is_none() {
            if self.selection.early_results.len() < 8 {
                self.selection
                    .early_results
                    .insert(generation, result.clone());
            } else {
                self.selection.waiting = false;
                self.selection.uncertain = true;
                self.selection.notice = Some(koru_i18n::Key::ProfileChooserUncertain);
            }
            return true;
        }
        match result {
            R::ProfilesSelected { profiles } if !self.enrollment_cancelling => {
                let profiles = crate::account_ui::selection_result_rows(
                    &self.account_state,
                    generation,
                    profiles.clone(),
                );
                self.profiles_selected(profiles);
            }
            R::Cancelled if self.enrollment_cancelling => {
                self.account_state
                    .enrollments
                    .retain(|row| row.state == koru_agent_wire::EnrollmentState::Complete);
                self.account_cancelled();
            }
            R::Failed { .. } => {
                self.selection.settled();
                self.selection.submitted = None;
                self.enrollment_cancelling = false;
                if !self.keep_local_selection_form() {
                    self.screen = Screen::ExistingProfiles;
                }
                self.selection.notice = Some(koru_i18n::Key::ProfileChooserRefused);
            }
            _ => return true,
        }
        self.selection.floor = self.selection.floor.max(generation);
        true
    }
    fn chooser_copy(&self, key: koru_i18n::Key) -> &'static str {
        koru_i18n::t(self.language, key)
    }
    pub(super) fn keep_local_selection_form(&self) -> bool {
        matches!(self.screen, Screen::Profile | Screen::Workspace)
            || self.created_profile_retry.is_some()
    }
    fn selection_body(&self) -> String {
        self.selection
            .notice
            .map(|key| self.chooser_copy(key).to_owned())
            .or_else(|| self.error.clone())
            .unwrap_or_else(|| self.chooser_copy(koru_i18n::Key::ProfileChooserBody).into())
    }

    /// The rows a person may tick: authorized, and **not already on this
    /// computer**. A Profile that is here has nothing to import — its row
    /// says so and offers *Open local content* instead of a checkbox. It used
    /// to be a checkbox like the others, counted in *N selected* and sent in
    /// the selection, which read as importing a Profile a second time.
    pub(super) fn authorized_selection_ids(&self) -> BTreeSet<String> {
        self.account_profiles
            .iter()
            .zip(&self.account_profile_sources)
            .filter(|(row, source)| **source != ProfileSource::Local && self.choice_authorized(row))
            .map(|(row, _)| row.profile_id.clone())
            .collect()
    }

    /// Whether the row at `index` is a Profile this computer already holds.
    fn already_here(&self, index: usize) -> bool {
        self.account_profile_sources.get(index) == Some(&ProfileSource::Local)
    }
    fn completed_selection_ids(&self) -> BTreeSet<String> {
        self.account_state
            .enrollments
            .iter()
            .filter(|row| row.state == koru_agent_wire::EnrollmentState::Complete)
            .map(|row| row.profile_id.clone())
            .collect()
    }
    fn selection_ids(&self) -> Vec<String> {
        self.selection
            .choice(&self.authorized_selection_ids(), &BTreeSet::new())
    }
    fn enrollment_selection_ids(&self) -> Vec<String> {
        self.selection.choice(
            &self.authorized_selection_ids(),
            &self.completed_selection_ids(),
        )
    }
    pub(super) fn selection_has_unfinished(&self) -> bool {
        self.selection.locked()
            || self
                .account_state
                .enrollments
                .iter()
                .any(|row| row.state != koru_agent_wire::EnrollmentState::Complete)
    }
    fn member_retry(&self, index: usize) -> bool {
        !self.selection.locked()
            && self.account_profiles.get(index).is_some_and(|profile| {
                self.account_state.enrollments.iter().any(|row| {
                    row.profile_id == profile.profile_id
                        && row.state != koru_agent_wire::EnrollmentState::Complete
                        && (row.failure_code.is_some()
                            || matches!(
                                row.state,
                                koru_agent_wire::EnrollmentState::Waiting
                                    | koru_agent_wire::EnrollmentState::Failed
                            ))
                })
            })
    }
    pub(super) fn selection_controls(&self) -> Vec<Hit> {
        let mut controls = Vec::new();
        let authorized = self.authorized_selection_ids();
        for (index, profile) in self.account_profiles.iter().enumerate() {
            if !self.selection.locked() && authorized.contains(&profile.profile_id) {
                controls.push(Hit::Existing(index));
            }
            if self.account_profile_sources.get(index) == Some(&ProfileSource::Local) {
                controls.push(Hit::OpenExisting(index));
            } else if self.member_retry(index) {
                controls.push(Hit::RetryExisting(index));
            }
        }
        if !self.selection.locked() && !self.selection_ids().is_empty() {
            controls.push(Hit::Primary);
        }
        if self.selection_has_unfinished() && !self.selection.waiting {
            controls.push(Hit::CancelSelection);
        }
        if !self.selection.locked() {
            controls.push(Hit::RefreshProfiles);
        }
        controls.extend([Hit::CreateNew, Hit::ContinueOffline]);
        controls
    }

    pub(super) fn activate_selection(&mut self, hit: Hit) {
        let Some(previous_focus) = self
            .selection_controls()
            .iter()
            .position(|control| *control == hit)
        else {
            return;
        };
        match hit {
            Hit::Existing(index) => {
                self.selection
                    .toggle(&self.account_profiles[index].profile_id);
                self.selected_existing = Some(index);
            }
            Hit::Primary => {
                let selected = self.selection_ids();
                let ids = self.enrollment_selection_ids();
                if ids.is_empty() {
                    self.selection.submitted = Some(selected);
                    self.open_first_connected_selection();
                    return;
                }
                self.selection.floor = self.selection.floor.max(self.account_state.generation);
                if self.selection.begin(selected) {
                    self.enrollment_cancelling = false;
                    self.pending = Some(Action::SelectAccountProfiles { profile_ids: ids });
                    self.error = None;
                }
            }
            Hit::OpenExisting(index) => {
                self.selected_existing = Some(index);
                self.pending = Some(Action::RestoreLocalProfile {
                    profile_id: self.account_profiles[index].profile_id.clone(),
                });
                self.screen = Screen::RestoringProfile;
            }
            Hit::RetryExisting(index) => {
                self.pending = Some(Action::Account(
                    koru_agent_wire::AccountCommand::RetryEnrollment {
                        profile_id: Some(self.account_profiles[index].profile_id.clone()),
                    },
                ))
            }
            Hit::RefreshProfiles => {
                self.pending = Some(Action::Account(
                    koru_agent_wire::AccountCommand::ListProfileChoices,
                ))
            }
            Hit::CancelSelection => {
                self.selection.submission = Some(crate::account_ui::CommandSubmissionId::next());
                self.selection.floor = self.selection.floor.max(self.account_state.generation);
                self.selection.generation = None;
                self.selection.early_results.clear();
                self.selection.waiting = true;
                self.enrollment_cancelling = true;
                self.pending = Some(Action::Account(
                    koru_agent_wire::AccountCommand::CancelEnrollment,
                ));
            }
            _ => {}
        }
        self.focus = self
            .selection_controls()
            .iter()
            .position(|control| *control == hit)
            .unwrap_or(previous_focus);
    }

    fn chooser_button(&mut self, label: &str, x: f32, y: f32, max_width: f32) -> Rect {
        let width = button_width(&mut self.engine, label).min(max_width);
        let height = self
            .engine
            .simple_layout_wrapped(label, theme::role::row(), (width - 24.0).max(1.0))
            .height()
            + 14.0;
        Rect::new(x, y, width, height.max(32.0))
    }

    pub(super) fn selection_layout(&mut self) -> ChooserLayout {
        use koru_i18n::Key as K;
        let c = self.content();
        let title_h = self
            .engine
            .simple_layout_wrapped(
                self.chooser_copy(K::ProfileChooserTitle),
                theme::role::subtitle(),
                c.w,
            )
            .height();
        let title = Rect::new(c.x, c.y + 40.0, c.w, title_h);
        let body_copy = self.selection_body();
        let body_h = self
            .engine
            .simple_layout_wrapped(&body_copy, theme::role::meta(), c.w)
            .height();
        let body = Rect::new(c.x, title.y + title.h + 10.0, c.w, body_h);
        let count_copy = self.selection_count();
        let count_h = self
            .engine
            .simple_layout_wrapped(&count_copy, theme::role::meta(), c.w)
            .height();
        let count = Rect::new(c.x, body.y + body.h + 12.0, c.w, count_h);
        let specs = [
            (
                Hit::Primary,
                if !self.selection_ids().is_empty() && self.enrollment_selection_ids().is_empty() {
                    K::ProfileChooserOpen
                } else {
                    K::ProfileChooserConnect
                },
                !self.selection.locked() && !self.selection_ids().is_empty(),
            ),
            (
                Hit::CancelSelection,
                K::ProfileChooserCancel,
                self.selection_has_unfinished() && !self.selection.waiting,
            ),
            (
                Hit::RefreshProfiles,
                K::ProfileChooserRefresh,
                !self.selection.locked(),
            ),
            (Hit::CreateNew, K::ProfileChooserCreate, true),
            (Hit::ContinueOffline, K::AccountContinueOffline, true),
        ];
        let mut footer = Vec::new();
        let mut x = c.x;
        let mut y = 0.0;
        let mut row_h: f32 = 0.0;
        for (hit, key, enabled) in specs {
            if hit == Hit::CancelSelection && !self.selection_has_unfinished() {
                continue;
            }
            let label = self.chooser_copy(key);
            let mut rect = self.chooser_button(label, x, y, c.w);
            if x > c.x && x + rect.w > c.x + c.w {
                x = c.x;
                y += row_h + 8.0;
                row_h = 0.0;
                rect.x = x;
                rect.y = y;
            }
            x += rect.w + 8.0;
            row_h = row_h.max(rect.h);
            footer.push((hit, rect, label, enabled));
        }
        let footer_top = c.y + c.h - y - row_h;
        for (_, rect, _, _) in &mut footer {
            rect.y += footer_top;
        }
        let viewport = Rect::new(
            c.x,
            count.y + count.h + 8.0,
            c.w,
            (footer_top - count.y - count.h - 20.0).max(1.0),
        );
        let width = (viewport.w - theme::SCROLLBAR_GUTTER_W).max(1.0);
        let mut rows = Vec::new();
        let mut top = 0.0;
        for index in 0..self.account_profiles.len() {
            let source = self
                .chooser_copy(match self.account_profile_sources[index] {
                    ProfileSource::Local => K::ProfileChooserLocal,
                    ProfileSource::TrustedDevice => K::ProfileChooserSource,
                    ProfileSource::Unavailable => K::ProfileChooserWaiting,
                })
                .to_owned();
            let source_h = self
                .engine
                .simple_layout_wrapped(&source, theme::role::meta(), (width - 28.0).max(1.0))
                .height();
            let row = self
                .account_state
                .enrollments
                .iter()
                .find(|row| row.profile_id == self.account_profiles[index].profile_id);
            let mut status = row
                .map(|row| {
                    if row.state == koru_agent_wire::EnrollmentState::Complete {
                        self.chooser_copy(K::ProfileChooserComplete).to_owned()
                    } else if row.failure_code.is_some() {
                        let projected = crate::account_ui::state_for_profile(
                            &self.account_state,
                            Some(&row.profile_id),
                        );
                        crate::account_ui::project_for_lang(&projected, None, self.language)
                            .title
                            .to_owned()
                    } else {
                        self.chooser_copy(K::ProfileChooserConnecting).to_owned()
                    }
                })
                .unwrap_or_default();
            if let Some(at) = row
                .and_then(|row| row.retry_at_millis)
                .and_then(chrono::DateTime::from_timestamp_millis)
            {
                let time = at
                    .with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string();
                status.push('\n');
                status.push_str(&koru_i18n::fill(
                    self.chooser_copy(K::ProfileChooserRetryAt),
                    &[&time],
                ));
            }
            let status_h = if status.is_empty() {
                0.0
            } else {
                self.engine
                    .simple_layout_wrapped(&status, theme::role::meta(), (width - 28.0).max(1.0))
                    .height()
                    + 6.0
            };
            let row_y = viewport.y + top - self.existing_profile_scroll;
            let mut height = 36.0 + source_h + status_h + 12.0;
            let action = if self.account_profile_sources[index] == ProfileSource::Local {
                Some((Hit::OpenExisting(index), K::ProfileChooserOpen))
            } else if self.member_retry(index) {
                Some((Hit::RetryExisting(index), K::ProfileChooserRetry))
            } else {
                None
            };
            let action = action.map(|(hit, key)| {
                let label = self.chooser_copy(key);
                let rect = self.chooser_button(label, c.x + 14.0, row_y + height, width - 28.0);
                height += rect.h + 10.0;
                (hit, rect, label)
            });
            rows.push(RowLayout {
                rect: Rect::new(c.x, row_y, width, height),
                source,
                source_height: source_h,
                status,
                action,
            });
            top += height + 8.0;
        }
        ChooserLayout {
            viewport,
            rows,
            content_height: top,
            title,
            body,
            count,
            footer,
        }
    }

    pub(super) fn selection_hit(&mut self, x: f32, y: f32) -> Option<Hit> {
        let layout = self.selection_layout();
        let controls = self.selection_controls();
        for (hit, rect, _, enabled) in layout.footer {
            if enabled && rect.contains(x, y) {
                return Some(hit);
            }
        }
        if !layout.viewport.contains(x, y) {
            return None;
        }
        for (index, row) in layout.rows.iter().enumerate() {
            if let Some((hit, rect, _)) = row.action
                && rect.contains(x, y)
            {
                return Some(hit);
            }
            if row.rect.contains(x, y) && controls.contains(&Hit::Existing(index)) {
                return Some(Hit::Existing(index));
            }
        }
        None
    }

    fn draw_chooser_button(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        hit: Hit,
        rect: Rect,
        label: &str,
        enabled: bool,
        selected: bool,
    ) {
        let p = theme::p();
        scene::fill(
            canvas,
            root,
            if selected { p.row_selected } else { p.panel_bg },
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
        );
        scene::stroke(
            canvas,
            root,
            if enabled
                && (selected
                    || self.hover == Some(hit)
                    || self.controls().get(self.focus) == Some(&hit))
            {
                p.accent
            } else {
                p.panel_border
            },
            theme::BORDER_W,
            &scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0),
        );
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            label,
            rect.x + 12.0,
            rect.y + 7.0,
            rect.w - 24.0,
            theme::role::row(),
            "theme::role::row()",
            if enabled { p.text } else { p.text_dim },
        );
    }

    fn selection_count(&self) -> String {
        if self.selection.waiting {
            self.chooser_copy(koru_i18n::Key::ProfileChooserPending)
                .into()
        } else {
            koru_i18n::fill(
                self.chooser_copy(koru_i18n::Key::ProfileChooserCount),
                &[&self.selection_ids().len().to_string()],
            )
        }
    }

    pub(super) fn draw_selection(&mut self, canvas: &mut Scene, root: Affine) {
        use koru_i18n::Key as K;
        let p = theme::p();
        let layout = self.selection_layout();
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            koru_i18n::t(self.language, K::ProfileChooserTitle),
            layout.title.x,
            layout.title.y,
            layout.title.w,
            theme::role::subtitle(),
            "theme::role::subtitle()",
            p.text,
        );
        let body = self.selection_body();
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            &body,
            layout.body.x,
            layout.body.y,
            layout.body.w,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let count = self.selection_count();
        draw_wrapped_text(
            &mut self.engine,
            canvas,
            root,
            &count,
            layout.count.x,
            layout.count.y,
            layout.count.w,
            theme::role::meta(),
            "theme::role::meta()",
            p.text_dim,
        );
        let viewport = layout.viewport;
        canvas.push_clip_layer(
            Fill::NonZero,
            root,
            &vello::kurbo::Rect::new(
                viewport.x as f64,
                viewport.y as f64,
                (viewport.x + viewport.w) as f64,
                (viewport.y + viewport.h) as f64,
            ),
        );
        for (index, row) in layout.rows.iter().enumerate() {
            if row.rect.y + row.rect.h < viewport.y || row.rect.y > viewport.y + viewport.h {
                continue;
            }
            let profile = &self.account_profiles[index];
            let here = self.already_here(index);
            let checked = !here && self.selection.checked.contains(&profile.profile_id);
            let _row = inspect::scope(
                "ProfileRow",
                "container",
                root,
                scene::rect(row.rect.x, row.rect.y, row.rect.w, row.rect.h),
                |record| {
                    record.property("checked", checked, "view state");
                    record.property("source", &row.source, "account provider");
                },
            );
            scene::fill(
                canvas,
                root,
                if checked { p.row_selected } else { p.panel_bg },
                &scene::rrect(row.rect.x, row.rect.y, row.rect.w, row.rect.h, 8.0),
            );
            scene::stroke(
                canvas,
                root,
                if self.controls().get(self.focus) == Some(&Hit::Existing(index)) {
                    p.accent
                } else {
                    p.panel_border
                },
                theme::BORDER_W,
                &scene::rrect(row.rect.x, row.rect.y, row.rect.w, row.rect.h, 8.0),
            );
            // No checkbox on a Profile that is already here: there is no
            // choice to make about it. Its name keeps the same inset, so the
            // list still reads as one column.
            let check = Rect::new(row.rect.x + 12.0, row.rect.y + 11.0, 16.0, 16.0);
            if !here {
                scene::stroke(
                    canvas,
                    root,
                    p.accent,
                    theme::BORDER_W,
                    &scene::rrect(check.x, check.y, check.w, check.h, 3.0),
                );
            }
            if checked {
                scene::fill(
                    canvas,
                    root,
                    p.accent,
                    &scene::rrect(check.x + 4.0, check.y + 4.0, 8.0, 8.0, 2.0),
                );
            }
            canvas.push_clip_layer(
                Fill::NonZero,
                root,
                &vello::kurbo::Rect::new(
                    (row.rect.x + 36.0) as f64,
                    row.rect.y as f64,
                    (row.rect.x + row.rect.w - 12.0) as f64,
                    (row.rect.y + 34.0) as f64,
                ),
            );
            let name = profile
                .name
                .clone()
                .unwrap_or_else(|| self.chooser_copy(K::ProfileChooserUnnamed).into());
            draw_text(
                &mut self.engine,
                canvas,
                root,
                &name,
                row.rect.x + 36.0,
                row.rect.y + 8.0,
                theme::role::section_heading(),
                "theme::role::section_heading()",
                p.text,
            );
            canvas.pop_layer();
            draw_wrapped_text(
                &mut self.engine,
                canvas,
                root,
                &row.source,
                row.rect.x + 14.0,
                row.rect.y + 36.0,
                row.rect.w - 28.0,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
            if !row.status.is_empty() {
                draw_wrapped_text(
                    &mut self.engine,
                    canvas,
                    root,
                    &row.status,
                    row.rect.x + 14.0,
                    row.rect.y + 42.0 + row.source_height,
                    row.rect.w - 28.0,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text,
                );
            }
            if let Some((hit, rect, label)) = row.action {
                self.draw_chooser_button(canvas, root, hit, rect, label, true, false);
            }
        }
        canvas.pop_layer();
        if let Some(bar) = ScrollbarGeom::for_content(
            viewport,
            layout.content_height,
            self.existing_profile_scroll,
        ) {
            scrollbar::draw(
                canvas,
                root,
                bar,
                bar.on_thumb(self.pointer.0, self.pointer.1)
                    || self.existing_profile_scroll_drag.is_some(),
            );
        }
        for (hit, rect, label, enabled) in layout.footer {
            self.draw_chooser_button(canvas, root, hit, rect, label, enabled, hit == Hit::Primary);
        }
    }
}

#[cfg(test)]
mod tests;
