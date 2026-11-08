// SPDX-License-Identifier: MPL-2.0

//! The Agents navigator's schedule pass.
//!
//! `ScheduleIndex` does the only I/O on its own worker. This pass supplies the
//! window facts it cannot know (workspace, language, device names, and the
//! already-loaded run index) and hands immutable rows to Explore.

use super::MainView;

impl MainView {
    pub(super) fn schedule_pill_presentations(
        &self,
    ) -> std::collections::HashMap<String, (String, bool)> {
        self.schedule_index
            .rows()
            .iter()
            .map(|row| {
                (
                    koru_editor::pillinfo::schedule_entity_key(&row.schedule.to_storage()),
                    (row.name.clone(), row.paused),
                )
            })
            .collect()
    }

    fn push_schedule_pill_presentations(&mut self) -> bool {
        let schedules = self.schedule_pill_presentations();
        let mut changed = false;
        for tab in self.tabs_mut().iter_mut() {
            if let super::TabView::Leaf(leaf) = &mut tab.view {
                changed |= leaf.set_schedule_pills(&schedules);
            }
        }
        changed
    }

    pub(super) fn maintain_schedules(&mut self, runs_changed: bool) -> bool {
        // Schedule presentation needs the owner's identity even when no run
        // pill or Link Device dialog has ever opened. The shared bootstrap is
        // one-shot per live Agent generation and retries on reconnect.
        self.request_device_names_if_needed();
        let workspace = self
            .current_workspace()
            .map(|id| koru_delegate_core::WorkspaceId::from_uuid(id.as_uuid()));
        let mut changed = self
            .schedule_index
            .set_source(self.delegate_client.clone(), workspace);
        changed |= self.schedule_index.poll();
        while let Some(answer) = self.schedule_index.take_rename_result() {
            let editing_same = self.workspace_rename.as_ref().is_some_and(|field| {
                matches!(
                    field.target,
                    super::workspace_rename::Target::Schedule(schedule)
                        if schedule == answer.schedule
                )
            });
            if editing_same {
                match answer.result {
                    Ok(_) => self.workspace_rename = None,
                    Err(_) => {
                        let current = self
                            .schedule_index
                            .subject(answer.schedule)
                            .map(|subject| subject.name);
                        if let Some(field) = self.workspace_rename.as_mut() {
                            field.set_sending(false);
                            if let Some(current) = current {
                                field.refresh_schedule_expected(current);
                            }
                            field.set_hint(koru_i18n::t(
                                crate::format::lang(),
                                koru_i18n::Key::ScheduleRenameFailed,
                            ));
                        }
                    }
                }
            }
            changed = true;
        }
        while let Some(answer) = self.schedule_index.take_lifecycle_result() {
            if answer.result.is_err() {
                self.toast = Some((
                    koru_ui::Toast::new(
                        koru_i18n::t(
                            crate::format::lang(),
                            koru_i18n::Key::ScheduleLifecycleFailed,
                        )
                        .to_owned(),
                        None,
                        std::time::Instant::now(),
                    ),
                    super::ToastPayload::None,
                ));
            }
            changed = true;
        }
        while let Some(answer) = self.schedule_index.take_retire_result() {
            if answer.result.is_err() {
                self.toast = Some((
                    koru_ui::Toast::new(
                        koru_i18n::t(crate::format::lang(), koru_i18n::Key::ScheduleDeleteFailed)
                            .to_owned(),
                        None,
                        std::time::Instant::now(),
                    ),
                    super::ToastPayload::None,
                ));
            }
            changed = true;
        }
        if self.schedule_index.take_runs_refresh() {
            self.run_index.refresh();
        }
        let refresh_runs = runs_changed || self.schedule_index.needs_presentation();
        changed |= self.schedule_index.set_presentation(
            crate::format::lang(),
            &self.reminder_environment.resolution.current_zone,
            self.this_device,
            &self.device_names,
            refresh_runs.then_some(self.run_index.rows()),
        );
        if changed {
            changed |= self.explore.set_agent_schedules(
                self.schedule_index.rows().to_vec(),
                self.schedule_index.state(),
            );
            changed |= self.push_schedule_pill_presentations();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use koru_agent_wire::{FromShell, LinkedDeviceView, Request};
    use koru_delegate_api::wire::{ListSchedulesResponse, ScheduleNextFiring, ScheduleView};
    use koru_delegate_core::{
        ActorRef, DeviceId, DocumentId, Objective, ProfileId, ScheduleId, SessionProfileKey,
        WorkspaceId, WorkspaceScope,
    };
    use koru_delegate_schedule::{
        DelegationTemplate, RecurrenceRule, ScheduleDefinition, ScheduleHeader, ScheduleLifecycle,
        ScheduleState,
    };
    use koru_workspace_host::View;

    use super::*;

    fn presented(schedule: ScheduleId) -> crate::schedule_index::ScheduleRow {
        crate::schedule_index::ScheduleRow {
            schedule,
            name: "Daily review".into(),
            paused: false,
            rule: "Every day at 9:00 AM".into(),
            next: "Next: Sep 14, 9:00 AM".into(),
            owner: "Owner: this device".into(),
            state: "Active".into(),
            runs: Vec::new(),
            created_at_ms: 1,
        }
    }

    fn linked(id: DeviceId, name: &str) -> LinkedDeviceView {
        LinkedDeviceView {
            device_id: id.to_storage(),
            display_name: name.into(),
            linked_at_millis: 0,
            connected: true,
            last_seen_millis: None,
            introduced_by: None,
            build_label: None,
            build_drift: None,
            build_severity: None,
            update_needed_on: None,
            connection: None,
            last_sync_millis: None,
            history: Vec::new(),
        }
    }

    fn local_floating(owner: DeviceId) -> ScheduleView {
        let workspace = WorkspaceId::new();
        ScheduleView {
            workspace,
            document: DocumentId::new(),
            name: Some(koru_delegate_schedule::ScheduleName::new("Daily review").unwrap()),
            header: ScheduleHeader {
                schedule: ScheduleId::mint(),
                created_at_ms: 1,
                initial_owner: owner,
                definition: ScheduleDefinition {
                    revision: 1,
                    effective_at_ms: 0,
                    template: DelegationTemplate {
                        requested_by: ActorRef::Person {
                            profile: ProfileId::new(),
                        },
                        objective: Objective::new("fixture").unwrap(),
                        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                        requested_model: None,
                        scope: WorkspaceScope::Temporary,
                        grants: BTreeSet::from([workspace]),
                        context: Vec::new(),
                    },
                    recurrence: RecurrenceRule::floating("0 9 * * MON-FRI".parse().unwrap()),
                    lifecycle: ScheduleLifecycle::Active,
                },
            },
            state: ScheduleState::Active { owner },
            next_firing: Some(ScheduleNextFiring::Exact {
                at_ms: 1_767_268_800_000,
            }),
            firings: Vec::new(),
        }
    }

    #[test]
    fn startup_device_identity_makes_local_floating_next_exact() {
        let (agent, requests) = crate::agent::AgentLink::for_test();
        let mut view = MainView::new_with_agent(
            crate::agent::UpdateFeed::absent(),
            agent.clone(),
            true,
            koru_workspaces_ui::WorkspaceLinks::inert(),
        );
        let owner = DeviceId::new();
        view.schedule_index.take_listing(ListSchedulesResponse {
            schedules: vec![local_floating(owner)],
            incomplete: false,
        });

        view.maintain_schedules(false);
        assert!(view.device_names_requested_generation.is_none());
        assert!(requests.try_recv().is_err());
        assert!(view.schedule_index.rows()[0].next.contains("owning device"));
        agent.connect_for_test();
        view.maintain_schedules(false);
        assert!(matches!(
            requests
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            FromShell::Request {
                body: Request::LinkedDevices,
                ..
            }
        ));
        agent.disconnect_for_test();
        view.maintain_schedules(false);
        assert!(requests.try_recv().is_err());
        agent.connect_for_test();
        view.maintain_schedules(false);
        assert!(matches!(
            requests
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            FromShell::Request {
                body: Request::LinkedDevices,
                ..
            }
        ));
        view.maintain_schedules(false);
        assert!(
            requests.try_recv().is_err(),
            "each live generation receives one bootstrap request"
        );

        agent.answer_linked_devices_for_test(Some(linked(owner, "Studio")), Vec::new());
        view.refresh_device_names();
        view.maintain_schedules(false);

        let row = &view.schedule_index.rows()[0];
        assert_eq!(row.owner, "Owner: Studio (this device)");
        assert_eq!(
            view.device_names_acknowledged_generation,
            agent.connected_generation()
        );
        assert!(requests.try_recv().is_err(), "the answer settles bootstrap");
        assert!(row.next.starts_with("Next:"));
        assert!(!row.next.contains("owning device’s local time"));
        assert!(!row.rule.contains("owning device’s local time"));
    }

    #[test]
    fn schedule_retirement_confirmation_defaults_to_cancel() {
        let (agent, _) = crate::agent::AgentLink::for_test();
        let mut view = MainView::new_with_agent(
            crate::agent::UpdateFeed::absent(),
            agent,
            true,
            koru_workspaces_ui::WorkspaceLinks::inert(),
        );
        let owner = DeviceId::new();
        let schedule = local_floating(owner);
        let schedule_id = schedule.header.schedule;
        view.this_device = Some(owner);
        view.schedule_index.take_listing(ListSchedulesResponse {
            schedules: vec![schedule],
            incomplete: false,
        });

        assert!(view.confirm_delete_schedules(vec![schedule_id]));
        let (confirm, subject) = view.confirm.as_ref().unwrap();
        assert_eq!(
            confirm.default_action(),
            Some(koru_ui::confirm::Outcome::Cancel)
        );
        assert_eq!(
            subject,
            &crate::mainview::trash::ConfirmSubject::RetireSchedules(vec![schedule_id])
        );
        assert!(view.settle_confirm(false));
        assert!(view.confirm.is_none());
        assert!(view.schedule_index.subject(schedule_id).is_some());
    }

    #[test]
    fn following_a_pill_keeps_the_schedule_selected_after_document_refresh() {
        let (mut view, workspace) = crate::mainview::harness::on_document();
        let schedule = ScheduleId::mint();
        view.explore.set_agent_schedules(
            vec![presented(schedule)],
            crate::schedule_index::CatalogState::Ready,
        );

        assert!(view.reveal_schedule(schedule));
        let node = view.explore.agent_schedule_node(schedule).unwrap();
        assert!(view.sidebar.is_open());
        assert_eq!(
            view.explore.navigator_mode(),
            crate::panes::explore::NavigatorMode::Agents
        );
        assert_eq!(view.explore.selection_nodes(), vec![node]);

        view.refresh_explore(&workspace.ws, true);
        assert_eq!(
            view.explore.selection_nodes(),
            vec![node],
            "the open document must not erase the schedule the pill picked"
        );
    }

    #[test]
    fn a_pill_invoked_schedule_rename_draws_on_the_pill_without_agents_visible() {
        use koru_core::node::{Attr, AttrValue};

        let (mut view, mut workspace) = crate::mainview::harness::on_document();
        let owner = DeviceId::new();
        let schedule_view = local_floating(owner);
        let schedule = schedule_view.header.schedule;
        view.this_device = Some(owner);
        view.schedule_index.take_listing(ListSchedulesResponse {
            schedules: vec![schedule_view],
            incomplete: false,
        });
        let root = view.document_root().unwrap();
        let at = workspace
            .ws
            .read(root)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .len();
        workspace
            .ws
            .insert_pill(
                root,
                at,
                &Attr::labelled(
                    koru_core::attributes::ROLE_SCHEDULE,
                    AttrValue::Text(schedule.to_storage()),
                    "Daily review",
                ),
            )
            .unwrap();
        view.adopt(&workspace.ws);
        assert_ne!(
            view.explore.navigator_mode(),
            crate::panes::explore::NavigatorMode::Agents
        );

        assert!(view.start_schedule_rename(schedule));
        let row = view
            .workspace_rename_row()
            .expect("the inline field uses the visible pill as its row");
        let leaf = view.focused_view_rect();
        assert!(
            row.x >= leaf.x
                && row.y >= leaf.y
                && row.x + row.w <= leaf.x + leaf.w
                && row.y + row.h <= leaf.y + leaf.h
        );
    }
}
