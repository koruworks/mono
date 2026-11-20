// SPDX-License-Identifier: MPL-2.0

//! **`koru-artifacts-ui`**: its surfaces, controls and support, a record each,
//! written from the source they name. `registry/MANIFEST.md` lists the
//! files read, the modules excluded and why, and what building each
//! surface needs.

use super::*;

const OWNER: &str = "koru-artifacts-ui";

const LIB: &str = "crates/koru-artifacts-ui/src/lib.rs";
const TAB: &str = "crates/koru-artifacts-ui/src/tab.rs";

/// Every record of this crate, in the order the navigator lists them.
pub(super) const RECORDS: &[Entry] = &[
    // Construction: side effects: new(root) starts read and register threads
    // that dial the gateway (skipped under cfg(test) and no-reader);
    // without_reader() builds it inert for a test or a host that drives it
    record(
        "C-ARTIFACTS-COLUMN",
        "ArtifactsSurface",
        Key::UiLabArtifactsColumnName,
        Key::UiLabArtifactsColumnPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_artifacts_ui", LIB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_artifacts_ui::ArtifactsSurface"),
            painter("koru_artifacts_ui::ArtifactsSurface::new"),
            painter("koru_artifacts_ui::ArtifactsSurface::without_reader"),
            painter("koru_artifacts_ui::ArtifactsSurface::draw"),
            support("koru_artifacts_ui::ArtifactsSurface::rows"),
            support("koru_artifacts_ui::ArtifactsSurface::row_parts"),
            support("koru_artifacts_ui::ArtifactsSurface::mouse_down"),
            support("koru_artifacts_ui::ArtifactsSurface::key"),
            support("koru_artifacts_ui::ArtifactsSurface::take_catalog"),
            support("koru_artifacts_ui::ArtifactsSurface::take_wants_open"),
            support("koru_artifacts_ui::ArtifactsSurface::register"),
            support("koru_artifacts_ui::Row"),
            support("koru_artifacts_ui::RowParts"),
            support("koru_artifacts_ui::Notice"),
            support("koru_artifacts_ui::VERSIONS_SHOWN"),
            support("koru_artifacts_ui::Press"),
            support("koru_artifacts_ui::Work"),
            support("koru_artifacts_ui::MODE_ICON"),
            support("koru_artifacts_ui::model::Catalog"),
            support("koru_artifacts_ui::model::FamilyEntry"),
            support("koru_artifacts_ui::model::RunEntry"),
            support("koru_artifacts_ui::model::Load"),
            support("koru_artifacts_ui::model::Registering"),
            support("koru_artifacts_ui::text::provenance_clauses"),
            support("koru_artifacts_ui::text::version_row"),
        ],
    ),
    // Construction: side effects: new(root) starts an acting reader thread
    // (skipped under cfg(test) and no-reader); without_reader() builds an
    // inert tab with its own TextEngine and an empty MarkdownView
    record(
        "C-ARTIFACT-TAB",
        "ArtifactTab",
        Key::UiLabArtifactTabName,
        Key::UiLabArtifactTabPurpose,
        Family::Compositions,
        Kind::DomainComposition,
        None,
        &[],
        source(OWNER, "koru_artifacts_ui", TAB),
        DESKTOP_NOT_ON_PHONES,
        &[
            painter("koru_artifacts_ui::ArtifactTab"),
            painter("koru_artifacts_ui::ArtifactTab::new"),
            painter("koru_artifacts_ui::ArtifactTab::without_reader"),
            painter("koru_artifacts_ui::ArtifactTab::draw"),
            support("koru_artifacts_ui::ArtifactTab::open"),
            support("koru_artifacts_ui::ArtifactTab::relayout"),
            support("koru_artifacts_ui::ArtifactTab::laid_rows"),
            support("koru_artifacts_ui::ArtifactTab::mouse_down"),
            support("koru_artifacts_ui::ArtifactTab::key"),
            support("koru_artifacts_ui::ArtifactTab::save_as_next_version"),
            boundary("koru_artifacts_ui::ArtifactTab::view"),
            support("koru_artifacts_ui::OpenedArtifact"),
            support("koru_artifacts_ui::VersionPlace"),
            support("koru_artifacts_ui::SavingVersion"),
            support("koru_artifacts_ui::TabRow"),
        ],
    ),
];
