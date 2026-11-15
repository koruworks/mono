// SPDX-License-Identifier: MPL-2.0

//! Passive exact-scoped Cloud copy composition. No Store, main editor, image
//! cache or draft handle can enter this surface. Native transport owns authority.
use super::inventory::{button, words};
use koru_draw::{
    el::{self, El, col},
    text::TextEngine,
    theme::Space,
};
use koru_i18n::Key as TextKey;
use koru_store_wire::cloud_document::{
    CloudDocumentFailure, CloudDocumentInspectionV1, CloudDocumentSelection, EntityReference,
    FileRemovalMode, RichText,
};
use koru_ui::confirm::{Confirm, Outcome};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewKey {
    pub owner_scope: u64,
    pub view_epoch: u64,
    pub selection: CloudDocumentSelection,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Unavailable,
    Loading,
    Ready,
    Removing,
    SourceChanged,
    OutcomeUnknown,
    Acknowledged,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    Back,
    Reload(ViewKey),
    Remove { key: ViewKey, node: String },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Back,
    Reload,
    Remove(String),
}
struct PendingConfirmation {
    key: ViewKey,
    node: String,
    dialog: Option<Confirm>,
}
#[derive(Default)]
pub(crate) struct Presentation {
    key: Option<ViewKey>,
    phase: Phase,
    view: Option<CloudDocumentInspectionV1>,
    confirmation: Option<PendingConfirmation>,
}
impl Presentation {
    pub fn open(&mut self, key: ViewKey) -> bool {
        self.clear();
        if key.owner_scope == 0 || !key.selection.validate() {
            return false;
        }
        self.key = Some(key);
        self.phase = Phase::Loading;
        true
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn key(&self) -> Option<&ViewKey> {
        self.key.as_ref()
    }
    pub fn install(&mut self, key: &ViewKey, json: &str) -> bool {
        if self.key.as_ref() != Some(key) || self.phase != Phase::Loading {
            return false;
        }
        self.confirmation = None;
        self.view = CloudDocumentInspectionV1::from_json(json)
            .ok()
            .filter(|v| v.selection == key.selection);
        self.phase = if self.view.is_some() {
            Phase::Ready
        } else {
            Phase::Unavailable
        };
        true
    }
    pub fn failed(&mut self, key: &ViewKey, failure: CloudDocumentFailure) -> bool {
        if self.key.as_ref() != Some(key) {
            return false;
        }
        self.confirmation = None;
        self.view = None;
        self.phase = match failure {
            CloudDocumentFailure::Unavailable => Phase::Unavailable,
            CloudDocumentFailure::SourceChanged => Phase::SourceChanged,
            CloudDocumentFailure::OutcomeUnknown => Phase::OutcomeUnknown,
        };
        true
    }
    pub fn acknowledged(&mut self, key: &ViewKey) -> bool {
        if self.key.as_ref() != Some(key) || self.phase != Phase::Removing {
            return false;
        }
        self.confirmation = None;
        self.view = None;
        self.phase = Phase::Acknowledged;
        true
    }
    fn eligible(&self, key: &ViewKey, node: &str) -> bool {
        self.key.as_ref() == Some(key)
            && self.phase == Phase::Ready
            && self.view.as_ref().is_some_and(|view| {
                view.can_request_removal
                    && view.removable_files.iter().any(|file| file.node_id == node)
            })
    }
    pub fn activate(&mut self, key: &ViewKey, control: &Control) -> Option<Intent> {
        if self.key.as_ref() != Some(key) {
            return None;
        }
        match control {
            Control::Back => {
                self.clear();
                Some(Intent::Back)
            }
            Control::Reload if !matches!(self.phase, Phase::Loading | Phase::Removing) => {
                self.confirmation = None;
                self.view = None;
                self.phase = Phase::Loading;
                Some(Intent::Reload(key.clone()))
            }
            Control::Remove(node) if self.eligible(key, node) => {
                let slot = self
                    .view
                    .as_ref()
                    .and_then(|v| v.removable_files.iter().find(|f| &f.node_id == node))
                    .is_some_and(|f| f.mode == FileRemovalMode::RetainedSlot);
                self.confirmation = Some(PendingConfirmation {
                    key: key.clone(),
                    node: node.clone(),
                    dialog: Some(
                        Confirm::localized(
                            t(if slot {
                                TextKey::CloudCopyRemoveAttachmentQuestion
                            } else {
                                TextKey::CloudCopyRemoveQuestion
                            }),
                            t(if slot {
                                TextKey::CloudCopyRemoveAttachmentExplanation
                            } else {
                                TextKey::CloudCopyRemoveExplanation
                            }),
                            t(if slot {
                                TextKey::CloudCopyRemoveAttachment
                            } else {
                                TextKey::CloudCopyRemove
                            }),
                            t(TextKey::ProfileRemoveCancel),
                        )
                        .declaring_no_default(),
                    ),
                });
                None
            }
            _ => None,
        }
    }
    pub fn confirmation(&mut self) -> Option<&mut Confirm> {
        self.confirmation.as_mut().and_then(|p| p.dialog.as_mut())
    }
    pub fn take_dialog(&mut self) -> Option<(ViewKey, Confirm)> {
        let pending = self.confirmation.as_mut()?;
        Some((pending.key.clone(), pending.dialog.take()?))
    }
    pub fn answer(&mut self, key: &ViewKey, answer: Outcome) -> Option<Intent> {
        if answer == Outcome::Keep {
            return None;
        }
        let pending = self.confirmation.take()?;
        if answer != Outcome::Confirm || &pending.key != key || !self.eligible(key, &pending.node) {
            return None;
        }
        self.phase = Phase::Removing;
        Some(Intent::Remove {
            key: pending.key,
            node: pending.node,
        })
    }
    pub fn compose(&self, engine: &mut TextEngine) -> Composition {
        let mut controls = Vec::new();
        let mut tree = col("Cloud copy page")
            .gap(Space::S3)
            .child(words(
                surface("title"),
                t(TextKey::CloudCopyTitle).into(),
                true,
            ))
            .child(action(
                engine,
                &mut controls,
                Control::Back,
                TextKey::Back,
                true,
            ))
            .child(words(
                surface("passive"),
                t(TextKey::CloudCopyPassive).into(),
                false,
            ));
        let status = match self.phase {
            Phase::Loading => Some(TextKey::CloudCopyLoading),
            Phase::Unavailable => Some(TextKey::CloudCopyUnavailable),
            Phase::SourceChanged => Some(TextKey::CloudCopyChanged),
            Phase::Removing => Some(TextKey::CloudCopyRemoving),
            Phase::OutcomeUnknown => Some(TextKey::CloudCopyUnknown),
            Phase::Acknowledged => Some(TextKey::CloudCopyAcknowledged),
            Phase::Ready => None,
        };
        if let Some(status) = status {
            tree = tree.child(words(surface("status"), t(status).into(), false));
        }
        tree = tree.child(action(
            engine,
            &mut controls,
            Control::Reload,
            TextKey::CloudCopyReload,
            !matches!(self.phase, Phase::Loading | Phase::Removing),
        ));
        if let Some(view) = &self.view {
            tree = tree.child(words(
                surface("document-title"),
                view.document.title.clone(),
                true,
            ));
            let eligible: HashSet<_> = view
                .removable_files
                .iter()
                .map(|file| file.node_id.as_str())
                .collect();
            if !view.can_request_removal || eligible.is_empty() {
                tree = tree.child(words(
                    surface("no-actions"),
                    t(TextKey::CloudCopyNoActions).into(),
                    false,
                ));
            }
            // Parent-before-child validation makes this one pass. Deep structure
            // remains flat in reading order; no recursive editor/navigation tree.
            for block in &view.document.blocks {
                let id = uuid::Uuid::parse_str(&block.id)
                    .expect("validated block id")
                    .as_u128();
                let mut row = col("Cloud copy block")
                    .gap(Space::S2)
                    .pad(Space::S2)
                    .id(el::Key::model(id, "cloud-copy-block"))
                    .semantics(el::Role::Group);
                if block.in_trash || block.kind == "trash" {
                    row = row.child(words(
                        el::Key::model(id, "cloud-copy-trash"),
                        t(TextKey::CloudInventoryTrash).into(),
                        block.kind == "trash",
                    ));
                }
                if let Some(text) = &block.rich_text {
                    row = row.child(words(
                        el::Key::model(id, "cloud-copy-text"),
                        passive_text(text),
                        block.kind == "document" || block.properties.heading_level.is_some(),
                    ));
                } else if let Some(name) = &block.properties.name {
                    row = row.child(words(
                        el::Key::model(id, "cloud-copy-name"),
                        name.clone(),
                        block.kind == "document",
                    ));
                }
                if let Some(reference) = &block.reference {
                    row = row.child(words(
                        el::Key::model(id, "cloud-copy-reference"),
                        reference_label(reference),
                        false,
                    ));
                }
                if let Some(blob) = &block.blob {
                    row = row
                        .child(words(
                            el::Key::model(id, "cloud-copy-mime"),
                            blob.mime_type.clone(),
                            false,
                        ))
                        .child(words(
                            el::Key::model(id, "cloud-copy-preview"),
                            t(TextKey::CloudCopyPreviewUnavailable).into(),
                            false,
                        ));
                }
                if view.can_request_removal && eligible.contains(block.id.as_str()) {
                    let slot = view
                        .removable_files
                        .iter()
                        .find(|f| f.node_id == block.id)
                        .is_some_and(|f| f.mode == FileRemovalMode::RetainedSlot);
                    row = row.child(action(
                        engine,
                        &mut controls,
                        Control::Remove(block.id.clone()),
                        if slot {
                            TextKey::CloudCopyRemoveAttachment
                        } else {
                            TextKey::CloudCopyRemove
                        },
                        self.phase == Phase::Ready,
                    ));
                }
                tree = tree.child(row);
            }
        }
        Composition { tree, controls }
    }
}
pub(crate) struct Composition {
    pub tree: El,
    pub controls: Vec<(Control, el::Key)>,
}
fn t(key: TextKey) -> &'static str {
    koru_i18n::t(crate::format::lang(), key)
}
fn surface(part: &'static str) -> el::Key {
    el::Key::surface("cloud-copy", part)
}
fn action(
    engine: &mut TextEngine,
    controls: &mut Vec<(Control, el::Key)>,
    control: Control,
    label: TextKey,
    enabled: bool,
) -> El {
    let key = match &control {
        Control::Back => surface("back"),
        Control::Reload => surface("reload"),
        Control::Remove(id) => el::Key::model(
            uuid::Uuid::parse_str(id)
                .expect("validated file id")
                .as_u128(),
            "cloud-copy-remove",
        ),
    };
    controls.push((control, key));
    button(engine, key, label, enabled)
}
fn reference_label(reference: &EntityReference) -> String {
    match reference {
        EntityReference::Document { .. } => t(TextKey::CloudInventoryDocument).into(),
        EntityReference::Block { .. } => t(TextKey::CloudCopyBlockReference).into(),
        EntityReference::Run { .. } => t(TextKey::DelegateRunPillLabel).into(),
        EntityReference::Url { url } => url.clone(),
        EntityReference::Label { value } => value.clone(),
    }
}
fn passive_text(rich: &RichText) -> String {
    let mut objects: HashMap<usize, Vec<String>> = HashMap::new();
    for object in &rich.inline_objects {
        objects.entry(object.anchor_utf8).or_default().push(
            object
                .label
                .clone()
                .unwrap_or_else(|| reference_label(&object.reference)),
        );
    }
    let mut out = String::new();
    for (index, ch) in rich.text.char_indices() {
        if let Some(labels) = objects.remove(&index) {
            out.push_str(&labels.join(" "));
        } else {
            out.push(ch);
        }
    }
    if let Some(labels) = objects.remove(&rich.text.len()) {
        out.push_str(&labels.join(" "));
    }
    out
}

#[cfg(test)]
mod tests;
