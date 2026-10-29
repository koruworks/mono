// SPDX-License-Identifier: MPL-2.0

use std::str::FromStr;

use crate::*;

fn project_id() -> ProjectId {
    super::id("66666666-6666-4666-8666-666666666666")
}

fn scope() -> WorkspaceScope {
    WorkspaceScope {
        profile: super::id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
        workspace: super::id("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
    }
}

fn summary() -> ProjectSummary {
    ProjectSummary {
        id: project_id(),
        name: "Koru — 계획".into(),
        root_directory: Some("C:\\work tree".into()),
        workspace_scope: Some(scope()),
        created_at_ms: 1_756_000_000_000,
    }
}

#[test]
fn a_scope_is_a_typed_pair_rather_than_a_compound_string() {
    let value = serde_json::to_value(scope()).unwrap();
    assert!(value["profile"].is_string());
    assert!(value["workspace"].is_string());
    // Two fields, not one key somebody has to know how to split.
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert_eq!(
        serde_json::from_value::<WorkspaceScope>(value).unwrap(),
        scope()
    );

    // The ids carry the protocol's identity rules with them: a catalog
    // profile is not a launch profile, and neither is a v1 UUID.
    assert!(CatalogProfileId::from_str("00000000-0000-1000-8000-000000000000").is_err());
    assert!(CatalogWorkspaceId::from_str("not-a-uuid").is_err());
    assert!(ProjectId::from_str("66666666-6666-4666-8666-666666666666").is_ok());
}

#[test]
fn a_project_row_round_trips_and_carries_no_ordering_or_lifecycle() {
    let project = summary();
    let json = serde_json::to_string(&project).unwrap();
    assert_eq!(
        serde_json::from_str::<ProjectSummary>(&json).unwrap(),
        project
    );
    // The absences are the decision: ordering is the surface's, and hiding a
    // project is derived from its workspace rather than stored on the row.
    for absent in ["sort_key", "archived", "trashed", "deleted_at", "state"] {
        assert!(!json.contains(absent), "{absent} entered the project row");
    }

    // An unscoped project is a project with no workspace, not a special one.
    let unscoped = ProjectSummary {
        workspace_scope: None,
        root_directory: None,
        ..project
    };
    assert_eq!(
        serde_json::from_str::<ProjectSummary>(&serde_json::to_string(&unscoped).unwrap()).unwrap(),
        unscoped
    );
}

#[test]
fn names_are_bounded_unicode_and_roots_are_bounded_paths() {
    for accepted in ["Koru", "계획", "a b", "  padded  "] {
        validate_project_name(accepted).unwrap();
    }
    for refused in ["", "   ", "line\nbreak", "\u{7}bell", "tab\there"] {
        assert_eq!(
            validate_project_name(refused),
            Err(ErrorCode::InvalidRequest),
            "{refused:?} was accepted"
        );
    }
    // The bound counts scalars, so it does not depend on which script the
    // name is written in.
    let longest = "ê".repeat(MAX_PROJECT_NAME_CHARS);
    validate_project_name(&longest).unwrap();
    assert_eq!(
        validate_project_name(&format!("{longest}ê")),
        Err(ErrorCode::InvalidRequest)
    );

    validate_project_root(None).unwrap();
    validate_project_root(Some("C:\\work tree")).unwrap();
    assert_eq!(
        validate_project_root(Some("")),
        Err(ErrorCode::InvalidRequest)
    );
    assert_eq!(
        validate_project_root(Some(&"x".repeat(MAX_PROJECT_ROOT_CHARS + 1))),
        Err(ErrorCode::InvalidRequest)
    );

    assert_eq!(summary().validate(), Ok(()));
    assert_eq!(
        ProjectSummary {
            name: String::new(),
            ..summary()
        }
        .validate(),
        Err(ErrorCode::InvalidRequest)
    );
}

#[test]
fn a_summary_without_a_placement_still_decodes_and_gains_none() {
    let placed = super::state::sample_summary().with_project(Some(project_id()));
    let json = serde_json::to_value(&placed).unwrap();
    assert_eq!(json["project_id"], project_id().to_string());
    assert_eq!(
        serde_json::from_value::<SessionSummary>(json.clone()).unwrap(),
        placed
    );

    // A frame written before projects existed has no such key at all. The
    // defaulted field is what makes it readable — and what a *new variant*
    // could never have been, which is why the verbs took a version instead.
    let mut legacy = json;
    legacy.as_object_mut().unwrap().remove("project_id");
    let decoded = serde_json::from_value::<SessionSummary>(legacy).unwrap();
    assert_eq!(decoded.project_id, None);
    assert_eq!(decoded, placed.with_project(None));

    // And in the other direction: an older peer skips a field it does not
    // declare rather than failing the whole summary.
    let mut future = serde_json::to_value(&decoded).unwrap();
    future
        .as_object_mut()
        .unwrap()
        .insert("project_colour".into(), "teal".into());
    assert_eq!(
        serde_json::from_value::<SessionSummary>(future).unwrap(),
        decoded
    );
}

#[test]
fn the_catalog_stream_carries_invalidation_and_never_a_row() {
    let value = serde_json::to_value(CatalogEvent::ProjectsChanged).unwrap();
    assert_eq!(value["kind"], "projects_changed");
    assert_eq!(value.as_object().unwrap().len(), 1, "{value}");
    assert_eq!(
        serde_json::from_value::<CatalogEvent>(value).unwrap(),
        CatalogEvent::ProjectsChanged
    );
    // A closed, behavior-bearing enum: an arm nobody declared is an error,
    // not a shrug.
    assert!(serde_json::from_str::<CatalogEvent>(r#"{"kind":"projects_reordered"}"#).is_err());
}

#[test]
fn the_project_verbs_arrive_under_a_version_and_a_capability() {
    // The version bump is the point: `#[serde(default)]` makes an added field
    // readable by an older peer and does nothing at all for a new variant.
    // The project verbs arrived at 10 and the ceiling has moved past them
    // since — a later version can only add, so the floor is what matters, and
    // the floor is what the negotiation below proves rather than an assertion
    // over two constants that the compiler already knows the answer to.
    assert!(capability::ALL.contains(&capability::PROJECTS));
    CapabilityName::from_str(capability::PROJECTS).unwrap();

    // Negotiation enables only the intersection, so a host that speaks
    // projects and a window that does not agree on nothing.
    let host = ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new([CapabilityName::from_str(capability::PROJECTS).unwrap()]),
    };
    let pre_project = ProtocolOffer {
        versions: ProtocolRange { min: 1, max: 9 },
        capabilities: CapabilitySet::default(),
    };
    let agreement = negotiate(&pre_project, &host).unwrap();
    assert_eq!(agreement.version, 9);
    assert!(!agreement.capabilities.supports(capability::PROJECTS));

    let current = negotiate(&host, &host).unwrap();
    assert_eq!(current.version, MAX_PROTOCOL_VERSION);
    assert!(current.capabilities.supports(capability::PROJECTS));
}

/// **v11 and `workspace-lifecycle`, for exactly the reason v10 needed a bump.**
/// The supervised verb is a new request tag and a new response tag; a v10 host
/// handed one answers that the frame is unreadable, and "unreadable" is not
/// the same answer as "I closed nothing" to a caller about to delete a
/// workspace. A peer that stopped at v10 therefore agrees on 10 and does not
/// hold the capability, whichever of the two it happens to have asked for.
#[test]
fn the_workspace_lifecycle_verb_arrives_under_a_version_and_a_capability() {
    // The verb arrived at 11 and the ceiling has moved past it since. A floor,
    // like the project verbs' above, proved by the negotiation rather than
    // asserted over constants.
    assert!(capability::ALL.contains(&capability::WORKSPACE_LIFECYCLE));
    CapabilityName::from_str(capability::WORKSPACE_LIFECYCLE).unwrap();
    // Not a second spelling of `terminate`: that one is the lease-holder
    // ending its own session, this one ends somebody else's.
    assert_ne!(capability::WORKSPACE_LIFECYCLE, capability::TERMINATE);

    let host = ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new([
            CapabilityName::from_str(capability::PROJECTS).unwrap(),
            CapabilityName::from_str(capability::WORKSPACE_LIFECYCLE).unwrap(),
        ]),
    };
    let pre_lifecycle = ProtocolOffer {
        versions: ProtocolRange { min: 1, max: 10 },
        capabilities: CapabilitySet::new([CapabilityName::from_str(capability::PROJECTS).unwrap()]),
    };
    let agreement = negotiate(&pre_lifecycle, &host).unwrap();
    assert_eq!(agreement.version, 10);
    assert!(agreement.capabilities.supports(capability::PROJECTS));
    assert!(
        !agreement
            .capabilities
            .supports(capability::WORKSPACE_LIFECYCLE)
    );

    let current = negotiate(&host, &host).unwrap();
    assert_eq!(current.version, MAX_PROTOCOL_VERSION);
    assert!(
        current
            .capabilities
            .supports(capability::WORKSPACE_LIFECYCLE)
    );
}

/// **v12 adds a verb to the lifecycle family and no capability to go with it.**
///
/// The project-rescope verb is a new request tag and a new response tag, so it
/// needs the version for v10's and v11's reason — an unknown tag is unreadable,
/// not ignorable. It does *not* get a capability of its own, and that is the
/// decision this test records: `workspace-lifecycle` names an authority family
/// rather than a frame, both verbs take the same launch-scoped secret, and no
/// peer would ever want to close a workspace's terminals while refusing to let
/// its projects follow the workspace. So the capability is what says a peer
/// speaks the family, and the version is what tells a v11 peer from a v12 one.
#[test]
fn the_project_rescope_verb_arrives_under_a_version_and_the_lifecycle_capability() {
    let host = ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new([
            CapabilityName::from_str(capability::PROJECTS).unwrap(),
            CapabilityName::from_str(capability::WORKSPACE_LIFECYCLE).unwrap(),
        ]),
    };
    // A peer that stopped at v11 holds the capability and still cannot be sent
    // a rescope: it would answer that the frame is unreadable, and the host is
    // what refuses first instead.
    let pre_migration = ProtocolOffer {
        versions: ProtocolRange { min: 1, max: 11 },
        capabilities: CapabilitySet::new([
            CapabilityName::from_str(capability::PROJECTS).unwrap(),
            CapabilityName::from_str(capability::WORKSPACE_LIFECYCLE).unwrap(),
        ]),
    };
    let agreement = negotiate(&pre_migration, &host).unwrap();
    assert_eq!(agreement.version, 11);
    assert!(
        agreement
            .capabilities
            .supports(capability::WORKSPACE_LIFECYCLE),
        "the capability is the family's and does not move with the version"
    );

    let current = negotiate(&host, &host).unwrap();
    assert_eq!(current.version, MAX_PROTOCOL_VERSION);

    // And no twelfth-version capability was minted alongside it. The count is
    // pinned rather than floored: a capability is append-only and a new one is
    // a decision, so this line is what makes adding one deliberate. (v14
    // minted `delegation`, the twenty-fourth; `tests::delegation` records it.)
    assert_eq!(capability::ALL.len(), 31);
}

/// **v13 adds a verb to the *project* family and no capability to go with it.**
///
/// The per-project scope rewrite is the orphan area's second resolution
/// (ADR-264: an orphaned project's only resolutions are move and delete). It is
/// a new request tag, so it needs the version for v10's reason — an unknown tag
/// is unreadable, not ignorable — and it takes no capability of its own, which
/// is the decision this test records. `projects` already names the whole family:
/// its verbs, its catalog subscription and its placement at create. A peer that
/// may rename, re-root and delete a project has no business being refused the
/// one field of it that had no verb, and minting a second capability would have
/// made "may edit a project" mean two different things.
///
/// It is also, deliberately, **not** gated like v12's rescope. That one is the
/// Gateway's, on the launch-scoped lifecycle authority, because it moves
/// somebody's projects between personas wholesale. This one is a person in a
/// window moving one project they can already see.
#[test]
fn the_project_scope_verb_arrives_under_a_version_and_the_projects_capability() {
    // The ceiling moved to v14 with the delegation family and is pinned in
    // `tests::delegation`; what this test proves is the v13 floor, by
    // negotiating against a capped peer.
    let window = ProtocolOffer {
        versions: ProtocolRange {
            min: MIN_PROTOCOL_VERSION,
            max: MAX_PROTOCOL_VERSION,
        },
        capabilities: CapabilitySet::new([CapabilityName::from_str(capability::PROJECTS).unwrap()]),
    };
    // A peer that stopped at v12 holds `projects` and still cannot be sent a
    // scope rewrite: it would answer that the frame is unreadable, and the host
    // is what refuses first instead.
    let pre_scope = ProtocolOffer {
        versions: ProtocolRange { min: 1, max: 12 },
        capabilities: CapabilitySet::new([CapabilityName::from_str(capability::PROJECTS).unwrap()]),
    };
    let agreement = negotiate(&pre_scope, &window).unwrap();
    assert_eq!(agreement.version, 12);
    assert!(
        agreement.capabilities.supports(capability::PROJECTS),
        "the capability is the family's and does not move with the version"
    );

    let current = negotiate(&window, &window).unwrap();
    assert_eq!(current.version, MAX_PROTOCOL_VERSION);
    assert!(current.capabilities.supports(capability::PROJECTS));

    // Still no capability minted for it, and in particular it did not join the
    // lifecycle family: this verb is not the Gateway's. (The twenty-fourth is
    // v14's `delegation`, a different family again.)
    assert_eq!(capability::ALL.len(), 31);
    assert!(
        !current
            .capabilities
            .supports(capability::WORKSPACE_LIFECYCLE),
        "the window's scope rewrite must not need the Gateway's authority"
    );
}

#[test]
fn the_project_error_codes_are_stable_and_correctly_classed() {
    for (code, class, token) in [
        (
            ErrorCode::ProjectNotFound,
            ErrorClass::NotFound,
            "project_not_found",
        ),
        (
            ErrorCode::ProjectHasSessions,
            ErrorClass::Conflict,
            "project_has_sessions",
        ),
        (
            ErrorCode::ProjectMismatch,
            ErrorClass::Conflict,
            "project_mismatch",
        ),
    ] {
        assert_eq!(code.as_str(), token);
        assert_eq!(code.class(), class);
        assert!(ErrorCode::ALL.contains(&code));
        assert_eq!(
            serde_json::from_str::<ErrorCode>(&format!("\"{token}\"")).unwrap(),
            code
        );
    }
}
