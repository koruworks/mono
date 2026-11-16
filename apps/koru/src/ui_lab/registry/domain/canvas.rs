// SPDX-License-Identifier: MPL-2.0

//! **`koru-canvas-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-canvas-ui";

const CAMERA: &str = "crates/koru-canvas-ui/src/camera.rs";
const LIB: &str = "crates/koru-canvas-ui/src/lib.rs";
const MODEL: &str = "crates/koru-canvas-ui/src/model.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: pure: new(lang) builds its own TextEngine (a clone of the
    // process font prototype) and empty state; the crate touches no disk or
    // network
    record(
        "C-CANVAS-SURFACE",
        "CanvasSurface",
        Key::UiLabCanvasSurfaceName,
        Key::UiLabCanvasSurfacePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_canvas_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_canvas_ui::CanvasSurface"),
            painter("koru_canvas_ui::CanvasSurface::new"),
            painter("koru_canvas_ui::CanvasSurface::draw"),
            support("koru_canvas_ui::CanvasSurface::set_scene"),
            support("koru_canvas_ui::CanvasSurface::take_requests"),
            support("koru_canvas_ui::CanvasSurface::mouse_down"),
            support("koru_canvas_ui::CanvasSurface::key"),
            support("koru_canvas_ui::CanvasSurface::semantics"),
            support("koru_canvas_ui::CanvasSurface::counters"),
            support("koru_canvas_ui::Scene"),
            support("koru_canvas_ui::CanvasRequest"),
            support("koru_canvas_ui::ObjectKey"),
            support("koru_canvas_ui::SemanticObject"),
            support("koru_canvas_ui::Counters"),
            support("koru_canvas_ui::DEFAULT_WIDTH"),
            support("koru_canvas_ui::MIN_CARD_WIDTH"),
            support("koru_canvas_ui::MAX_WIDTH"),
        ],
    ),
    // Construction: pure arithmetic
    record(
        "H-CANVAS-CAMERA",
        "Camera",
        Key::UiLabCanvasCameraName,
        Key::UiLabCanvasCameraPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_canvas_ui", CAMERA),
        DESKTOP_NOT_ON_PHONES,
        &[
            support("koru_canvas_ui::Camera"),
            support("koru_canvas_ui::Camera::new"),
            support("koru_canvas_ui::Camera::to_view"),
            support("koru_canvas_ui::Camera::to_canvas"),
            support("koru_canvas_ui::Camera::zoom_at"),
            support("koru_canvas_ui::Camera::fit"),
            support("koru_canvas_ui::Camera::MIN_ZOOM"),
            support("koru_canvas_ui::Camera::MAX_ZOOM"),
        ],
    ),
    // Construction: plain values the container reads out of its document
    record(
        "C-CANVAS-OBJECTS",
        "SceneObject",
        Key::UiLabCanvasObjectsName,
        Key::UiLabCanvasObjectsPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_canvas_ui", MODEL),
        DESKTOP_NOT_ON_PHONES,
        &[
            boundary_with(
                "koru_canvas_ui::SceneObject",
                &[
                    values(
                        Key::UiLabControlKind,
                        "koru_canvas_ui::ObjectKind",
                        MODEL,
                        Declared::Enum,
                        &[
                            literal("Card"),
                            literal("Image"),
                            literal("Start"),
                            literal("Agent"),
                            literal("Output"),
                        ],
                    ),
                    values(
                        Key::UiLabControlState,
                        "koru_canvas_ui::NodeStatus",
                        MODEL,
                        Declared::Enum,
                        &[
                            literal("Idle"),
                            literal("Running"),
                            literal("NeedsResponse"),
                            literal("Done"),
                            literal("Failed"),
                            literal("Stopped"),
                            literal("NotStarted"),
                        ],
                    ),
                ],
            ),
            boundary("koru_canvas_ui::SceneObject::new"),
            boundary("koru_canvas_ui::ObjectKind"),
            boundary("koru_canvas_ui::NodeStatus"),
            boundary("koru_canvas_ui::SceneImage"),
            boundary("koru_canvas_ui::SceneArrow"),
        ],
    ),
];
