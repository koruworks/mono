// SPDX-License-Identifier: MPL-2.0

//! **One entry, four gestures, one modal** (ROADMAP P1.14, D7, D9, D18, D28).
//!
//! Four things a person can do reach delegation — the `+` menu, `/delegate`
//! in the editor's slash menu, a task's right-click menu, and a document
//! selection's right-click menu — and all four go through [`begin`]. Nothing
//! else opens the surface, which is what makes "one code path" a property the
//! tests can assert rather than a convention four call sites happen to follow:
//! the origins differ only in the pre-fill they arrive with, and
//! [`modal::Delegation::open`] is the single constructor they all reach.
//!
//! ## Why this lives in the container and not in a `koru-delegate-ui` crate
//!
//! The `koru-*-ui` crates exist for surfaces **three apps host** — Settings,
//! the workspace manager, search — and every one of them is chartered to
//! *name no document, no workspace and no session*, with a dependency test in
//! both directions holding it. This surface fails both halves: it names a
//! workspace (D28's grant), documents (the context it pins) and session
//! projects (the working scope), and in M1 it is Koru's alone — the phone has
//! no delegation entry (D15) and neither Koru Terminal nor Koru AI delegates.
//! So it sits beside `reminder_creation` and `link_device`, which is where
//! every other modal this container owns lives. ADR-350 records the choice and
//! what would change it.
//!
//! ## The four files
//!
//! - [`world`] — the values the modal is built from, gathered on the window
//!   thread from what this window already holds. **No disk, no store, no
//!   network**, which is what makes opening the surface free.
//! - [`modal`] — the state machine: the four fields, the grant, the focus
//!   ring, and what each control does. Pure.
//! - [`paint`] — one geometry, read by the drawing pass and the hit test.
//! - [`job`] — the only thing that reaches the world: pinning the context and
//!   creating the run, on a worker thread.
//! - [`pills`] — what the crumb the gesture leaves behind says afterwards, and
//!   the worker that reads the Run document to find out.

pub(super) mod job;
pub(super) mod modal;
mod paint;
pub(super) mod pills;
pub(super) mod world;

#[cfg(test)]
mod tests;

use koru_core::{DocumentId, NodeId};
use koru_delegate_context::ContextSource;
use koru_delegate_core::{BlockNodeId, RunId};

use super::MainView;
use super::context_menu::{self, ContextMenu, MenuSubject};

pub(crate) use modal::{Delegation, Hit, Outcome, Phase};

/// **Where a delegation came from** (D7). Four gestures, one modal.
///
/// The variants carry only what the gesture knew. Everything else — the
/// agents, the projects, the workspace and its name — is the window's, and is
/// resolved once by [`begin`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum DelegationOrigin {
    /// The tab strip's `+`. It knows nothing, so the modal opens empty.
    PlusMenu,
    /// `/delegate` in the editor. The row it was typed on becomes the context
    /// **and what is left of its words becomes the objective** — the command
    /// itself is erased before the modal opens, so what remains is what the
    /// person wrote around it. Typed alone on a blank line it carries
    /// nothing, which is the empty box this gesture always opened.
    SlashCommand {
        document: DocumentId,
        block: Option<NodeId>,
        objective: String,
    },
    /// A task's right-click menu. Its title becomes the objective's first
    /// line, and the task's own row becomes the context.
    TaskContextMenu { task: TaskSubject },
    /// A document selection's right-click menu. The selected row becomes the
    /// context **and its words become the objective**: somebody who
    /// right-clicks a line and chooses *Delegate* has written what they want
    /// done, and an empty box asks them to write it twice.
    SelectionContextMenu {
        document: DocumentId,
        selection: Option<NodeId>,
        objective: String,
    },
    /// **A finished run's *Ask again*** (ADR-366). The fifth gesture, and the
    /// only one that arrives with the whole request already filled in: the
    /// objective, the agent, the working folder, what the run was given and
    /// what it could reach.
    ///
    /// It names no document and no row, so it leaves no crumb -- the `+`
    /// menu's answer, and for the same reason: the gesture happened in a run's
    /// own view, and writing a mark into whatever document was open would put
    /// it somewhere nobody pointed at.
    ///
    /// **The old run is not in this beyond being read.** Pressing *Delegate*
    /// makes a new run; nothing is sent to the old one, and cancelling here
    /// leaves both untouched.
    ReRun { seed: Box<koru_runs_ui::ReRun> },
}

/// Which task a delegation came from, and what it is called.
///
/// The title travels because the objective's first line is the one thing this
/// gesture knows that the others do not, and resolving it later would mean
/// reading a document from a surface that must not.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TaskSubject {
    pub(crate) document: DocumentId,
    pub(crate) node: NodeId,
    pub(crate) title: String,
}

impl DelegationOrigin {
    /// The objective's first line, when the gesture knew one.
    fn objective(&self) -> String {
        match self {
            Self::TaskContextMenu { task } => task.title.clone(),
            Self::SelectionContextMenu { objective, .. } | Self::SlashCommand { objective, .. } => {
                objective.clone()
            }
            Self::ReRun { seed } => seed.objective.as_str().to_owned(),
            Self::PlusMenu => String::new(),
        }
    }

    /// **Where this gesture's pill lands** (D7, D8).
    ///
    /// The row the gesture happened on, in the document it happened in — the
    /// crumb follows the origin. Four answers, decided once here rather than
    /// four times at four call sites:
    ///
    /// - `/delegate`: the row it was typed on. The typed text is erased before
    ///   the modal opens, so the pill *follows* what is left rather than
    ///   replacing anything somebody wrote.
    /// - A selection's *Delegate…*: the selected row, the same way.
    /// - A task's *Delegate…*: **the task's own row**. A task is a line a
    ///   person keeps looking at, so the record of having handed it to an
    ///   agent belongs on that line and nowhere else — reading the row tells
    ///   you what was delegated and how it is going, which is the whole point
    ///   of a crumb.
    /// - The `+` menu: **nowhere**. It names no document, and writing a
    ///   durable mark into whatever happened to be on screen would put it in a
    ///   document the person never pointed at. The run is reached from the
    ///   Runs surface instead. ADR-357 records this and what would change it.
    ///
    /// Deliberately **not** [`subject`](Self::subject): what a run is *given*
    /// and where its crumb *goes* are different questions. A `/delegate` typed
    /// on a document's title row pins the whole document as context and still
    /// leaves its pill on that row.
    fn pill_anchor(&self) -> Option<(DocumentId, NodeId)> {
        match self {
            // Neither names a row a crumb could go on.
            Self::PlusMenu | Self::ReRun { .. } => None,
            Self::SlashCommand {
                document, block, ..
            } => block.map(|node| (*document, node)),
            Self::TaskContextMenu { task } => Some((task.document, task.node)),
            Self::SelectionContextMenu {
                document,
                selection,
                ..
            } => selection.map(|node| (*document, node)),
        }
    }

    /// The document and block this gesture would attach, if any.
    fn subject(&self) -> Option<(DocumentId, Option<NodeId>)> {
        match self {
            // A re-run's context is the earlier run's pins, not a row this
            // window is looking at -- see `MainView::rerun_context`.
            Self::PlusMenu | Self::ReRun { .. } => None,
            Self::SlashCommand {
                document, block, ..
            } => Some((*document, *block)),
            Self::TaskContextMenu { task } => Some((task.document, Some(task.node))),
            Self::SelectionContextMenu {
                document,
                selection,
                ..
            } => Some((*document, *selection)),
        }
    }
}

impl MainView {
    /// **The single path into delegation.** Every gesture calls this.
    ///
    /// Returns whether the surface opened, so a menu row and a drained view
    /// request both answer "did anything change" the way the rest of the
    /// container does.
    ///
    /// **Nothing here touches disk, the store or the network.** The agents
    /// come from the compiled-in launch catalog and the host's last accepted
    /// readiness snapshot; the projects come from the registry the session
    /// backend already pushed; the workspace and its name come from the pane
    /// context. First use subscribes through the backend worker, and explicit
    /// Retry requests a new probe without waiting in this call.
    pub(crate) fn begin_delegation(&mut self, origin: DelegationOrigin) -> bool {
        // **The one door into the modal**, so a build that hides Koru AI
        // (Delegate included) refuses every gesture here once — the `+` row,
        // `/delegate`, a task's and a selection's menu (koru-features).
        if !koru_features::ai() {
            return false;
        }
        // One outstanding receiver must stay owned until its answer is
        // drained. Replacing the hidden surface is harmless; replacing its
        // `Job` would lose a durable run or schedule's only completion path,
        // including the run pill and index refresh owed after success.
        if self.delegate_job.is_some() {
            return self.show_delegation_in_flight();
        }
        self.ensure_provider_readiness();
        let lang = crate::format::lang();
        let Some((_, profile, workspaces, Some(workspace))) = self.pane_context.clone() else {
            // No workspace, nothing to delegate in or grant. Silent rather
            // than a modal that could only say so.
            return false;
        };
        let workspace_name = workspaces
            .iter()
            .find(|entry| entry.id == workspace)
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        let store_workspace = koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid());

        // An agent turned off on *Agent providers* is not a choice here.
        let disabled = self.disabled_agents().to_vec();
        let profiles: Vec<_> = koru_session_profiles::built_in_profiles()
            .into_iter()
            .filter(|profile| {
                profile
                    .label
                    .agent_key()
                    .is_none_or(|key| !disabled.iter().any(|off| off == key))
            })
            .collect();
        let agents = world::agents_for(
            &profiles,
            koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
            self.provider_readiness.as_deref(),
            koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell),
            koru_i18n::t(lang, koru_i18n::Key::TerminalCustomProfile),
        );
        let temporary_base = self.delegate_temporary_base();
        let scopes = world::scopes_for(
            self.explore.agent_projects(),
            koru_i18n::t(lang, koru_i18n::Key::DelegateScopeTemporary),
            &temporary_base,
        );
        let context = match &origin {
            DelegationOrigin::ReRun { seed } => self.rerun_context(seed),
            _ => self.prefilled_context(store_workspace, &origin),
        };
        let (chosen_agent, chosen_scope, grant, dropped_context) = match &origin {
            DelegationOrigin::ReRun { seed } => (
                Some(seed.agent.clone()),
                Some(seed.scope.clone()),
                seed.grants.contains(&store_workspace),
                seed.dropped,
            ),
            // Every other gesture takes the surface's own defaults: the first
            // installed agent, the first folder that can be worked in, and
            // D28's grant on.
            _ => (None, None, true, false),
        };

        let world = world::World {
            workspace: store_workspace,
            workspace_name,
            profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            agents,
            scopes,
            context,
            attachable: self.open_document_context(store_workspace),
            objective: origin.objective(),
            pill_anchor: origin
                .pill_anchor()
                .map(|(document, node)| world::PillAnchor { document, node }),
            temporary_base,
            base_problem: None,
            dropped_context,
            chosen_model: match &origin {
                DelegationOrigin::ReRun { seed } => seed.requested_model.clone(),
                _ => None,
            },
            chosen_agent,
            chosen_scope,
            grant,
        };
        // **The origin is not carried into the surface.** Everything it knew
        // is already in `world`; a modal that also remembered which gesture
        // made it would be a place for the four to start differing.
        self.delegation = Some(Delegation::open_with_environment(
            &mut self.engine,
            world,
            self.reminder_environment.locale.clone(),
            self.reminder_environment.resolution.current_zone.as_str(),
        ));
        self.discover_delegate_models();
        true
    }

    /// Open the ordinary delegation form over one canonical schedule
    /// snapshot. The form receives only values already held by the schedule
    /// index; opening it performs no Store or network work.
    pub(crate) fn begin_schedule_edit(
        &mut self,
        subject: crate::schedule_index::ScheduleSubject,
    ) -> bool {
        if !koru_features::ai() {
            return false;
        }
        if self.delegate_job.is_some() {
            return self.show_delegation_in_flight();
        }
        if Some(subject.owner) != self.this_device
            || matches!(
                subject.state,
                koru_delegate_schedule::ScheduleState::HandoffPending { .. }
                    | koru_delegate_schedule::ScheduleState::Retired
            )
        {
            return false;
        }
        self.ensure_provider_readiness();
        let lang = crate::format::lang();
        let Some((_, profile, workspaces, Some(workspace))) = self.pane_context.clone() else {
            return false;
        };
        let workspace_id = koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid());
        if workspace_id != subject.workspace {
            return false;
        }
        let template = &subject.definition.template;
        let temporary_base = self.delegate_temporary_base();
        let mut scopes = world::scopes_for(
            self.explore.agent_projects(),
            koru_i18n::t(lang, koru_i18n::Key::DelegateScopeTemporary),
            &temporary_base,
        );
        if !scopes.iter().any(|row| row.scope == template.scope) {
            scopes.push(world::ScopeRow {
                scope: template.scope.clone(),
                name: koru_i18n::t(lang, koru_i18n::Key::DelegateScheduleCurrentScope).to_owned(),
                path: None,
                disabled: None,
            });
        }
        let context = template
            .context
            .iter()
            .map(|pointer| self.schedule_context_row(pointer))
            .collect();
        let world = world::World {
            workspace: workspace_id,
            workspace_name: workspaces
                .iter()
                .find(|entry| entry.id == workspace)
                .map(|entry| entry.name.clone())
                .unwrap_or_default(),
            profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            agents: world::agents_for(
                &koru_session_profiles::built_in_profiles(),
                koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
                self.provider_readiness.as_deref(),
                koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell),
                koru_i18n::t(lang, koru_i18n::Key::TerminalCustomProfile),
            ),
            scopes,
            context,
            attachable: self.open_document_context(workspace_id),
            objective: template.objective.as_str().to_owned(),
            pill_anchor: None,
            temporary_base,
            base_problem: None,
            dropped_context: false,
            chosen_model: template.requested_model.clone(),
            chosen_agent: Some(template.agent.clone()),
            chosen_scope: Some(template.scope.clone()),
            grant: template.grants.contains(&workspace_id),
        };
        let seed = modal::EditSeed {
            schedule: subject.schedule,
            expected_revision: subject.definition.revision,
            expected_name: subject.name.clone(),
            requested_by: template.requested_by,
            grants: template.grants.clone(),
        };
        let modal = Delegation::open_edit_with_environment(
            &mut self.engine,
            world,
            self.reminder_environment.locale.clone(),
            self.reminder_environment.resolution.current_zone.as_str(),
            seed,
            &subject.name,
            &subject.definition.recurrence,
        );
        self.delegation = Some(modal);
        self.discover_delegate_models();
        true
    }

    fn schedule_context_row(
        &self,
        pointer: &koru_delegate_schedule::ScheduleContextPointer,
    ) -> world::ContextRow {
        let lang = crate::format::lang();
        match pointer {
            koru_delegate_schedule::ScheduleContextPointer::Document {
                workspace,
                document,
            } => world::ContextRow {
                source: ContextSource::Document {
                    workspace: *workspace,
                    document: *document,
                },
                label: self.pinned_label(*document, koru_i18n::Key::RunContextDocument),
                block: false,
            },
            koru_delegate_schedule::ScheduleContextPointer::Block {
                workspace,
                document,
                node,
            } => world::ContextRow {
                source: ContextSource::Block {
                    workspace: *workspace,
                    document: *document,
                    node: *node,
                },
                label: self.pinned_label(*document, koru_i18n::Key::RunContextBlock),
                block: true,
            },
            koru_delegate_schedule::ScheduleContextPointer::Blob {
                workspace,
                blob,
                blob_kind,
            } => world::ContextRow {
                source: ContextSource::Blob {
                    workspace: *workspace,
                    blob: blob.clone(),
                    blob_kind: *blob_kind,
                },
                label: koru_i18n::t(
                    lang,
                    match blob_kind {
                        koru_delegate_core::BlobKind::File => koru_i18n::Key::RunContextFile,
                        koru_delegate_core::BlobKind::Image => koru_i18n::Key::RunContextImage,
                        koru_delegate_core::BlobKind::Pdf => koru_i18n::Key::RunContextPdf,
                    },
                )
                .to_owned(),
                block: false,
            },
        }
    }

    /// Refresh an open draft from the accepted host snapshot, without starting work.
    pub(super) fn update_delegate_readiness(&mut self) -> bool {
        let Some(modal) = self
            .delegation
            .as_mut()
            .filter(|modal| matches!(modal.phase, Phase::Editing { .. }))
        else {
            return false;
        };
        let lang = crate::format::lang();
        let agents = world::agents_for(
            &koru_session_profiles::built_in_profiles(),
            koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS,
            self.provider_readiness.as_deref(),
            koru_i18n::t(lang, koru_i18n::Key::TerminalPlatformShell),
            koru_i18n::t(lang, koru_i18n::Key::TerminalCustomProfile),
        );
        if modal.world.agents == agents {
            return false;
        }
        modal.replace_agents(agents);
        paint::reveal_focus(&mut self.engine, modal, self.viewport);
        self.discover_delegate_models();
        true
    }

    /// **Where Koru would make a folder for a run that belongs to no
    /// project** (ADR-365), as this window last heard it.
    ///
    /// The chosen folder comes from the settings link, which holds whatever
    /// the authority last confirmed and the registry's default before that.
    /// Turning it into a path is `koru_settings`', spelled once there so the
    /// window and the worker that makes the folder cannot disagree — and it
    /// reads the environment and no disk, which is what lets this be called
    /// while a person is waiting for a surface to open.
    fn delegate_temporary_base(&self) -> String {
        koru_settings::delegate_temporary_base(
            self.delegate_folder
                .authoritative()
                .delegate_temporary_root
                .as_deref(),
        )
        .to_string_lossy()
        .into_owned()
    }

    /// Take whatever the settings worker has, and tell an open surface.
    ///
    /// The section changes for reasons that have nothing to do with this — a
    /// theme, a shortcut — so the row is only rebuilt when the folder it
    /// shows actually moved.
    pub(crate) fn maintain_delegate_folder(&mut self) -> bool {
        let before = self.delegate_temporary_base();
        let agents_before = self.disabled_agents().to_vec();
        if self.delegate_folder.poll().is_none() {
            return false;
        }
        // The same link carries the agents turned off (Agent providers): a
        // list another window changed, or the stored one arriving at start,
        // reaches every surface here.
        let agents_moved = self.disabled_agents() != agents_before.as_slice();
        if agents_moved {
            self.apply_hidden_agents();
            self.sync_agent_providers();
        }
        let after = self.delegate_temporary_base();
        if after == before {
            return agents_moved;
        }
        if let Some(modal) = self.delegation.as_mut() {
            modal.chose_temporary_base(Ok(after));
        }
        self.discover_delegate_models();
        true
    }

    /// **A folder was chosen** (ADR-365): remember it, and show it.
    ///
    /// The write goes down the settings link's own worker — nothing here
    /// touches the file — and the surface takes the choice as its answer at
    /// once, because a person is looking at the row they just changed and one
    /// that waited for a round trip would read as a press that did nothing.
    /// The authority's reply then replaces it, so a refusal reconciles.
    pub(crate) fn delegate_folder_chosen(
        &mut self,
        chosen: koru_workspace_host::DelegateFolder,
    ) -> bool {
        let answer = match chosen {
            koru_workspace_host::DelegateFolder::Chosen(folder) => {
                let root = folder.to_string_lossy().into_owned();
                self.delegate_folder.save(
                    koru_settings::GlobalPatch {
                        delegate_temporary_root: koru_settings::Field::Set(root.clone()),
                        ..koru_settings::GlobalPatch::default()
                    },
                    |patch, section| patch.clone().apply(section),
                );
                Ok(root)
            }
            // Nothing is stored and nothing is sent: the row keeps the folder
            // it had, and says why the new one was not taken.
            koru_workspace_host::DelegateFolder::NotWritable => {
                Err(koru_i18n::Key::DelegateScopeBaseRefused)
            }
        };
        match self.delegation.as_mut() {
            Some(modal) => {
                modal.chose_temporary_base(answer);
                self.discover_delegate_models();
                true
            }
            // The surface went while the dialog was up. The setting is still
            // the person's choice and is still remembered; there is simply
            // nothing on screen to redraw.
            None => false,
        }
    }

    /// **What a finished run was given, as rows to hand a new one**
    /// (ADR-366).
    ///
    /// The pins the surface handed over are already filtered: anything whose
    /// source this device can see is in the trash or gone was left out there,
    /// where the state of each pin is known (D48). What is left is turned from
    /// a *pin* -- a document at one version -- back into a *source*, because a
    /// new run pins the material as it stands now rather than inheriting a
    /// version somebody has since moved past. Asking for the same work again
    /// on the same page means the page as it is.
    ///
    /// The label is what this window calls the document, when it happens to
    /// have it open, and otherwise the same words the run's own view uses for
    /// it. A pin carries ids and no title, deliberately, and this surface may
    /// not read the store to find one.
    fn rerun_context(&self, seed: &koru_runs_ui::ReRun) -> Vec<world::ContextRow> {
        let lang = crate::format::lang();
        seed.context
            .iter()
            .map(|item| match item {
                koru_delegate_core::RunContextItem::Document {
                    workspace,
                    document,
                    ..
                } => world::ContextRow {
                    source: ContextSource::Document {
                        workspace: *workspace,
                        document: *document,
                    },
                    label: self.pinned_label(*document, koru_i18n::Key::RunContextDocument),
                    block: false,
                },
                koru_delegate_core::RunContextItem::Block {
                    workspace,
                    document,
                    node,
                    ..
                } => world::ContextRow {
                    source: ContextSource::Block {
                        workspace: *workspace,
                        document: *document,
                        node: *node,
                    },
                    label: self.pinned_label(*document, koru_i18n::Key::RunContextBlock),
                    block: true,
                },
                koru_delegate_core::RunContextItem::Blob {
                    workspace,
                    blob,
                    blob_kind,
                } => world::ContextRow {
                    source: ContextSource::Blob {
                        workspace: *workspace,
                        blob: blob.clone(),
                        blob_kind: *blob_kind,
                    },
                    label: koru_i18n::t(
                        lang,
                        match blob_kind {
                            koru_delegate_core::BlobKind::File => koru_i18n::Key::RunContextFile,
                            koru_delegate_core::BlobKind::Image => koru_i18n::Key::RunContextImage,
                            koru_delegate_core::BlobKind::Pdf => koru_i18n::Key::RunContextPdf,
                        },
                    )
                    .to_owned(),
                    block: false,
                },
            })
            .collect()
    }

    /// What to call a pinned document: the tab showing it, or the word for
    /// what kind of thing it is. Never empty, because a row with no words is a
    /// row that reads as nothing attached.
    fn pinned_label(
        &self,
        document: koru_delegate_core::DocumentId,
        fallback: koru_i18n::Key,
    ) -> String {
        let label = self.document_label(DocumentId::from_uuid(document.as_uuid()));
        match label.is_empty() {
            true => koru_i18n::t(crate::format::lang(), fallback).to_owned(),
            false => label,
        }
    }

    /// The context a gesture arrives with — one row, or none.
    ///
    /// The label is the document's title as this window already knows it: the
    /// tab it is open in. A gesture on a document this window has never opened
    /// carries no title, and the row says so with an empty name rather than
    /// this surface reading the store to find one.
    fn prefilled_context(
        &self,
        workspace: koru_delegate_core::WorkspaceId,
        origin: &DelegationOrigin,
    ) -> Vec<world::ContextRow> {
        let Some((document, block)) = origin.subject() else {
            return Vec::new();
        };
        // **A row that is the document's own root is the document.** Pinning it
        // as a block would name a node the run's reader then has to explain,
        // and it is the same content either way. Filtered here rather than at
        // the gesture, because the root is the window's to know — and because
        // the *pill* still belongs on that row even when the context does not
        // name it.
        let block =
            block.filter(|node| Some(*node) != koru_workspace_host::View::document_root(self));
        let label = self.document_label(document);
        let store_document = koru_delegate_core::DocumentId::from_uuid(document.as_uuid());
        let source = match block {
            Some(node) => ContextSource::Block {
                workspace,
                document: store_document,
                node: BlockNodeId::from_uuid(node.as_uuid()),
            },
            None => ContextSource::Document {
                workspace,
                document: store_document,
            },
        };
        vec![world::ContextRow {
            source,
            label,
            block: block.is_some(),
        }]
    }

    /// The document open in the active tab, as a context row.
    ///
    /// `None` on a tab that is not a document — Welcome, Settings, a session,
    /// the workspace manager — which is the honest answer and the one that
    /// takes the control off the surface rather than dimming it.
    fn open_document_context(
        &self,
        workspace: koru_delegate_core::WorkspaceId,
    ) -> Option<world::ContextRow> {
        let document = self.tabs().active_tab().document.or(self.active_document)?;
        Some(world::ContextRow {
            source: ContextSource::Document {
                workspace,
                document: koru_delegate_core::DocumentId::from_uuid(document.as_uuid()),
            },
            label: self.document_label(document),
            block: false,
        })
    }

    /// What this window calls a document — the label of a tab showing it.
    fn document_label(&self, document: DocumentId) -> String {
        self.panes
            .iter_groups()
            .find_map(|(_, group)| {
                group
                    .tabs()
                    .iter()
                    .find(|tab| tab.document == Some(document))
                    .map(|tab| tab.label.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn delegation_open(&self) -> bool {
        self.delegation.is_some()
    }

    /// Keys, while the surface stands. It is a blocker, so **every** key is
    /// answered here and nothing reaches the document underneath.
    pub(crate) fn delegation_key(
        &mut self,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        let Some(modal) = self.delegation.as_mut() else {
            return false;
        };
        let outcome = modal.key(&mut self.engine, key, text, mods);
        paint::reveal_focus(&mut self.engine, modal, self.viewport);
        self.settle_delegation(outcome);
        true
    }

    pub(crate) fn delegation_scroll(&mut self, delta: f32) -> bool {
        let Some(modal) = self.delegation.as_mut() else {
            return false;
        };
        paint::scroll_by(&mut self.engine, modal, self.viewport, delta);
        true
    }

    pub(crate) fn delegation_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let viewport = self.viewport;
        let Some(modal) = self.delegation.as_mut() else {
            return false;
        };
        let outside = !paint::panel(&mut self.engine, modal, viewport).contains(x, y);
        let editing = matches!(modal.phase, Phase::Editing { .. });
        if outside && editing {
            // The scrim owns the press: close the draft, but never let the
            // same click activate the document underneath it.
            self.delegation = None;
            return true;
        }
        if outside {
            return true;
        }
        if let Some(geom) = paint::scrollbar(&mut self.engine, modal, viewport)
            && geom.in_gutter(x, y)
        {
            let grab = if geom.on_thumb(x, y) {
                y - geom.thumb_top
            } else {
                geom.thumb_h * 0.5
            };
            modal.scroll = geom.scroll_for_pointer(y, grab);
            modal.scrollbar_drag = Some(grab);
            return true;
        }
        if let Some((_, rect)) = paint::layout(&mut self.engine, modal, viewport)
            .into_iter()
            .find(|(hit, rect)| *hit == Hit::ModelSearch && rect.contains(x, y))
        {
            modal.activate_with_engine(&mut self.engine, Hit::ModelSearch);
            modal.dragging = modal.models.search.click(&mut self.engine, rect, x, y);
            return true;
        }
        if let Some((hit, rect)) = paint::layout(&mut self.engine, modal, viewport)
            .into_iter()
            .find(|(hit, rect)| {
                matches!(
                    hit,
                    Hit::ScheduleTime
                        | Hit::ScheduleDay
                        | Hit::ScheduleAdvanced
                        | Hit::ScheduleZone
                ) && rect.contains(x, y)
            })
        {
            modal.activate_with_engine(&mut self.engine, hit);
            if let Some(field) = modal.recurrence_field_mut(hit) {
                let tx = x - rect.x - koru_ui::text_field::PAD + field.scroll();
                field.click(&mut self.engine, tx);
                modal.dragging = true;
            }
            return true;
        }
        if let Some(rect) = paint::objective_rect(&mut self.engine, modal, viewport)
            && rect.contains(x, y)
        {
            let engine = &mut self.engine;
            modal.dragging = modal.objective.click(engine, rect, x, y);
            modal.activate_with_engine(&mut self.engine, Hit::Objective);
            return true;
        }
        if let Some(hit) = paint::hit_at(&mut self.engine, modal, viewport, x, y) {
            let outcome = modal.activate_with_engine(&mut self.engine, hit);
            paint::reveal_focus(&mut self.engine, modal, viewport);
            self.settle_delegation(outcome);
        }
        true
    }

    pub(crate) fn delegation_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let viewport = self.viewport;
        let Some(modal) = self.delegation.as_mut() else {
            return false;
        };
        if let Some(grab) = modal.scrollbar_drag
            && let Some(geom) = paint::scrollbar(&mut self.engine, modal, viewport)
        {
            modal.scroll = geom.scroll_for_pointer(y, grab);
            return true;
        }
        if modal.dragging
            && modal.focused() == Some(Hit::ModelSearch)
            && let Some((_, rect)) = paint::layout(&mut self.engine, modal, viewport)
                .into_iter()
                .find(|(hit, _)| *hit == Hit::ModelSearch)
        {
            modal.models.search.drag(&mut self.engine, rect, x, y);
            return true;
        }
        if modal.dragging
            && let Some(
                hit @ (Hit::ScheduleTime
                | Hit::ScheduleDay
                | Hit::ScheduleAdvanced
                | Hit::ScheduleZone),
            ) = modal.focused()
            && let Some(rect) = paint::field_rect(&mut self.engine, modal, viewport, hit)
            && let Some(field) = modal.recurrence_field_mut(hit)
        {
            let tx = x - rect.x - koru_ui::text_field::PAD + field.scroll();
            field.drag_to(&mut self.engine, tx);
            return true;
        }
        if modal.dragging
            && let Some(rect) = paint::objective_rect(&mut self.engine, modal, viewport)
        {
            let engine = &mut self.engine;
            modal.objective.drag(engine, rect, x, y);
            return true;
        }
        let hover = paint::hit_at(&mut self.engine, modal, viewport, x, y);
        let changed = modal.hover != hover;
        modal.hover = hover;
        changed
    }

    pub(crate) fn delegation_mouse_up(&mut self) -> bool {
        let Some(modal) = self.delegation.as_mut() else {
            return false;
        };
        modal.objective.release();
        modal.models.search.release();
        let dragged = modal.scrollbar_drag.take().is_some();
        std::mem::take(&mut modal.dragging) || dragged
    }

    fn settle_delegation(&mut self, outcome: Outcome) -> bool {
        self.discover_delegate_models();
        match outcome {
            Outcome::Keep => true,
            Outcome::Ignored => false,
            Outcome::Close => {
                self.delegation = None;
                true
            }
            // **The surface goes first, then the tab.** Opening a run selects
            // a tab and records a jump; leaving a modal up over it would put
            // a scrim over the thing the person just asked to see.
            // `open_run_tab` is the one door — the pill, the navigator and
            // Ctrl+Shift+E all go through it — so this cannot drift into a
            // second way of opening a run.
            Outcome::ViewRun(run) => {
                self.delegation = None;
                self.open_run_tab(run);
                true
            }
            // **A native dialog is not a modal's to open** (ADR-365): it
            // blocks, and this container performs every picker off the press
            // through the shell, which is what `ask_pane` is.
            Outcome::RefreshProvider(kind) => {
                self.refresh_managed_provider(kind);
                true
            }
            Outcome::ChooseFolder => {
                let lang = crate::format::lang();
                let start = self
                    .delegation
                    .as_ref()
                    .map(|modal| std::path::PathBuf::from(&modal.world.temporary_base));
                self.ask_pane(koru_workspace_host::PaneRequest::ChooseDelegateFolder {
                    title: koru_i18n::t(lang, koru_i18n::Key::DelegateFolderPickerTitle).to_owned(),
                    start,
                })
            }
            Outcome::Delegate(request) => {
                // `begin_delegation` keeps this unreachable through the UI,
                // but the ownership invariant belongs at the assignment too:
                // no future route may overwrite a receiver that has not been
                // drained. Retire the impossible second surface and say why.
                if self.delegate_job.is_some() {
                    self.delegation = None;
                    return self.show_delegation_in_flight();
                }
                let correlation = request.correlation();
                // The agent a canvas's new node starts with, when it is still
                // ready (`CanvasRuns::default_agent`).
                if matches!(request.submission, modal::Submission::RunOnce { .. }) {
                    self.canvas_runs.remember_modal_agent(request.agent.clone());
                }
                let client = self.delegate_client.clone();
                let wake = self.agent.wake_proxy();
                match job::start(client, *request, wake) {
                    Ok(started) => self.delegate_job = Some(started),
                    Err(phase) => {
                        if let Some(modal) = self.delegation.as_mut() {
                            modal.answered_for(correlation, phase);
                        }
                    }
                }
                true
            }
        }
    }

    /// Explain why another delegation cannot open while the preceding
    /// request is still awaiting its one durable answer. The worker's own
    /// localized progress wording is accurate for both request kinds and
    /// avoids inventing a second description of the same state.
    fn show_delegation_in_flight(&mut self) -> bool {
        let Some(mode) = self.delegate_job.as_ref().map(|job| job.correlation.mode) else {
            return false;
        };
        let key = match mode {
            modal::repeat::Kind::RunOnce => koru_i18n::Key::DelegateSending,
            modal::repeat::Kind::Repeat => koru_i18n::Key::DelegateScheduleSending,
        };
        let key = self
            .delegate_job
            .as_ref()
            .is_some_and(|job| job.correlation.edit)
            .then_some(koru_i18n::Key::DelegateScheduleSaving)
            .unwrap_or(key);
        self.toast = Some((
            koru_ui::Toast::new(
                koru_i18n::t(crate::format::lang(), key).to_owned(),
                None,
                std::time::Instant::now(),
            ),
            super::ToastPayload::None,
        ));
        true
    }

    /// Drain the worker. Called from `maintain`, never from a draw.
    pub(crate) fn maintain_delegation(&mut self) -> bool {
        let Some(job) = self.delegate_job.as_ref() else {
            return false;
        };
        let Some(answer) = job.try_take() else {
            return false;
        };
        let correlation = job.correlation;
        let anchor = job.pill_anchor;
        let schedule_name = job.schedule_name.clone();
        self.delegate_job = None;
        if answer.correlation != correlation || !correlation.accepts_phase(&answer.phase) {
            return false;
        }
        // **The crumb, on the window thread, the moment the run is real**
        // (D7, D8). The pill is a durable mark in a person's document, so it
        // is left through the document's own editing path — the shell serves
        // the queue in the same sweep it serves task commands — and never from
        // the worker, which holds no document and no workspace.
        //
        // It is queued whether or not the surface that asked is still up: the
        // run exists either way (§7.1), and the crumb is the gesture's promise
        // rather than the modal's.
        // **A run that has just been created is a run the Agents navigator
        // does not know about.** Its list is answered by a worker that only
        // looks when it is asked, so the one moment a new run appears is the
        // moment to ask: nothing else on this device knows a run was made.
        if matches!(&answer.phase, Phase::Done { .. }) {
            self.run_index.refresh();
        }
        if let (Some(anchor), Phase::Done { document, .. }) = (anchor, &answer.phase) {
            self.pending_run_pills
                .push(koru_workspace_host::runpill::RunPill {
                    document: anchor.document,
                    node: anchor.node,
                    run: *document,
                    label: koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::DelegateRunPillLabel,
                    )
                    .to_owned(),
                });
        }
        if let (Some(anchor), Some(name), Phase::Scheduled { schedule }) =
            (anchor, schedule_name, &answer.phase)
            && !answer.correlation.edit
        {
            self.pending_schedule_pills
                .push(koru_workspace_host::schedulepill::SchedulePill {
                    document: anchor.document,
                    node: anchor.node,
                    schedule: *schedule,
                    label: name.as_str().to_owned(),
                });
            self.schedule_index.refresh();
        }
        let edit_refused =
            answer.correlation.edit && matches!(&answer.phase, Phase::Refused { .. });
        if edit_refused {
            self.schedule_index.refresh();
        }
        let shown = self
            .delegation
            .as_mut()
            .is_some_and(|modal| modal.answered_for(answer.correlation, answer.phase.clone()));
        if shown {
            return true;
        }
        // Somebody closed the modal while the request was out, or replaced it
        // with a surface carrying another exact request. The old answer is
        // still real, but it may only become a toast; it must never mutate the
        // new draft.
        self.toast = Some((
            koru_ui::Toast::new(
                toast_line(&answer.phase, answer.correlation.edit),
                None,
                std::time::Instant::now(),
            ),
            super::ToastPayload::None,
        ));
        true
    }

    pub(crate) fn draw_delegation(
        &mut self,
        canvas: &mut vello::Scene,
        root: vello::kurbo::Affine,
    ) {
        let viewport = self.viewport;
        if let Some(modal) = self.delegation.as_mut() {
            paint::draw(modal, canvas, root, &mut self.engine, viewport);
        }
    }
}

/// What a toast says about an answer whose surface has gone.
fn toast_line(phase: &Phase, edit: bool) -> String {
    let lang = crate::format::lang();
    match phase {
        Phase::Done { run, .. } => koru_i18n::fill(
            koru_i18n::t(lang, koru_i18n::Key::DelegateSent),
            &[&run_label(*run)],
        ),
        Phase::Scheduled { .. } if edit => {
            koru_i18n::t(lang, koru_i18n::Key::DelegateScheduleUpdated).to_owned()
        }
        Phase::Scheduled { .. } => {
            koru_i18n::t(lang, koru_i18n::Key::DelegateScheduleCreated).to_owned()
        }
        Phase::Refused { code } => {
            koru_i18n::fill(koru_i18n::t(lang, koru_i18n::Key::DelegateRefused), &[code])
        }
        Phase::Editing { .. } | Phase::Sending | Phase::Scheduling => String::new(),
    }
}

/// **What a run is called where a person reads about it.**
///
/// The two-word name derived from the id (ADR-360), not the id: a UUID is
/// not something anyone recognises, and this is the same label the navigator
/// row, the tab and the pill already carry. Derived rather than looked up, so
/// the window thread reads no store to say it.
///
/// One function for the modal and the toast, because they say the same
/// sentence and only differ in where it is drawn.
pub(crate) fn run_label(run: RunId) -> String {
    koru_delegate_core::RunName::of(run).to_string()
}

impl MainView {
    /// **The editor's half of two of the four gestures.**
    ///
    /// `/delegate` and a document selection's *Delegate…* row both leave a
    /// [`koru_editor::DelegateRequest`] on the active view, because neither
    /// may open a surface itself. This turns one into an origin and calls the
    /// single path — which is why the two gestures cannot drift apart from the
    /// two the container raises directly.
    pub(crate) fn drain_delegate_request(&mut self) -> bool {
        let request = match self.active_view() {
            super::TabView::Leaf(leaf) => leaf.take_delegate_request(),
            _ => None,
        };
        let Some(request) = request else {
            return false;
        };
        let Some(document) = self.active_document else {
            return false;
        };
        // The row the gesture happened on, whatever it is — including the
        // document's own title row, which is where the pill still belongs.
        // `prefilled_context` is where a root row stops being a *context*
        // block; the two questions are answered in the two places that can
        // answer them.
        let block = request.node;
        let origin = if request.from_selection {
            DelegationOrigin::SelectionContextMenu {
                document,
                selection: block,
                objective: request.objective,
            }
        } else {
            DelegationOrigin::SlashCommand {
                document,
                block,
                objective: request.objective,
            }
        };
        self.begin_delegation(origin)
    }
}

impl MainView {
    /// The surface's rectangle, for the dictation indicator to hang from.
    pub(crate) fn delegation_panel(&self) -> Option<koru_draw::geometry::Rect> {
        // The panel is measured, so this needs the engine; `dictation_anchor`
        // reads it from an `&self`. Measuring is pure and cheap, and a cached
        // rectangle would be a second answer to drift, so the anchor is the
        // panel's *outer* bound computed from the last drawn viewport.
        self.delegation
            .as_ref()
            .map(|_| paint::anchor(self.viewport))
    }
}

// ------------------------------------------------------------ the pill's caption

impl MainView {
    /// **Keep every run pill on screen saying the truth** (D8, D16).
    ///
    /// Three steps, all cheap, none of them I/O: gather the runs the open
    /// leaves are drawing pills for, take whatever the worker has answered,
    /// and start the worker again when something on screen is still moving.
    /// The captions themselves ride the entity-title channel every routed
    /// document pill already uses, so a resolved run redraws through exactly
    /// one path.
    pub(crate) fn maintain_run_pills(&mut self, now: std::time::Instant) -> bool {
        let mut wanted: std::collections::HashMap<String, koru_store_core::ids::DocumentId> =
            std::collections::HashMap::new();
        for tab in self.tabs_mut().iter_mut() {
            if let super::TabView::Leaf(leaf) = &mut tab.view {
                for run in leaf.run_pill_documents() {
                    wanted.insert(
                        run.to_storage(),
                        koru_store_core::ids::DocumentId::from_uuid(run.as_uuid()),
                    );
                }
            }
        }
        // **A canvas's agent nodes are read by the same worker**, not a
        // second poller: one read per moving run, whoever is showing it.
        wanted.extend(self.canvas_runs.run_documents());
        self.run_pills_wanted = wanted;

        let mut changed = false;
        if let Some(answers) = self.run_pill_worker.take() {
            for answer in answers {
                let fresh = self.run_pill_states.get(&answer.document) != Some(&answer.state);
                self.run_pill_states
                    .insert(answer.document.clone(), answer.state);
                changed |= fresh;
            }
            if changed {
                changed = self.recompose_run_pill_captions();
            }
        }

        // **The idle case is the common one**, and it asks nothing: a document
        // whose delegations have all finished leaves `unsettled` empty, and a
        // window with no run pill on screen never reaches the worker at all.
        let unsettled = pills::unsettled(&self.run_pills_wanted, &self.run_pill_states);
        if let Some((_, _, _, Some(workspace))) = self.pane_context.clone() {
            let client = self.delegate_client.clone();
            self.run_pill_worker.refresh(
                now,
                client.as_ref(),
                koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid()),
                &unsettled,
                self.agent.wake_proxy(),
            );
        }
        changed
    }

    /// **The menu a right-click on a run pill opens** (ADR-369).
    ///
    /// What the run produced is already in hand: the same header the pill's
    /// caption is projected from carries it, so **nothing here reads the
    /// store or the artifact database**, and the rows are fixed at open the
    /// way every other menu in this window's are. A run that has registered
    /// nothing says so rather than opening an empty panel.
    pub(crate) fn drain_run_pill_menu(&mut self, at: (f32, f32)) -> bool {
        let asked = match &mut self.tabs_mut().active_mut().view {
            super::TabView::Leaf(leaf) => leaf.take_wants_run_menu(),
            _ => None,
        };
        let Some(document) = asked else {
            return false;
        };
        // A run pill is document content and still draws; with Delegate
        // hidden pressing it opens nothing (koru-features).
        if !koru_features::ai() {
            return false;
        }
        self.open_run_pill_menu(document, at)
    }

    pub(crate) fn drain_schedule_pill_menu(&mut self, at: (f32, f32)) -> bool {
        let asked = match &mut self.tabs_mut().active_mut().view {
            super::TabView::Leaf(leaf) => leaf.take_wants_schedule_menu(),
            _ => None,
        };
        let Some(schedule) = asked.and_then(|id| id.parse().ok()) else {
            return false;
        };
        if !koru_features::ai() {
            return false;
        }
        self.open_schedule_menu(vec![schedule], at)
    }

    /// The same menu, over a run this window has already resolved — the half
    /// that builds the rows, so opening one can be driven without a pointer.
    pub(crate) fn open_run_pill_menu(&mut self, document: DocumentId, at: (f32, f32)) -> bool {
        // Titles are drawn and never logged (standing requirement 7).
        let titles: Vec<String> = self
            .run_pill_states
            .get(&document.to_storage())
            .map(|state| {
                state
                    .results
                    .iter()
                    .map(|result| result.title.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        context_menu::set_run_pill_results(titles);
        let items = context_menu::run_pill_items();
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::RunPill { document },
            at,
            items,
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        self.submenu = None;
        true
    }

    /// **Open one of a run's results as a tab.**
    ///
    /// Through `open_artifact_tab`, the one primitive — so a result reached
    /// from a pill, from the run's own Result view and from the Artifacts
    /// column all land in the same tab. The device holding the bytes is named
    /// here (D27) because a device's name is this window's to resolve and an
    /// identity never derives from one.
    pub(crate) fn open_run_pill_result(&mut self, document: DocumentId, index: usize) -> bool {
        let Some((run, result)) =
            self.run_pill_states
                .get(&document.to_storage())
                .and_then(|state| {
                    state
                        .results
                        .get(index)
                        .cloned()
                        .map(|result| (state.run, result))
                })
        else {
            return false;
        };
        let here = self.this_device == Some(result.held_by);
        let held_elsewhere = (!here).then(|| self.device_names.get(&result.held_by).cloned());
        self.open_artifact_tab(koru_artifacts_ui::OpenedArtifact {
            version: result.artifact,
            title: result.title.as_str().to_owned(),
            size_bytes: result.size_bytes,
            produced_by: Some(run),
            held_elsewhere,
        })
    }

    /// Compose every resolved run's caption and push it to the leaves.
    ///
    /// The caption is built here rather than in the worker because it depends
    /// on two things the worker does not hold: the language, and **which
    /// device this is** — D16's rule is that the executor is named where it is
    /// not running, so the same run says different things on two machines from
    /// one stored record.
    pub(crate) fn recompose_run_pill_captions(&mut self) -> bool {
        let lang = crate::format::lang();
        let here = self.this_device;
        let names = self.device_names.clone();
        let captions: Vec<(String, String)> = self
            .run_pill_states
            .iter()
            .map(|(key, state)| {
                (
                    key.clone(),
                    state.caption(lang, here, |device| names.get(&device).cloned()),
                )
            })
            .collect();
        // A run executing on a device this window has never been introduced to
        // shows its state alone. Asking the agent once for the linked-device
        // list is what turns that into a name; asking again every frame is not.
        let unnamed = self.run_pill_states.values().any(|state| {
            state
                .device
                .is_some_and(|device| !names.contains_key(&device))
        });
        if unnamed {
            self.request_device_names_if_needed();
        }
        let mut changed = false;
        for (key, caption) in captions {
            if self.entity_titles.get(&key) != Some(&caption) {
                self.entity_titles.insert(key, caption);
                changed = true;
            }
        }
        if !changed {
            return false;
        }
        let titles = self.entity_titles.clone();
        let trashed = self.entity_trashed.clone();
        let live = self.entity_live();
        let mut acted = false;
        for tab in self.tabs_mut().iter_mut() {
            if let super::TabView::Leaf(leaf) = &mut tab.view {
                acted |= leaf.set_entity_titles(&titles, &trashed, &live);
            }
        }
        acted
    }

    /// When the window should look at its run pills again. `None` while
    /// nothing on screen is still moving — an idle surface contributes no
    /// clock.
    pub(crate) fn run_pill_deadline(&self) -> Option<std::time::Instant> {
        let unsettled = self.run_pills_wanted.iter().any(|(key, _)| {
            !self.run_pill_states.contains_key(key)
                || self
                    .run_pill_states
                    .get(key)
                    .is_some_and(|state| !state.status.is_terminal())
        });
        self.run_pill_worker.deadline(unsettled)
    }

    /// **Open a run** somebody clicked the pill of.
    ///
    /// One call site, deliberately: the run's own views — Overview, Activity,
    /// Result — are **P1.16**'s, and they are not on this branch. When that
    /// plan lands, this is where its tab opens; until then a click resolves
    /// the pill, finds the run, and has nowhere to put it, which is the honest
    /// state of the branch rather than a placeholder surface.
    /// Open the run a pill points at (P1.16's views).
    ///
    /// The pill names the run's *document*; the views are keyed by `RunId`.
    /// The caption cache already read that run's header to say what the pill
    /// says, so the mapping is a lookup rather than a round trip — and a
    /// pill whose run has not been read yet does nothing rather than opening
    /// an empty tab.
    pub(crate) fn open_run(&mut self, run: DocumentId, _where_: koru_editor::OpenWhere) -> bool {
        let Some(state) = self.run_pill_states.get(&run.to_storage()) else {
            return false;
        };
        let id = state.run;
        self.open_run_tab(id)
    }

    /// Drain a click or an Enter on a run pill.
    pub(crate) fn drain_run_pill_open(&mut self) -> bool {
        let asked = match self.active_view() {
            super::TabView::Leaf(leaf) => leaf.take_wants_run_open(),
            _ => None,
        };
        let Some((run, where_)) = asked else {
            return false;
        };
        self.open_run(run, where_)
    }

    pub(crate) fn drain_schedule_pill_open(&mut self) -> bool {
        let asked = match self.active_view() {
            super::TabView::Leaf(leaf) => leaf.take_wants_schedule_open(),
            _ => None,
        };
        let Some((schedule, _where_)) = asked else {
            return false;
        };
        if !koru_features::ai() {
            return false;
        }
        let Ok(schedule) = schedule.parse() else {
            return false;
        };
        self.reveal_schedule(schedule)
    }

    pub(super) fn reveal_schedule(&mut self, schedule: koru_delegate_core::ScheduleId) -> bool {
        // Schedules live in the Agents navigator and are made by Delegate:
        // both must be shown for there to be anywhere to reveal one.
        if !(koru_features::ai() && koru_features::agent_views()) {
            return false;
        }
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        self.explore.show_agents();
        let Some(node) = self.explore.agent_schedule_node(schedule) else {
            self.explore_stale = true;
            return true;
        };
        self.explore.set_selected(None);
        self.explore.select_only(node);
        self.explore_stale |= self.explore.reveal(node);
        self.relayout();
        self.explore.apply_reveal(self.regions.sidebar);
        self.explore.focus_tree(self.regions.sidebar);
        true
    }
}
