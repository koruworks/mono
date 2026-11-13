// SPDX-License-Identifier: MPL-2.0

//! Production controls hosted under the composite's element tree.

use super::*;
use crate::{panes::explore::rail::Bucket, tasks_view::ListFace};
use accesskit::{Action, Role};
use koru_draw::{el, text::TextEngine, theme::Space};
use koru_ui::{ActionButton, ActionStyle, Dropdown, InputStyle, Menu, MenuItem, TextInput};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, SemanticNode, ViewSlot, WindowSlot,
};
use std::{
    borrow::Cow,
    time::{Duration, Instant},
};
use winit::keyboard::NamedKey;

#[derive(Clone)]
pub(super) enum Choice {
    Query(RailTarget, String),
    Document(DocumentId, String),
    Page(usize, String),
    Status(String),
}
impl MenuItem for Choice {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Self::Query(_, label)
            | Self::Document(_, label)
            | Self::Page(_, label)
            | Self::Status(label) => Cow::Borrowed(label),
        }
    }
}

pub(super) struct Chrome {
    pub engine: TextEngine,
    pub input: TextInput,
    pub search_focus: bool,
    pub header_focus: Option<&'static str>,
    pub menu: Option<Menu<Choice>>,
    menu_choices: Vec<Choice>,
    selector_menu: bool,
    viewport: (f32, f32),
    menu_instance: u64,
    lists: Vec<ListFace>,
    reader: Option<koru_search_ui::EntitySearch>,
    query: String,
    seq: u64,
    deadline: Option<Instant>,
    dismissed: bool,
    list_page: usize,
    pub selector: Rect,
    pub search: Rect,
    tasks_switch: Rect,
    document_switch: Rect,
    pub tree: el::El,
    pub frame: el::Frame,
    cache: el::TextCache,
}

fn label(key: koru_i18n::Key) -> &'static str {
    koru_i18n::t(crate::format::lang(), key)
}
fn bucket_label(bucket: Bucket) -> &'static str {
    use koru_i18n::Key::*;
    label(match bucket {
        Bucket::Today => A11yTaskBucketToday,
        Bucket::Next => A11yTaskBucketNext,
        Bucket::Upcoming => A11yTaskBucketUpcoming,
        Bucket::Later => A11yTaskBucketLater,
        Bucket::Completed => A11yTaskBucketCompleted,
    })
}

impl Chrome {
    pub fn new() -> Self {
        let mut engine = TextEngine::new();
        let input = TextInput::new(&mut engine, theme::role::row());
        let tree = el::col("Task document surface");
        let mut cache = el::TextCache::default();
        let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
        let frame = tree.layout(&mut engine, &mut cache, zero, el::FrameCx::root());
        Self {
            engine,
            input,
            search_focus: false,
            header_focus: None,
            menu: None,
            menu_choices: vec![],
            selector_menu: false,
            viewport: (0.0, 0.0),
            menu_instance: 0,
            lists: vec![],
            reader: None,
            query: String::new(),
            seq: 0,
            deadline: None,
            dismissed: false,
            list_page: 0,
            selector: zero,
            search: zero,
            tasks_switch: zero,
            document_switch: zero,
            tree,
            frame,
            cache,
        }
    }

    pub fn set_lists(&mut self, lists: &[ListFace]) {
        if self.lists != lists {
            self.lists = lists.to_vec();
            self.list_page = 0;
            if self.selector_menu {
                self.menu = None;
            }
        }
    }

    pub fn layout(&mut self, viewport: (f32, f32), swapped: bool, focus: Region) -> Regions {
        let resized = self.viewport != viewport;
        self.viewport = viewport;
        let narrow = viewport.0 < 2.0 * theme::CONTENT_MIN_W;
        let header = |name| el::row(name).h(theme::Metric::new("KoruDropdown height", Dropdown::H));
        let task = el::col("Task region")
            .fill()
            .child(header("Task selector").pad_x(Space::S2))
            .child(el::col("Task child").fill());
        let doc = el::col("Document region")
            .fill()
            .child(header("Document search").pad_x(Space::S2))
            .child(el::col("Document child").fill());
        let body = if narrow {
            el::row("Regions")
                .fill()
                .child(if focus == Region::Tasks { task } else { doc })
        } else {
            el::row("Regions").fill().children(if swapped {
                vec![doc, task]
            } else {
                vec![task, doc]
            })
        };
        let mut tree = el::col("Task document surface");
        if narrow {
            tree = tree.child(header("Region switch").children([
                el::row("Show tasks").fill(),
                el::row("Show document").fill(),
            ]));
        }
        tree = tree.child(body);
        let frame = tree.layout(
            &mut self.engine,
            &mut self.cache,
            Rect::new(0.0, 0.0, viewport.0.max(0.0), viewport.1.max(0.0)),
            el::FrameCx::root(),
        );
        let zero = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.selector = frame.rect_of("Task selector").unwrap_or(zero);
        self.search = frame.rect_of("Document search").unwrap_or(zero);
        self.tasks_switch = frame.rect_of("Show tasks").unwrap_or(zero);
        self.document_switch = frame.rect_of("Show document").unwrap_or(zero);
        let regions = Regions {
            tasks: frame.rect_of("Task child").unwrap_or(zero),
            document: frame.rect_of("Document child").unwrap_or(zero),
        };
        self.frame = frame;
        self.tree = tree;
        if resized && self.menu.is_some() {
            self.show_page(self.list_page);
        }
        regions
    }

    fn query_label(&self, target: RailTarget) -> String {
        match target {
            RailTarget::Bucket(bucket) => bucket_label(bucket).into(),
            RailTarget::List(id) => self
                .lists
                .iter()
                .find(|list| list.id == id)
                .map(|list| list.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn open_selector(&mut self, target: RailTarget) {
        let mut choices: Vec<_> = [
            Bucket::Today,
            Bucket::Next,
            Bucket::Upcoming,
            Bucket::Later,
            Bucket::Completed,
        ]
        .into_iter()
        .map(|bucket| Choice::Query(RailTarget::Bucket(bucket), bucket_label(bucket).into()))
        .collect();
        choices.extend(
            self.lists
                .iter()
                .map(|list| Choice::Query(RailTarget::List(list.id), list.name.clone())),
        );
        self.menu_choices = choices;
        self.selector_menu = true;
        let selected = self
            .menu_choices
            .iter()
            .position(|choice| matches!(choice, Choice::Query(value, _) if *value == target))
            .unwrap_or(0);
        self.show_page(selected / self.page_size());
        if let Some(menu) = &mut self.menu {
            menu.hover = menu
                .items()
                .iter()
                .position(|choice| matches!(choice, Choice::Query(value, _) if *value == target));
        }
        self.search_focus = false;
    }

    fn page_size(&self) -> usize {
        // Two navigation rows remain reachable on every page. The shell's
        // minimum window height fits at least three normal menu rows.
        (((self.viewport.1 - 2.0 * (koru_ui::menu::PAD + koru_ui::menu::EDGE_MARGIN))
            / theme::ROW_H)
            .floor() as usize)
            .saturating_sub(2)
            .clamp(1, 10)
    }

    fn show_page(&mut self, page: usize) {
        let size = self.page_size();
        let page = page.min(self.menu_choices.len().saturating_sub(1) / size);
        self.list_page = page;
        let mut choices = Vec::new();
        if page > 0 {
            choices.push(Choice::Page(
                page - 1,
                label(if self.selector_menu {
                    koru_i18n::Key::MiniAppPreviousLists
                } else {
                    koru_i18n::Key::MiniAppPreviousResults
                })
                .into(),
            ));
        }
        choices.extend(
            self.menu_choices
                .iter()
                .skip(page * size)
                .take(size)
                .cloned(),
        );
        if (page + 1) * size < self.menu_choices.len() {
            choices.push(Choice::Page(
                page + 1,
                label(if self.selector_menu {
                    koru_i18n::Key::MiniAppNextLists
                } else {
                    koru_i18n::Key::MiniAppNextResults
                })
                .into(),
            ));
        }
        let anchor = if self.selector_menu {
            self.selector
        } else {
            self.search
        };
        self.menu_instance = menu_instance(choices.len());
        self.menu = Some(Menu::new(
            &mut self.engine,
            (anchor.x, anchor.y + anchor.h),
            choices,
        ));
    }

    fn show_search_choices(&mut self, choices: Vec<Choice>) {
        self.menu_choices = choices;
        self.selector_menu = false;
        self.show_page(0);
    }

    pub fn changed_query(&mut self, workspace: WorkspaceId) {
        let text = self.input.raw_text();
        if self.query == text {
            return;
        }
        self.query = text.to_owned();
        self.dismissed = false;
        self.menu = None;
        if text.trim().is_empty() || text.len() > 1024 {
            self.dismissed = true;
            return;
        }
        let reader = self.reader.get_or_insert_with(|| {
            koru_search_ui::EntitySearch::new_at(crate::search::gateway_root())
        });
        self.seq = reader.request(
            format!("doc:{text}"),
            vec![koru_store_core::ids::WorkspaceId::from_uuid(
                workspace.as_uuid(),
            )],
        );
        self.deadline = Some(Instant::now() + Duration::from_millis(40));
        self.show_search_choices(vec![Choice::Status(
            label(koru_i18n::Key::MiniAppSearching).into(),
        )]);
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    pub fn poll(&mut self, workspace: WorkspaceId, now: Instant) -> bool {
        if !self.deadline.is_some_and(|due| now >= due) {
            return false;
        }
        let Some(reader) = &mut self.reader else {
            self.deadline = None;
            return false;
        };
        let result = reader.poll();
        self.deadline = reader.pending().then_some(now + Duration::from_millis(40));
        let Some(result) = result else {
            return false;
        };
        self.accept_result(workspace, result)
    }

    fn accept_result(
        &mut self,
        workspace: WorkspaceId,
        result: koru_search_ui::EntitySearchUpdate,
    ) -> bool {
        if result.seq < self.seq
            || self.dismissed
            || !self.search_focus
            || self.query.trim().is_empty()
        {
            return false;
        }
        let mut choices = result
            .documents
            .into_iter()
            .filter(|hit| {
                hit.workspace.as_uuid() == workspace.as_uuid()
                    && hit.kind == "document"
                    && hit.node.is_none()
            })
            .take(20)
            .map(|hit| Choice::Document(DocumentId::from_uuid(hit.document.as_uuid()), hit.title))
            .collect::<Vec<_>>();
        if choices.is_empty() {
            choices.push(Choice::Status(
                label(if result.unavailable {
                    koru_i18n::Key::MiniAppSearchUnavailable
                } else {
                    koru_i18n::Key::MiniAppNoResults
                })
                .into(),
            ));
        }
        self.show_search_choices(choices);
        true
    }
}

fn menu_instance(rows: usize) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(rows as u64 + 1, std::sync::atomic::Ordering::Relaxed)
}

impl MiniApp {
    pub fn header_focused(&self) -> bool {
        self.chrome.header_focus.is_some()
    }
    pub fn blur_header(&mut self) {
        self.chrome.header_focus = None;
        self.chrome.search_focus = false;
        self.chrome.menu = None;
    }
    pub fn focus_header(&mut self) {
        self.set_header(if self.chrome.selector.w > 0.0 {
            "selector"
        } else {
            "search"
        });
    }
    fn set_header(&mut self, part: &'static str) {
        self.chrome.header_focus = Some(part);
        self.chrome.search_focus = part == "search";
        self.chrome.menu = None;
    }
    fn header_controls(&self) -> Vec<(&'static str, Rect)> {
        [
            ("tasks", self.chrome.tasks_switch),
            ("document", self.chrome.document_switch),
            ("selector", self.chrome.selector),
            ("search", self.chrome.search),
        ]
        .into_iter()
        .filter(|(_, rect)| rect.w > 0.0)
        .collect()
    }
    pub fn tasks_focused(&self) -> bool {
        self.focus == Region::Tasks && self.chrome.menu.is_none() && !self.header_focused()
    }
    pub fn revise_accessibility(&self, rev: &mut Revision) {
        rev.u64(self.surface)
            .bool(self.chrome.search_focus)
            .bool(self.focus == Region::Tasks);
        rev.str(self.chrome.header_focus.unwrap_or(""));
        for rect in [self.regions.tasks, self.regions.document] {
            rev.f32(rect.x).f32(rect.y).f32(rect.w).f32(rect.h);
        }
        if let Some(document) = self.document {
            rev.uuid(document.as_uuid());
        }
        rev.str(self.chrome.input.raw_text());
        rev.u64(
            self.chrome
                .menu
                .as_ref()
                .map_or(0, |_| self.chrome.menu_instance),
        );
        rev.u64(
            self.chrome
                .menu
                .as_ref()
                .and_then(|menu| menu.hover)
                .map_or(0, |i| i as u64 + 1),
        );
    }

    pub fn chrome_semantics(
        &self,
        window: WindowSlot,
        view: ViewSlot,
        pane: Rect,
    ) -> Vec<SemanticNode> {
        let occurrence = |part| {
            Occurrence::instance(
                window,
                view,
                "mini-app",
                self.surface,
                ControlPart::new(part),
            )
        };
        let moved = |rect: Rect| Rect::new(pane.x + rect.x, pane.y + rect.y, rect.w, rect.h);
        let mut nodes = Vec::new();
        for (part, rect, name, role) in [
            (
                "selector",
                self.chrome.selector,
                self.chrome.query_label(self.tasks.target()),
                Role::ComboBox,
            ),
            (
                "search",
                self.chrome.search,
                label(koru_i18n::Key::MiniAppSearch).into(),
                Role::TextInput,
            ),
            (
                "tasks",
                self.chrome.tasks_switch,
                label(koru_i18n::Key::A11yTasksHeading).into(),
                Role::Button,
            ),
            (
                "document",
                self.chrome.document_switch,
                label(koru_i18n::Key::CloudTabLabelDocument).into(),
                Role::Button,
            ),
        ] {
            if rect.w <= 0.0 {
                continue;
            }
            let mut node = SemanticNode::new(occurrence(part), role)
                .with_label(name)
                .with_bounds(moved(rect))
                .with_actions([Action::Click, Action::Focus]);
            if part == "search" {
                node = node.with_value(self.chrome.input.raw_text());
            }
            nodes.push(node);
        }
        if let Some(menu) = &self.chrome.menu {
            let children = menu
                .items()
                .iter()
                .enumerate()
                .map(|(i, choice)| {
                    let node = SemanticNode::new(
                        Occurrence::instance(
                            window,
                            view,
                            "mini-app-menu",
                            self.chrome.menu_instance + 1 + i as u64,
                            ControlPart::new("item"),
                        ),
                        if matches!(choice, Choice::Status(_)) {
                            Role::Label
                        } else {
                            Role::MenuItem
                        },
                    )
                    .with_label(choice.label().into_owned())
                    .with_bounds(moved(menu.item_rect(self.viewport, i)));
                    if matches!(choice, Choice::Status(_)) {
                        node
                    } else {
                        node.with_actions([Action::Click, Action::Focus])
                            .with_selected(menu.hover == Some(i))
                    }
                })
                .collect();
            nodes.push(
                SemanticNode::new(
                    Occurrence::instance(
                        window,
                        view,
                        "mini-app-menu",
                        self.chrome.menu_instance,
                        ControlPart::new("root"),
                    ),
                    Role::Menu,
                )
                .with_bounds(moved(menu.panel(self.viewport)))
                .with_children(children),
            );
        }
        nodes
    }

    pub fn chrome_accessibility_focus(
        &self,
        window: WindowSlot,
        view: ViewSlot,
    ) -> Option<Occurrence> {
        if let Some(menu) = &self.chrome.menu {
            return Some(Occurrence::instance(
                window,
                view,
                "mini-app-menu",
                self.chrome.menu_instance + menu.hover.map_or(0, |i| i as u64 + 1),
                ControlPart::new(if menu.hover.is_some() { "item" } else { "root" }),
            ));
        }
        self.chrome
            .header_focus
            .or(self.chrome.search_focus.then_some("search"))
            .map(|part| {
                Occurrence::instance(
                    window,
                    view,
                    "mini-app",
                    self.surface,
                    ControlPart::new(part),
                )
            })
    }

    pub fn chrome_accessibility_action(&mut self, target: Occurrence, action: Action) -> bool {
        if !matches!(action, Action::Click | Action::Focus) {
            return false;
        }
        if let ObjectKey::Instance {
            kind: "mini-app-menu",
            id,
        } = target.object
        {
            let Some(index) = id
                .checked_sub(self.chrome.menu_instance + 1)
                .and_then(|i| usize::try_from(i).ok())
            else {
                return false;
            };
            let Some(menu) = &mut self.chrome.menu else {
                return false;
            };
            let Some(choice) = menu.items().get(index).cloned() else {
                return false;
            };
            if matches!(choice, Choice::Status(_)) {
                return false;
            }
            if action == Action::Focus {
                menu.hover = Some(index);
            } else {
                self.choose(choice);
            }
            return true;
        }
        if target.object
            != (ObjectKey::Instance {
                kind: "mini-app",
                id: self.surface,
            })
        {
            return false;
        }
        if action == Action::Focus {
            let Some((part, _)) = self
                .header_controls()
                .into_iter()
                .find(|(part, _)| target.part == ControlPart::new(part))
            else {
                return false;
            };
            self.set_header(part);
            return true;
        }
        let rect = if target.part == ControlPart::new("selector") {
            self.chrome.selector
        } else if target.part == ControlPart::new("search") {
            self.chrome.search
        } else if target.part == ControlPart::new("tasks") {
            self.chrome.tasks_switch
        } else if target.part == ControlPart::new("document") {
            self.chrome.document_switch
        } else {
            return false;
        };
        rect.w > 0.0 && self.chrome_down(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
    }
    pub fn set_lists(&mut self, lists: &[ListFace]) {
        self.chrome.set_lists(lists);
    }

    fn choose(&mut self, choice: Choice) {
        self.chrome.menu = None;
        match choice {
            Choice::Query(target, _) => {
                self.tasks.set_target(target);
                self.focus_region(Region::Tasks);
            }
            Choice::Document(document, _) => {
                self.blur_header();
                self.request_document(document, None);
            }
            Choice::Page(page, _) => {
                self.chrome.show_page(page);
            }
            Choice::Status(_) => {}
        }
    }

    pub(super) fn chrome_down(&mut self, x: f32, y: f32) -> bool {
        if let Some(menu) = &self.chrome.menu {
            let choice = menu
                .item_at(self.viewport, x, y)
                .and_then(|index| menu.items().get(index))
                .cloned();
            self.chrome.menu = None;
            if let Some(choice) = choice {
                self.choose(choice);
                return true;
            }
        }
        for (rect, region) in [
            (self.chrome.tasks_switch, Region::Tasks),
            (self.chrome.document_switch, Region::Document),
        ] {
            if rect.w > 0.0 && rect.contains(x, y) {
                self.focus_region(region);
                return true;
            }
        }
        if self.chrome.selector.w > 0.0 && self.chrome.selector.contains(x, y) {
            self.chrome.header_focus = Some("selector");
            self.chrome.open_selector(self.tasks.target());
            return true;
        }
        if self.chrome.search.w > 0.0 && self.chrome.search.contains(x, y) {
            self.focus = Region::Document;
            self.chrome.search_focus = true;
            self.chrome.header_focus = Some("search");
            self.chrome
                .input
                .click(&mut self.chrome.engine, self.chrome.search, x, y);
            return true;
        }
        self.chrome.search_focus = false;
        self.chrome.header_focus = None;
        false
    }

    pub(super) fn chrome_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.header_focused()
            && *key == Key::Named(NamedKey::Tab)
            && !mods.control_key()
            && !mods.super_key()
            && !mods.alt_key()
        {
            let controls = self.header_controls();
            let index = controls
                .iter()
                .position(|(part, _)| Some(*part) == self.chrome.header_focus)
                .unwrap_or(0);
            let next = if mods.shift_key() {
                index.checked_sub(1)
            } else {
                (index + 1 < controls.len()).then_some(index + 1)
            };
            if let Some(next) = next {
                self.set_header(controls[next].0);
            } else {
                self.blur_header();
            }
            return true;
        }
        if *key == Key::Named(NamedKey::Escape)
            && (self.chrome.search_focus || self.chrome.menu.is_some())
        {
            if self.chrome.menu.is_none() {
                self.blur_header();
            } else {
                self.chrome.menu = None;
            }
            self.chrome.dismissed = true;
            return true;
        }
        if let Some(menu) = &mut self.chrome.menu {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.chrome.menu = None;
                    return true;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                    let enabled = menu
                        .items()
                        .iter()
                        .map(|choice| !matches!(choice, Choice::Status(_)))
                        .collect::<Vec<_>>();
                    menu.step(
                        &enabled,
                        if *key == Key::Named(NamedKey::ArrowDown) {
                            1
                        } else {
                            -1
                        },
                    );
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(choice) = menu.hover.and_then(|i| menu.items().get(i)).cloned() {
                        self.choose(choice);
                    }
                    return true;
                }
                _ => {}
            }
        }
        if self.chrome.search_focus {
            self.chrome
                .input
                .key(&mut self.chrome.engine, key, text, mods);
            self.chrome.changed_query(self.workspace);
            return true;
        }
        if let Some(part) = self.chrome.header_focus {
            if *key == Key::Named(NamedKey::Escape) {
                self.blur_header();
                return true;
            }
            if *key == Key::Named(NamedKey::Enter) || *key == Key::Named(NamedKey::Space) {
                if let Some((_, rect)) = self
                    .header_controls()
                    .into_iter()
                    .find(|(name, _)| *name == part)
                {
                    return self.chrome_down(rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
                }
            }
            return true;
        }
        false
    }

    pub(super) fn draw_surface(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        let target_label = self.chrome.query_label(self.tasks.target());
        let chrome = &mut self.chrome;
        let regions = self.regions;
        chrome
            .frame
            .paint_host(&chrome.tree, scene, root, |node, scene, root| {
                match node.name {
                    "Task selector" => Dropdown::new(&target_label)
                        .open(chrome.menu.is_some() && chrome.selector_menu)
                        .draw(
                            scene,
                            root,
                            &mut chrome.engine,
                            node.rect,
                            node.rect.contains(pointer.0, pointer.1),
                            1.0,
                        ),
                    "Document search" => chrome.input.draw(
                        scene,
                        root,
                        &mut chrome.engine,
                        node.rect,
                        InputStyle::search(),
                        Some(label(koru_i18n::Key::MiniAppSearch)),
                        chrome.search_focus,
                    ),
                    "Show tasks" | "Show document" => {
                        let key = if node.name == "Show tasks" {
                            koru_i18n::Key::A11yTasksHeading
                        } else {
                            koru_i18n::Key::CloudTabLabelDocument
                        };
                        ActionButton::new(label(key), ActionStyle::Ghost, None).draw(
                            scene,
                            root,
                            &mut chrome.engine,
                            node.rect,
                            node.rect.contains(pointer.0, pointer.1),
                        );
                    }
                    "Task child" | "Document child" => {
                        let region = if node.name == "Task child" {
                            Region::Tasks
                        } else {
                            Region::Document
                        };
                        let rect = node.rect;
                        let transform = regions.transform(region, root);
                        let pointer = regions.local(region, pointer.0, pointer.1);
                        scene.push_clip_layer(
                            vello::peniko::Fill::NonZero,
                            root,
                            &scene::rect(rect.x, rect.y, rect.w, rect.h),
                        );
                        match region {
                            Region::Tasks => View::draw(
                                &mut self.tasks,
                                scene,
                                workspace,
                                images,
                                transform,
                                caret_on && self.focus == region,
                                pointer,
                            ),
                            Region::Document => {
                                if let Some(leaf) = &mut self.leaf {
                                    View::draw(
                                        leaf,
                                        scene,
                                        workspace,
                                        images,
                                        transform,
                                        caret_on && self.focus == region && !chrome.search_focus,
                                        pointer,
                                    );
                                } else {
                                    koru_ui::text_paragraph::KoruTextParagraph::new(
                                        label(koru_i18n::Key::MiniAppEmpty),
                                        theme::role::row(),
                                        "theme::role::row()",
                                        theme::p().text,
                                    )
                                    .draw(
                                        scene,
                                        root,
                                        &mut chrome.engine,
                                        rect,
                                    );
                                }
                            }
                        }
                        scene.pop_layer();
                    }
                    _ => {}
                }
                let focused = match node.name {
                    "Task selector" => Some("selector"),
                    "Show tasks" => Some("tasks"),
                    "Show document" => Some("document"),
                    _ => None,
                };
                if focused.is_some() && focused == chrome.header_focus {
                    scene::stroke(
                        scene,
                        root,
                        theme::p().focus_ring,
                        theme::BORDER_W,
                        &scene::rect(node.rect.x, node.rect.y, node.rect.w, node.rect.h),
                    );
                }
            });
        if let Some(menu) = &chrome.menu {
            menu.draw(
                scene,
                root,
                &mut chrome.engine,
                self.viewport,
                &menu
                    .items()
                    .iter()
                    .map(|choice| !matches!(choice, Choice::Status(_)))
                    .collect::<Vec<_>>(),
            );
        }
    }
}

#[cfg(test)]
mod tests;
