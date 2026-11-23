// SPDX-License-Identifier: MPL-2.0

//! **`koru-window`**: the native window host adapters — the two event loops,
//! the screen-reader bridge, input, rendering and appearance — and two
//! boundaries the operating system owns, the window frame and input-method
//! composition, a record each, written from the source they name.
//! `registry/MANIFEST.md` lists the files read, the modules excluded and why.

use super::*;

const OWNER: &str = "koru-window";

const ACCESSIBILITY: &str = "crates/koru-window/src/accessibility.rs";
const APP: &str = "crates/koru-window/src/app.rs";
const APPEARANCE: &str = "crates/koru-window/src/appearance.rs";
const FRAME: &str = "crates/koru-window/src/frame.rs";
const INPUT: &str = "crates/koru-window/src/input.rs";
const NON_ACTIVATING: &str = "crates/koru-window/src/non_activating.rs";
const RENDERER: &str = "crates/koru-window/src/renderer.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: needs the process's event loop: run and run_windows take
    // over the thread until the last window closes; a WindowApp or
    // MultiWindowApp is the application's own type
    record(
        "H-WINDOW-RUNNERS",
        "WindowApp",
        Key::UiLabWindowRunnersName,
        Key::UiLabWindowRunnersPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", APP),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_window::WindowApp"),
            support("koru_window::run"),
            support("koru_window::ClosingWindowState"),
            support("koru_window::MultiWindowApp"),
            support("koru_window::run_windows"),
            support("koru_window::WindowKey"),
            support("koru_window::WindowRequest"),
            support("koru_window::RecordingRequest"),
            support_with(
                "koru_window::WindowActivation",
                &[values(
                    Key::UiLabControlMode,
                    "koru_window::WindowActivation",
                    NON_ACTIVATING,
                    Declared::Enum,
                    &[literal("Focused"), literal("Passive")],
                )],
            ),
        ],
    ),
    // Construction: needs a live window: the adapter is installed before the
    // first reveal and retired with the window; off Windows it is an inert
    // stand-in that never builds a tree
    record(
        "H-WINDOW-ACCESSIBILITY",
        "WindowBridge",
        Key::UiLabWindowAccessibilityName,
        Key::UiLabWindowAccessibilityPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window::accessibility", ACCESSIBILITY),
        [
            (Platform::Windows, Availability::Existing),
            (Platform::MacOs, Availability::Planned),
            (Platform::Linux, Availability::Unavailable),
            (Platform::Android, Availability::NotApplicable),
            (Platform::Ios, Availability::NotApplicable),
        ],
        &[
            support("koru_window::accessibility::WindowBridge"),
            support("koru_window::accessibility::WindowBridge::before_first_reveal"),
            support("koru_window::accessibility::Provider"),
            support("koru_window::accessibility::Pending"),
            support("koru_window::accessibility::Pending::is_empty"),
        ],
    ),
    // Construction: calls on a live window; Windows sets DWM attributes and
    // its own hit-tested border, a Mac seats the system's traffic lights
    record(
        "C-WINDOW-FRAME",
        "frame::apply",
        Key::UiLabWindowFrameName,
        Key::UiLabWindowFramePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", FRAME),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::NotAudited),
            (Platform::Android, Availability::Unavailable),
            (Platform::Ios, Availability::Unavailable),
        ],
        &[
            boundary("koru_window::frame::apply"),
            boundary("koru_window::frame::apply_caption"),
            boundary("koru_window::hairline::draw"),
            boundary("koru_window::hairline::edges"),
            boundary("koru_window::hairline::flush"),
            boundary("koru_window::hairline::Bounds"),
            boundary("koru_window::hairline::Edges"),
            boundary("koru_window::resize::edge"),
            boundary("koru_window::resize::is_ours"),
            boundary("koru_window::resize::BORDER"),
            boundary("koru_window::resize::CORNER"),
            boundary("koru_window::snap::half"),
            boundary("koru_window::snap::centred"),
            boundary("koru_window::snap::reset_rect"),
            boundary("koru_window::snap::reachable"),
            boundary("koru_window::snap::place"),
            boundary("koru_window::snap::place_exact"),
            boundary("koru_window::snap::nudge_onscreen"),
            boundary("koru_window::snap::Side"),
            boundary("koru_window::snap::Placement"),
            boundary("koru_window::traffic_lights::place"),
            boundary("koru_window::traffic_lights::shell_owns_drag"),
            boundary("koru_window::traffic_lights::LANE_W"),
        ],
    ),
    // Construction: plain events the loop builds from winit's
    record(
        "C-IME-COMPOSITION",
        "ImeEvent",
        Key::UiLabImeCompositionName,
        Key::UiLabImeCompositionPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", INPUT),
        [
            (Platform::Windows, Availability::NativeOwned),
            (Platform::MacOs, Availability::NativeOwned),
            (Platform::Linux, Availability::NativeOwned),
            (Platform::Android, Availability::NativeOwned),
            (Platform::Ios, Availability::NativeOwned),
        ],
        &[boundary_with(
            "koru_window::ImeEvent",
            &[values(
                Key::UiLabControlKind,
                "koru_window::ImeEvent",
                INPUT,
                Declared::Enum,
                &[
                    literal("Enabled"),
                    literal("Preedit"),
                    literal("Commit"),
                    literal("Disabled"),
                ],
            )],
        )],
    ),
    // Construction: pure: plain records and a set of held keys
    record(
        "H-WINDOW-INPUT",
        "RichKeyEvent",
        Key::UiLabWindowInputName,
        Key::UiLabWindowInputPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", INPUT),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_window::RichKeyEvent"),
            support("koru_window::RichMouseEvent"),
            support("koru_window::KeyState"),
            support("koru_window::KeyState::is_pressed"),
            support("koru_window::KeyQuarantine"),
            support("koru_window::KeyQuarantine::suppress"),
            support("koru_window::KeyQuarantine::suppress_all"),
            support("koru_window::KeyQuarantine::consumes"),
            support("koru_window::KeyQuarantine::released"),
        ],
    ),
    // Construction: a GPU device: Warmup::start spawns a thread that builds
    // one; RendererBackend::load and store read and write a machine-local
    // state file; offscreen::raster renders without a window
    record(
        "H-WINDOW-RENDERER",
        "RendererBackend",
        Key::UiLabWindowRendererName,
        Key::UiLabWindowRendererPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", RENDERER),
        DESKTOP_NOT_ON_PHONES,
        &[
            support_with(
                "koru_window::RendererBackend",
                &[values(
                    Key::UiLabControlValues,
                    "koru_window::RendererBackend",
                    RENDERER,
                    Declared::Enum,
                    &[
                        literal("Auto"),
                        literal("Dx12"),
                        literal("Vulkan"),
                        literal("Gl"),
                    ],
                )],
            ),
            support("koru_window::RendererBackend::load"),
            support("koru_window::RendererBackend::store"),
            support("koru_window::build_renderer"),
            support("koru_window::configured_backends"),
            support("koru_window::default_backends"),
            support("koru_window::context_with"),
            support("koru_window::renderer::context"),
            support("koru_window::renderer::Warmup"),
            support("koru_window::renderer::Warmup::start"),
            support("koru_window::renderer::Warmed"),
            support("koru_window::renderer::record_adapter"),
            support("koru_window::renderer::adapter_description"),
            support("koru_window::offscreen::raster"),
            support("koru_window::offscreen::raster_with"),
            support("koru_window::offscreen::Rgba"),
            support("koru_window::offscreen::OffscreenError"),
            support("koru_window::framestats::enabled"),
            support("koru_window::framestats::Phase"),
            support("koru_window::framestats::Frames"),
        ],
    ),
    // Construction: changes process state when applied: Appearance::apply and
    // CaretAppearance::apply write koru_draw::theme's process globals;
    // system_theme reads the platform's setting
    record(
        "H-SYSTEM-APPEARANCE",
        "Appearance",
        Key::UiLabSystemAppearanceName,
        Key::UiLabSystemAppearancePurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_window", APPEARANCE),
        DESKTOP_PHONES_PARTIAL,
        &[
            support("koru_window::Appearance"),
            support("koru_window::Appearance::from_section"),
            support("koru_window::Appearance::apply"),
            support("koru_window::Appearance::follows_system"),
            support("koru_window::Appearance::effective_dark"),
            support("koru_window::CaretAppearance"),
            support("koru_window::system_theme::system_is_dark"),
            support("koru_window::system_theme::probe"),
            support("koru_window::system_theme::SystemThemeProbe"),
        ],
    ),
];
