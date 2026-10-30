// SPDX-License-Identifier: MPL-2.0

//! **The two switcher pages, written down as they draw today** — P0 of
//! `docs/contributing/refactor-ui/ELEMENTS-PLAN.final.md`.
//!
//! One text file per page beside this one, a section per case and a line per
//! element: the panel, every row and its parts, what the keyboard is on, the
//! accessibility tree, and every paint the inspector records under the panel.
//! The element migration of the two pages is held to it.
//!
//! Only a developer writes it, and only by name:
//!
//! ```text
//! cargo test -p koru capture_popover_baseline -- --ignored
//! ```
//!
//! An ordinary `cargo test` compares and never writes, so a difference is
//! never re-blessed by accident; a changed baseline is a reviewed diff.
//!
//! Numbers compare within [`TOLERANCE`] logical units. Text set in a face the
//! repository does not bundle — the Arabic cases fall back to the system's —
//! is written `~`, because its advances belong to the machine, not to Koru.
//! Scale 1.5 is not written at all: [`scale_does_not_move_anything`] holds it
//! to the 1.0 record, which is the claim worth making about it.

use std::fmt::Write as _;
use std::time::Duration;

use koru_core::{ProfileId, WorkspaceId};
use koru_draw::inspect::{Record, Snapshot};
use koru_i18n::Lang;
use koru_workspace_host::accessibility::{SemanticNode, ViewSlot, WindowSlot};
use vello::kurbo::Affine;

use super::accessibility::Context;
use super::*;

/// Logical units. A difference smaller than this is the arithmetic's, not
/// the layout's.
const TOLERANCE: f64 = 0.01;

/// The window the popover is clamped inside.
const VIEW: (f32, f32) = (1200.0, 700.0);

const WORKSPACES: &str = include_str!("popover_baseline/workspaces.txt");
const PROFILES: &str = include_str!("popover_baseline/profiles.txt");
/// What the plan approved the Workspaces page to change from P0 (D2, S1),
/// held apart from the P0 record.
const WORKSPACES_APPROVED: &str = include_str!("popover_baseline/workspaces.approved.txt");
/// What the plan approved the Profiles page to change from P0 (D2, P5).
const PROFILES_APPROVED: &str = include_str!("popover_baseline/profiles.approved.txt");

/// The faces the repository ships; anything else a run lands in is the
/// machine's fallback.
fn bundled(family: &str) -> bool {
    family.starts_with("Inter") || family.starts_with("Phosphor")
}

// ------------------------------------------------------------------ fixtures

/// A model id that is the same on every run, so a row's key is its model's
/// and never its position.
fn uuid(n: u16) -> uuid::Uuid {
    uuid::Uuid::from_u128(0x6b6f_7275_0000_4000_8000_0000_0000_0000 | u128::from(n))
}

fn key(id: uuid::Uuid) -> String {
    format!("{:04}", id.as_u128() & 0xffff)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Names {
    Normal,
    Long,
    /// Forty workspaces or Profiles, some long: the stress case.
    Forty,
}

impl Names {
    fn token(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Long => "long",
            Self::Forty => "forty",
        }
    }
}

fn workspace(n: u16, name: &str) -> WorkspaceEntry {
    WorkspaceEntry::new(WorkspaceId::from_uuid(uuid(n)), name, 0)
}

fn profile(n: u16, name: &str, email: &str) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::from_uuid(uuid(n)),
        name: name.into(),
        email: email.into(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: format!("C:/profiles/{n}"),
    }
}

/// The pane as the switcher sees it: three Profiles, the middle one open; a
/// handful of workspaces, one pinned, one coloured, one with an icon, the
/// second one current.
fn fixture(names: Names) -> ExploreView {
    let mut v = ExploreView::new();
    let (profiles, workspaces) = match names {
        Names::Normal => (
            vec![
                profile(101, "koru", "koru@example.com"),
                profile(102, "dev-data", "dev@example.com"),
                profile(103, "space-b", "space-b@example.com"),
            ],
            vec![
                workspace(1, "Example"),
                workspace(2, "Client work"),
                workspace(3, "Reading list"),
                workspace(4, "Travel"),
            ],
        ),
        Names::Long => (
            vec![
                profile(
                    101,
                    "Alexandra Konstantinopoulou-Whitfield",
                    "alexandra.konstantinopoulou-whitfield@example-company.com",
                ),
                profile(102, "dev-data", "dev@example.com"),
                profile(
                    103,
                    "Research and development archive 2019–2026",
                    "archive@example.com",
                ),
            ],
            vec![
                workspace(1, "Quarterly planning for the European expansion programme"),
                workspace(2, "Client work for Müller & Söhne GmbH, Zürich office"),
                workspace(3, "Reading list"),
                workspace(4, "Travel"),
            ],
        ),
        Names::Forty => (
            (0..40)
                .map(|i| {
                    let name = if i % 7 == 3 {
                        format!("Profile {i:02} with a name long enough to be cut short")
                    } else {
                        format!("Profile {i:02}")
                    };
                    profile(101 + i, &name, &format!("p{i:02}@example.com"))
                })
                .collect(),
            (0..40)
                .map(|i| {
                    let name = if i % 7 == 3 {
                        format!("Workspace {i:02} with a name long enough to be cut short")
                    } else {
                        format!("Workspace {i:02}")
                    };
                    workspace(1 + i, &name)
                })
                .collect(),
        ),
    };
    let mut workspaces = workspaces;
    workspaces[0].pinned = true;
    workspaces[2].color = Some("blue".into());
    workspaces[3].icon = Some("🚀".into());
    let current_profile = profiles[1].id;
    let current = workspaces[1].id;
    v.set_context(profiles, current_profile, workspaces, current);
    v
}

// --------------------------------------------------------------------- cases

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Page {
    Workspaces,
    Profiles,
}

impl Page {
    fn token(self) -> &'static str {
        match self {
            Self::Workspaces => "workspaces",
            Self::Profiles => "profiles",
        }
    }
}

/// What the person is doing when the frame is taken.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum State {
    /// Just opened and settled: no slide, no fade, the keyboard where the
    /// page puts it.
    Rest,
    /// The pointer on a list row's words.
    HoverRow,
    /// The pointer on a list row's gear.
    HoverGear,
    /// Opened, then Down twice: the keyboard owns the lit row.
    Keyboard,
    /// The list scrolled to its end.
    ScrolledToEnd,
    /// The pointer on one of the foot's three items.
    Foot(HeadButton),
    /// The open Profile syncing to Koru Cloud, two other devices holding it.
    Synced,
}

impl State {
    fn token(self) -> String {
        match self {
            Self::Rest => "rest".into(),
            Self::HoverRow => "hover-row".into(),
            Self::HoverGear => "hover-gear".into(),
            Self::Keyboard => "keyboard".into(),
            Self::ScrolledToEnd => "scrolled-to-end".into(),
            Self::Foot(button) => format!("foot-{}", format!("{button:?}").to_lowercase()),
            Self::Synced => "synced".into(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Case {
    page: Page,
    /// The pane's width: 256 is both the sidebar's resting width and its
    /// floor, 600 its ceiling.
    width: f32,
    dark: bool,
    lang: Lang,
    names: Names,
    state: State,
}

impl Case {
    fn title(&self) -> String {
        format!(
            "== {} width={} theme={} lang={} names={} state={}",
            self.page.token(),
            self.width,
            if self.dark { "dark" } else { "light" },
            self.lang.tag(),
            self.names.token(),
            self.state.token()
        )
    }

    fn area(&self) -> Rect {
        Rect::new(theme::RAIL_W, 0.0, self.width, VIEW.1)
    }
}

/// Every case for one page: the full matrix at rest, then the states, each
/// at the default width, light, English and ordinary names.
fn cases(page: Page) -> Vec<Case> {
    let mut out = Vec::new();
    for width in [theme::SIDEBAR_MIN_W, theme::SIDEBAR_MAX_W] {
        for dark in [false, true] {
            for lang in [Lang::En, Lang::De, Lang::Ar] {
                for names in [Names::Normal, Names::Long] {
                    out.push(Case {
                        page,
                        width,
                        dark,
                        lang,
                        names,
                        state: State::Rest,
                    });
                }
            }
        }
    }
    let base = Case {
        page,
        width: theme::SIDEBAR_MIN_W,
        dark: false,
        lang: Lang::En,
        names: Names::Normal,
        state: State::Rest,
    };
    let mut states = vec![State::HoverRow, State::HoverGear, State::Keyboard];
    if page == Page::Profiles {
        states.extend([
            State::Foot(HeadButton::Status),
            State::Foot(HeadButton::Devices),
            State::Foot(HeadButton::New),
            State::Synced,
        ]);
    }
    out.extend(states.into_iter().map(|state| Case { state, ..base }));
    for state in [State::Rest, State::ScrolledToEnd] {
        out.push(Case {
            names: Names::Forty,
            state,
            ..base
        });
        out.push(Case {
            names: Names::Forty,
            state,
            dark: true,
            ..base
        });
    }
    out
}

/// The view, opened on `case`'s page and brought to `case`'s state.
fn arrange(case: &Case) -> ExploreView {
    let mut v = fixture(case.names);
    let area = case.area();
    match case.page {
        Page::Workspaces => v.toggle_popover(PopoverKind::Summary),
        Page::Profiles => v.toggle_profile_popover(),
    }
    v.advance_popover(Instant::now() + Duration::from_secs(5));
    let rects = v.popover_rects(area, VIEW);
    let list_row = |n: usize| {
        rects
            .iter()
            .filter(|(row, _)| matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_)))
            .nth(n)
            .map(|(_, r)| *r)
            .expect("a list row")
    };
    match case.state {
        State::Rest => {}
        State::HoverRow => {
            let r = list_row(2);
            v.mouse_move(area, VIEW, r.x + r.w * 0.4, r.y + r.h * 0.5);
        }
        State::HoverGear => {
            let lane = trailing_lane(list_row(2));
            v.mouse_move(area, VIEW, lane.x + lane.w * 0.5, lane.y + lane.h * 0.5);
        }
        State::Keyboard => {
            use winit::keyboard::{Key, NamedKey};
            for _ in 0..2 {
                v.popover_key(&Key::Named(NamedKey::ArrowDown));
            }
        }
        State::ScrolledToEnd => while v.scroll_popover(area, VIEW, -40.0) {},
        State::Foot(button) => {
            let b = v
                .popover_head_button_rect(area, VIEW, button)
                .expect("the foot's item");
            v.mouse_move(area, VIEW, b.x + b.w * 0.5, b.y + b.h * 0.5);
        }
        State::Synced => {
            let open = v.current_profile.expect("an open Profile");
            v.set_account_state(&koru_agent_wire::AccountState {
                stage: koru_agent_wire::AccountStage::SignedIn,
                cloud_profiles: vec![koru_agent_wire::AccountProfileCloud {
                    profile_id: open.to_string(),
                    stage: koru_agent_wire::CloudConnectionStage::Syncing,
                }],
                ..Default::default()
            });
            v.set_linked_count(2);
        }
    }
    v
}

// ------------------------------------------------------------------ the record

fn rect(r: Rect) -> String {
    format!("{:.3} {:.3} {:.3} {:.3}", r.x, r.y, r.w, r.h)
}

fn bounds(b: [f64; 4]) -> String {
    format!(
        "{:.3} {:.3} {:.3} {:.3}",
        b[0],
        b[1],
        b[2] - b[0],
        b[3] - b[1]
    )
}

/// A property value as one token: `#RRGGBB / alpha 1.000` → `#RRGGBB@1.000`.
fn token(value: &str) -> String {
    value.replace(" / alpha ", "@").replace(' ', "_")
}

/// The properties a record carries that describe what was painted. Debug
/// dumps of a face and the inspector's own disclaimers are left out.
fn painted(name: &str) -> bool {
    !matches!(
        name,
        "face attributes"
            | "variation coordinates"
            | "synthetic italic"
            | "tracking"
            | "font style"
    )
}

fn row_key(v: &ExploreView, row: PopoverRow) -> String {
    match row {
        PopoverRow::Workspace(i) => format!("ws:{}", key(v.workspaces[i].id.as_uuid())),
        PopoverRow::Profile(i) => format!("profile:{}", key(v.profiles[i].id.as_uuid())),
        PopoverRow::WorkspacesHeading => "heading".into(),
        PopoverRow::ProfilesHeading => "heading".into(),
        PopoverRow::ProfilesFooter => "foot".into(),
        PopoverRow::LibraryAction(action) => format!("action:{}", actions::token(action)),
    }
}

fn context(area: Rect, lang: Lang) -> Context {
    Context {
        window: WindowSlot::from_raw(1),
        view: ViewSlot::from_raw(1),
        area,
        rail: ExploreView::new().rail_of(area),
        viewport: VIEW,
        lang,
    }
}

/// The popover's node in the column's tree.
fn menu(tree: SemanticNode) -> Option<SemanticNode> {
    tree.children
        .into_iter()
        .find(|node| node.role == accesskit::Role::Menu)
}

fn semantic_lines(out: &mut String, node: &SemanticNode, depth: usize) {
    let mut line = format!(
        "sem {depth} {:?} {} \"{}\"",
        node.role,
        node.occurrence.locator(),
        node.label.as_deref().unwrap_or("")
    );
    if let Some(b) = node.bounds {
        let _ = write!(line, " {}", rect(b));
    }
    if let Some(position) = node.position {
        let _ = write!(line, " position={position}");
    }
    if let Some(size) = node.size_of_set {
        let _ = write!(line, " size={size}");
    }
    if let Some(selected) = node.selected {
        let _ = write!(line, " selected={selected}");
    }
    if node.disabled {
        line.push_str(" disabled");
    }
    if !node.actions.is_empty() {
        let actions: Vec<String> = node.actions.iter().map(|a| format!("{a:?}")).collect();
        let _ = write!(line, " actions={}", actions.join(","));
    }
    out.push_str(&line);
    out.push('\n');
    for child in &node.children {
        semantic_lines(out, child, depth + 1);
    }
}

/// Every record under the panel, indented by depth below it.
fn paint_lines(out: &mut String, snapshot: &Snapshot) {
    let records = &snapshot.records;
    let Some(panel) = records.iter().position(|r| r.name == "FloatingPanel") else {
        out.push_str("paint none\n");
        return;
    };
    let depth = |mut at: usize| {
        let mut d = 0;
        while at != panel {
            match records[at].parent {
                Some(parent) => {
                    at = parent;
                    d += 1;
                }
                None => return None,
            }
        }
        Some(d)
    };
    let system = |r: &Record| {
        r.properties
            .iter()
            .any(|p| p.name == "rendered family" && !bundled(&p.value))
    };
    // A run set in a fallback face is the machine's, and so is the line that
    // holds it; the row and the panel around them are still Koru's.
    let mut machine = vec![false; records.len()];
    for (i, r) in records.iter().enumerate() {
        if system(r) {
            machine[i] = true;
            if let Some(line) = r.parent
                && records[line].name.ends_with(" line")
            {
                machine[line] = true;
            }
        }
    }
    // Bidi reorders a mixed line, so its bundled runs sit wherever the
    // fallback runs' advances put them.
    for (i, r) in records.iter().enumerate() {
        if let Some(line) = r.parent
            && machine[line]
            && records[line].name.ends_with(" line")
        {
            machine[i] = true;
        }
    }
    for (i, r) in records.iter().enumerate() {
        let Some(d) = depth(i) else { continue };
        let at = if machine[i] {
            "~ ~ ~ ~".to_string()
        } else {
            bounds(r.bounds)
        };
        let mut line = format!("paint {d} {} {} {at}", token(r.name), r.kind);
        for p in r.properties.iter().filter(|p| painted(p.name)) {
            let value = if machine[i] && p.name != "font size" && p.name != "foreground" {
                "~".to_string()
            } else {
                token(&p.value)
            };
            let _ = write!(line, " {}={value}", token(p.name));
        }
        out.push_str(&line);
        out.push('\n');
    }
}

/// One frame of `v` at `case`, drawn under `scale`.
fn frame(v: &ExploreView, case: &Case, scale: f64) -> Snapshot {
    let root = Affine::scale(scale);
    let mut scene = vello::Scene::new();
    let mut engine = TextEngine::new();
    let capture =
        koru_draw::inspect::Frame::begin(1, root, koru_draw::scene::rect(0.0, 0.0, VIEW.0, VIEW.1));
    v.draw_popover(&mut scene, root, &mut engine, case.area(), VIEW, None);
    capture.finish()
}

/// `case`, written down. The caller holds the theme lock.
fn record(case: &Case) -> String {
    theme::set_dark(case.dark);
    koru_i18n::with_current(case.lang, || {
        let v = arrange(case);
        let area = case.area();
        let mut out = case.title();
        out.push('\n');
        let panel = v.popover_panel(area, VIEW).expect("an open panel");
        let _ = writeln!(out, "panel {}", rect(panel));
        if let Some(band) = v.popover_list_band(area, VIEW) {
            let _ = writeln!(out, "band {}", rect(band));
        }
        let cx = context(area, case.lang);
        let focus = v
            .accessible_focus(&cx)
            .map_or("none".into(), |o| o.locator());
        let _ = writeln!(out, "focus {focus}");
        for (row, r) in v.popover_rects(area, VIEW) {
            let row_key = row_key(&v, row);
            let _ = writeln!(out, "row {row_key} {}", rect(r));
            match row {
                PopoverRow::Workspace(_) => {
                    let parts = v.workspace_row_parts(row, r);
                    let _ = writeln!(out, "part {row_key} tile {}", rect(parts.chip));
                    let _ = writeln!(out, "part {row_key} name {}", rect(parts.name));
                    let _ = writeln!(out, "part {row_key} lane {}", rect(parts.trailing));
                }
                PopoverRow::Profile(_) => {
                    let _ = writeln!(out, "part {row_key} lane {}", rect(trailing_lane(r)));
                }
                PopoverRow::ProfilesFooter => {
                    for button in [HeadButton::Status, HeadButton::Devices, HeadButton::New] {
                        let b = v
                            .popover_head_button_rect(area, VIEW, button)
                            .expect("the foot's item");
                        let name = format!("{button:?}").to_lowercase();
                        let _ = writeln!(out, "part {row_key} {name} {}", rect(b));
                    }
                }
                _ => {}
            }
        }
        match menu(v.semantics(&cx)) {
            Some(node) => semantic_lines(&mut out, &node, 0),
            None => out.push_str("sem none\n"),
        }
        paint_lines(&mut out, &frame(&v, case, 1.0));
        out
    })
}

fn page_record(page: Page) -> String {
    let _theme = crate::tests::lock_theme();
    let was = theme::is_dark();
    let mut out = String::new();
    for case in cases(page) {
        out.push_str(&record(&case));
        out.push('\n');
    }
    theme::set_dark(was);
    out
}

// ------------------------------------------------------------------ comparing

fn same_token(want: &str, got: &str) -> bool {
    if want == got || want == "~" {
        return true;
    }
    // `key=value` compares its value the same way.
    if let (Some((wk, wv)), Some((gk, gv))) = (want.split_once('='), got.split_once('='))
        && wk == gk
    {
        return same_token(wv, gv);
    }
    match (want.parse::<f64>(), got.parse::<f64>()) {
        (Ok(w), Ok(g)) => (w - g).abs() <= TOLERANCE + 1e-9,
        _ => false,
    }
}

/// The first line of `got` that differs from `want`, said in full.
fn first_difference(want: &str, got: &str) -> Option<String> {
    let mut section = "";
    let (mut w, mut g) = (want.lines(), got.lines());
    for n in 1.. {
        match (w.next(), g.next()) {
            (None, None) => return None,
            (Some(wl), Some(gl)) => {
                if wl.starts_with("== ") {
                    section = wl;
                }
                let (wt, gt): (Vec<_>, Vec<_>) = (
                    wl.split_whitespace().collect(),
                    gl.split_whitespace().collect(),
                );
                if wt.len() != gt.len() || wt.iter().zip(&gt).any(|(a, b)| !same_token(a, b)) {
                    return Some(format!(
                        "line {n}, in {section}\n  baseline: {wl}\n  now:      {gl}"
                    ));
                }
            }
            (wl, gl) => {
                return Some(format!(
                    "line {n}, in {section}: one record ends first\n  baseline: {wl:?}\n  now:      {gl:?}"
                ));
            }
        }
    }
    None
}

fn file_of(page: Page) -> &'static str {
    match page {
        Page::Workspaces => WORKSPACES,
        Page::Profiles => PROFILES,
    }
}

/// **What was painted, apart from who painted it.** A paint line's depth,
/// and the records that only group paint (an element, a `Row` part), are
/// the inspector's ownership shape, which the element migration changes on
/// purpose (ELEMENTS-PLAN P4). Every fill, stroke, line and run still
/// compares, in paint order, with its bounds and values. What the page's
/// heading paints is marked `paint heading`, whichever record owned it.
fn painted_view(record: &str) -> String {
    let mut out = String::with_capacity(record.len());
    // The page part a line was painted under, and the depth it began at.
    let mut owner: Option<(&str, usize)> = None;
    for line in record.lines() {
        if let Some(rest) = line.strip_prefix("paint ") {
            let mut tokens = rest.split_whitespace();
            let depth: usize = tokens.next().and_then(|d| d.parse().ok()).unwrap_or(0);
            let tokens: Vec<&str> = tokens.collect();
            let (name, kind) = (tokens.first().copied(), tokens.get(1).copied());
            if owner.is_some_and(|(_, at)| depth <= at) {
                owner = None;
            }
            let row =
                |want: &str| name == Some("Row") && kind == Some("part") && tokens.contains(&want);
            let element =
                |want: &[&str]| kind == Some("element") && name.is_some_and(|n| want.contains(&n));
            if row("row=WorkspacesHeading")
                || row("row=ProfilesHeading")
                || element(&["Workspaces_heading", "Profiles_heading"])
            {
                owner = Some(("heading", depth));
            } else if row("row=ProfilesFooter") || element(&["Profiles_foot"]) {
                owner = Some(("foot", depth));
            }
            if kind == Some("element") || (name == Some("Row") && kind == Some("part")) {
                continue;
            }
            out.push_str(match owner {
                Some(("heading", _)) => "paint heading ",
                Some(("foot", _)) => "paint foot ",
                _ => "paint ",
            });
            out.push_str(&tokens.join(" "));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// A line the plan approved to differ from P0 on `page`: it is held to the
/// page's approved file instead (ELEMENTS-PLAN §5), never written over P0.
/// `band` is the section's list, top and bottom.
fn approved_line(page: Page, section: &str, line: &str, band: Option<(f64, f64)>) -> bool {
    match page {
        // D2: the gear is a node beside its row, and the heading is one.
        // S1: the heading is translated, so outside English its words move.
        Page::Workspaces => {
            line.starts_with("sem ")
                || (line.starts_with("paint heading ") && !section.contains(" lang=en "))
                // D2: the pointer on the gear lights the gear, so the focus
                // the page reports is the gear's node, not its row's.
                || (line.starts_with("focus ") && section.ends_with("state=hover-gear"))
        }
        // D2, as the Workspaces page: the gear is a node beside its row,
        // the heading is one, and the pointer on the gear is focus on it.
        Page::Profiles => {
            line.starts_with("sem ")
                || (line.starts_with("focus ") && section.ends_with("state=hover-gear"))
                // P5: the devices badge is painted with its own item, before
                // `+`, not after it; nothing overlaps, so only the order of
                // the foot's paint moves, and only where there is a badge.
                || (section.ends_with("state=synced") && line.starts_with("paint foot "))
                // E7: a Profile's name is elided like a workspace's, so a
                // long one ends in `…` where P0 cut it at a word. The name is
                // the list's only 18-unit line.
                || (line.starts_with("paint ")
                    && !line.starts_with("paint heading ")
                    && !line.starts_with("paint foot ")
                    && (line.contains(" line_height=18 ") || line.contains(" font_size=13 ")))
                // P5: a row's line wholly outside the visible list was
                // painted and clipped away; the frame does not paint what
                // cannot be seen (P4's frame-time rule). Nothing visible.
                || (line.starts_with("paint ")
                    && !line.starts_with("paint heading ")
                    && !line.starts_with("paint foot ")
                    && paint_span(line)
                        .zip(band)
                        .is_some_and(|((top, bottom), (band_top, band_bottom))| {
                            top >= band_bottom - 0.01 || bottom <= band_top + 0.01
                        }))
        }
    }
}

/// A paint line's top and bottom edges, when written in numbers.
fn paint_span(line: &str) -> Option<(f64, f64)> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let at = if matches!(tokens.get(1), Some(&"heading" | &"foot")) {
        5
    } else {
        4
    };
    let top: f64 = tokens.get(at)?.parse().ok()?;
    let h: f64 = tokens.get(at + 2)?.parse().ok()?;
    Some((top, top + h))
}

/// `record` split into what P0 still holds and what the approved file
/// holds; each case's title heads its section in both.
fn split(page: Page, record: &str) -> (String, String) {
    let (mut kept, mut approved) = (String::new(), String::new());
    let mut section = "";
    let mut band: Option<(f64, f64)> = None;
    for line in record.lines() {
        if let Some(rest) = line.strip_prefix("band ") {
            let n: Vec<f64> = rest
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if let (Some(y), Some(h)) = (n.get(1), n.get(3)) {
                band = Some((*y, y + h));
            }
        }
        if line.starts_with("== ") {
            section = line;
            band = None;
            kept.push_str(line);
            kept.push('\n');
            approved.push_str(line);
            approved.push('\n');
        } else if approved_line(page, section, line, band) {
            approved.push_str(line);
            approved.push('\n');
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    (kept, approved)
}

fn approved_of(page: Page) -> Option<&'static str> {
    match page {
        Page::Workspaces => Some(WORKSPACES_APPROVED),
        Page::Profiles => Some(PROFILES_APPROVED),
    }
}

fn compare(page: Page) {
    let got = painted_view(&page_record(page));
    let want = painted_view(file_of(page));
    let (want_kept, _) = split(page, &want);
    let (got_kept, got_approved) = split(page, &got);
    if let Some(difference) = first_difference(&want_kept, &got_kept) {
        panic!(
            "the {} page no longer draws as its P0 baseline does — {difference}\n\
             A deliberate change is re-captured by name and reviewed as a diff:\n  \
             cargo test -p koru capture_popover_baseline -- --ignored",
            page.token()
        );
    }
    if let Some(approved) = approved_of(page)
        && let Some(difference) = first_difference(approved, &got_approved)
    {
        panic!(
            "the {} page's approved differences from P0 changed — {difference}\n\
             Re-captured by name and reviewed as a diff:\n  \
             cargo test -p koru capture_popover_approved -- --ignored",
            page.token()
        );
    }
}

/// Today's Workspaces page, as P0 wrote it down.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the chrome is set in the system face on macOS; the baseline is Inter's"
)]
fn the_workspaces_page_matches_its_baseline() {
    compare(Page::Workspaces);
}

/// Today's Profiles page, as P0 wrote it down.
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "the chrome is set in the system face on macOS; the baseline is Inter's"
)]
fn the_profiles_page_matches_its_baseline() {
    compare(Page::Profiles);
}

/// **Writes the baseline.** Run only by name, by a developer who means it.
#[test]
#[ignore = "writes the popover baseline; run by name: cargo test -p koru capture_popover_baseline -- --ignored"]
fn capture_popover_baseline() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/panes/explore/popover_baseline"
    );
    for page in [Page::Workspaces, Page::Profiles] {
        let path = format!("{dir}/{}.txt", page.token());
        std::fs::write(&path, page_record(page)).expect("write the baseline");
        eprintln!("wrote {path}");
    }
}

/// **Writes the approved differences**, apart from P0, for the pages that
/// have them. Run only by name, by a developer who means it.
#[test]
#[ignore = "writes the approved differences; run by name: cargo test -p koru capture_popover_approved -- --ignored"]
fn capture_popover_approved() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/panes/explore/popover_baseline"
    );
    for page in [Page::Workspaces, Page::Profiles] {
        let path = format!("{dir}/{}.approved.txt", page.token());
        let (_, approved) = split(page, &painted_view(&page_record(page)));
        std::fs::write(&path, approved).expect("write the approved file");
        eprintln!("wrote {path}");
    }
}

// ---------------------------------------------------- facts the record cannot show

/// Display scale is applied once, outside the layout: at 1.5 every record is
/// where it is at 1.0, in logical units.
#[test]
fn scale_does_not_move_anything() {
    let _theme = crate::tests::lock_theme();
    for page in [Page::Workspaces, Page::Profiles] {
        let case = Case {
            page,
            width: theme::SIDEBAR_MIN_W,
            dark: theme::is_dark(),
            lang: Lang::En,
            names: Names::Normal,
            state: State::HoverRow,
        };
        let v = arrange(&case);
        let (mut one, mut half) = (String::new(), String::new());
        paint_lines(&mut one, &frame(&v, &case, 1.0));
        paint_lines(&mut half, &frame(&v, &case, 1.5));
        assert!(one.lines().count() > 10, "{page:?} recorded its paint");
        if let Some(difference) = first_difference(&one, &half) {
            panic!("{page:?} moved at scale 1.5 — {difference}");
        }
    }
}

/// **One lit row, shared by the pointer and the arrows**, and it is what the
/// accessibility tree reports as focused (ELEMENTS.final D2's starting
/// point): a hover is the keyboard's position, and the keyboard's is the
/// hover's.
#[test]
fn the_lit_row_is_one_index_shared_by_pointer_and_arrows() {
    use winit::keyboard::{Key, NamedKey};
    for page in [Page::Workspaces, Page::Profiles] {
        let case = Case {
            page,
            width: theme::SIDEBAR_MIN_W,
            dark: false,
            lang: Lang::En,
            names: Names::Normal,
            state: State::HoverRow,
        };
        let mut v = arrange(&case);
        let area = case.area();
        let cx = context(area, Lang::En);
        // The published node at the lit row's rectangle: what a client is
        // told the keyboard is on. The foot publishes its three items, not
        // itself, and the one it reports is the `+` Enter presses there.
        let node_at = |v: &ExploreView, index: usize| {
            let (row, r) = v.popover_rects(area, VIEW)[index];
            let r = match row {
                PopoverRow::ProfilesFooter => v
                    .popover_head_button_rect(area, VIEW, HeadButton::New)
                    .expect("the foot's +"),
                _ => r,
            };
            // The row's node: its whole rectangle, or since P4 the first
            // node inside it — the workspace's primary, before its gear.
            let inside = |b: Rect| {
                b.x >= r.x && b.y >= r.y && b.x + b.w <= r.x + r.w && b.y + b.h <= r.y + r.h
            };
            menu(v.semantics(&cx))
                .expect("the popover's tree")
                .children
                .into_iter()
                .find(|node| node.bounds.is_some_and(inside))
                .map(|node| node.occurrence)
        };
        let lit = v.popover_hover().expect("the pointer lit a row");
        assert_eq!(
            v.accessible_focus(&cx),
            node_at(&v, lit),
            "{page:?}: the pointer's row is the focused node"
        );
        v.popover_key(&Key::Named(NamedKey::ArrowDown));
        let next = v.popover_hover().expect("the arrow moved the lit row");
        assert_eq!(
            next,
            lit + 1,
            "{page:?}: Down moves on from the pointer's row"
        );
        assert_eq!(
            v.accessible_focus(&cx),
            node_at(&v, next),
            "{page:?}: and the focused node moves with it"
        );
    }
}

/// **The gear's hit region is the whole trailing lane, in every state** —
/// current or not, lit or not — never the 22-unit plate it paints.
#[test]
fn the_gears_hit_region_is_the_whole_trailing_lane() {
    for page in [Page::Workspaces, Page::Profiles] {
        for state in [State::Rest, State::HoverRow, State::HoverGear] {
            let case = Case {
                page,
                width: theme::SIDEBAR_MIN_W,
                dark: false,
                lang: Lang::En,
                names: Names::Normal,
                state,
            };
            let v = arrange(&case);
            let area = case.area();
            for (row, r) in v.popover_rects(area, VIEW) {
                if !matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_)) {
                    continue;
                }
                let lane = trailing_lane(r);
                let inset = 0.5;
                for (x, y) in [
                    (lane.x + inset, lane.y + inset),
                    (lane.x + lane.w - inset, lane.y + inset),
                    (lane.x + inset, lane.y + lane.h - inset),
                    (lane.x + lane.w - inset, lane.y + lane.h - inset),
                ] {
                    assert_eq!(
                        v.popover_part_at(area, VIEW, x, y).map(|(_, _, part)| part),
                        Some(PopoverPart::Gear),
                        "{page:?} {state:?} {row:?}: ({x}, {y}) is in the lane"
                    );
                }
                assert_eq!(
                    v.popover_part_at(area, VIEW, lane.x - inset, lane.y + lane.h * 0.5)
                        .map(|(_, _, part)| part),
                    Some(PopoverPart::Row),
                    "{page:?} {state:?} {row:?}: just before the lane is the row"
                );
            }
        }
    }
}

// --------------------------------------------------------------- frame work

/// **How long the two pages take to lay out and paint**, headless: the
/// popover's own share of a frame's encode, which is the part the element
/// migration changes. Prints a table for `ELEMENTS-RESULTS.md`.
///
/// ```text
/// cargo test -p koru measure_popover_frame_work -- --ignored --nocapture
/// ```
///
/// Not `KORU_FRAME_STATS`: that is the whole window's encode, render and
/// present averaged over 120 frames, and it needs a person driving the app.
/// This one repeats, and isolates the surface being replaced.
#[test]
#[ignore = "a measurement, not a check; run by name with --nocapture"]
fn measure_popover_frame_work() {
    use std::time::Instant as Clock;
    use winit::keyboard::{Key, NamedKey};

    const PASSES: usize = 7;
    let _theme = crate::tests::lock_theme();
    let was = theme::is_dark();
    theme::set_dark(false);

    fn draw(v: &ExploreView, area: Rect, engine: &mut TextEngine) -> f64 {
        let mut scene = vello::Scene::new();
        let start = Clock::now();
        let _ = v.popover_rects(area, VIEW);
        v.draw_popover(&mut scene, Affine::IDENTITY, engine, area, VIEW, None);
        start.elapsed().as_secs_f64() * 1e6
    }

    fn summary(mut samples: Vec<f64>) -> (f64, f64, usize) {
        samples.sort_by(f64::total_cmp);
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
        (at(0.5), at(0.95), samples.len())
    }

    let mut table = String::from(
        "| Page | Load | Scenario | Frames | Median µs | p95 µs |\n| --- | --- | --- | --- | --- | --- |\n",
    );
    for page in [Page::Workspaces, Page::Profiles] {
        for names in [Names::Normal, Names::Forty] {
            let case = Case {
                page,
                width: theme::SIDEBAR_MIN_W,
                dark: false,
                lang: Lang::En,
                names,
                state: State::Rest,
            };
            let area = case.area();
            let mut engine = TextEngine::new();
            let mut scenarios: Vec<(&str, Vec<f64>)> = Vec::new();

            let mut open = Vec::new();
            let mut hover = Vec::new();
            let mut scroll = Vec::new();
            let mut resize = Vec::new();
            let mut themes = Vec::new();
            let mut off = Vec::new();
            let mut on = Vec::new();
            for _ in 0..PASSES {
                // Open: the fade's frames, then the settled one.
                let mut v = fixture(names);
                match page {
                    Page::Workspaces => v.toggle_popover(PopoverKind::Summary),
                    Page::Profiles => v.toggle_profile_popover(),
                }
                for step in 1..=12 {
                    if let Some(p) = v.popover.as_mut() {
                        p.progress = step as f32 / 12.0;
                    }
                    open.push(draw(&v, area, &mut engine));
                }
                v.advance_popover(Instant::now() + Duration::from_secs(5));

                // A hover sweep over every row, top to bottom.
                for (_, r) in v.popover_rects(area, VIEW) {
                    v.mouse_move(area, VIEW, r.x + r.w * 0.4, r.y + r.h * 0.5);
                    hover.push(draw(&v, area, &mut engine));
                }

                // Scroll to the end, a wheel notch at a time.
                while v.scroll_popover(area, VIEW, -40.0) {
                    scroll.push(draw(&v, area, &mut engine));
                }
                if scroll.is_empty() {
                    scroll.push(draw(&v, area, &mut engine));
                }

                // The sidebar dragged from its floor to its ceiling.
                let mut w = theme::SIDEBAR_MIN_W;
                while w <= theme::SIDEBAR_MAX_W {
                    let wide = Rect::new(area.x, area.y, w, area.h);
                    resize.push(draw(&v, wide, &mut engine));
                    w += 16.0;
                }

                // Light and dark, back and forth.
                for dark in [true, false, true, false] {
                    theme::set_dark(dark);
                    themes.push(draw(&v, area, &mut engine));
                }

                // The inspector off, then armed (F12 open).
                for _ in 0..8 {
                    off.push(draw(&v, area, &mut engine));
                    let capture = koru_draw::inspect::Frame::begin(
                        1,
                        Affine::IDENTITY,
                        koru_draw::scene::rect(0.0, 0.0, VIEW.0, VIEW.1),
                    );
                    on.push(draw(&v, area, &mut engine));
                    drop(capture.finish());
                }
                v.popover_key(&Key::Named(NamedKey::Escape));
            }
            scenarios.push(("open", open));
            scenarios.push(("hover sweep", hover));
            scenarios.push(("scroll to end", scroll));
            scenarios.push(("resize 256→600", resize));
            scenarios.push(("theme switch", themes));
            scenarios.push(("inspector off", off));
            scenarios.push(("inspector on", on));
            for (name, samples) in scenarios {
                let (median, p95, n) = summary(samples);
                let _ = writeln!(
                    table,
                    "| {} | {} | {name} | {n} | {median:.0} | {p95:.0} |",
                    page.token(),
                    names.token()
                );
            }
        }
    }
    theme::set_dark(was);
    // The workspace's test profile is optimised; what differs from a release
    // build is the assertions, so the table says which.
    eprintln!(
        "\nPopover frame work (debug assertions {}):\n\n{table}",
        if cfg!(debug_assertions) { "on" } else { "off" }
    );
}
