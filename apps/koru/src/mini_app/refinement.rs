// SPDX-License-Identifier: MPL-2.0
//! Declaration snapshots are derived from retained native owners, never content.
use super::*;
use crate::panes::explore::rail::Bucket;
use koru_ai_core::mini_apps::{Declaration, RefinementTarget, TaskSelection, UpdateEnvelope};

impl MiniApp {
    pub fn adopt_created_surface(&mut self, name: &str) -> bool {
        let Some(id) = name
            .strip_prefix("mini-app-")
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .filter(|id| !id.is_nil())
        else {
            return false;
        };
        self.live_id = id;
        true
    }
    pub fn declaration_revision(&self) -> u64 {
        self.revision
    }

    pub fn refinement_target(&self) -> RefinementTarget {
        RefinementTarget {
            tab_id: self.surface,
            surface_id: self.live_id,
            base_revision: self.revision,
        }
    }

    pub fn effective_definition(&self) -> Declaration {
        use koru_ai_core::mini_apps::{DocumentReference, ListReference};
        let selection = match self.tasks.target() {
            RailTarget::List(id) => TaskSelection::List(id.as_uuid()),
            RailTarget::Bucket(bucket) => TaskSelection::Bucket(
                match bucket {
                    Bucket::Today => "today",
                    Bucket::Next => "next",
                    Bucket::Upcoming => "upcoming",
                    Bucket::Later => "later",
                    Bucket::Completed => "completed",
                }
                .into(),
            ),
            _ => unreachable!("mini-app selection is a bucket or list"),
        };
        let mut definition = Declaration::parse(koru_ai_core::mini_apps::FIXTURE.as_bytes())
            .expect("owned bounded declaration fixture");
        definition.envelope.surface.surface_id = self.refinement_target().surface_name();
        for component in &mut definition.envelope.surface.components {
            match component.id.as_str() {
                "root" => {
                    component.arrangement = Some(
                        if self.swapped {
                            "document-first"
                        } else {
                            "tasks-first"
                        }
                        .into(),
                    )
                }
                "tasks" => {
                    component.bucket = match &selection {
                        TaskSelection::Bucket(bucket) => Some(bucket.clone()),
                        _ => None,
                    };
                    component.list = match selection {
                        TaskSelection::List(list_id) => Some(ListReference {
                            workspace: "current".into(),
                            list_id,
                        }),
                        _ => None,
                    };
                }
                "document" => {
                    component.document = self.document.map(|document| DocumentReference {
                        workspace: "current".into(),
                        document_id: koru_ai_core::DocumentId::from_uuid(document.as_uuid()),
                    })
                }
                _ => {}
            }
        }
        Declaration::validate(definition.envelope).expect("native owners form a valid declaration")
    }

    pub fn refine(
        &mut self,
        target: RefinementTarget,
        envelope: &UpdateEnvelope,
    ) -> Result<(), &'static str> {
        if target != self.refinement_target() || envelope.update.surface_id != target.surface_name()
        {
            return Err("the mini app changed while the refinement was being prepared");
        }
        let swapped = envelope.arrangement()?;
        if self.swapped != swapped {
            self.swapped = swapped;
            self.revision += 1;
            self.layout();
        }
        Ok(())
    }

    pub(crate) fn select_tasks(&mut self, target: RailTarget) {
        if self.tasks.set_target(target) {
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests;
