// SPDX-License-Identifier: MPL-2.0

//! The closed catalog adapts to Koru elements. The editor stays owned by
//! MiniApp; this module owns placement and local controls only.
use super::*;
use koru_ai_core::mini_apps::{Declaration, composition as model};
use koru_draw::{
    el,
    text::TextEngine,
    theme::{Metric, Space},
};
use koru_ui::{ActionButton, ActionStyle};
use koru_workspace_host::accessibility::{
    ControlPart, Occurrence, SemanticNode, ViewSlot, WindowSlot,
};
use std::collections::{HashMap, HashSet};
use winit::keyboard::NamedKey;

const SAVE: el::Key = el::Key::surface("composition", "save");
const PIN: el::Key = el::Key::surface("composition", "pin");

struct ButtonMeasure(f32);
impl el::Leaf for ButtonMeasure {
    fn measure(&self, width: Option<f32>) -> (f32, f32) {
        (width.unwrap_or(self.0), ActionButton::H)
    }
}

pub(super) struct Surface {
    pub declaration: Declaration,
    surface: u64,
    pub focused: Option<el::Key>,
    pub frame: el::Frame,
    tree: el::El,
    engine: TextEngine,
    cache: el::TextCache,
    offsets: HashMap<el::Key, f32>,
    stacked: HashSet<el::Key>,
    layout_inputs: Option<((f32, f32), Option<DocumentId>)>,
    pub dirty: bool,
}

impl Surface {
    pub fn new(declaration: Declaration, surface: u64) -> Self {
        let mut engine = TextEngine::new();
        let mut cache = el::TextCache::default();
        let tree = el::col("Composition");
        let frame = tree.layout(
            &mut engine,
            &mut cache,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            el::FrameCx::root(),
        );
        Self {
            declaration,
            surface,
            focused: None,
            frame,
            tree,
            engine,
            cache,
            offsets: HashMap::new(),
            stacked: HashSet::new(),
            layout_inputs: None,
            dirty: true,
        }
    }
    fn key(&self, id: &str) -> el::Key {
        let mut hash = blake3::Hasher::new();
        hash.update(&self.surface.to_le_bytes());
        hash.update(id.as_bytes());
        let bytes: [u8; 16] = hash.finalize().as_bytes()[..16].try_into().unwrap();
        el::Key::model(u128::from_le_bytes(bytes), "composition")
    }
    pub fn references(&self, document: DocumentId) -> bool {
        self.declaration
            .document_references()
            .iter()
            .any(|id| id.as_uuid() == document.as_uuid())
    }
    pub fn layout(&mut self, viewport: (f32, f32), selected: Option<DocumentId>) -> Regions {
        if !self.dirty && self.layout_inputs == Some((viewport, selected)) {
            return self.regions();
        }
        self.layout_inputs = Some((viewport, selected));
        self.dirty = false;
        self.stacked.clear();
        // Let the actual element allocations identify nested rows that cannot
        // fit. Rebuild those as columns; all measuring still belongs to el.
        for _ in 0..=model::MAX_DEPTH {
            let content = self.build("root", viewport, selected, false);
            let actions = el::row("Composition actions")
                .gap(Space::S2)
                .pad_x(Space::S4)
                .child(
                    control(
                        "Save mini app",
                        SAVE,
                        koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppSave),
                    )
                    .fill(),
                )
                .child(
                    control(
                        "Pin mini app",
                        PIN,
                        koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppPin),
                    )
                    .fill(),
                );
            self.tree = el::col("Composition").child(actions).child(content.fill());
            self.frame = self.tree.layout(
                &mut self.engine,
                &mut self.cache,
                Rect::new(0.0, 0.0, viewport.0.max(0.0), viewport.1.max(0.0)),
                el::FrameCx::root(),
            );
            let mut changed = false;
            for node in &self.frame.nodes {
                if node.name == "Composition row"
                    && node.overflow
                    && let Some(key) = node.key
                {
                    changed |= self.stacked.insert(key);
                }
            }
            if !changed {
                break;
            }
        }
        for node in &self.frame.nodes {
            if let (Some(key), Some((_, offset))) = (node.key, node.scroll) {
                self.offsets.insert(key, offset);
            }
        }
        self.regions()
    }
    fn regions(&self) -> Regions {
        let host = self.declaration.composition().unwrap().document_host();
        Regions {
            tasks: Rect::new(0.0, 0.0, 0.0, 0.0),
            document: self
                .frame
                .node(self.key(host))
                .map_or(Rect::new(0.0, 0.0, 0.0, 0.0), |n| n.rect),
        }
    }
    fn build(
        &mut self,
        id: &str,
        viewport: (f32, f32),
        selected: Option<DocumentId>,
        in_scroll: bool,
    ) -> el::El {
        let node = self
            .declaration
            .composition()
            .unwrap()
            .node(id)
            .unwrap()
            .clone();
        let key = self.key(id);
        let narrow = viewport.0 < 2.0 * theme::CONTENT_MIN_W || self.stacked.contains(&key);
        // Document-free control subtrees become bounded scroll controls. The native
        // document viewport never moves with a control list's scroll offset.
        let controls = !in_scroll && !self.contains_host(id) && self.contains_buttons(id);
        let children = node.children().to_vec();
        let mut element = match &node.kind {
            model::NodeKind::Row { .. } if !narrow => el::row("Composition row"),
            model::NodeKind::Row { .. } | model::NodeKind::Column { .. } => {
                el::col("Composition column")
            }
            model::NodeKind::Box { .. } => el::boxed("Composition box"),
            model::NodeKind::Text { text, role, ink } => el::text(text)
                .role(text_role(*role))
                .ink(ink_role(*ink))
                .wrap(),
            model::NodeKind::Spacer { space } => el::spacer().h(spacing(*space)).w(spacing(*space)),
            model::NodeKind::Divider => el::divider(theme::Line::Divider),
            model::NodeKind::DocumentHost { .. } => el::col("Document host").fill(),
            model::NodeKind::Button {
                label,
                action: model::Action::SelectDocument { document, .. },
            } => {
                let width =
                    ActionButton::new(label, ActionStyle::Secondary, None).width(&mut self.engine);
                el::leaf("Document selection", ButtonMeasure(width))
                    .semantic()
                    .h(Metric::new("KoruButton height", ActionButton::H))
                    .hit()
                    .focusable()
                    .semantics(el::Role::Button)
                    .name_text(label)
                    .action(el::Action::Click)
                    .action(el::Action::Focus)
                    .max_w(el::Length::Runtime(
                        (viewport.0 - Space::S8.px(theme::Profile::default())).max(0.0),
                        "composition viewport",
                    ))
                    .selected(
                        selected.is_some_and(|id| id.as_uuid() == document.document_id.as_uuid()),
                    )
            }
        }
        .id(key);
        for child in children {
            element = element.child(self.build(&child, viewport, selected, in_scroll || controls));
        }
        if let Some(space) = node.layout.gap {
            element = element.gap(spacing(space));
        }
        if let Some(space) = node.layout.padding {
            element = element.pad(spacing(space));
        }
        element = match node.layout.align {
            Some(model::Align::Start) => element.items_start(),
            Some(model::Align::Center) => element.items_center(),
            Some(model::Align::End) => element.items_end(),
            Some(model::Align::Stretch) => element.items_stretch(),
            None => element,
        };
        // Fill is the existing engine's parent-axis contract. Cross-axis Fill
        // uses its default stretch; Hug explicitly requests intrinsic size.
        let parent_horizontal = self
            .declaration
            .composition()
            .unwrap()
            .nodes()
            .iter()
            .find(|parent| parent.children().iter().any(|child| child == id))
            .is_some_and(|parent| {
                matches!(parent.kind, model::NodeKind::Row { .. })
                    && viewport.0 >= 2.0 * theme::CONTENT_MIN_W
                    && !self.stacked.contains(&self.key(&parent.id))
            });
        if (parent_horizontal && node.layout.width == Some(model::Sizing::Fill))
            || (!parent_horizontal
                && !in_scroll
                && !controls
                && node.layout.height == Some(model::Sizing::Fill))
        {
            element = element.fill();
        }
        if (parent_horizontal && node.layout.height == Some(model::Sizing::Fill))
            || (!parent_horizontal && node.layout.width == Some(model::Sizing::Fill))
        {
            element = element.align_self(el::Align::Stretch);
        }
        if node.layout.width == Some(model::Sizing::Hug) {
            element = element.hug_w();
        }
        if node.layout.height == Some(model::Sizing::Hug) {
            element = element.hug_h();
        }
        if let Some(surface) = node.decoration.surface {
            element = element.bg(match surface {
                model::Surface::Ground => theme::Surface::Ground,
                model::Surface::Raised => theme::Surface::Raised,
                model::Surface::Chip => theme::Surface::Chip,
            });
        }
        if let Some(radius) = node.decoration.radius {
            element = element.radius(radius_role(radius));
        }
        if controls {
            let scroll_key = el::Key {
                part: "scroll",
                ..key
            };
            element = el::scroll_y("Composition controls")
                .id(scroll_key)
                .scrollbar()
                .offset(self.offsets.get(&scroll_key).copied().unwrap_or(0.0))
                .max_h(el::Length::Runtime(
                    (viewport.1 - theme::CONTENT_MIN_W).max(ActionButton::H),
                    "viewport less native content minimum",
                ))
                .child(element.hug_h());
            if !parent_horizontal && node.layout.width == Some(model::Sizing::Fill) {
                element = element.align_self(el::Align::Stretch);
            }
            if parent_horizontal && node.layout.width == Some(model::Sizing::Fill) {
                element = element.fill();
            }
        }
        element
    }
    fn contains_host(&self, id: &str) -> bool {
        let tree = self.declaration.composition().unwrap();
        id == tree.document_host()
            || tree
                .node(id)
                .unwrap()
                .children()
                .iter()
                .any(|child| self.contains_host(child))
    }
    fn contains_buttons(&self, id: &str) -> bool {
        let node = self.declaration.composition().unwrap().node(id).unwrap();
        matches!(node.kind, model::NodeKind::Button { .. })
            || node
                .children()
                .iter()
                .any(|child| self.contains_buttons(child))
    }
    fn document_for(&self, key: el::Key) -> Option<DocumentId> {
        self.declaration
            .composition()?
            .nodes()
            .iter()
            .find_map(|node| {
                if self.key(&node.id) != key {
                    return None;
                }
                match &node.kind {
                    model::NodeKind::Button {
                        action: model::Action::SelectDocument { document, .. },
                        ..
                    } => Some(DocumentId::from_uuid(document.document_id.as_uuid())),
                    _ => None,
                }
            })
    }
    fn controls(&self) -> Vec<el::Key> {
        self.frame
            .nodes
            .iter()
            .filter(|node| node.focusable)
            .filter_map(|node| node.key)
            .collect()
    }
    fn reveal(&mut self, key: el::Key) {
        if let Some((index, offset)) = self.frame.reveal(key)
            && let Some(scroll_key) = self.frame.nodes[index].key
        {
            self.offsets.insert(scroll_key, offset);
            self.dirty = true;
        }
    }
    pub fn scroll(&mut self, pointer: (f32, f32), delta: MouseScrollDelta) -> bool {
        let Some(node) = self
            .frame
            .nodes
            .iter()
            .rev()
            .find(|node| node.scroll.is_some() && node.rect.contains(pointer.0, pointer.1))
        else {
            return false;
        };
        let Some(key) = node.key else {
            return false;
        };
        let amount = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * ActionButton::H,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        let (_, offset) = node.scroll.unwrap();
        self.offsets.insert(key, (offset - amount).max(0.0));
        self.dirty = true;
        true
    }
    fn occurrence(&self, key: el::Key, window: WindowSlot, view: ViewSlot) -> Occurrence {
        match key.object {
            el::Object::Model(id) => Occurrence::model(
                window,
                view,
                uuid::Uuid::from_u128(id),
                ControlPart::new("composition"),
            ),
            _ => Occurrence::instance(
                window,
                view,
                "mini-app",
                self.surface,
                ControlPart::new(key.part),
            ),
        }
    }
}

fn control(name: &'static str, key: el::Key, label: &str) -> el::El {
    el::row(name)
        .id(key)
        .h(Metric::new("KoruButton height", ActionButton::H))
        .hit()
        .focusable()
        .semantics(el::Role::Button)
        .name_text(label)
        .action(el::Action::Click)
        .action(el::Action::Focus)
}

impl MiniApp {
    pub(super) fn composition_down(&mut self, x: f32, y: f32) -> bool {
        let Some(surface) = &mut self.composition else {
            return false;
        };
        let key = surface.frame.hit_at(x, y);
        surface.focused = key;
        if let Some(key) = key {
            self.composition_activate(key);
            return true;
        }
        false
    }
    fn composition_activate(&mut self, key: el::Key) {
        if key == SAVE {
            self.request_artifact_save();
        } else if key == PIN {
            self.request_pin();
        } else if let Some(document) = self.composition.as_ref().and_then(|s| s.document_for(key)) {
            if self.document != Some(document) || self.pending.is_some() {
                self.request_document(document, None);
            }
        }
    }
    pub(super) fn composition_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        let Some(surface) = &mut self.composition else {
            return false;
        };
        let Some(focused) = surface.focused else {
            return false;
        };
        match key {
            Key::Named(NamedKey::Tab)
                if !mods.control_key() && !mods.alt_key() && !mods.super_key() =>
            {
                let controls = surface.controls();
                let index = controls.iter().position(|k| *k == focused).unwrap_or(0);
                let next = if mods.shift_key() {
                    index.checked_sub(1)
                } else {
                    Some(index + 1)
                };
                surface.focused = next.and_then(|i| controls.get(i).copied());
                if let Some(key) = surface.focused {
                    surface.reveal(key);
                    self.layout();
                }
            }
            Key::Named(NamedKey::Escape) => surface.focused = None,
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.composition_activate(focused),
            _ => {}
        }
        true
    }
    pub(super) fn composition_semantics(
        &self,
        window: WindowSlot,
        view: ViewSlot,
        pane: Rect,
    ) -> Vec<SemanticNode> {
        let surface = self.composition.as_ref().unwrap();
        surface
            .frame
            .nodes
            .iter()
            .filter(|n| n.focusable)
            .filter_map(|node| {
                let key = node.key?;
                let label = if key == SAVE {
                    self.save_label().to_owned()
                } else if key == PIN {
                    self.pin_label().to_owned()
                } else {
                    match &node.label {
                        Some(el::Name::Text(text)) => text.clone(),
                        _ => return None,
                    }
                };
                let rect = node.hit_region().unwrap_or(node.rect);
                Some(
                    SemanticNode::new(
                        surface.occurrence(key, window, view),
                        accesskit::Role::Button,
                    )
                    .with_label(label)
                    .with_bounds(Rect::new(pane.x + rect.x, pane.y + rect.y, rect.w, rect.h))
                    .with_actions([accesskit::Action::Click, accesskit::Action::Focus])
                    .with_selected(node.selected.unwrap_or(false)),
                )
            })
            .collect()
    }
    pub(super) fn composition_accessibility_focus(
        &self,
        window: WindowSlot,
        view: ViewSlot,
    ) -> Option<Occurrence> {
        let surface = self.composition.as_ref()?;
        surface
            .focused
            .map(|key| surface.occurrence(key, window, view))
    }
    pub(super) fn composition_accessibility_action(
        &mut self,
        target: Occurrence,
        action: accesskit::Action,
    ) -> bool {
        if !matches!(action, accesskit::Action::Click | accesskit::Action::Focus) {
            return false;
        }
        let Some(surface) = &mut self.composition else {
            return false;
        };
        let key = surface
            .controls()
            .into_iter()
            .find(|key| surface.occurrence(*key, target.window, target.view) == target);
        let Some(key) = key else {
            return false;
        };
        surface.focused = Some(key);
        surface.reveal(key);
        self.layout();
        if action == accesskit::Action::Click {
            self.composition_activate(key);
        }
        true
    }
    pub(super) fn draw_composition(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        let save = self.save_label();
        let pin = self.pin_label();
        let surface = self.composition.as_mut().unwrap();
        let document_key = surface.key(surface.declaration.composition().unwrap().document_host());
        let labels: Vec<_> = surface
            .declaration
            .composition()
            .unwrap()
            .nodes()
            .iter()
            .filter_map(|node| {
                if let model::NodeKind::Button { label, .. } = &node.kind {
                    Some((surface.key(&node.id), label.clone()))
                } else {
                    None
                }
            })
            .collect();
        let focused = surface.focused;
        surface.frame.paint_with_host(
            &surface.tree,
            scene,
            root,
            &mut surface.engine,
            |node, scene, root, engine| {
                let Some(key) = node.key else {
                    return;
                };
                let label = if key == SAVE {
                    Some(save)
                } else if key == PIN {
                    Some(pin)
                } else {
                    labels
                        .iter()
                        .find(|(id, _)| *id == key)
                        .map(|(_, label)| label.as_str())
                };
                if let Some(label) = label {
                    ActionButton::new(
                        label,
                        if node.selected == Some(true) {
                            ActionStyle::Primary
                        } else if key == SAVE || key == PIN {
                            ActionStyle::Ghost
                        } else {
                            ActionStyle::Secondary
                        },
                        None,
                    )
                    .draw_focused_elided(
                        scene,
                        root,
                        engine,
                        node.rect,
                        node.hit_region()
                            .is_some_and(|r| r.contains(pointer.0, pointer.1)),
                        focused == Some(key),
                    );
                } else if key == document_key && node.rect.w > 0.0 && node.rect.h > 0.0 {
                    let r = node.rect;
                    scene.push_clip_layer(
                        vello::peniko::Fill::NonZero,
                        root,
                        &scene::rect(r.x, r.y, r.w, r.h),
                    );
                    if let Some(leaf) = &mut self.leaf {
                        View::draw(
                            leaf,
                            scene,
                            workspace,
                            images,
                            root * Affine::translate((r.x as f64, r.y as f64)),
                            caret_on && focused.is_none(),
                            (pointer.0 - r.x, pointer.1 - r.y),
                        );
                    }
                    scene.pop_layer();
                }
            },
        );
    }
}

fn spacing(space: model::Space) -> Space {
    match space {
        model::Space::S0 => Space::S0,
        model::Space::S0_5 => Space::S0_5,
        model::Space::S1 => Space::S1,
        model::Space::S1_5 => Space::S1_5,
        model::Space::S2 => Space::S2,
        model::Space::S2_5 => Space::S2_5,
        model::Space::S3 => Space::S3,
        model::Space::S3_5 => Space::S3_5,
        model::Space::S4 => Space::S4,
        model::Space::S5 => Space::S5,
        model::Space::S6 => Space::S6,
        model::Space::S7 => Space::S7,
        model::Space::S8 => Space::S8,
        model::Space::S10 => Space::S10,
        model::Space::S12 => Space::S12,
    }
}
fn text_role(role: model::TextRole) -> theme::TextRole {
    match role {
        model::TextRole::Ui => theme::roles::UI,
        model::TextRole::Row => theme::roles::ROW,
        model::TextRole::Hint => theme::roles::HINT,
        model::TextRole::Meta => theme::roles::META,
        model::TextRole::PopoverHeading => theme::roles::POPOVER_HEADING,
    }
}
fn ink_role(ink: model::Ink) -> theme::Ink {
    match ink {
        model::Ink::Primary => theme::Ink::Primary,
        model::Ink::Secondary => theme::Ink::Secondary,
        model::Ink::Hint => theme::Ink::Hint,
        model::Ink::Accent => theme::Ink::Accent,
    }
}
fn radius_role(radius: model::Radius) -> theme::Radius {
    match radius {
        model::Radius::None => theme::Radius::None,
        model::Radius::S => theme::Radius::S,
        model::Radius::M => theme::Radius::M,
        model::Radius::L => theme::Radius::L,
        model::Radius::XL => theme::Radius::XL,
        model::Radius::XXL => theme::Radius::XXL,
        model::Radius::Full => theme::Radius::Full,
    }
}

#[cfg(test)]
mod tests;
