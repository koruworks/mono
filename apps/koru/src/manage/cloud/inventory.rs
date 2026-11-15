// SPDX-License-Identifier: MPL-2.0

//! Cloud inventory as a token-based element column. The frame owns geometry
//! for paint, controls, keyboard order, accessibility and F12 ancestry.
use super::*;
use koru_account_wire::cloud_plan::documents::*;
use koru_account_wire::cloud_plan::*;
use koru_draw::el::{self, El, Frame, FrameCx, TextCache, col, leaf, text};
use koru_draw::theme::{Space, roles};
use profiles::inventory::{PageData, Selection};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub(crate) struct InventoryLaid {
    pub frame: Rc<RefCell<Frame>>,
    tree: Rc<El>,
    stamp: Stamp,
}
#[derive(Clone, PartialEq)]
struct Stamp {
    copy: bool,
    revision: u64,
    scope: u64,
    account: Option<String>,
    column: Rect,
    lang: koru_i18n::Lang,
    theme: &'static str,
    contrast: theme::themes::Contrast,
    styles: [theme::TextStyle; 3],
    names: Vec<(ReadUuid, Option<String>)>,
}
impl std::fmt::Debug for InventoryLaid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InventoryLaid(<content>)")
    }
}
impl PartialEq for InventoryLaid {
    fn eq(&self, other: &Self) -> bool {
        let a = self.frame.borrow();
        let b = other.frame.borrow();
        a.nodes.len() == b.nodes.len()
            && a.nodes.iter().zip(&b.nodes).all(|(a, b)| {
                a.key == b.key && a.rect == b.rect && a.label == b.label && a.disabled == b.disabled
            })
    }
}

pub(crate) fn control_key(control: Control) -> Option<el::Key> {
    let part = match control {
        Control::CloudInventory => "inventory-view",
        Control::CloudInventoryBack => "inventory-back",
        Control::CloudInventoryWorkspaces => "inventory-workspaces",
        Control::CloudInventoryFirst => "inventory-first",
        Control::CloudInventoryNext => "inventory-next",
        Control::CloudInventoryPrevious => "inventory-previous",
        Control::CloudInventoryWorkspace(id, _) => {
            return Some(el::Key::model(id.as_u128(), "inventory-workspace"));
        }
        Control::CloudInventoryDetails(id) => {
            return Some(el::Key::model(id.as_u128(), "inventory-details"));
        }
        Control::CloudInventoryProfileDetails(id) => {
            return Some(el::Key::model(id.as_u128(), "inventory-profile-details"));
        }
        Control::CloudInventoryDocument(id) => {
            return Some(el::Key::model(id.as_u128(), "inventory-document"));
        }
        Control::CloudCopyBack => return Some(el::Key::surface("cloud-copy", "back")),
        Control::CloudCopyReload => return Some(el::Key::surface("cloud-copy", "reload")),
        Control::CloudCopyRemove(id) => {
            return Some(el::Key::model(id.as_u128(), "cloud-copy-remove"));
        }
        _ => return None,
    };
    Some(el::Key::surface("manage", part))
}

struct ButtonLeaf {
    label: String,
    width: f32,
}
impl el::Leaf for ButtonLeaf {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        (width.unwrap_or(self.width), ActionButton::H)
    }
    fn paint(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        _: &FrameCx,
        state: el::ElState,
    ) {
        let button = ActionButton::new(&self.label, ActionStyle::Ghost, None);
        if state.disabled {
            button.draw_disabled(canvas, root, engine, at);
        } else {
            button.draw_focused(canvas, root, engine, at, state.hovered, state.focused);
        }
    }
}
fn action(
    engine: &mut TextEngine,
    controls: &mut Vec<(Control, String)>,
    control: Control,
    key: TextKey,
    enabled: bool,
) -> El {
    let label = koru_i18n::t(crate::format::lang(), key).to_owned();
    controls.push((control, label.clone()));
    button(
        engine,
        control_key(control).expect("inventory control"),
        key,
        enabled,
    )
}
pub(super) fn button(engine: &mut TextEngine, id: el::Key, key: TextKey, enabled: bool) -> El {
    let label = koru_i18n::t(crate::format::lang(), key).to_owned();
    let width = ActionButton::new(&label, ActionStyle::Ghost, None).width(engine);
    leaf("Cloud action", ButtonLeaf { label, width })
        .semantic()
        .id(id)
        .hit()
        .focusable()
        .disabled(!enabled)
        .semantics(el::Role::Button)
        .name(key)
        .action(el::Action::Click)
        .action(el::Action::Focus)
}
pub(super) fn words(id: el::Key, label: String, heading: bool) -> El {
    text(&label)
        .id(id)
        .role(if heading { roles::SUBTITLE } else { roles::UI })
        .wrap()
        .semantics(if heading {
            el::Role::Heading
        } else {
            el::Role::Group
        })
        .name_text(label)
}
fn amount(state: UsageState, reason: Option<UsageReason>, bytes: Option<Decimal>) -> String {
    let lang = crate::format::lang();
    let key = match state {
        UsageState::Known => {
            return koru_i18n::fill(
                koru_i18n::t(lang, TextKey::CloudStorageUsed),
                &[&koru_i18n::number::decimal(
                    lang,
                    bytes.expect("validated known amount").get() as u64,
                )],
            );
        }
        UsageState::Building => TextKey::CloudStorageCalculating,
        UsageState::Unknown if reason == Some(UsageReason::CloudSchemaUpdateRequired) => {
            TextKey::CloudStorageSchema
        }
        UsageState::Unknown => TextKey::CloudStorageUnknown,
    };
    koru_i18n::t(lang, key).into()
}
fn identifier(id: uuid::Uuid) -> String {
    koru_i18n::fill(
        koru_i18n::t(crate::format::lang(), TextKey::CloudInventoryIdentifier),
        &[&id.to_string()],
    )
}

impl ManageView {
    pub(super) fn lay_inventory(&mut self) -> profile_page::Laid {
        let lang = crate::format::lang();
        let t = |key| koru_i18n::t(lang, key);
        let state = self.c.cloud_inventory();
        let names = match &state.page {
            Some(PageData::Workspaces(page)) => page
                .items
                .iter()
                .filter_map(|r| r.placement.profile_id)
                .map(|id| {
                    (
                        id,
                        self.c.cloud_inventory_profile_name(id).map(str::to_owned),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let cx = FrameCx::root();
        let stamp = Stamp {
            copy: false,
            revision: state.revision,
            scope: self.c.account().active_scope,
            account: self.c.account().active_account_id.clone(),
            column: self.column(),
            lang,
            theme: cx.theme.key,
            contrast: cx.contrast,
            styles: [
                theme::role::ui(),
                theme::role::subtitle(),
                theme::role::row(),
            ],
            names,
        };
        // Manage re-enters layout for pointer paint. Keep the bounded page's
        // shaped text/frame until state, authorized names or appearance changes.
        if self
            .laid
            .inventory
            .as_ref()
            .is_some_and(|laid| laid.stamp == stamp)
        {
            return self.laid.clone();
        }
        let checking = state.checking();
        let mut controls = Vec::new();
        let mut tree = col("Cloud inventory page")
            .gap(Space::S3)
            .child(words(
                el::Key::surface("manage", "inventory-title"),
                t(TextKey::CloudInventoryTitle).into(),
                true,
            ))
            .child(action(
                &mut self.engine,
                &mut controls,
                Control::CloudInventoryBack,
                TextKey::Back,
                true,
            ));
        if matches!(state.selection, Selection::Documents { .. }) {
            tree = tree.child(action(
                &mut self.engine,
                &mut controls,
                Control::CloudInventoryWorkspaces,
                TextKey::Workspaces,
                !checking,
            ));
        }
        if state.restarted {
            tree = tree.child(words(
                el::Key::surface("manage", "inventory-restarted"),
                t(TextKey::CloudInventoryRestarted).into(),
                false,
            ));
        }
        if checking || state.page.is_none() {
            tree = tree.child(words(
                el::Key::surface("manage", "inventory-status"),
                t(if state.failed {
                    TextKey::CloudInventoryUnavailable
                } else {
                    TextKey::CloudInventoryLoading
                })
                .into(),
                false,
            ));
        }
        if state.failed {
            tree = tree.child(action(
                &mut self.engine,
                &mut controls,
                Control::CloudInventoryFirst,
                TextKey::TryAgain,
                !checking,
            ));
        }
        match &state.page {
            Some(PageData::Workspaces(page)) => {
                let mut groups: Vec<(PlacementV1, Vec<&WorkspaceUsageItemV1>)> = Vec::new();
                for row in &page.items {
                    if let Some((_, rows)) = groups
                        .iter_mut()
                        .find(|(placement, _)| placement == &row.placement)
                    {
                        rows.push(row);
                    } else {
                        groups.push((row.placement.clone(), vec![row]));
                    }
                }
                for (placement, rows) in groups {
                    let (key, title) = match placement.profile_id {
                        Some(id) => (
                            el::Key::model(id.0.as_u128(), "inventory-profile"),
                            self.c
                                .cloud_inventory_profile_name(id)
                                .unwrap_or(t(TextKey::CloudInventoryProfile))
                                .to_owned(),
                        ),
                        None => (
                            el::Key::surface(
                                "manage",
                                if placement.state == PlacementState::Unassigned {
                                    "inventory-unassigned"
                                } else {
                                    "inventory-placement-unknown"
                                },
                            ),
                            t(if placement.state == PlacementState::Unassigned {
                                TextKey::CloudInventoryUnassigned
                            } else {
                                TextKey::CloudInventoryPlacementUnknown
                            })
                            .into(),
                        ),
                    };
                    tree = tree.child(words(key, title, true));
                    if let Some(profile) = placement.profile_id {
                        if let Some(total) = state.profile_total(profile) {
                            tree = tree.child(words(
                                el::Key::model(profile.0.as_u128(), "inventory-profile-total"),
                                koru_i18n::fill(
                                    t(TextKey::CloudInventoryProfileTotal),
                                    &[&koru_i18n::number::decimal(lang, total as u64)],
                                ),
                                false,
                            ));
                        }
                        if self.c.cloud_inventory_profile_name(profile).is_none() {
                            tree = tree.child(action(
                                &mut self.engine,
                                &mut controls,
                                Control::CloudInventoryProfileDetails(profile.0),
                                TextKey::CloudInventoryDetails,
                                true,
                            ));
                            if state.profile_details.contains(&profile.0) {
                                tree = tree.child(words(
                                    el::Key::model(profile.0.as_u128(), "inventory-profile-id"),
                                    identifier(profile.0),
                                    false,
                                ));
                            }
                        }
                    }
                    for row in rows {
                        let name = row.display_name.as_deref().filter(|n| !n.trim().is_empty());
                        let id = row.workspace_id.0;
                        let mut group = col("Cloud Workspace")
                            .pad(Space::S3)
                            .gap(Space::S2)
                            .id(el::Key::model(id.as_u128(), "inventory-workspace-group"))
                            .semantics(el::Role::Group)
                            .name_text(name.unwrap_or(t(TextKey::CloudInventoryWorkspace)))
                            .child(words(
                                el::Key::model(id.as_u128(), "inventory-row"),
                                name.unwrap_or(t(TextKey::CloudInventoryWorkspace)).into(),
                                false,
                            ))
                            .child(words(
                                el::Key::model(id.as_u128(), "inventory-usage"),
                                amount(row.usage.state, row.usage.reason, row.usage.files_bytes),
                                false,
                            ))
                            .child(action(
                                &mut self.engine,
                                &mut controls,
                                Control::CloudInventoryWorkspace(id, row.incarnation_id.0),
                                TextKey::CloudInventoryDocuments,
                                !checking,
                            ));
                        if name.is_none() {
                            group = group.child(action(
                                &mut self.engine,
                                &mut controls,
                                Control::CloudInventoryDetails(id),
                                TextKey::CloudInventoryDetails,
                                true,
                            ));
                            if state.details.contains(&id) {
                                group = group.child(words(
                                    el::Key::model(id.as_u128(), "inventory-id"),
                                    identifier(id),
                                    false,
                                ));
                            }
                        }
                        tree = tree.child(group);
                    }
                }
                if page.items.is_empty() {
                    tree = tree.child(words(
                        el::Key::surface("manage", "inventory-empty"),
                        t(TextKey::CloudInventoryEmpty).into(),
                        false,
                    ));
                }
            }
            Some(PageData::Documents(page)) => {
                tree = tree.child(words(
                    el::Key::surface("manage", "inventory-overlap"),
                    t(TextKey::CloudInventoryNonAdditive).into(),
                    false,
                ));
                for (membership, key, part) in [
                    (
                        MembershipState::Listed,
                        TextKey::CloudInventoryDocuments,
                        "inventory-listed",
                    ),
                    (
                        MembershipState::Trash,
                        TextKey::CloudInventoryTrash,
                        "inventory-trash",
                    ),
                    (
                        MembershipState::Unfiled,
                        TextKey::CloudInventoryUnfiled,
                        "inventory-unfiled",
                    ),
                    (
                        MembershipState::Structural,
                        TextKey::CloudInventoryStructure,
                        "inventory-structure",
                    ),
                    (
                        MembershipState::Unknown,
                        TextKey::CloudInventoryDocumentsUnavailable,
                        "inventory-unknown",
                    ),
                ] {
                    let rows: Vec<_> = page
                        .items
                        .iter()
                        .filter(|r| r.membership.state == membership)
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    tree = tree.child(words(el::Key::surface("manage", part), t(key).into(), true));
                    if membership == MembershipState::Unfiled {
                        tree = tree.child(words(
                            el::Key::surface("manage", "inventory-recovery"),
                            t(TextKey::CloudInventoryRecovery).into(),
                            false,
                        ));
                    }
                    for row in rows {
                        let id = row.cloud_unit_id.0;
                        let name = row.display_name.as_deref().filter(|n| !n.trim().is_empty());
                        let mut group = col("Cloud document")
                            .pad(Space::S3)
                            .gap(Space::S2)
                            .id(el::Key::model(id.as_u128(), "inventory-document-group"))
                            .semantics(el::Role::Group)
                            .name_text(name.unwrap_or(t(TextKey::CloudInventoryDocument)))
                            .child(words(
                                el::Key::model(id.as_u128(), "inventory-row"),
                                name.unwrap_or(t(TextKey::CloudInventoryDocument)).into(),
                                false,
                            ))
                            .child(words(
                                el::Key::model(id.as_u128(), "inventory-usage"),
                                amount(row.usage.state, row.usage.reason, row.usage.files_bytes),
                                false,
                            ));
                        if name.is_none() {
                            group = group.child(action(
                                &mut self.engine,
                                &mut controls,
                                Control::CloudInventoryDetails(id),
                                TextKey::CloudInventoryDetails,
                                true,
                            ));
                            if state.details.contains(&id) {
                                group = group.child(words(
                                    el::Key::model(id.as_u128(), "inventory-id"),
                                    identifier(row.document_id.map_or(id, |d| d.0)),
                                    false,
                                ));
                            }
                        }
                        if row.document_id.is_some()
                            && row.source_head_digest.is_some()
                            && membership != MembershipState::Structural
                        {
                            group = group.child(action(
                                &mut self.engine,
                                &mut controls,
                                Control::CloudInventoryDocument(id),
                                TextKey::CloudCopyTitle,
                                !checking,
                            ));
                        }
                        tree = tree.child(group);
                    }
                }
                if page.items.is_empty() {
                    tree = tree.child(words(
                        el::Key::surface("manage", "inventory-empty"),
                        t(TextKey::CloudInventoryEmpty).into(),
                        false,
                    ));
                }
            }
            None => {}
        }
        if state.page.is_some() {
            tree = tree.child(
                col("Cloud inventory paging")
                    .gap(Space::S2)
                    .child(action(
                        &mut self.engine,
                        &mut controls,
                        Control::CloudInventoryPrevious,
                        TextKey::CloudInventoryPrevious,
                        !checking && state.can_previous(),
                    ))
                    .child(action(
                        &mut self.engine,
                        &mut controls,
                        Control::CloudInventoryNext,
                        TextKey::CloudInventoryNext,
                        !checking && state.page.as_ref().and_then(PageData::cursor).is_some(),
                    ))
                    .child(action(
                        &mut self.engine,
                        &mut controls,
                        Control::CloudInventoryFirst,
                        TextKey::CloudInventoryFirst,
                        !checking,
                    )),
            );
        }
        let column = self.column();
        let mut cache = TextCache::default();
        let provisional = tree.layout(&mut self.engine, &mut cache, column, FrameCx::root());
        let height = provisional
            .nodes
            .iter()
            .skip(1)
            .map(|n| n.rect.y + n.rect.h - column.y)
            .fold(0.0f32, f32::max);
        let frame = tree.layout(
            &mut self.engine,
            &mut cache,
            Rect::new(column.x, column.y, column.w, height),
            FrameCx::root(),
        );
        let mut placed = Vec::new();
        let mut disabled = Vec::new();
        for (control, label) in controls {
            let node = frame
                .node(control_key(control).unwrap())
                .expect("control laid out");
            if node.disabled {
                disabled.push(control);
            }
            placed.push(Placed {
                control,
                label,
                rect: node.rect,
                look: Look::Action(ActionStyle::Ghost),
            });
        }
        profile_page::Laid {
            inventory: Some(InventoryLaid {
                frame: Rc::new(RefCell::new(frame)),
                tree: Rc::new(tree),
                stamp,
            }),
            controls: placed,
            disabled,
            height,
            ..Default::default()
        }
    }
    pub(super) fn paint_inventory(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(laid) = &self.laid.inventory else {
            return;
        };
        let mut frame = laid.frame.borrow_mut();
        for node in &mut frame.nodes {
            node.state.hovered =
                node.key.is_some() && node.key == self.page_hover.and_then(control_key);
            node.state.focused =
                node.key.is_some() && node.key == self.page_focus.and_then(control_key);
        }
        frame.paint(&laid.tree, canvas, root, &mut self.engine);
    }
    pub(super) fn activate_inventory(&mut self, control: Control) -> Option<bool> {
        control_key(control)?;
        if self.laid.disabled.contains(&control) {
            return Some(true);
        }
        if let Control::CloudInventoryDocument(unit) = control {
            self.c.open_cloud_copy(unit);
            self.page_scroll = 0.0;
            self.sync();
            return Some(true);
        }
        let copy_control = match control {
            Control::CloudCopyBack => Some(super::copy::Control::Back),
            Control::CloudCopyReload => Some(super::copy::Control::Reload),
            Control::CloudCopyRemove(node) => Some(super::copy::Control::Remove(node.to_string())),
            _ => None,
        };
        if let Some(control) = copy_control {
            let key = self.c.cloud_copy().view.key()?.clone();
            if let Some(intent) = self.c.cloud_copy_mut().view.activate(&key, &control) {
                self.c.cloud_copy_intent(intent);
            }
            if let Some((key, confirm)) = self.c.cloud_copy_mut().view.take_dialog() {
                self.dialog = Some(dialogs::Dialog::CloudFile { key, confirm });
            }
            self.page_scroll = 0.0;
            self.sync();
            return Some(true);
        }
        let state = self.c.cloud_inventory_mut();
        match control {
            Control::CloudInventory => state.open(),
            Control::CloudInventoryBack => state.hide(),
            Control::CloudInventoryWorkspaces => state.workspaces(),
            Control::CloudInventoryFirst => state.first(),
            Control::CloudInventoryNext => state.next(),
            Control::CloudInventoryPrevious => state.previous(),
            Control::CloudInventoryWorkspace(workspace, incarnation) => {
                state.documents(ReadUuid(workspace), ReadUuid(incarnation));
            }
            Control::CloudInventoryDetails(id) => state.toggle_details(id),
            Control::CloudInventoryProfileDetails(id) => state.toggle_profile_details(id),
            _ => unreachable!(),
        }
        if !matches!(
            control,
            Control::CloudInventoryDetails(_) | Control::CloudInventoryProfileDetails(_)
        ) {
            self.page_scroll = 0.0;
        }
        self.sync();
        Some(true)
    }
}

impl ManageView {
    pub(super) fn lay_cloud_copy(&mut self) -> profile_page::Laid {
        let cx = FrameCx::root();
        let stamp = Stamp {
            copy: true,
            revision: self.c.cloud_copy().revision,
            scope: self.c.account().active_scope,
            account: self.c.account().active_account_id.clone(),
            column: self.column(),
            lang: crate::format::lang(),
            theme: cx.theme.key,
            contrast: cx.contrast,
            styles: [
                theme::role::ui(),
                theme::role::subtitle(),
                theme::role::row(),
            ],
            names: Vec::new(),
        };
        if self
            .laid
            .inventory
            .as_ref()
            .is_some_and(|laid| laid.stamp == stamp)
        {
            return self.laid.clone();
        }
        let composition = self.c.cloud_copy().view.compose(&mut self.engine);
        let column = self.column();
        let mut cache = TextCache::default();
        let provisional =
            composition
                .tree
                .layout(&mut self.engine, &mut cache, column, FrameCx::root());
        let height = provisional
            .nodes
            .iter()
            .skip(1)
            .map(|n| n.rect.y + n.rect.h - column.y)
            .fold(0.0f32, f32::max);
        let frame = composition.tree.layout(
            &mut self.engine,
            &mut cache,
            Rect::new(column.x, column.y, column.w, height),
            FrameCx::root(),
        );
        let mut controls = Vec::new();
        let mut disabled = Vec::new();
        for (control, key) in composition.controls {
            let control = match control {
                super::copy::Control::Back => Control::CloudCopyBack,
                super::copy::Control::Reload => Control::CloudCopyReload,
                super::copy::Control::Remove(node) => {
                    Control::CloudCopyRemove(uuid::Uuid::parse_str(&node).expect("validated node"))
                }
            };
            let node = frame.node(key).expect("copy control laid out");
            if node.disabled {
                disabled.push(control);
            }
            let label = match &node.label {
                Some(el::Name::Key(key)) => koru_i18n::t(crate::format::lang(), *key).to_owned(),
                Some(el::Name::Text(text)) => text.clone(),
                None => String::new(),
            };
            controls.push(Placed {
                control,
                label,
                rect: node.rect,
                look: Look::Action(ActionStyle::Ghost),
            });
        }
        profile_page::Laid {
            inventory: Some(InventoryLaid {
                frame: Rc::new(RefCell::new(frame)),
                tree: Rc::new(composition.tree),
                stamp,
            }),
            controls,
            disabled,
            height,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests;
