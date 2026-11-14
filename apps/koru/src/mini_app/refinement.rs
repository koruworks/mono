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
        let target = self.refinement_target();
        if let Some((cached, definition)) = self.definition_cache.borrow().as_ref()
            && *cached == target
        {
            return definition.clone();
        }
        let definition = self.build_definition();
        *self.definition_cache.borrow_mut() = Some((target, definition.clone()));
        definition
    }

    pub fn refinement_context(&self) -> koru_ai_core::mini_apps::RefinementContext {
        koru_ai_core::mini_apps::RefinementContext {
            target: self.refinement_target(),
            declaration: self.effective_definition().envelope,
        }
    }

    fn build_definition(&self) -> Declaration {
        use koru_ai_core::mini_apps::{DocumentReference, ListReference};
        if let Some(surface) = &self.composition {
            let mut envelope = surface.declaration.envelope.clone();
            envelope.surface.surface_id = self.refinement_target().surface_name();
            let host = surface.declaration.composition().unwrap().document_host();
            if let Some(document) = self.document
                && let Some(component) = envelope
                    .surface
                    .components
                    .iter_mut()
                    .find(|c| c.id == host)
            {
                component.document = Some(DocumentReference {
                    workspace: "current".into(),
                    document_id: koru_ai_core::DocumentId::from_uuid(document.as_uuid()),
                });
            }
            return Declaration::validate(envelope).expect("retained composition is validated");
        }
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
        let next = envelope.refine(&self.effective_definition())?;
        if let Some(surface) = &mut self.composition {
            if surface.declaration.envelope.surface.components != next.envelope.surface.components {
                surface.declaration = next;
                surface.dirty = true;
                self.revision += 1;
                self.layout();
            }
            return Ok(());
        }
        let swapped = next.swapped().ok_or("catalog cannot change")?;
        if self.swapped != swapped {
            self.swapped = swapped;
            self.revision += 1;
            self.layout();
        }
        Ok(())
    }

    pub(crate) fn select_tasks(&mut self, target: RailTarget) {
        if self.composition.is_some() {
            return;
        }
        if self.tasks.set_target(target) {
            self.revision += 1;
        }
    }
}

#[cfg(test)]
mod tests;
