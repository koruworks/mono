// SPDX-License-Identifier: MPL-2.0

//! **`koru-runs-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-runs-ui";

const LIB: &str = "crates/koru-runs-ui/src/lib.rs";
const STATE: &str = "crates/koru-runs-ui/src/state.rs";
const SUMMARY: &str = "crates/koru-runs-ui/src/summary.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: new() is pure: a TextEngine, a TextInput for the rating
    // note, a MarkdownView and RunLinks::inert() (no thread, no I/O, no
    // clock). with_links(RunLinks) takes whatever link it is given;
    // RunLinks::scripted() makes a fully scriptable, service-free surface (the
    // crate's own suite drives it that way). for_host(segment, root) calls
    // RunLinks::open, which spawns the koru-run-view worker thread that
    // connects to the gateway and reads/writes the preamble file.
    record(
        "C-RUNS-SURFACE",
        "RunsSurface",
        Key::UiLabRunsSurfaceName,
        Key::UiLabRunsSurfacePurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_runs_ui", LIB),
        DESKTOP_ANDROID_PARTIAL,
        &[
            painter("koru_runs_ui::RunsSurface"),
            support("koru_runs_ui::RunsSurface::new"),
            support("koru_runs_ui::RunsSurface::with_links"),
            support("koru_runs_ui::RunsSurface::for_host"),
            painter("koru_runs_ui::RunsSurface::draw"),
            support("koru_runs_ui::RunsSurface::relayout"),
            support("koru_runs_ui::RunsSurface::set_viewport"),
            support("koru_runs_ui::RunsSurface::set_lang"),
            support("koru_runs_ui::RunsSurface::set_locale"),
            support("koru_runs_ui::RunsSurface::set_now_ms"),
            support("koru_runs_ui::RunsSurface::open"),
            support("koru_runs_ui::RunsSurface::close"),
            support("koru_runs_ui::RunsSurface::poll"),
            support_with(
                "koru_runs_ui::RunsSurface::set_tab",
                &[values(
                    Key::UiLabControlMode,
                    "koru_runs_ui::Tab",
                    STATE,
                    Declared::Enum,
                    &[
                        labelled("Overview", Key::RunTabOverview),
                        labelled("Activity", Key::RunTabActivity),
                        labelled("Result", Key::RunTabResult),
                        labelled("SystemPrompt", Key::RunTabSystemPrompt),
                    ],
                )],
            ),
            support("koru_runs_ui::RunsSurface::set_handover"),
            support("koru_runs_ui::RunsSurface::set_device_name"),
            support("koru_runs_ui::RunsSurface::set_can_reveal"),
            support("koru_runs_ui::RunsSurface::set_can_re_run"),
            support("koru_runs_ui::RunsSurface::set_profile"),
            support("koru_runs_ui::RunsSurface::mouse_down"),
            support("koru_runs_ui::RunsSurface::mouse_up"),
            support("koru_runs_ui::RunsSurface::mouse_move"),
            support("koru_runs_ui::RunsSurface::key"),
            support("koru_runs_ui::RunsSurface::scroll_by"),
            support("koru_runs_ui::RunsSurface::cursor_icon"),
            support("koru_runs_ui::RunsSurface::invalidate"),
            support("koru_runs_ui::RunsSurface::take_requests"),
            support("koru_runs_ui::RunsSurface::respond"),
            support("koru_runs_ui::RunsSurface::is_confirming"),
            support("koru_runs_ui::RunsSurface::confirmation_default"),
            support("koru_runs_ui::Request"),
            support("koru_runs_ui::OpenedResult"),
            support("koru_runs_ui::ReRun"),
            support("koru_runs_ui::Operation"),
            support("koru_runs_ui::Refusal"),
            support("koru_runs_ui::Tab"),
            support("koru_runs_ui::Availability"),
            support("koru_runs_ui::Standing"),
            support("koru_runs_ui::Takeover"),
            support("koru_runs_ui::Saving"),
            support("koru_runs_ui::SaveFailure"),
            support("koru_runs_ui::Snapshot"),
            support("koru_runs_ui::PreambleView"),
            support("koru_runs_ui::RunFolder"),
        ],
    ),
    // Construction: pure: summary(lang, status, latest_step, elapsed_ms) and
    // failed_because(lang, status, reason) build strings from closed
    // vocabularies through koru-i18n; no state, no I/O.
    record(
        "H-RUN-SUMMARY",
        "Summary",
        Key::UiLabRunsSummaryName,
        Key::UiLabRunsSummaryPurpose,
        Family::Compositions,
        Kind::Support,
        None,
        &[],
        source(OWNER, "koru_runs_ui", SUMMARY),
        DESKTOP_ANDROID_PARTIAL,
        &[
            support("koru_runs_ui::Summary"),
            support("koru_runs_ui::summary"),
            support("koru_runs_ui::failed_because"),
        ],
    ),
];
