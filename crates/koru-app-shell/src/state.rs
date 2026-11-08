// SPDX-License-Identifier: MPL-2.0

//! The container: a set of windows, their tabs, their chrome, and the drain.
//!
//! **This is not a second `ApplicationHandler`.** Nothing here touches winit's
//! event loop. [`ShellState`] implements
//! [`MultiWindowApp`](koru_window::MultiWindowApp) and is handed to
//! [`run_windows`](koru_window::run_windows); every entry point below is a
//! method the shared dispatch core calls, which is what rev 1's review caught
//! and rev 2 fixed.
//!
//! ## Where SP-0's `Shell` and SP-1's runner disagreed
//!
//! Seven places, each recorded where it lands:
//!
//! 1. **`WindowKey` is the runner's.** [`MultiWindowApp::windows`] is *the*
//!    authority over which windows exist, so a shell key of its own would be a
//!    second source of truth and a map on every event.
//! 2. **Window creation is asynchronous.** SP-0 created a window inside the
//!    drain and asked the host to seat it in the same call. The runner opens
//!    windows in its own settle and reports [`MultiWindowApp::seated`] once the
//!    window *and* its render surface exist — a better commit point for
//!    two-phase pop-out than the one SP-0 invented, because it is the moment the
//!    destination is genuinely real.
//! 3. **The engine is not always there.** See
//!    [`SurfaceCx::engine`](crate::surface::SurfaceCx::engine).
//! 4. **Pointer input arrives split.** The runner offers eight callbacks behind
//!    two `wants_*` flags; the shell opts into both and folds them back into one
//!    [`PointerEvent`].
//! 5. **`Dragged` has to be synthesised.** With `wants_rich_mouse` on, the
//!    dispatch core never sets its own `dragging` flag, so a move with a button
//!    held arrives as `pointer_moved`. The shell remembers which button is down
//!    and reports the move as [`PointerEvent::Dragged`], which is what the flag
//!    would have produced — and it is *more* than the flag produced, because a
//!    right-button drag was invisible before.
//! 6. **Ctrl+Tab is resolved here.** SP-0 expected the runner to do it; a runner
//!    that knew about tabs would be a runner with shell policy in it.
//! 7. **`SidePaneSurface` is gone**, deferred to SP-11 by owner decision. The
//!    shell keeps the pane *slots* — a [`Panel`] a side, feeding
//!    `regions::solve` — and what is drawn in them is nobody's yet.

use std::time::Instant;

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_shell_chrome::{banner, caption, regions, tabs};
use koru_window::{
    ClosingWindowState, ImeEvent, KeyState, MultiWindowApp, RichKeyEvent, RichMouseEvent,
    WindowKey, WindowRequest,
};
use vello::Scene;
use vello::kurbo::Affine;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::MouseButton;
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes, WindowId};

use koru_ui::tooltip::{Tooltip, TooltipContent};

use crate::banner_state::BannerState;
use crate::chrome;
use crate::command::{Commands, DRAIN_PASSES, ShellAction, ShellCommand};
use crate::element::TabElement;
use crate::host::{BannerSlot, HostCx, PopoverFrame, ShellHost};
use crate::keys::{Minter, TabKey};
use crate::panel::{self, Panel};
use crate::popover::{Popover, SlideFrom};
use crate::surface::{CloseResponse, PointerEvent, Surface, SurfaceCx};
use crate::tabslide::TabSlides;
use crate::tabstrip::TabStrip;
use crate::zoom_label::ZoomLabel;

/// The floor both apps share (owner, 2026-08-31): below it the caption
/// arithmetic has no answer — even with both banner slots at zero width, the
/// window controls and the sidebar toggle leave the lane less than one minimum
/// tab.
///
/// **A second copy of `koru_workspace_host::settings::MIN_WINDOW_SIZE`, and
/// deliberately so**: the shell may not see that crate, and the alternative
/// would be putting a window's minimum size in `koru-draw` beside the type
/// scale, where nothing about a window belongs. If one moves, move both — the
/// number is the same because DESIGN §4 works the 720 case for the shared
/// chrome, not for either app separately.
pub const MIN_WINDOW_SIZE: (f32, f32) = (720.0, 600.0);

/// **Display zoom bounds and step**, re-exported under the names every caller
/// here already uses.
///
/// They used to be three literals, with a note saying `koru_workspace_host`
/// held exactly the same three — a comment doing a constant's job, and it was
/// right up until a third reader appeared. They now live beside the
/// `theme::zoom()` global they are about, which every crate that touches zoom
/// already depends on, so there is one place to change 2.0 into 3.0.
pub use theme::{ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

/// Snap a zoom factor to the [`ZOOM_STEP`] grid and clamp it to the bounds —
/// **what a host's in/out chord steps through**.
///
/// Snapping stops float drift accumulating across many in/out cycles, so a
/// window keeps reading 120% rather than drifting to 119.99999%. A value that
/// is *chosen* rather than stepped — a Settings row offering 125% — is not
/// snapped: it goes to [`ShellState::set_zoom`], which clamps to the same
/// bounds and leaves the value alone.
pub fn stepped_zoom(zoom: f32) -> f32 {
    theme::clamp_zoom(zoom)
}

/// Which side pane.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// One tab in a shell window.
///
/// The surface is an `Option` for exactly one reason, and it is never observable
/// from outside: the never-empty floor is minted by the *strip*, through the
/// factory it was built with, and what goes behind it is the *host's*
/// ([`ShellHost::floor_surface`]). Those two cannot happen in the same borrow —
/// the strip is a field of the window and the host is a field of the shell — so
/// the strip mints the tab, [`ShellState::seat_floors`] fills it in from the
/// host, and the two are seated together before anything can draw.
pub struct ShellTab {
    key: TabKey,
    title: String,
    /// Measured at draw and cached, never on the hit path — the rule
    /// `koru-shell-chrome` states and its geometry tests depend on.
    width: f32,
    surface: Option<Box<dyn Surface>>,
    placeholder: bool,
    /// Whether the ✕ is drawn and pressable. The floor never is; content is
    /// whatever the host asked for.
    closable: bool,
    pinned: Option<uuid::Uuid>,
    home: Option<uuid::Uuid>,
    show_title: bool,
}

impl ShellTab {
    fn floor(key: TabKey) -> Self {
        Self {
            key,
            title: String::new(),
            width: theme::TAB_MIN_W,
            surface: None,
            placeholder: true,
            closable: false,
            pinned: None,
            home: None,
            show_title: false,
        }
    }

    fn content(key: TabKey, title: String, closable: bool, surface: Box<dyn Surface>) -> Self {
        Self {
            key,
            title,
            width: theme::TAB_MIN_W,
            surface: Some(surface),
            placeholder: false,
            closable,
            pinned: None,
            home: None,
            show_title: false,
        }
    }

    pub fn key(&self) -> TabKey {
        self.key
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn closable(&self) -> bool {
        self.closable
    }
}

impl TabElement for ShellTab {
    fn pinned_scope(&self) -> Option<uuid::Uuid> {
        self.pinned
    }

    fn set_pinned_scope(&mut self, scope: Option<uuid::Uuid>) {
        self.pinned = scope;
    }

    fn home_scope(&self) -> Option<uuid::Uuid> {
        self.home
    }

    fn set_home_scope(&mut self, scope: Option<uuid::Uuid>) {
        self.home = scope;
    }

    /// A floor tab holds nothing yet, so pinning one would pin a placeholder.
    /// Everything else a host opened is content and pins.
    fn pinnable(&self) -> bool {
        !self.placeholder
    }

    fn show_title(&self) -> bool {
        self.show_title
    }

    fn set_show_title(&mut self, show: bool) {
        self.show_title = show;
    }

    fn is_reusable(&self) -> bool {
        self.placeholder
    }

    fn is_placeholder(&self) -> bool {
        self.placeholder
    }
}

/// One chromed window.
pub struct ShellWindow {
    key: WindowKey,
    tabs: TabStrip<ShellTab>,
    slides: TabSlides,
    tab_scroll: f32,
    /// The window's own logical size, straight from the last `resized`. Never
    /// laid out against directly — [`viewport`](Self::viewport) is.
    logical: (f32, f32),
    /// **The space everything in this window is laid out and hit-tested in**:
    /// [`logical`](Self::logical) divided by [`zoom`](Self::zoom).
    ///
    /// At 200% a window reflows as if it were half as wide, and the root
    /// transform scales that layout back up to fill the glass. That is the
    /// same trick already used for the OS DPI factor, one level down: a view
    /// draws in logical pixels and `koru-window` multiplies by the scale.
    viewport: (f32, f32),
    scale: f32,
    /// The display zoom this window draws at. One value for the process today
    /// — [`ShellState::set_zoom`] moves every window — but it is held per
    /// window because every reader of it is one.
    zoom: f32,
    /// Cached so drawing and hit-testing ask the same arithmetic.
    regions: regions::Regions,
    left: Panel,
    right: Panel,
    /// A resize drag in flight: which column, and how far the pointer was from
    /// the column's own right edge when it was grabbed. Keeping the *offset*
    /// rather than the start point is what stops the edge jumping to the
    /// pointer on the first move.
    pane_drag: Option<(Side, f32)>,
    banner: BannerState,
    popover: Option<Popover>,
    window_buttons: bool,
    context: bool,
    host_action: bool,
    /// The button currently held, so a move can be reported as a drag. The
    /// dispatch core's own `dragging` flag is never set once `wants_rich_mouse`
    /// is on, so this is where the drag lives.
    held: Option<MouseButton>,
    /// Where the pointer last was, in window space.
    ///
    /// **`scroll_by` carries no position**, and SP-9 answered it with the
    /// content rect's centre — which sends every wheel to the active surface
    /// even when the pointer is over a pane. A remembered point is what lets a
    /// wheel land where the pointer actually is.
    pointer: (f32, f32),
    /// What the pointer is over, resolved once per move by the same arithmetic
    /// the press uses, so the highlight and the click cannot disagree.
    hover: Option<regions::Hit>,
    banner_hover: Option<BannerSlot>,
    /// One interaction clock for the chips (DESIGN, "The banner" §8).
    tooltip: Tooltip,
    /// **The transient `120%` flash**, armed by [`ShellState::set_zoom`].
    ///
    /// Per window rather than per process even though the zoom is one value,
    /// because the *flash* is not: a second window that is occluded when the
    /// chord lands has nothing to show, and each window's fade is its own clock
    /// against its own frames.
    zoom_label: ZoomLabel,
    maximised: bool,
    /// Queued for `MultiWindowApp::take_window_request`, which the runner
    /// drains after every event.
    request: Option<WindowRequest>,
}

impl ShellWindow {
    fn new(key: WindowKey, logical: (f32, f32), scale: f32, zoom: f32) -> Self {
        let mut window = Self {
            key,
            // The factory mints a *tab*; `seat_floors` puts the host's surface
            // behind it. See [`ShellTab`].
            tabs: TabStrip::new(ShellTab::floor(TabKey::UNSEATED), || {
                ShellTab::floor(TabKey::UNSEATED)
            }),
            slides: TabSlides::new(),
            tab_scroll: 0.0,
            logical,
            // Both filled in by `resolve` below, which is the one place that
            // knows the viewport is the logical size over the zoom.
            viewport: logical,
            scale,
            zoom,
            regions: regions::solve(logical.0, logical.1, 0.0, 0.0),
            left: Panel::new(theme::SIDEBAR_W, false),
            right: Panel::new(theme::CONTEXT_W, false),
            pane_drag: None,
            banner: BannerState::default(),
            popover: None,
            window_buttons: true,
            context: false,
            host_action: false,
            held: None,
            pointer: (0.0, 0.0),
            hover: None,
            banner_hover: None,
            tooltip: Tooltip::default(),
            zoom_label: ZoomLabel::new(),
            maximised: false,
            request: None,
        };
        window.resolve();
        window
    }

    /// The zoom, as a divisor. A guard rather than a trusting divide because a
    /// zero would put every region at infinity — `ShellState::set_zoom` clamps,
    /// so this only ever catches a bug.
    fn divisor(&self) -> f32 {
        if self.zoom.is_finite() && self.zoom > 0.0 {
            self.zoom
        } else {
            1.0
        }
    }

    fn resolve(&mut self) {
        // **The zoom is a division here and a multiplication in the transform**,
        // and nothing between the two knows it happened.
        let zoom = self.divisor();
        self.viewport = (self.logical.0 / zoom, self.logical.1 / zoom);
        self.regions = regions::solve(
            self.viewport.0,
            self.viewport.1,
            self.left.layout_width(),
            self.context_width(),
        );
    }

    /// The transform every draw in this window goes under.
    ///
    /// **This is the whole of display zoom.** The chrome, the panes, the
    /// popover, the tooltip and the host's surfaces all draw in the window's
    /// layout space and are scaled by this one affine — so the caption band,
    /// a tab's label, a tree row and a terminal grid grow together, and none
    /// of them has to be told a zoom exists. `koru-window` multiplies the DPI
    /// factor on top when it appends the frame.
    pub fn root(&self) -> Affine {
        Affine::scale(f64::from(self.zoom))
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// A point the runner reported, in this window's layout space.
    ///
    /// Every coordinate that arrives from outside — a pointer, a caption test,
    /// a cursor query — is in the window's own logical pixels, and everything
    /// it is compared against was solved in the zoomed-down space. One divide,
    /// at each of the four doors.
    fn unzoom(&self, x: f32, y: f32) -> (f32, f32) {
        let zoom = self.divisor();
        (x / zoom, y / zoom)
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
        self.resolve();
    }

    /// The context column's layout width, **gated on the host still offering
    /// it** ([`ShellHost::context_offered`]).
    ///
    /// A column nobody can toggle must not keep taking room, and the person's
    /// open/shut choice must survive the tab switch that took the offer away —
    /// so the gate lives here rather than in `Panel`, and shutting the pane is
    /// exactly what it does *not* do.
    fn context_width(&self) -> f32 {
        if self.context {
            self.right.layout_width()
        } else {
            0.0
        }
    }

    pub fn key(&self) -> WindowKey {
        self.key
    }

    pub fn content(&self) -> Rect {
        self.regions.content
    }

    pub fn regions(&self) -> &regions::Regions {
        &self.regions
    }

    pub fn banner(&self) -> &BannerState {
        &self.banner
    }

    pub fn popover(&self) -> Option<Popover> {
        self.popover
    }

    pub fn pane(&mut self, side: Side) -> &mut Panel {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    pub fn tabs(&self) -> &TabStrip<ShellTab> {
        &self.tabs
    }

    pub fn slides(&self) -> &TabSlides {
        &self.slides
    }

    /// The lane the tabs run in, **starting after the banner** — the one
    /// arithmetic change the banner needed, consumed here.
    pub fn strip(&self) -> Rect {
        caption::tab_strip_with_banner(
            self.regions.caption,
            self.window_buttons,
            self.context,
            self.host_action,
            self.banner.slots(),
        )
    }

    pub fn maximised(&self) -> bool {
        self.maximised
    }

    pub fn hover(&self) -> Option<regions::Hit> {
        self.hover
    }

    pub fn banner_hover(&self) -> Option<BannerSlot> {
        self.banner_hover
    }

    pub fn pointer(&self) -> (f32, f32) {
        self.pointer
    }

    pub fn viewport(&self) -> Rect {
        Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1)
    }

    /// Where a pane is **drawn**, which during a slide is not where it was laid
    /// out: `regions::solve` took its space the instant it was toggled and the
    /// column is still arriving. DESIGN, "The banner" §6, for the caption's
    /// half of the same rule.
    ///
    /// **The width is solved, not asked for.** `Panel::drawn_width` answers what
    /// the column *wants* — `full`, unclamped — and `regions::solve` protects
    /// content by giving way in order, so at a narrow width the two disagree.
    /// Drawing at the wanted width paints over content the layout never gave up
    /// *and* puts the column's own rows outside `pane_at`'s hit region, so a row
    /// you can see is a row that types into the surface behind it. Running the
    /// same solve over the drawn widths is the whole fix: it is pure arithmetic,
    /// it applies the same give-way order, and it keeps the slide correct in both
    /// directions because a column sliding out is still asking for its old width.
    ///
    /// The case is real at the shared floor: `solve(720, h, 232, 268)` grants the
    /// context column 168, not 268.
    pub fn drawn_pane(&self, side: Side) -> Rect {
        // The gate `resolve` applies to the layout width applies to the drawn
        // one too, or a column the host has stopped offering would keep painting
        // over the content that has already reclaimed its room.
        if side == Side::Right && !self.context {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let panel = match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        };
        let drawn = regions::solve(
            self.viewport.0,
            self.viewport.1,
            self.left.drawn_width(),
            if self.context {
                self.right.drawn_width()
            } else {
                0.0
            },
        );
        let w = match side {
            Side::Left => drawn.sidebar.w,
            Side::Right => drawn.context.w,
        };
        if w <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let below = self.regions.caption.h;
        match side {
            Side::Left => Rect::new(
                -panel.slide_offset(),
                0.0,
                w,
                (self.viewport.1 - 0.0).max(0.0),
            ),
            Side::Right => Rect::new(
                self.viewport.0 - w + panel.slide_offset(),
                below,
                w,
                (self.viewport.1 - below).max(0.0),
            ),
        }
    }

    /// **The band at the left column's right edge that resizes it.**
    ///
    /// A zero-width rect when the column is shut, gone, or still sliding — a
    /// grip on an edge that is travelling under the pointer would resize to
    /// wherever the animation happened to be, and there is nothing to grab on a
    /// column that is not there.
    ///
    /// **The left column only.** Koru offers one on its right-hand pane too and
    /// the arithmetic is the mirror of this, but the Terminal's right column is
    /// Details at a fixed `CONTEXT_W` and inventing limits for a column nobody
    /// has asked to drag is an inner platform. It is one `match` away the day
    /// that changes.
    pub fn pane_grip(&self, side: Side) -> Rect {
        let none = Rect::new(0.0, 0.0, 0.0, 0.0);
        if side != Side::Left || !self.left.is_open() || self.left.animating() {
            return none;
        }
        panel::grip(self.drawn_pane(Side::Left))
    }

    /// Set the dragged column's width, clamped. Returns whether it moved.
    ///
    /// **Chrome geometry only.** `Panel::layout_width` is unchanged, so the
    /// content keeps the width it was laid out against until
    /// [`commit_pane_width`](Self::commit_pane_width) — which is the whole
    /// mitigation for the drag, and not a nicety: `set_bounds` on the active
    /// surface is `reflow`, and a drag that paid it per mouse-move is exactly
    /// the O(rows) cost AGENTS.md's responsiveness rules forbid.
    fn set_pane_width(&mut self, side: Side, w: f32) -> bool {
        let clamped = w.clamp(panel::SIDEBAR_MIN_W, panel::SIDEBAR_MAX_W);
        self.pane(side).set_full(clamped)
    }

    /// Let the layout catch up with the column. **One reflow for the whole
    /// drag**, and the caller does the reflow only when this says the width
    /// actually moved.
    fn commit_pane_width(&mut self, side: Side) -> bool {
        if !self.pane(side).commit_width() {
            return false;
        }
        self.resolve();
        true
    }

    /// **Where the left column's drawn right edge is** — what the caption and
    /// the pane header band both start at while a slide is in flight.
    ///
    /// `Panel::slide_offset` is *not* this number and never was: it is
    /// `(1 - progress) * full`, which is `full` at rest with the column **shut**,
    /// not zero. Subtracting it from the settled caption therefore left a shut
    /// pane's caption drawn 232 px to the left of where it hit-tested, for as
    /// long as the pane stayed shut — the banner walked off the window's left
    /// edge, the lane shifted under the pointer, and hovering the `+` reported
    /// the profile chip's tooltip because the press arithmetic was still using
    /// the settled rect. Drawing and hit-testing diverging is the one thing the
    /// chrome exists to prevent.
    ///
    /// The edge that is actually wanted is the *drawn* column's far side, which
    /// is `drawn_pane`'s own rect: zero when the column is gone, `sidebar.w` when
    /// it is flush, and the interpolation between them mid-slide — in both
    /// directions, because a column sliding out still asks for its old width.
    fn drawn_left_edge(&self) -> f32 {
        let pane = self.drawn_pane(Side::Left);
        if pane.w <= 0.0 {
            0.0
        } else {
            (pane.x + pane.w).max(0.0)
        }
    }

    /// The caption as it is **drawn**: its left edge follows the sliding pane,
    /// so the chips travel with it rather than jumping at the ends of the slide
    /// (DESIGN, "The banner" §6). Hit-testing keeps using `regions.caption`.
    ///
    /// At rest the two are the same rect, open or shut. They differ only for the
    /// 140 ms of a slide, which is the whole of what §6 licenses.
    pub fn drawn_caption(&self) -> Rect {
        let x = self.drawn_left_edge();
        let caption = self.regions.caption;
        Rect::new(x, caption.y, (self.viewport.0 - x).max(0.0), caption.h)
    }

    /// **The identity band at the top of the left pane**, settled — the rect
    /// hit-testing uses, and a zero-width one whenever the pane is shut.
    ///
    /// `CAPTION_H` tall, so it is the band the caption occupies beside it
    /// (DESIGN, "The window is cut into four"), and derived from
    /// `regions.sidebar` so it is final the instant the pane is toggled — the
    /// same rule `regions.caption` follows, and the reason there is no third code
    /// path for the slide.
    pub fn pane_header(&self) -> Rect {
        let sidebar = self.regions.sidebar;
        Rect::new(
            sidebar.x,
            sidebar.y,
            sidebar.w,
            theme::CAPTION_H.min(sidebar.h.max(0.0)),
        )
    }

    /// The same band as it is **drawn**, travelling with the slide.
    pub fn drawn_pane_header(&self) -> Rect {
        let pane = self.drawn_pane(Side::Left);
        Rect::new(
            pane.x,
            pane.y,
            pane.w,
            theme::CAPTION_H.min(pane.h.max(0.0)),
        )
    }

    /// The pointer left. Every highlight this window holds goes with it.
    fn clear_hover(&mut self) -> bool {
        let moved = self.hover.is_some() || self.banner_hover.is_some();
        self.hover = None;
        self.banner_hover = None;
        moved | self.tooltip.clear()
    }

    fn widths(&self) -> Vec<f32> {
        self.tabs.iter().map(|tab| tab.width).collect()
    }

    fn active_key(&self) -> Option<TabKey> {
        self.tabs.get(self.tabs.active()).map(|tab| tab.key)
    }

    /// Whether this point is the caption's drag band: inside the caption and
    /// claimed by none of its furniture, the banner included.
    fn is_caption(&self, x: f32, y: f32) -> bool {
        if !self.regions.caption.contains(x, y) {
            return false;
        }
        if banner::hit(
            self.regions.caption,
            self.window_buttons,
            self.banner.slots(),
            x,
            y,
        )
        .is_some()
        {
            return false;
        }
        matches!(
            regions::hit_with_banner(
                &self.regions,
                &self.widths(),
                self.tab_scroll,
                self.window_buttons,
                self.context,
                self.host_action,
                true,
                self.banner.slots(),
                x,
                y,
            ),
            regions::Hit::Caption
        )
    }
}

/// Who fills a new window's first tab.
///
/// **Two paths, and the difference is whether it can be refused.** A window the
/// application opened for itself takes the host's floor
/// ([`ShellHost::floor_surface`], which cannot fail — it is the same factory the
/// never-empty tab strip mints through). A window the shell was *asked* for
/// mid-dispatch — `OpenWindow`, `PopOutTab` — goes through
/// [`ShellHost::seat_window`], which may refuse, and that refusal is the whole
/// of two-phase pop-out.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Seat {
    Floor,
    Host,
}

/// A window the host asked for, waiting for the runner and then for the host.
struct PendingWindow {
    key: WindowKey,
    seat: Seat,
    /// The tab that moves in once the destination exists, and the window it
    /// comes from. `None` for a plain new window.
    popout: Option<(WindowKey, TabKey)>,
    /// The window's logical size, not its layout viewport: the zoom is applied
    /// when the `ShellWindow` is built, from whatever it is by then.
    logical: (f32, f32),
    scale: f32,
    /// Set by [`MultiWindowApp::seated`]: the runner has a window and a render
    /// surface. The host has not been asked yet, because `seated` carries no
    /// `TextEngine` and [`ShellHost::seat_window`] is entitled to one.
    runner_seated: bool,
}

/// The shell.
pub struct ShellState<H: ShellHost> {
    host: H,
    minter: Minter,
    windows: Vec<ShellWindow>,
    /// What [`MultiWindowApp::windows`] answers: every live window plus every
    /// one asked for and not yet seated, in creation order. A `Vec` because the
    /// runner wants a slice and a settle after every event must allocate
    /// nothing.
    keys: Vec<WindowKey>,
    /// Asked for, not yet seated. The source tab of a pop-out stays exactly
    /// where it is until its destination exists, which is the whole of
    /// two-phase.
    pending: Vec<PendingWindow>,
    /// Drained after every dispatch, never during one.
    queued: Vec<ShellCommand>,
    /// **The display zoom, for every window in the process.**
    ///
    /// One value rather than one per window because it is a preference about
    /// the person, not about a window — which is also why a host that persists
    /// it writes one row. The shell is the source of truth: a host asks with
    /// [`ShellAction::Zoom`] and reads the answer back here.
    zoom: f32,
}

impl<H: ShellHost> ShellState<H> {
    pub fn new(host: H) -> Self {
        Self {
            host,
            minter: Minter::default(),
            windows: Vec::new(),
            keys: Vec::new(),
            pending: Vec::new(),
            queued: Vec::new(),
            zoom: 1.0,
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Ask for a window.
    ///
    /// It does not exist yet: the runner opens it on its next settle and reports
    /// [`MultiWindowApp::seated`], and only then is the host asked to seat it.
    /// The key is minted now so a caller has something to address.
    pub fn open_window(&mut self, logical: (f32, f32), scale: f32) -> WindowKey {
        let key = self.minter.window();
        self.keys.push(key);
        self.pending.push(PendingWindow {
            key,
            seat: Seat::Floor,
            popout: None,
            logical,
            scale,
            runner_seated: false,
        });
        key
    }

    /// The display zoom every window in this process draws at.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// **Move the display zoom — the whole window, not the content in it.**
    ///
    /// This is the one path, and it is what `ShellAction::Zoom` performs. Three
    /// things happen and they have to happen together, which is why they are
    /// not three public calls:
    ///
    /// 1. **Every window's layout viewport is re-derived** as its logical size
    ///    over the new zoom, and its regions are re-solved against that. The
    ///    caption, the tab lane, the panes and the content rect all move.
    /// 2. **Every window re-settles** — the context offer and the banner ladder
    ///    are re-walked against the caption they now have, exactly as a resize
    ///    does, and the active surface is re-bounded so it lays out for the
    ///    room it can actually show.
    /// 3. **Every surface is invalidated**, active or not. That is the same
    ///    reach `system_theme_changed` has, and for the same reason: what a
    ///    background tab has cached was measured against the old space.
    ///
    /// **Clamped, not snapped.** A stepping chord snaps before it asks
    /// ([`stepped_zoom`]); a Settings row offering 125% is a chosen value and
    /// keeps it. Both end up inside [`ZOOM_MIN`]..=[`ZOOM_MAX`].
    ///
    /// Answers whether anything moved.
    ///
    /// **And it arms the `120%` flash** ([`ZoomLabel`]) on every window, which is
    /// the one thing this function does that is not layout. Koru's indicator
    /// arms itself by edge-detecting the `theme::zoom()` global because its view
    /// sits below the level zoom lives at; the shell *is* that level, so the arm
    /// is a line here instead of a comparison in a draw pass — and the terminal,
    /// whose zoom never touches that global at all, gets the indicator by the
    /// same line rather than by a second mechanism.
    pub fn set_zoom(&mut self, mut engine: Option<&mut TextEngine>, next: f32) -> bool {
        if !next.is_finite() {
            return false;
        }
        let zoom = next.clamp(ZOOM_MIN, ZOOM_MAX);
        if zoom == self.zoom {
            return false;
        }
        self.zoom = zoom;
        // One reading of the clock for every window, so two windows flashing the
        // same step do not fade a frame apart.
        let now = Instant::now();
        let keys: Vec<WindowKey> = self.windows.iter().map(|window| window.key).collect();
        for key in keys {
            if let Some(window) = self.window_mut(key) {
                window.set_zoom(zoom);
                window.zoom_label.show(now);
            }
            self.settle_context(engine.as_deref_mut(), key);
            self.settle_banner(key);
            self.size_active(engine.as_deref_mut(), key);
            self.invalidate(key);
        }
        self.drain(engine);
        true
    }

    pub fn window_keys(&self) -> impl Iterator<Item = WindowKey> + '_ {
        self.windows.iter().map(|window| window.key)
    }

    pub fn window(&self, key: WindowKey) -> Option<&ShellWindow> {
        self.windows.iter().find(|window| window.key == key)
    }

    pub fn window_mut(&mut self, key: WindowKey) -> Option<&mut ShellWindow> {
        self.windows.iter_mut().find(|window| window.key == key)
    }

    pub fn tab_count(&self, key: WindowKey) -> usize {
        self.window(key).map_or(0, |window| window.tabs.len())
    }

    pub fn active_tab(&self, key: WindowKey) -> Option<TabKey> {
        self.window(key)?.active_key()
    }

    pub fn tab_title(&self, key: WindowKey, tab: TabKey) -> Option<&str> {
        self.window(key)?
            .tabs
            .iter()
            .find(|candidate| candidate.key == tab)
            .map(|candidate| candidate.title.as_str())
    }

    pub fn content_rect(&self, key: WindowKey) -> Option<Rect> {
        self.window(key).map(ShellWindow::content)
    }

    /// Close a window. The runner drops it on its next settle, after
    /// [`ShellHost::window_closing`] has had the live OS state.
    pub fn close_window(&mut self, key: WindowKey) {
        self.keys.retain(|candidate| *candidate != key);
        self.pending.retain(|pending| pending.key != key);
    }

    /// Every surface in this window, and only this one.
    ///
    /// The runner asks `system_theme_changed` of **every** window, so
    /// invalidating one window's surfaces here gives every surface in the
    /// process exactly one call — which is what SP-0's `invalidate_everywhere`
    /// did in a loop the runner now owns.
    pub fn invalidate(&mut self, key: WindowKey) {
        if let Some(window) = self.window_mut(key) {
            for tab in window.tabs.iter_mut() {
                if let Some(surface) = tab.surface.as_mut() {
                    surface.invalidate();
                }
            }
        }
    }

    // ------------------------------------------------------------ the surface

    /// Lend the active surface its context, run one thing, and keep whatever it
    /// recorded. **This is the only way a surface is reached.**
    fn with_active<R>(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        run: impl FnOnce(&mut Box<dyn Surface>, &mut SurfaceCx<'_>) -> R,
    ) -> Option<R> {
        let scale = self.window(key)?.scale;
        let mut commands = Commands::new(key);
        let answer = {
            let window = self.windows.iter_mut().find(|window| window.key == key)?;
            let index = window.tabs.active();
            let surface = window.tabs.get_mut(index)?.surface.as_mut()?;
            let mut cx = SurfaceCx {
                engine,
                scale,
                commands: &mut commands,
            };
            run(surface, &mut cx)
        };
        self.queued.extend(commands.drain());
        Some(answer)
    }

    fn size_active(&mut self, engine: Option<&mut TextEngine>, key: WindowKey) {
        let Some(bounds) = self.content_rect(key) else {
            return;
        };
        self.with_active(engine, key, |surface, cx| surface.set_bounds(cx, bounds));
    }

    /// Put the host's surface behind every floor tab the strip has minted.
    ///
    /// Called after every strip mutation and before anything can draw, so a tab
    /// with no surface is never observable. See [`ShellTab`] for why the two
    /// halves cannot be minted in one borrow.
    fn seat_floors(&mut self, mut engine: Option<&mut TextEngine>, key: WindowKey) {
        loop {
            let Some(flat) = self.window(key).and_then(|window| {
                window
                    .tabs
                    .position(|tab| tab.surface.is_none() && tab.key == TabKey::UNSEATED)
            }) else {
                return;
            };
            let surface = {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host.floor_surface(&mut cx, key)
            };
            // The same question `commit_seats` asks, and it has to be asked here
            // too: this is the floor the strip mints when the last tab closes,
            // and a Welcome tab that arrives that way is the same tab.
            let presentation = self.host.floor_tab(key);
            let tab_key = self.minter.tab();
            {
                let Some(window) = self.window_mut(key) else {
                    return;
                };
                let Some(tab) = window.tabs.get_mut(flat) else {
                    return;
                };
                tab.key = tab_key;
                tab.surface = Some(surface);
                tab.title = presentation.title;
                tab.closable = presentation.closable;
            }
            let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
            self.host.tab_opened(&mut cx, key, tab_key);
        }
    }

    // -------------------------------------------------------------- the tabs

    pub fn select_tab(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        tab: TabKey,
    ) -> bool {
        let Some(index) = self
            .window(key)
            .and_then(|window| window.tabs.position(|candidate| candidate.key == tab))
        else {
            return false;
        };
        self.select_index(engine, key, index)
    }

    fn select_index(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        index: usize,
    ) -> bool {
        let Some(window) = self.window(key) else {
            return false;
        };
        if window.tabs.active() == index {
            return false;
        }
        let previous = window.active_key();
        // **Deactivate before activate.** ADR-263 needs an unfinished IME
        // preedit cancelled before the destination attaches, so composition
        // reaches neither terminal.
        self.with_active(engine.as_deref_mut(), key, |surface, cx| {
            surface.deactivated(cx)
        });
        let moved = self
            .window_mut(key)
            .is_some_and(|window| window.tabs.select(index));
        self.size_active(engine.as_deref_mut(), key);
        self.with_active(engine.as_deref_mut(), key, |surface, cx| {
            surface.activated(cx)
        });
        self.announce_activation(engine, key, previous);
        moved
    }

    /// Tell the host which tab is now showing, once the surfaces have settled.
    ///
    /// **After both surface hooks**, so a host attaching a controller here can
    /// rely on the outgoing surface's preedit already being cancelled — the
    /// contract `Surface::deactivated` states, and the reason S16 holds.
    fn announce_activation(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        previous: Option<TabKey>,
    ) {
        let Some(active) = self.active_tab(key) else {
            return;
        };
        if previous == Some(active) {
            return;
        }
        {
            let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
            self.host.tab_activated(&mut cx, key, active, previous);
        }
        // **The one place a tab switch changes the chrome.** Whether Details has
        // content is a fact about the active tab, so the offer is re-asked here
        // rather than waiting for the next frame to notice — a press between the
        // switch and that frame would otherwise hit-test against the old rects.
        //
        // **After `tab_activated`, and it has to be**: the host learns which tab
        // is active there, and the offer is a question about that. It re-sizes
        // the surface itself when the answer moves, which is why the caller's
        // earlier `size_active` is not the last word.
        self.settle_context(engine, key);
    }

    /// Ctrl+Tab, resolved from `physical_key` and handed down as a direction —
    /// never as a chord in anyone's key map.
    pub fn cycle_tab(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        delta: i32,
    ) -> bool {
        let Some(window) = self.window(key) else {
            return false;
        };
        let count = window.tabs.len() as i32;
        if count <= 1 {
            return false;
        }
        let next = (window.tabs.active() as i32 + delta).rem_euclid(count) as usize;
        self.select_index(engine, key, next)
    }

    pub fn close_tab(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        tab: TabKey,
    ) -> bool {
        let Some(index) = self
            .window(key)
            .and_then(|window| window.tabs.position(|candidate| candidate.key == tab))
        else {
            return false;
        };
        let was_active = self
            .window(key)
            .is_some_and(|window| window.tabs.active() == index);
        let previous = self.active_tab(key);
        if was_active {
            self.with_active(engine.as_deref_mut(), key, |surface, cx| {
                surface.deactivated(cx)
            });
        }
        let closed = self
            .window_mut(key)
            .is_some_and(|window| window.tabs.close(index));
        if !closed {
            return false;
        }
        if let Some(window) = self.window_mut(key) {
            // Adding, closing or restoring tabs renumbers the list, at which
            // point an offset recorded against an old index would be applied to
            // a different tab.
            window.slides.clear();
        }
        {
            let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
            self.host.tab_closed(&mut cx, key, tab);
        }
        self.seat_floors(engine.as_deref_mut(), key);
        self.size_active(engine.as_deref_mut(), key);
        self.with_active(engine.as_deref_mut(), key, |surface, cx| {
            surface.activated(cx)
        });
        self.announce_activation(engine, key, previous);
        true
    }

    fn open_tab(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        title: String,
        closable: bool,
        surface: Box<dyn Surface>,
    ) {
        let tab_key = self.minter.tab();
        let tab = ShellTab::content(tab_key, title, closable, surface);
        let previous = self.active_tab(key);
        self.with_active(engine.as_deref_mut(), key, |surface, cx| {
            surface.deactivated(cx)
        });
        if let Some(window) = self.window_mut(key) {
            window.tabs.push(tab);
            window.slides.clear();
        }
        {
            let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
            self.host.tab_opened(&mut cx, key, tab_key);
        }
        self.seat_floors(engine.as_deref_mut(), key);
        self.size_active(engine.as_deref_mut(), key);
        self.with_active(engine.as_deref_mut(), key, |surface, cx| {
            surface.activated(cx)
        });
        self.announce_activation(engine, key, previous);
    }

    // ----------------------------------------------------- the context column

    /// **Ask the host whether the context column is offered**, and take the
    /// answer. Answers whether it moved.
    ///
    /// Cheap by contract — one `bool` off the host's own model — because it is
    /// asked wherever the answer can have changed rather than being pushed. The
    /// alternative was a `ShellAction` the host records when its active tab
    /// changes, which is a second place the truth lives and one more thing a
    /// host can forget; `banner` is already a pull for the same reason.
    ///
    /// The caption's arithmetic depends on it (the toggle is a slot in the
    /// trailing chain), so a change re-walks the collapse ladder — and the
    /// content rect moves by the column's width, so the active surface is told
    /// its new size. A surface that was not would keep laying out against the
    /// width it last heard: for a terminal that is a PTY still 268 px wide
    /// inside a view that is not.
    fn settle_context(&mut self, engine: Option<&mut TextEngine>, key: WindowKey) {
        let offered = self.host.context_offered(key);
        let Some(window) = self.window_mut(key) else {
            return;
        };
        if window.context == offered {
            return;
        }
        window.context = offered;
        window.resolve();
        self.settle_banner(key);
        self.size_active(engine, key);
    }

    // ------------------------------------------------------------ the banner

    /// Re-walk the collapse ladder against the settled caption.
    ///
    /// Called when the caption changes or the banner's content does — **never
    /// per animation frame**, which is DESIGN's rule and the reason the ladder
    /// reads a cached label width rather than measuring.
    pub fn settle_banner(&mut self, key: WindowKey) -> bool {
        let Some(window) = self.window(key) else {
            return false;
        };
        let caption = window.regions.caption;
        let (window_buttons, context) = (window.window_buttons, window.context);
        // The band is `None` — not a zero-width rect — for a host that keeps its
        // identity in the caption, because "shut" and "not here at all" are two
        // different arrangements and the ladder answers them differently.
        let pane_header = self
            .host
            .identity_in_pane(key)
            .then(|| window.pane_header());
        let offered = self.host.banner(key);
        let Some(window) = self.window_mut(key) else {
            return false;
        };
        window
            .banner
            .settle(offered, caption, pane_header, window_buttons, context)
    }

    /// Which banner slot a window-space point is in, asking the pane header band
    /// before the caption.
    ///
    /// Nothing overlaps — the band is inside `regions.sidebar` and the caption
    /// starts where the sidebar ends — so the order is only a precedence for the
    /// pixel they share at `x == sidebar.w`, where the caption's own rule already
    /// says the caption wins. Both use the **settled** rects.
    fn banner_hit(&self, key: WindowKey, x: f32, y: f32) -> Option<BannerSlot> {
        let window = self.window(key)?;
        let header = window.pane_header();
        if header.w > 0.0
            && let Some(hit) = banner::pane_hit(header, window.banner.pane_slots(), x, y)
        {
            return Some(window.banner.pane_slot_for(hit));
        }
        let hit = banner::hit(
            window.regions.caption,
            window.window_buttons,
            window.banner.slots(),
            x,
            y,
        )?;
        Some(window.banner.slot_for(hit))
    }

    /// **The square that actually carries a list.**
    ///
    /// A press already names the square it landed on, so this is a no-op for it.
    /// A *chord* names the list instead — `Ctrl+Alt+W` means "the workspaces",
    /// not "the second chip" — and from ladder step 3 down there is no second
    /// chip: both lists live under one merged identity square. Without this the
    /// chord would anchor its panel to a zero-width rect at the band's left
    /// edge, which is a popover hanging off nothing.
    fn carrier(&self, key: WindowKey, slot: BannerSlot) -> BannerSlot {
        if !matches!(slot, BannerSlot::Profile | BannerSlot::Workspace) {
            return slot;
        }
        let Some(window) = self.window(key) else {
            return slot;
        };
        let merged = if window.pane_header().w > 0.0 {
            window.banner.pane_merged()
        } else {
            window.banner.caption_merged()
        };
        if merged { BannerSlot::Identity } else { slot }
    }

    fn open_popover(&mut self, key: WindowKey, slot: BannerSlot) {
        let slot = self.carrier(key, slot);
        let Some(window) = self.window(key) else {
            return;
        };
        let Some(anchor) = self.slot_rect(key, slot) else {
            return;
        };
        // **The viewport is what the popover must not land over.** For a caption
        // chip that is the caption, so the content rect is the clamp. For a chip
        // in the pane header the thing to clear is the band, and the column below
        // it is exactly where the popover belongs — clamping to the content rect
        // would throw it across the divider, away from the chip that opened it.
        let content = if self.pane_header_slot(key, slot) {
            let viewport = window.viewport();
            let below = window.pane_header().h;
            Rect::new(0.0, below, viewport.w, (viewport.h - below).max(0.0))
        } else {
            window.regions.content
        };
        let size = self.host.popover_size(key, slot);
        if size.0 <= 0.0 || size.1 <= 0.0 {
            return;
        }
        if let Some(window) = self.window_mut(key) {
            window.popover = Some(Popover::open(slot, anchor, content, size));
        }
    }

    /// **The host swapped the open panel's view.** Re-ask the size — the host
    /// has already switched, so the answer is the new view's — and slide.
    ///
    /// A no-op with nothing open, which is not defensive: a host can queue this
    /// from a press that the same drain later dismisses, and the panel that
    /// would have animated is gone.
    fn slide_popover(&mut self, key: WindowKey, from: SlideFrom) {
        let Some(slot) = self
            .window(key)
            .and_then(ShellWindow::popover)
            .map(|popover| popover.slot)
        else {
            return;
        };
        let size = self.host.popover_size(key, slot);
        if size.0 <= 0.0 || size.1 <= 0.0 {
            return;
        }
        let now = Instant::now();
        if let Some(popover) = self
            .window_mut(key)
            .and_then(|window| window.popover.as_mut())
        {
            popover.slide_to(size, from, now);
        }
    }

    // --------------------------------------------------------------- pointer

    /// One pointer event. Chrome claims first, and what it claims never reaches
    /// a surface.
    fn pointer(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        event: PointerEvent,
    ) -> bool {
        // **The host's overlays outrank everything, including the chrome.**
        // That is what makes a modal modal: the terminate confirmation has to
        // win the click over the grid beneath it, and it can only do that from
        // above.
        let claimed = {
            let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
            self.host.overlay_pointer(&mut cx, key, event)
        };
        if claimed {
            self.drain(engine);
            return true;
        }
        let Some((x, y)) = event.position() else {
            let cleared = self
                .window_mut(key)
                .is_some_and(|window| window.clear_hover());
            let changed = self
                .with_active(engine.as_deref_mut(), key, |surface, cx| {
                    surface.pointer(cx, event)
                })
                .unwrap_or(false);
            self.drain(engine);
            return changed || cleared;
        };
        let pressed = matches!(event, PointerEvent::Pressed { .. });
        let moved = self.track_hover(key, x, y);

        // **A resize drag outranks everything below the overlays**, because
        // while one is in flight the pointer is not "over" anything: it is
        // holding an edge, and letting a move under it reach a pane's rows
        // would light them up as the column ran past them.
        if let Some(changed) = self.pane_resize(engine.as_deref_mut(), key, event, x, y) {
            self.drain(engine);
            return changed;
        }

        // An open popover eats the press that dismisses it, and the shell — not
        // the host — decides which press that is.
        if pressed && let Some(popover) = self.window(key).and_then(ShellWindow::popover) {
            let changed = if popover.contains(x, y) {
                // **A panel mid-swap is deaf too.** DESIGN's rule for a
                // dismissed popover — it is on screen for the length of its
                // fade and must not answer a click that lands on a menu which
                // is visibly leaving — is the same rule here: half the rows
                // under the pointer belong to a view that is going, and the
                // other half are not where they will be. The panel still eats
                // the press, so it does not fall through to the column under
                // it either.
                if popover.animating() {
                    true
                } else {
                    let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                    self.host.popover_pressed(&mut cx, key, popover.slot, x, y)
                }
            } else {
                if let Some(window) = self.window_mut(key) {
                    window.popover = None;
                }
                true
            };
            self.drain(engine);
            return changed;
        }

        // **And an open popover claims every other pointer event over its own
        // panel.** The press above is only half of it: a move has to light the
        // panel's rows rather than the column underneath — the panel floats over
        // the pane it came from, so without this the row *behind* the popover
        // highlights — and a wheel has to scroll the panel's list rather than
        // that column's. What the host does with the wheel is the host's; that
        // the pane never sees it is the shell's.
        if let Some(popover) = self.window(key).and_then(ShellWindow::popover)
            && popover.contains(x, y)
        {
            let scrolled = if let PointerEvent::Wheel { lines, .. } = event {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host
                    .popover_scrolled(&mut cx, key, popover.slot, lines)
            } else {
                false
            };
            self.drain(engine);
            // A move over the panel repaints whether or not anything else did:
            // the host draws its rows' hover from the pointer the shell has just
            // recorded, and a highlight that only appears on the next unrelated
            // frame is a highlight that lags the pointer.
            return scrolled || moved || matches!(event, PointerEvent::Moved { .. });
        }

        if pressed && let Some(slot) = self.banner_hit(key, x, y) {
            // A disabled chip is quiet, not hidden (S9): it stays on screen,
            // dimmed, carrying the reason in its tooltip — and it does not
            // open. Refusing the press *here* rather than in the host is what
            // keeps every host's disabled chip behaving the same way.
            if !self.slot_enabled(key, slot) {
                return moved;
            }
            let changed = {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host.slot_pressed(&mut cx, key, slot)
            };
            self.drain(engine);
            return changed;
        }
        if pressed && let Some(hit) = self.chrome_hit(key, x, y) {
            let changed = self.perform_chrome(engine.as_deref_mut(), key, hit);
            self.drain(engine);
            return changed || moved;
        }
        // A pane claims the pointer over its own column, in window
        // coordinates — the pane travels with its slide, and a surface-local
        // point would move under the pointer mid-animation.
        if let Some(side) = self.pane_at(key, x, y) {
            let changed = {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host.pane_pointer(&mut cx, key, side, event)
            };
            self.drain(engine);
            return changed || moved;
        }
        let Some(content) = self.content_rect(key) else {
            return moved;
        };
        if pressed && !content.contains(x, y) {
            return moved;
        }
        let local = event.translated(-content.x, -content.y);
        let changed = self
            .with_active(engine.as_deref_mut(), key, |surface, cx| {
                surface.pointer(cx, local)
            })
            .unwrap_or(false);
        self.drain(engine);
        changed || moved
    }

    /// Resolve what the pointer is over, once, by the arithmetic the press will
    /// use. Answers whether the highlight moved.
    fn track_hover(&mut self, key: WindowKey, x: f32, y: f32) -> bool {
        let banner_hover = self.banner_hit(key, x, y);
        let hit = self
            .chrome_hit(key, x, y)
            .filter(|_| banner_hover.is_none());
        let tooltip = banner_hover.and_then(|slot| {
            let anchor = self.slot_rect(key, slot)?;
            chrome::tooltip(&self.host.banner(key), slot, anchor)
        });
        let Some(window) = self.window_mut(key) else {
            return false;
        };
        window.pointer = (x, y);
        let mut moved = window.hover != hit || window.banner_hover != banner_hover;
        window.hover = hit;
        window.banner_hover = banner_hover;
        let content = tooltip.map(|text| {
            TooltipContent::new(
                match banner_hover {
                    Some(BannerSlot::Workspace) => 2,
                    Some(BannerSlot::Settings) => 3,
                    _ => 1,
                },
                text.anchor,
                text.text,
            )
        });
        moved |= window.tooltip.set_hover(content, Instant::now());
        moved
    }

    /// Whether the chip in this slot may be opened.
    fn slot_enabled(&self, key: WindowKey, slot: BannerSlot) -> bool {
        let offered = self.host.banner(key);
        match slot {
            // The merged square opens *both* sections, so it is offered
            // whenever either half is — a window that could reach neither is a
            // window somebody has to close.
            BannerSlot::Identity => {
                offered.profile.as_ref().is_some_and(|chip| chip.enabled)
                    || offered.workspace.as_ref().is_some_and(|chip| chip.enabled)
            }
            BannerSlot::Profile => offered.profile.as_ref().is_some_and(|chip| chip.enabled),
            BannerSlot::Workspace => offered.workspace.as_ref().is_some_and(|chip| chip.enabled),
            BannerSlot::Settings => true,
        }
    }

    /// Whether this slot is showing in the pane header band rather than the
    /// caption. Asked of the band's own slots, so it is `false` the moment the
    /// pane shuts and the merged square moves back into the caption.
    fn pane_header_slot(&self, key: WindowKey, slot: BannerSlot) -> bool {
        let Some(window) = self.window(key) else {
            return false;
        };
        let pane = window.banner.pane_slots();
        match slot {
            BannerSlot::Profile | BannerSlot::Identity => pane.profile,
            BannerSlot::Workspace => pane.workspace != banner::Workspace::Hidden,
            BannerSlot::Settings => false,
        }
    }

    /// Where a slot is, in whichever band it is showing. **The settled rect** —
    /// what the tooltip anchors to and what a popover hangs from, the same rule
    /// hit-testing follows.
    fn slot_rect(&self, key: WindowKey, slot: BannerSlot) -> Option<Rect> {
        let window = self.window(key)?;
        if self.pane_header_slot(key, slot) {
            let band = window.pane_header();
            let slots = window.banner.pane_slots();
            return Some(match slot {
                BannerSlot::Workspace => banner::pane_workspace_slot(band, slots),
                _ => banner::pane_profile_slot(band, slots),
            });
        }
        let caption = window.regions.caption;
        let slots = window.banner.slots();
        Some(match slot {
            BannerSlot::Profile | BannerSlot::Identity => banner::profile_slot(caption, slots),
            BannerSlot::Workspace => banner::workspace_slot(caption, slots),
            BannerSlot::Settings => banner::settings_slot(caption, window.window_buttons, slots),
        })
    }

    /// **The pane's resize grip: the hit, the drag and the commit.**
    ///
    /// `None` means this event was not the grip's and the caller carries on;
    /// `Some(changed)` means it was claimed.
    ///
    /// Three moments, and the middle one is the whole point:
    ///
    /// - **press** on the band — resolved here rather than in `chrome_hit` or
    ///   `pane_at`, because the band straddles the column's edge and half of it
    ///   is over the content, which either of those would answer for first;
    /// - **drag** moves the column's *drawn* width only. Nothing reflows, and
    ///   nothing else in the window is told, which is `Panel`'s
    ///   `full`/`committed` split doing the job it exists for;
    /// - **release** commits: the layout width catches up, the window is
    ///   re-cut, the banner re-walks its ladder against the new band, and the
    ///   active surface is sized **once**. Then the host is told, so a width
    ///   that reaches a file reaches it on release rather than sixty times a
    ///   second.
    ///
    /// An open popover refuses the press — it is anchored to the band the drag
    /// would move, and a panel that stayed put while its own chip slid away is
    /// a panel hanging off nothing.
    fn pane_resize(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        event: PointerEvent,
        x: f32,
        y: f32,
    ) -> Option<bool> {
        let held = self.window(key)?.pane_drag;
        match event {
            PointerEvent::Pressed {
                button: MouseButton::Left,
                ..
            } if held.is_none() => {
                if self
                    .window(key)
                    .is_some_and(|window| window.popover.is_some())
                {
                    return None;
                }
                let side = [Side::Left, Side::Right].into_iter().find(|side| {
                    self.window(key)
                        .is_some_and(|window| window.pane_grip(*side).contains(x, y))
                })?;
                let window = self.window_mut(key)?;
                window.pane_drag = Some((side, x - window.pane(side).full()));
                Some(true)
            }
            PointerEvent::Moved { .. } | PointerEvent::Dragged { .. } => {
                let (side, grab) = held?;
                let window = self.window_mut(key)?;
                Some(window.set_pane_width(side, x - grab))
            }
            PointerEvent::Released { .. } => {
                let (side, _) = held?;
                let window = self.window_mut(key)?;
                window.pane_drag = None;
                let moved = window.commit_pane_width(side);
                let width = window.pane(side).full();
                if moved {
                    self.settle_banner(key);
                    self.size_active(engine, key);
                }
                self.host.pane_resized(key, side, width);
                Some(true)
            }
            _ => None,
        }
    }

    /// Which pane a window-space point is in, if either. Zero-width columns are
    /// excluded explicitly, because `Rect::contains` is inclusive on every edge.
    fn pane_at(&self, key: WindowKey, x: f32, y: f32) -> Option<Side> {
        let window = self.window(key)?;
        if window.regions.sidebar.w > 0.0 && window.regions.sidebar.contains(x, y) {
            return Some(Side::Left);
        }
        if window.regions.context.w > 0.0 && window.regions.context.contains(x, y) {
            return Some(Side::Right);
        }
        None
    }

    fn chrome_hit(&self, key: WindowKey, x: f32, y: f32) -> Option<regions::Hit> {
        let window = self.window(key)?;
        let hit = regions::hit_with_banner(
            &window.regions,
            &window.widths(),
            window.tab_scroll,
            window.window_buttons,
            window.context,
            window.host_action,
            true,
            window.banner.slots(),
            x,
            y,
        );
        // **A pane is not chrome, and neither is the content.** `regions::hit`
        // answers `Sidebar` and `Context` because it resolves the whole window;
        // the shell owns the *slot* and the host owns what is in it, so both
        // fall through to `pane_at` rather than being claimed here. Claiming
        // them was what made a pane's rows unpressable.
        (!matches!(
            hit,
            regions::Hit::Content | regions::Hit::Sidebar | regions::Hit::Context
        ))
        .then_some(hit)
    }

    fn perform_chrome(
        &mut self,
        mut engine: Option<&mut TextEngine>,
        key: WindowKey,
        hit: regions::Hit,
    ) -> bool {
        match hit {
            regions::Hit::Tab(index) => self.select_index(engine.as_deref_mut(), key, index),
            regions::Hit::TabClose(index) => {
                let Some(tab) = self
                    .window(key)
                    .and_then(|window| window.tabs.get(index))
                    // A tab that is not closable has no ✕ to press, and the
                    // rect is still there — `tabs::layout` gives every tab
                    // one — so the refusal has to be here.
                    .filter(|tab| tab.closable)
                    .map(|tab| tab.key)
                else {
                    return false;
                };
                self.close_tab(engine, key, tab)
            }
            regions::Hit::NewTab => {
                let anchor = self
                    .new_tab_rect(key)
                    .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host.new_tab_pressed(&mut cx, key, anchor)
            }
            regions::Hit::ToggleSidebar => {
                self.toggle_pane_with(engine.as_deref_mut(), key, Side::Left)
            }
            regions::Hit::ToggleContext => {
                self.toggle_pane_with(engine.as_deref_mut(), key, Side::Right)
            }
            regions::Hit::Window(action) => {
                self.perform_window_action(engine, key, action);
                true
            }
            _ => false,
        }
    }

    /// Where the `+` sits, for a menu to hang from.
    fn new_tab_rect(&self, key: WindowKey) -> Option<Rect> {
        let window = self.window(key)?;
        Some(tabs::new_tab(
            window.strip(),
            &window.widths(),
            window.tab_scroll,
        ))
    }

    fn perform_window_action(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        action: koru_ui::caption::Action,
    ) {
        use koru_ui::caption::Action;
        let request = match action {
            Action::Minimise => WindowRequest::Minimise,
            Action::ToggleMaximise => WindowRequest::ToggleMaximise,
            Action::Close => {
                // Through the same path Alt+F4 takes, so a surface that must
                // detach before the window goes still gets its say.
                <Self as MultiWindowApp>::close_requested(self, key);
                return;
            }
            // The overflow menu is the host's: what is in it is product, and a
            // shell that opened one would be a shell with a menu model. What
            // the shell can supply is where it hangs from.
            Action::WindowMenu => {
                let anchor = self
                    .window(key)
                    .and_then(|window| {
                        koru_ui::caption::controls(window.drawn_caption())
                            .into_iter()
                            .find(|control| control.action == Action::WindowMenu)
                            .map(|control| control.rect)
                    })
                    .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
                let mut cx = HostCx::new(engine, &mut self.queued);
                self.host.window_menu_pressed(&mut cx, key, anchor);
                return;
            }
        };
        if let Some(window) = self.window_mut(key) {
            window.request = Some(request);
        }
    }

    /// Toggle a pane and re-walk the ladder, **once**: the layout width is final
    /// the instant the pane is toggled, so the settled caption is available
    /// immediately and the banner never re-measures mid-slide.
    ///
    /// The content rect moved by the column's width, so the active surface is
    /// told — once, here, rather than sixty times through the slide, which is
    /// the whole of `Panel`'s opening argument.
    pub fn toggle_pane(&mut self, key: WindowKey, side: Side) -> bool {
        self.toggle_pane_with(None, key, side)
    }

    /// The same, with the engine the dispatch is carrying — a surface that has
    /// to re-measure text against its new width needs one.
    fn toggle_pane_with(
        &mut self,
        engine: Option<&mut TextEngine>,
        key: WindowKey,
        side: Side,
    ) -> bool {
        let now = Instant::now();
        let Some(window) = self.window_mut(key) else {
            return false;
        };
        window.pane(side).toggle(now);
        window.resolve();
        self.settle_banner(key);
        self.size_active(engine, key);
        true
    }

    // ---------------------------------------------------------------- drawing

    /// **The frame, in the order the layers sit.**
    ///
    /// Background, the active surface, the panes, the caption, the popover, the
    /// host's overlays. The panes come after the surface because a sliding
    /// column draws over the content whose space it has already taken
    /// ([`Panel`]), and the caption comes after both because its own left edge
    /// is sliding too.
    fn draw_window(&mut self, key: WindowKey, scene: &mut Scene, engine: &mut TextEngine) {
        {
            let mut cx = HostCx::new(Some(engine), &mut self.queued);
            self.host.frame(&mut cx, key);
        }
        // The offer can move on anything the host was told about — a status
        // event, a project change — not only on a tab switch, and all of those
        // end in a frame. One `bool` per window per frame, in the pull idiom
        // `banner` already uses.
        self.settle_context(Some(engine), key);
        // Measure at draw, cache for hit. The engine is never on the hit path.
        let titles: Vec<(usize, String, bool)> = self
            .window(key)
            .into_iter()
            .flat_map(|window| window.tabs.iter().enumerate())
            .map(|(index, tab)| (index, tab.title.clone(), tab.closable))
            .collect();
        for (index, title, closable) in titles {
            let width = tabs::preferred_width(engine, &title, false, closable);
            if let Some(window) = self.window_mut(key)
                && let Some(tab) = window.tabs.get_mut(index)
            {
                tab.width = width;
            }
        }
        let offered = self.host.banner(key);
        let measured = offered
            .workspace
            .as_ref()
            .filter(|chip| !chip.label.is_empty())
            .map(|chip| {
                engine
                    .simple_layout(&chip.label, theme::role::row())
                    .width()
            })
            .unwrap_or_default();
        let moved = self
            .window_mut(key)
            .is_some_and(|window| window.banner.measured(measured));
        if moved {
            self.settle_banner(key);
        }
        let Some(window) = self.window(key) else {
            return;
        };
        let viewport = window.viewport();
        let bounds = window.regions.content;
        // **The display zoom, and the only place it is applied.** Everything
        // below draws in the window's layout space; this multiplies the lot.
        let root = window.root();
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.bg,
            &scene::rect(0.0, 0.0, viewport.w, viewport.h),
        );

        let inner = Affine::translate((f64::from(bounds.x), f64::from(bounds.y)));
        let local = Rect::new(0.0, 0.0, bounds.w, bounds.h);
        self.with_active(Some(engine), key, |surface, cx| {
            surface.draw(cx, scene, root * inner, local)
        });

        for side in [Side::Left, Side::Right] {
            let Some(pane) = self.window(key).map(|window| window.drawn_pane(side)) else {
                continue;
            };
            if pane.w <= 0.0 {
                continue;
            }
            // The same token the caption band wears, and Koru's explore pane
            // with it. See [`chrome::chrome_background`] for why it is not
            // `panel_bg`: `panel_bg` is what sits *on* a column, not what a
            // column is.
            scene::fill(
                scene,
                root,
                chrome::chrome_background(),
                &scene::rect(pane.x, pane.y, pane.w, pane.h),
            );
            // The hairline goes on the edge that faces the content, which is the
            // pane's right for the left column and its left for the right one —
            // a divider on the window's outer edge is a line against nothing.
            let divider = match side {
                Side::Left => pane.x + pane.w - 1.0,
                Side::Right => pane.x,
            };
            scene::fill(
                scene,
                root,
                p.panel_border,
                &scene::rect(divider, pane.y, 1.0, pane.h),
            );
            let mut cx = HostCx::new(Some(engine), &mut self.queued);
            self.host.draw_pane(&mut cx, scene, root, key, side, pane);
            // **After the host, because the band is the shell's.** The column's
            // rows are product; the identity chips in its header are chrome,
            // wearing the same four states and the same tint hash as the
            // caption's (DESIGN, "The banner" §0).
            if side == Side::Left {
                self.draw_pane_header(key, scene, engine, &offered);
            }
        }

        self.draw_caption(key, scene, engine, &offered);

        // The popover is the shell's panel with the host's rows in it.
        if let Some(popover) = self.window(key).and_then(ShellWindow::popover) {
            popover.draw(scene, root);
            let frame = PopoverFrame {
                bounds: popover.bounds(),
                travel: popover.view_offsets(),
            };
            let slot = popover.slot;
            let mut cx = HostCx::new(Some(engine), &mut self.queued);
            self.host
                .draw_popover(&mut cx, scene, root, key, slot, frame);
        }

        if let Some(window) = self.window_mut(key) {
            window
                .tooltip
                .draw(scene, root, engine, viewport, Instant::now());
        }

        let mut cx = HostCx::new(Some(engine), &mut self.queued);
        self.host.draw_overlay(&mut cx, scene, root, key, viewport);

        // **Last, so it is above everything** — the host's overlay included.
        // There is no layer mechanism here, only draw order, which is the same
        // convention Koru's `draw.rs` states at the top of itself. A readout
        // saying what the window's zoom now is has to be legible over whatever
        // the zoom just resized.
        if let Some(window) = self.window(key)
            && window.zoom_label.showing()
        {
            let zoom = window.zoom();
            let label = window.zoom_label;
            label.draw(scene, root, engine, viewport, zoom, Instant::now());
        }
    }

    /// The left pane's header band: its hairline and the identity chips.
    ///
    /// Draws nothing at all for a host that keeps its identity in the caption.
    /// The band exists for a host that answers `identity_in_pane`, and it draws
    /// its hairline whether or not there is a chip to put in it — a band that
    /// appeared only once a profile had been chosen would be a column whose top
    /// edge moved when the Gateway answered.
    fn draw_pane_header(
        &mut self,
        key: WindowKey,
        scene: &mut Scene,
        engine: &mut TextEngine,
        offered: &crate::host::Banner,
    ) {
        if !self.host.identity_in_pane(key) {
            return;
        }
        let Some(window) = self.window(key) else {
            return;
        };
        let slots = window.banner.pane_slots();
        chrome::draw_pane_header(
            scene,
            window.root(),
            engine,
            &chrome::PaneHeader {
                band: window.drawn_pane_header(),
                banner: offered,
                slots,
                merged: window.banner.pane_merged(),
                states: chrome::ChipStates {
                    hover: window.banner_hover,
                    open: window.popover.map(|popover| popover.slot),
                    pressed: None,
                },
            },
        );
    }

    /// The caption band, its furniture, the lane and the window buttons.
    fn draw_caption(
        &mut self,
        key: WindowKey,
        scene: &mut Scene,
        engine: &mut TextEngine,
        offered: &crate::host::Banner,
    ) {
        // Asked before the window is borrowed, because the host is `self`'s too
        // and the borrow below holds until the state is built.
        let settings_open = self.host.settings_open(key);
        let Some(window) = self.window(key) else {
            return;
        };
        let root = window.root();
        let caption = window.drawn_caption();
        chrome::draw_band(scene, root, caption);
        let state = chrome::CaptionState {
            caption,
            banner: offered,
            slots: window.banner.slots(),
            merged: window.banner.caption_merged(),
            window_buttons: window.window_buttons,
            context: window.context,
            host_action: window.host_action,
            maximised: window.maximised,
            settings_open,
            hover: window.hover,
            banner_hover: window.banner_hover,
            open: window.popover.map(|popover| popover.slot),
            pressed: None,
        };
        chrome::draw_banner(scene, root, engine, &state);

        let widths = window.widths();
        let scroll = window.tab_scroll;
        let active = window.tabs.active();
        let strip = caption::tab_strip_with_banner(
            caption,
            window.window_buttons,
            window.context,
            window.host_action,
            window.banner.slots(),
        );
        let hovered_tab = match window.hover {
            Some(regions::Hit::Tab(index)) | Some(regions::Hit::TabClose(index)) => Some(index),
            _ => None,
        };
        let hovered_close = match window.hover {
            Some(regions::Hit::TabClose(index)) => Some(index),
            _ => None,
        };
        let hovered_plus = window.hover == Some(regions::Hit::NewTab);
        let labels: Vec<(String, bool)> = window
            .tabs
            .iter()
            .map(|tab| (tab.title.clone(), tab.closable))
            .collect();
        let items: Vec<tabs::Tab<'_>> = labels
            .iter()
            .enumerate()
            .map(|(index, (label, closable))| tabs::Tab {
                label: label.as_str(),
                icon: None,
                closable: *closable,
                slide: window.slides.offset(index),
                dragging: false,
                // The shell's strip has no pin group, so no gap to keep out
                // of a chip.
                gap_after: 0.0,
                status: None,
            })
            .collect();
        tabs::draw(
            scene,
            root,
            engine,
            strip,
            &items,
            &widths,
            scroll,
            active,
            hovered_tab,
            hovered_close,
            hovered_plus,
            false,
            false,
        );
        if window.window_buttons {
            let hovered = match window.hover {
                Some(regions::Hit::Window(action)) => Some(action),
                _ => None,
            };
            koru_ui::caption::draw(
                scene,
                root,
                engine,
                caption,
                koru_ui::caption::State {
                    hovered,
                    maximized: window.maximised,
                    menu: koru_ui::caption::MenuVisual::Static,
                },
            );
        }
    }

    // -------------------------------------------------------------- the drain

    /// Perform everything recorded since the last drain.
    ///
    /// Called after every dispatch and never during one, which is what stops a
    /// surface reaching back into the shell mid-borrow. A command a command
    /// produced is queued for the next pass rather than recursed into, and the
    /// loop is bounded — see [`DRAIN_PASSES`].
    fn drain(&mut self, mut engine: Option<&mut TextEngine>) {
        let mut guard = 0;
        while !self.queued.is_empty() {
            guard += 1;
            assert!(guard < DRAIN_PASSES, "shell commands did not settle");
            for command in std::mem::take(&mut self.queued) {
                self.perform(engine.as_deref_mut(), command);
            }
        }
    }

    fn perform(&mut self, mut engine: Option<&mut TextEngine>, command: ShellCommand) {
        let key = command.window;
        match command.action {
            // `Redraw` is the caller's answer, not work: the runner is told a
            // frame is owed by the bool every dispatch already returns.
            ShellAction::Redraw => {}
            ShellAction::Window(WindowRequest::Close) => self.close_window(key),
            ShellAction::Window(request) => {
                if let Some(window) = self.window_mut(key) {
                    window.request = Some(request);
                }
            }
            ShellAction::OpenTab {
                title,
                closable,
                surface,
            } => {
                self.open_tab(engine.as_deref_mut(), key, title, closable, surface);
            }
            ShellAction::SelectTab(tab) => {
                self.select_tab(engine.as_deref_mut(), key, tab);
            }
            ShellAction::CloseTab(tab) => {
                self.close_tab(engine, key, tab);
            }
            ShellAction::SetTabTitle { tab, title } => {
                if let Some(window) = self.window_mut(key)
                    && let Some(target) = window
                        .tabs
                        .iter_mut()
                        .find(|candidate| candidate.key == tab)
                {
                    target.title = title;
                }
            }
            ShellAction::OpenWindow => {
                self.ask_for_window(key, None);
            }
            ShellAction::PopOutTab(tab) => {
                self.ask_for_window(key, Some((key, tab)));
            }
            ShellAction::HostRequest(request) => {
                let mut cx = HostCx::new(engine, &mut self.queued);
                self.host.host_request(&mut cx, key, request);
            }
            ShellAction::TogglePane(side) => {
                self.toggle_pane_with(engine, key, side);
            }
            // The window the command named is deliberately unused: a zoom moves
            // every window. See [`ShellAction::Zoom`].
            ShellAction::Zoom(zoom) => {
                self.set_zoom(engine, zoom);
            }
            ShellAction::Popover(slot) => match slot {
                Some(slot) => self.open_popover(key, slot),
                None => {
                    if let Some(window) = self.window_mut(key) {
                        window.popover = None;
                    }
                }
            },
            ShellAction::PopoverSlide(from) => self.slide_popover(key, from),
        }
    }

    fn ask_for_window(&mut self, source: WindowKey, popout: Option<(WindowKey, TabKey)>) {
        // The source's **logical** size, not its zoomed-down viewport: a new
        // window is asked for at the size of the glass, and applies the zoom
        // itself when it is built.
        let (logical, scale) = self
            .window(source)
            .map(|window| (window.logical, window.scale))
            .unwrap_or((MIN_WINDOW_SIZE, 1.0));
        let key = self.minter.window();
        self.keys.push(key);
        self.pending.push(PendingWindow {
            key,
            seat: Seat::Host,
            popout,
            logical,
            scale,
            runner_seated: false,
        });
    }

    /// Ask the host to seat every window the runner has opened.
    ///
    /// **Phase two of two-phase pop-out**, and it is the runner's own commit
    /// point: `seated` fires once the window *and* its render surface exist. A
    /// refusal drops the new window with the source tab untouched.
    fn commit_seats(&mut self, mut engine: Option<&mut TextEngine>) {
        while let Some(index) = self.pending.iter().position(|p| p.runner_seated) {
            let pending = self.pending.remove(index);
            let seated = {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                match pending.seat {
                    Seat::Floor => Ok(self.host.floor_surface(&mut cx, pending.key)),
                    Seat::Host => self.host.seat_window(&mut cx, pending.key),
                }
            };
            let surface = match seated {
                Ok(surface) => surface,
                Err(_) => {
                    self.keys.retain(|key| *key != pending.key);
                    let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                    self.host.window_open_failed(&mut cx);
                    continue;
                }
            };
            let mut window =
                ShellWindow::new(pending.key, pending.logical, pending.scale, self.zoom);
            // Asked once, at creation: a pane that animated open on the first
            // frame would be motion nobody asked for.
            for side in [Side::Left, Side::Right] {
                // The width first: `set_immediately` is what makes it the width
                // the content is laid out against from the first frame rather
                // than one that snaps when something is released.
                if let Some(width) = self.host.pane_width(pending.key, side) {
                    window
                        .pane(side)
                        .set_full(width.clamp(panel::SIDEBAR_MIN_W, panel::SIDEBAR_MAX_W));
                }
                // **Unconditionally**, open or shut: `set_immediately` is also
                // what pulls `committed` up to the restored width, so a column
                // that opens later is laid out against the width that was
                // remembered rather than the default it was minted with.
                let open = self.host.pane_open(pending.key, side);
                window.pane(side).set_immediately(open);
            }
            window.resolve();
            let tab_key = self.minter.tab();
            // How that first tab reads is the host's, exactly as it is for a tab
            // it opens (`ShellAction::OpenTab`). A popped-out session arrives
            // titled and closable through `seated_tab`; a floor-seated window
            // arrives through `floor_tab`, whose default is still the floor's
            // silence for a host that does not answer.
            let seated = match pending.seat {
                Seat::Floor => self.host.floor_tab(pending.key),
                Seat::Host => self.host.seated_tab(pending.key),
            };
            if let Some(tab) = window.tabs.get_mut(0) {
                tab.key = tab_key;
                tab.surface = Some(surface);
                tab.placeholder = false;
                tab.title = seated.title;
                tab.closable = seated.closable;
            }
            self.windows.push(window);
            {
                let mut cx = HostCx::new(engine.as_deref_mut(), &mut self.queued);
                self.host.tab_opened(&mut cx, pending.key, tab_key);
            }
            self.settle_context(engine.as_deref_mut(), pending.key);
            self.settle_banner(pending.key);
            self.size_active(engine.as_deref_mut(), pending.key);
            self.with_active(engine.as_deref_mut(), pending.key, |surface, cx| {
                surface.activated(cx)
            });
            self.announce_activation(engine.as_deref_mut(), pending.key, None);
            // Only now does the source give anything up.
            if let Some((source, tab)) = pending.popout {
                self.close_tab(engine.as_deref_mut(), source, tab);
            }
        }
        self.drain(engine);
    }

    /// Everything owed before a dispatch that carries an engine can run.
    ///
    /// Seating is the only thing on this list, and it is here rather than in
    /// `seated` because that hook carries no `TextEngine` and
    /// [`ShellHost::seat_window`] is entitled to one. A window the runner has
    /// opened is always drawn before it is interacted with, so the host commits
    /// before anything can be seen.
    fn ready(&mut self, engine: &mut TextEngine) {
        if self.pending.iter().any(|pending| pending.runner_seated) {
            self.commit_seats(Some(engine));
        }
    }

    /// Fold a rich mouse event into the one pointer enum, remembering which
    /// button is down so a later move can be reported as a drag.
    fn rich_pointer(&mut self, key: WindowKey, event: RichMouseEvent) -> PointerEvent {
        let pressed = event.state == KeyState::Pressed;
        if let Some(window) = self.window_mut(key) {
            window.held = pressed.then_some(event.button);
        }
        // The third zoom door. `pointer_moved` and `is_caption`/`cursor_icon`
        // are the other three; `scroll_by` needs none, because it reads the
        // point this already recorded rather than one the runner carries.
        let (x, y) = self
            .window(key)
            .map_or((event.x, event.y), |window| window.unzoom(event.x, event.y));
        if pressed {
            PointerEvent::Pressed {
                x,
                y,
                button: event.button,
                mods: event.modifiers,
                click_count: event.click_count.max(1),
            }
        } else {
            PointerEvent::Released {
                x,
                y,
                button: event.button,
                mods: event.modifiers,
            }
        }
    }
}

// ------------------------------------------------------------------ the runner

impl<H: ShellHost> MultiWindowApp for ShellState<H> {
    /// **The host's, forwarded.** SP-9 left this `()` and named it SP-10's
    /// question; the answer is a pass-through rather than a wake type of the
    /// shell's own. The shell never inspects one, so there is still exactly one
    /// dispatcher and exactly one proxy — which is the sentence ADR-262 was
    /// protecting, not the `()`.
    type Event = H::Event;

    fn attach(&mut self, proxy: winit::event_loop::EventLoopProxy<Self::Event>) {
        self.host.attach(proxy);
    }

    fn wake(&mut self, event: Self::Event) -> bool {
        let changed = {
            let mut cx = HostCx::new(None, &mut self.queued);
            self.host.wake(&mut cx, event)
        };
        self.drain(None);
        changed
    }

    fn windows(&self) -> &[WindowKey] {
        &self.keys
    }

    /// Undecorated, because the shell draws the caption, and never smaller than
    /// the floor both apps share. Everything else is the host's.
    fn window(&self, key: WindowKey) -> WindowAttributes {
        self.host
            .window_attributes(key)
            .unwrap_or_else(Window::default_attributes)
            .with_decorations(false)
            .with_min_inner_size(LogicalSize::new(MIN_WINDOW_SIZE.0, MIN_WINDOW_SIZE.1))
    }

    fn should_exit(&self) -> bool {
        self.host.should_exit(self.keys.len())
    }

    fn set_maximised(&mut self, key: WindowKey, maximised: bool) -> bool {
        let moved = self
            .window_mut(key)
            .is_some_and(|window| std::mem::replace(&mut window.maximised, maximised) != maximised);
        self.host.set_maximised(key, maximised) || moved
    }

    fn set_pinned(&mut self, key: WindowKey, pinned: bool) -> bool {
        self.host.set_pinned(key, pinned)
    }

    fn pinned(&self, key: WindowKey) -> bool {
        self.host.pinned(key)
    }

    fn take_activation_request(&mut self, key: WindowKey) -> bool {
        self.host.take_activation_request(key)
    }

    fn seated(&mut self, key: WindowKey, _id: WindowId) {
        if let Some(pending) = self.pending.iter_mut().find(|pending| pending.key == key) {
            pending.runner_seated = true;
        }
    }

    fn window_closing(&mut self, key: WindowKey, state: ClosingWindowState) {
        self.host.window_closing(key, state);
        self.windows.retain(|window| window.key != key);
        let mut cx = HostCx::new(None, &mut self.queued);
        self.host.window_closed(&mut cx, key);
        self.drain(None);
    }

    fn exiting(&mut self, windows: &[(WindowKey, ClosingWindowState)]) {
        for (key, state) in windows {
            self.host.window_closing(*key, *state);
        }
    }

    /// The ✕, Alt+F4, or the taskbar. The active surface decides, and a `Veto`
    /// means it will close itself later with
    /// `ShellAction::Window(WindowRequest::Close)`.
    fn close_requested(&mut self, key: WindowKey) {
        let answer = self
            .with_active(None, key, |surface, cx| surface.close_requested(cx))
            .unwrap_or(CloseResponse::Close);
        self.drain(None);
        if answer == CloseResponse::Close {
            self.close_window(key);
        }
    }

    fn take_window_request(&mut self, key: WindowKey) -> Option<WindowRequest> {
        self.window_mut(key)?.request.take()
    }

    /// **Both `wants_*` flags, always on.** The split they gate is what SP-0's
    /// event matrix measured as lossy: a middle click and a right-button release
    /// are invisible to a host that has not opted in, which is a default nobody
    /// chose.
    fn wants_drag(&self, _key: WindowKey) -> bool {
        true
    }

    fn wants_rich_mouse(&self, _key: WindowKey) -> bool {
        true
    }

    /// **The first of the four zoom doors.** The band was solved in the
    /// window's layout space and this point is in the window's own logical
    /// pixels, so it is divided before it is compared — or a caption drawn at
    /// 200% would be draggable only in its top-left quarter.
    fn is_caption(&self, key: WindowKey, x: f32, y: f32) -> bool {
        self.window(key).is_some_and(|window| {
            let (x, y) = window.unzoom(x, y);
            window.is_caption(x, y)
        })
    }

    fn cursor_icon(&self, key: WindowKey, x: f32, y: f32) -> CursorIcon {
        // Before the host is asked: an overlay draws under the same zoomed root
        // as the chrome, so it hit-tests in the same space.
        let (x, y) = self
            .window(key)
            .map_or((x, y), |window| window.unzoom(x, y));
        if let Some(icon) = self.host.overlay_cursor_icon(key, x, y) {
            return icon;
        }
        let Some(window) = self.window(key) else {
            return CursorIcon::Default;
        };
        // **The grip says what it is before anything under it does.** It
        // straddles the column's edge, so half of it is over the content and
        // half over the pane's own rows; both would otherwise answer, and a
        // band that resizes has to look like one on both sides of the hairline.
        // While a drag is in flight it keeps saying so wherever the pointer has
        // got to, including outside the window.
        if window.pane_drag.is_some()
            || [Side::Left, Side::Right]
                .into_iter()
                .any(|side| window.pane_grip(side).contains(x, y))
        {
            return CursorIcon::ColResize;
        }
        if let Some(side) = self.pane_at(key, x, y) {
            return self
                .host
                .pane_cursor_icon(key, side, x, y)
                .unwrap_or(CursorIcon::Default);
        }
        let content = window.content();
        if !content.contains(x, y) {
            return CursorIcon::Default;
        }
        window
            .tabs
            .get(window.tabs.active())
            .and_then(|tab| tab.surface.as_ref())
            .map(|surface| surface.cursor_icon(x - content.x, y - content.y))
            .unwrap_or(CursorIcon::Default)
    }

    /// The resize border is the window's, outermost, and stays in
    /// `koru-window` where `drag_resize_window` can only work inside the press
    /// that asked for it.
    fn resize_direction(
        &self,
        _key: WindowKey,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        koru_window::resize::is_ours()
            .then(|| koru_window::resize::edge(width, height, x, y, maximised))
            .flatten()
    }

    fn resized(&mut self, key: WindowKey, size: PhysicalSize<u32>, scale_factor: f64) {
        if scale_factor <= 0.0 {
            return;
        }
        let scale = scale_factor as f32;
        // The window's logical size. The layout viewport is this over the zoom,
        // and `resolve` is the one place that division happens.
        let logical = (size.width as f32 / scale, size.height as f32 / scale);
        // **A window the runner has opened but the host has not seated yet is
        // still a window that has a size**, and this is where it learns it.
        //
        // The runner reports `Resized` the moment the OS window exists — which
        // for a window restored maximised is the *maximised* size — and that is
        // several events before the first engine-bearing dispatch runs
        // `commit_seats`. This used to answer `window_mut` with `None` and drop
        // the event on the floor; `commit_seats` then built the window at the
        // size `open_window` had *asked* for. A maximised 1920 window laid its
        // content out at the remembered restored width and stayed there until
        // somebody dragged an edge, which is the missing several hundred
        // pixels of surface.
        if let Some(pending) = self.pending.iter_mut().find(|pending| pending.key == key) {
            pending.logical = logical;
            pending.scale = scale;
            return;
        }
        if let Some(window) = self.window_mut(key) {
            window.logical = logical;
            window.scale = scale;
            window.resolve();
        } else {
            return;
        }
        // The caption moved, so the ladder is re-walked — against the settled
        // caption, which a resize always leaves. The offer is re-asked with it:
        // no host's answer depends on size today, but the hook says "every
        // resize" and a hook that lies is worse than one that costs a `bool`.
        self.settle_context(None, key);
        self.settle_banner(key);
        self.size_active(None, key);
        self.drain(None);
    }

    /// A DPI change with no size change. SP-0's matrix found this unobserved;
    /// SP-1 added the hook, and every cached layout in the window is stale.
    fn scale_factor_changed(&mut self, key: WindowKey, scale_factor: f64) -> bool {
        if scale_factor <= 0.0 {
            return false;
        }
        // Pending for the same reason `resized` is: a DPI the window learned
        // before it was seated has to survive being seated.
        if let Some(pending) = self.pending.iter_mut().find(|pending| pending.key == key) {
            pending.scale = scale_factor as f32;
            return false;
        }
        if let Some(window) = self.window_mut(key) {
            window.scale = scale_factor as f32;
        }
        self.invalidate(key);
        true
    }

    fn moved(&mut self, key: WindowKey, position: PhysicalPosition<i32>) {
        self.host.window_moved(key, position);
    }

    fn pointer_moved(&mut self, key: WindowKey, x: f32, y: f32) -> bool {
        let held = self.window(key).and_then(|window| window.held);
        let (x, y) = self
            .window(key)
            .map_or((x, y), |window| window.unzoom(x, y));
        let mods = ModifiersState::empty();
        let event = match held {
            Some(button) => PointerEvent::Dragged { x, y, button, mods },
            None => PointerEvent::Moved { x, y, mods },
        };
        self.pointer(None, key, event)
    }

    fn pointer_left(&mut self, key: WindowKey) -> bool {
        self.pointer(None, key, PointerEvent::Left)
    }

    /// Never reached: `wants_rich_mouse` is on, so every press arrives through
    /// [`rich_mouse`](MultiWindowApp::rich_mouse). Required by the trait.
    fn click(
        &mut self,
        _key: WindowKey,
        _engine: &mut TextEngine,
        _x: f32,
        _y: f32,
        _ctrl: bool,
    ) -> bool {
        false
    }

    fn rich_mouse(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        event: RichMouseEvent,
    ) -> bool {
        self.ready(engine);
        let folded = self.rich_pointer(key, event);
        self.pointer(Some(engine), key, folded)
    }

    fn mouse_up(&mut self, key: WindowKey) -> bool {
        if let Some(window) = self.window_mut(key) {
            window.held = None;
        }
        false
    }

    /// **The wheel lands where the pointer is.**
    ///
    /// `scroll_by` carries no position, and SP-9 answered it with the content
    /// rect's centre — which sent every wheel to the active surface even when
    /// the pointer was over a pane. The remembered point is the honest answer,
    /// and it is the one the pane needs: the Terminal's session list scrolls.
    fn scroll_by(&mut self, key: WindowKey, delta: f32, mods: ModifiersState) -> bool {
        let (x, y) = self
            .window(key)
            .map(ShellWindow::pointer)
            .or_else(|| self.content_rect(key).map(|content| content.centre()))
            .unwrap_or((0.0, 0.0));
        self.pointer(
            None,
            key,
            PointerEvent::Wheel {
                x,
                y,
                lines: delta,
                mods,
            },
        )
    }

    /// Never reached: `rich_key` below claims every key. Required by the trait.
    fn key(
        &mut self,
        _key: WindowKey,
        _engine: &mut TextEngine,
        _logical: &Key,
        _text: Option<&str>,
        _mods: ModifiersState,
    ) -> bool {
        false
    }

    /// **Ctrl+Tab first, then the host, then the active surface** — and never a
    /// shell-owned accelerator table above any of them.
    fn rich_key(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        event: RichKeyEvent<'_>,
    ) -> bool {
        self.ready(engine);
        if event.state == KeyState::Pressed
            && event.modifiers.control_key()
            && event.physical_key == PhysicalKey::Code(KeyCode::Tab)
        {
            // Resolved from the physical key, for the reason `View::cycle_tab`
            // records: Ctrl+Tab and Ctrl+I are the same logical key on Windows.
            let delta = if event.modifiers.shift_key() { -1 } else { 1 };
            let changed = self.cycle_tab(Some(engine), key, delta);
            self.drain(Some(engine));
            return changed;
        }
        let claimed = {
            let mut cx = HostCx::new(Some(engine), &mut self.queued);
            self.host.window_key(&mut cx, key, event)
        };
        let changed = if claimed {
            true
        } else {
            self.with_active(Some(engine), key, |surface, cx| surface.key(cx, event))
                .unwrap_or(false)
        };
        self.drain(Some(engine));
        changed
    }

    /// IME follows the **tab**, not the window: a welcome tab and a session tab
    /// do not want the same thing.
    fn wants_ime(&self, key: WindowKey) -> bool {
        self.window(key)
            .and_then(|window| window.tabs.get(window.tabs.active()))
            .and_then(|tab| tab.surface.as_ref())
            .is_some_and(|surface| surface.wants_ime())
    }

    /// **The host's overlays see composition first (S16).** An open modal
    /// consumes it, so a commit never reaches the terminal behind it.
    fn ime(&mut self, key: WindowKey, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        self.ready(engine);
        let claimed = {
            let mut cx = HostCx::new(Some(engine), &mut self.queued);
            self.host.overlay_ime(&mut cx, key, event)
        };
        let changed = if claimed {
            true
        } else {
            self.with_active(Some(engine), key, |surface, cx| surface.ime(cx, event))
                .unwrap_or(false)
        };
        self.drain(Some(engine));
        changed
    }

    fn dropped_file(&mut self, key: WindowKey, path: &std::path::Path) -> bool {
        let changed = self
            .with_active(None, key, |surface, cx| surface.dropped_file(cx, path))
            .unwrap_or(false);
        self.drain(None);
        changed
    }

    fn focus_changed(&mut self, key: WindowKey, focused: bool) -> bool {
        let host = self.host.window_focus_changed(key, focused);
        let changed = self
            .with_active(None, key, |surface, cx| {
                surface.window_focus_changed(cx, focused)
            })
            .unwrap_or(false);
        self.drain(None);
        changed || host
    }

    fn occluded(&mut self, key: WindowKey, occluded: bool) {
        self.host.window_occluded(key, occluded);
    }

    /// The palette is a process global, so a background tab's cached layout is
    /// stale too — and the runner asks this of **every** window, which is what
    /// makes one call per surface come out of a per-window loop.
    fn system_theme_changed(&mut self, key: WindowKey, _dark: bool) -> bool {
        self.invalidate(key);
        true
    }

    fn maintenance_deadline(&self, key: WindowKey) -> Option<Instant> {
        let window = self.window(key)?;
        let panes = [
            window.left.animating(),
            window.right.animating(),
            window.popover.is_some_and(|popover| popover.animating()),
        ]
        .iter()
        .any(|animating| *animating)
        .then(Instant::now);
        window
            .tabs
            .iter()
            .filter_map(|tab| tab.surface.as_ref())
            .filter_map(|surface| surface.maintenance_deadline())
            .chain(panes)
            .chain(window.tooltip.maintenance_deadline())
            // The end of the flash's hold. This is the other half of the toast
            // rule: the pill asks for no frames while it sits still, so
            // *something* has to wake the window at the moment it starts to
            // fade, and a deadline is that something.
            .chain(window.zoom_label.maintenance_deadline())
            .min()
    }

    /// **Every surface's maintenance runs, active or not**, and only the active
    /// one's answer is a reason for a frame. That is the inactive-tab rule (R2)
    /// with an enforcement point.
    fn maintain(&mut self, key: WindowKey, now: Instant) -> bool {
        let Some(count) = self.window(key).map(|window| window.tabs.len()) else {
            return false;
        };
        let scale = self.window(key).map_or(1.0, |window| window.scale);
        let mut changed = false;
        for index in 0..count {
            let mut commands = Commands::new(key);
            let active = self
                .window(key)
                .is_some_and(|window| window.tabs.active() == index);
            if let Some(window) = self.windows.iter_mut().find(|window| window.key == key)
                && let Some(surface) = window
                    .tabs
                    .get_mut(index)
                    .and_then(|tab| tab.surface.as_mut())
            {
                let mut cx = SurfaceCx {
                    engine: None,
                    scale,
                    commands: &mut commands,
                };
                changed |= surface.maintain(&mut cx, now) && active;
            }
            self.queued.extend(commands.drain());
        }
        if let Some(window) = self.window_mut(key) {
            changed |= window.left.advance(now);
            changed |= window.right.advance(now);
            if let Some(popover) = window.popover.as_mut() {
                changed |= popover.advance(now);
            }
            changed |= window.slides.advance(now);
            changed |= window.tooltip.maintain(now);
            // Dropping the expired flash is what lets `animating` reach false
            // and returns the window to the idle wake.
            changed |= window.zoom_label.expire(now);
        }
        self.drain(None);
        changed
    }

    fn animating(&self, key: WindowKey) -> bool {
        let Some(window) = self.window(key) else {
            return false;
        };
        window.left.animating()
            || window.right.animating()
            || window.popover.is_some_and(|popover| popover.animating())
            || window.slides.animating()
            || window.tooltip.animating(Instant::now())
            // **Only while it is fading.** The hold is a `maintenance_deadline`,
            // not 87 frames of drawing the same pill in the same place.
            || window.zoom_label.animating(Instant::now())
            || window
                .tabs
                .get(window.tabs.active())
                .and_then(|tab| tab.surface.as_ref())
                .is_some_and(|surface| surface.animating())
    }

    fn draw(
        &mut self,
        key: WindowKey,
        scene: &mut Scene,
        engine: &mut TextEngine,
        _w: f32,
        _h: f32,
    ) {
        self.ready(engine);
        self.draw_window(key, scene, engine);
        self.drain(Some(engine));
    }

    fn presented(&mut self, key: WindowKey) {
        self.with_active(None, key, |surface, _| surface.presented());
        self.host.presented(key);
        self.drain(None);
    }
}

#[cfg(test)]
mod tests;
