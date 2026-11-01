// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::collections::HashSet;

#[test]
fn no_route_is_listed_twice() {
    let mut seen = HashSet::new();
    for route in ROUTES {
        assert!(
            seen.insert((route.method, route.path)),
            "{} {} is listed twice",
            route.method,
            route.path
        );
    }
}

#[test]
fn every_query_parameter_belongs_to_one_public_route_and_is_unique() {
    let mut seen = HashSet::new();
    for parameter in QUERY_PARAMETERS {
        assert!(
            seen.insert((parameter.method, parameter.path, parameter.name)),
            "duplicate query parameter {} {} {}",
            parameter.method,
            parameter.path,
            parameter.name
        );
        let routes: Vec<_> = ROUTES
            .iter()
            .filter(|route| route.method == parameter.method && route.path == parameter.path)
            .collect();
        assert_eq!(
            routes.len(),
            1,
            "orphaned query parameter {}",
            parameter.name
        );
        assert_eq!(routes[0].audience, Audience::Public);
    }
}

#[test]
fn reminder_entity_query_is_a_required_string_in_openapi() {
    let document = openapi(Audience::Public, 1);
    let parameters = document["paths"]["/v1/profiles/{id}/reminders"]["get"]["parameters"]
        .as_array()
        .unwrap();
    let entity = parameters
        .iter()
        .find(|parameter| parameter["name"] == "entity")
        .unwrap();
    assert_eq!(entity["in"], "query");
    assert_eq!(entity["required"], true);
    assert_eq!(entity["schema"]["type"], "string");
    for name in ["id"] {
        let path = parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap();
        assert_eq!(path["in"], "path");
        assert_eq!(path["required"], true);
        assert_eq!(path["schema"]["type"], "string");
    }

    let item = document["paths"]["/v1/profiles/{id}/reminders/{reminder}"]["get"]["parameters"]
        .as_array()
        .unwrap();
    for name in ["id", "reminder"] {
        assert!(item.iter().any(|parameter| {
            parameter["name"] == name && parameter["in"] == "path" && parameter["required"] == true
        }));
    }
}

#[test]
fn reminder_history_entity_query_is_optional_for_old_client_compatibility() {
    let document = openapi(Audience::Public, 1);
    let parameters =
        document["paths"]["/v1/profiles/{id}/reminder-history"]["delete"]["parameters"]
            .as_array()
            .unwrap();
    let entity = parameters
        .iter()
        .find(|parameter| parameter["name"] == "entity")
        .unwrap();
    assert_eq!(entity["in"], "query");
    assert_eq!(entity["required"], false);
    assert_eq!(entity["schema"]["type"], "string");
}

#[test]
fn reminder_openapi_names_every_stable_error_shape() {
    let document = openapi(Audience::Public, 1);
    let failure_ref = "#/components/schemas/Failure";
    let reminder_routes: Vec<_> = ROUTES
        .iter()
        .filter(|route| is_reminder_route(route))
        .collect();
    assert_eq!(reminder_routes.len(), 21);
    for route in reminder_routes {
        let operation = &document["paths"][route.path][route.method.to_ascii_lowercase()];
        for status in ["400", "404"] {
            assert_eq!(
                operation["responses"][status]["content"]["application/json"]["schema"]["$ref"],
                failure_ref,
                "{} {} {status}",
                route.method,
                route.path
            );
        }
        for (status, _) in reminder_error_responses(route) {
            assert!(operation["responses"].get(status).is_some());
        }
    }

    let conflict =
        &document["paths"]["/v1/profiles/{id}/reminders/{reminder}/snooze"]["post"]["responses"];
    assert_eq!(
        conflict["409"]["description"],
        "idempotency_conflict or profile_unit_required"
    );
    let projection = &document["paths"]["/v1/profiles/{id}/reminders/query"]["post"]["responses"];
    assert_eq!(
        projection["503"]["description"],
        "projection_unavailable, reminder_index_updating or profile_private_unavailable"
    );
    assert!(
        document["paths"]["/v1/profiles/{id}/reminder-settings"]["patch"]["responses"]
            .get("409")
            .is_none()
    );
}

#[test]
fn every_route_is_under_the_version_segment() {
    // The leading segment is the API version. A route outside one would be a
    // surface with no versioning story at all.
    for route in ROUTES {
        assert!(
            route.path.starts_with("/v1/") || route.path.starts_with("/v2/"),
            "{}",
            route.path
        );
    }
}

#[test]
fn every_route_is_described() {
    for route in ROUTES {
        assert!(!route.summary.is_empty(), "{}", route.path);
        assert!(!route.returns.is_empty(), "{}", route.path);
    }
}

#[test]
fn methods_are_uppercase_and_ordinary() {
    for route in ROUTES {
        assert!(
            ["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&route.method),
            "{} {}",
            route.method,
            route.path
        );
    }
}

#[test]
fn a_mutation_never_requires_only_a_read_scope() {
    // The check that stops a read-only credential from being able to change
    // something. Listing routes may read; anything that writes must ask for a
    // scope that says so.
    for route in ROUTES {
        if route.method == "GET" {
            continue;
        }
        match route.scope {
            Some(
                Scope::EditDocuments
                | Scope::ManageWorkspaces
                | Scope::DestroyWorkspaces
                | Scope::MigrateWorkspaces,
            ) => {}
            // **POSTs that only read.** They are a POST because they are
            // *work* — opening a document, walking a whole workspace into a
            // bundle — and neither changes anything, so a read scope is right.
            // Named individually rather than by widening the rule: the next
            // read-scoped POST should have to argue for itself here.
            _ if route.path.contains("/snapshot/") => {}
            _ if route.path.ends_with("/snapshot") => {}
            _ if route.path.ends_with("/export") => {}
            // **`/v1/search` is a POST for privacy, not for mutation.** The
            // access logger strips query values, and a body additionally keeps
            // search text out of URL histories, proxies and copied links. It
            // changes nothing, so a read scope is right.
            _ if route.path == "/v1/search" => {}
            // **`/v1/attributes/query` is a POST for the same privacy
            // reason.** The body names attribute keys and tag ids somebody is
            // filtering on; it reads the derived read model and changes
            // nothing.
            _ if route.path == "/v1/attributes/query" => {}
            // **`/v1/tasks/query` is the attribute query's second question**
            // and is a POST for the same privacy reason: the body carries the
            // same filters plus a lifecycle and a cursor. It reads the same
            // derived read model and changes nothing — the release ships no
            // semantic write over HTTP at all, so there is no task mutation
            // for a scope to guard here (roadmap §1, SPEC §10). `ReadTasks`
            // exists, unenforced, and is deliberately not consulted: enforcing
            // a second scope on one route would be half a permission model
            // whose whole is deferred.
            _ if route.path == "/v1/tasks/query" => {}
            // Reminder category queries carry explicit device time context in
            // the body; they are reads despite using POST.
            _ if route.path == "/v1/profiles/{id}/reminders/query" => {}
            // **A reader's own private view of a document is a write that
            // needs only read access.** Folding a toggle changes nothing in
            // the document and nothing anyone else sees; requiring
            // `EditDocuments` would stop a reader of a document they cannot
            // edit from closing a toggle in it. Logged as a principal-model
            // gap in the collapse/expand plan: there is no reader-own-state
            // scope yet.
            _ if route.path == "/v1/profiles/{id}/documents/{document}/private-state" => {}
            other => panic!(
                "{} {} writes but requires {other:?}",
                route.method, route.path
            ),
        }
    }
}

/// **Renaming a marker asks for exactly what capturing one asks for.**
///
/// A label is content a person wrote about their own document, so a caller that
/// may not write the document may not write what it is called. Pinned against
/// the capture route rather than spelled out, because the point is that the two
/// agree: if capture's scope ever moves, this fails rather than quietly leaving
/// a weaker way in through the back.
#[test]
fn renaming_a_history_marker_costs_the_same_scope_as_capturing_one() {
    let scope_of = |method: &str, path: &str| {
        ROUTES
            .iter()
            .find(|r| r.method == method && r.path == path)
            .unwrap_or_else(|| panic!("{method} {path} is not in the table"))
            .scope
    };
    let capture = scope_of("POST", "/v1/workspaces/{id}/documents/{document}/history");
    let relabel = scope_of("POST", "/v1/workspaces/{id}/history/{entry}/label");
    assert_eq!(relabel, capture, "{relabel:?} vs {capture:?}");
    assert_eq!(relabel, Some(Scope::EditDocuments));
}

/// **Moving a workspace is its own permission, and only that route holds it.**
///
/// The three workspace permissions are deliberately three: `manage_workspaces`
/// is in every ordinary token, `destroy_workspaces` takes a vouched grant, and
/// this one takes a vouched grant of its own so a window trusted to remove a
/// workspace is not thereby trusted to hand one to another persona. A second
/// route appearing under `migrate_workspaces` would be a second way to change
/// an owner, and there is exactly one.
#[test]
fn moving_a_workspace_is_the_only_route_that_takes_the_migration_scope() {
    let migrating: Vec<&str> = ROUTES
        .iter()
        .filter(|route| route.scope == Some(Scope::MigrateWorkspaces))
        .map(|route| route.path)
        .collect();
    assert_eq!(migrating, ["/v1/workspaces/{id}/move"]);
    // And it is not reachable by holding either of its neighbours.
    let move_route = ROUTES
        .iter()
        .find(|route| route.path == "/v1/workspaces/{id}/move")
        .expect("the move route is in the table");
    assert_eq!(move_route.method, "POST");
    assert_ne!(move_route.scope, Some(Scope::ManageWorkspaces));
    assert_ne!(move_route.scope, Some(Scope::DestroyWorkspaces));
}

#[test]
fn only_the_public_facts_are_scope_free() {
    // A scope-free route still requires a token — it is "any authenticated
    // caller", never "anyone". Only two routes qualify, and both describe the
    // store rather than anything in it.
    let free: Vec<&str> = ROUTES
        .iter()
        .filter(|r| r.scope.is_none())
        .map(|r| r.path)
        .collect();
    assert_eq!(free, ["/v1/status", "/v1/openapi.json"], "{free:?}");
}

/// **No path is spelled anywhere but this table.**
///
/// The fold makes the table the only list, and this is what stops a second one
/// growing back: a `.route(..)` beside a handler would be served, undescribed,
/// and invisible to every other test here — which is exactly how eleven routes
/// escaped before the fold existed.
///
/// The route modules do not import `axum::Router`, so `.route` is not callable
/// in them today and re-adding the import is visible in review. This catches
/// the case where somebody adds it back anyway.
#[test]
fn no_route_is_registered_outside_this_table() {
    for (name, source) in [
        ("status.rs", include_str!("../routes/status.rs")),
        ("meta.rs", include_str!("../routes/meta.rs")),
        ("admin.rs", include_str!("../routes/admin.rs")),
        ("sync.rs", include_str!("../routes/sync.rs")),
        ("bundles.rs", include_str!("../routes/bundles.rs")),
        ("debug.rs", include_str!("../routes/debug.rs")),
        ("workspaces.rs", include_str!("../routes/workspaces.rs")),
        ("reminders.rs", include_str!("../routes/reminders.rs")),
    ] {
        assert!(
            !source.contains(".route("),
            "routes/{name} registers a path of its own; every path belongs in ROUTES"
        );
        assert!(
            !source.contains("Router::new()"),
            "routes/{name} builds a router of its own; api_catalog::routes is the only one"
        );
    }
}

/// Every row is wired to something, and the wiring is what is served.
///
/// `(handler)()` is called for all of them, so a row whose closure panics —
/// axum's `MethodRouter` builders do panic on a malformed path — fails here
/// rather than at start-up in front of a person.
#[test]
fn every_row_builds_and_the_paths_are_grouped() {
    let _router = routes();
    let mut paths: Vec<&str> = ROUTES.iter().map(|r| r.path).collect();
    paths.sort_unstable();
    paths.dedup();
    // The combined account/lifecycle, task/reminder, document-version and
    // Cloud catalog has 96 rows over 76 paths. Account binding and local
    // removal/restoration share method-paired paths; lifecycle inventory
    // and enrolled Profile provisioning are internal, and so are the blob
    // pause, the blob release, the Profile transport mode, the documents
    // waiting on another device and one document's own Cloud coverage
    // (D256, D258, rung 2's space reclaim), as is the Welcome page's
    // installation-local progress (a query path and a per-row path). A
    // reader's private document state is one method-paired path.
    assert_eq!(ROUTES.len(), 98, "a row was added or removed");
    assert_eq!(paths.len(), 77, "a path was added or removed");
}

#[test]
fn the_complete_reminder_family_is_public_with_exact_scopes() {
    let expected = [
        ("GET", "/v1/profiles/{id}/reminders", Scope::ReadDocuments),
        ("POST", "/v1/profiles/{id}/reminders", Scope::EditDocuments),
        (
            "GET",
            "/v1/profiles/{id}/reminders/{reminder}",
            Scope::ReadDocuments,
        ),
        (
            "GET",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
            Scope::ReadDocuments,
        ),
        (
            "PUT",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
            Scope::EditDocuments,
        ),
        (
            "DELETE",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/pause",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/resume",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/snooze",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/workspaces/{workspace}/reminders/{reminder}/handle",
            Scope::EditDocuments,
        ),
        (
            "PUT",
            "/v1/profiles/{id}/reminders/{reminder}",
            Scope::EditDocuments,
        ),
        (
            "DELETE",
            "/v1/profiles/{id}/reminders/{reminder}",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/reminders/{reminder}/pause",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/reminders/{reminder}/resume",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/reminders/{reminder}/snooze",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/reminders/{reminder}/handle",
            Scope::EditDocuments,
        ),
        (
            "DELETE",
            "/v1/profiles/{id}/reminder-history",
            Scope::EditDocuments,
        ),
        (
            "GET",
            "/v1/profiles/{id}/reminder-settings",
            Scope::ReadDocuments,
        ),
        (
            "PATCH",
            "/v1/profiles/{id}/reminder-settings",
            Scope::EditDocuments,
        ),
        (
            "POST",
            "/v1/profiles/{id}/reminders/query",
            Scope::ReadDocuments,
        ),
        (
            "GET",
            "/v1/profiles/{id}/reminders/reconciliation",
            Scope::ReadDocuments,
        ),
    ];
    let actual: Vec<_> = ROUTES
        .iter()
        .filter(|route| route.path.contains("/reminder"))
        .map(|route| {
            assert_eq!(route.audience, Audience::Public, "{}", route.path);
            (route.method, route.path, route.scope.unwrap())
        })
        .collect();
    assert_eq!(actual, expected);
}

/// **The capability names the surface, not the storage.**
///
/// `search-fts5` was the proposed token. FTS5 is SQLite's, and the document is
/// tested never to name SQLite — but the deciding reason is that it would tell
/// a client nothing it could act on: query text is normalised into terms before
/// it reaches `MATCH`, so no operator, quotation mark or column filter a caller
/// types is ever interpreted. There is no query syntax to negotiate.
#[test]
fn the_search_capability_names_no_storage() {
    let capabilities = koru_store_core::version::CAPABILITIES;
    assert!(capabilities.contains(&"search"), "{capabilities:?}");
    for capability in capabilities {
        assert!(
            !capability.contains("fts") && !capability.contains("sqlite"),
            "{capability} names what is underneath"
        );
    }
}

// ----------------------------------------------------------------- audiences

/// **Internal routes are ours.** They include diagnostics and adapter seams
/// whose storage-shaped payload must not become a public protocol. Pinned as a
/// list rather than a rule, so moving a route between audiences is a decision
/// somebody makes on purpose.
#[test]
fn only_deliberate_adapter_and_diagnostic_routes_are_internal() {
    let internal: Vec<&str> = ROUTES
        .iter()
        .filter(|r| r.audience == Audience::Internal)
        .map(|r| r.path)
        .collect();
    assert_eq!(
        internal,
        [
            "/v1/internal/profiles/lifecycle-inventory",
            "/v1/internal/profiles/{id}/account-provisioning",
            // Ours: this installation's Welcome page progress. It is UI state
            // shared by every Profile on this machine and kept in the catalog's
            // installation-local `store_state`, so it promises a third party
            // nothing.
            "/v1/internal/welcome-progress",
            "/v1/internal/welcome-progress/{row}",
            "/v1/internal/workspaces/catalog-watch",
            "/v1/internal/workspaces/{id}/documents/seeded",
            "/v2/internal/workspaces/{id}/documents/seeded",
            // Ours: a Cloud diagnostic whose fields change whenever something
            // new is worth seeing, and which promises a third party nothing.
            "/v1/workspaces/{id}/documents/{document}/cloud-sync",
            // Ours: the same diagnostic narrowed to one document's own
            // coverage, for the context column beside that document.
            "/v1/workspaces/{id}/documents/{document}/cloud-sync/coverage",
            // Ours: the documents behind that diagnostic's *waiting for the
            // device that has these files*, for this installation's own
            // window. It carries titles, which is why it is not a report.
            "/v1/workspaces/{id}/cloud-sync/waiting",
            // Ours: a person's pause on this device's transfer engine, and
            // the device-local transport mode behind the Profile prompt.
            // Both are facts about *this installation* rather than about a
            // workspace anybody else can see (D230), so neither is a promise
            // to a third party.
            "/v1/workspaces/{id}/blobs/pause",
            "/v1/workspaces/{id}/blobs/release",
            "/v1/profiles/{id}/transport-mode",
            "/v1/debug/openapi.json",
            "/v1/debug/store",
            "/v1/debug/workspaces/{id}",
            "/v1/debug/documents/{id}",
            "/v1/debug/projection",
            "/v1/debug/search",
            "/v1/debug/attributes",
            "/v1/debug/tasks",
            "/v1/debug/sync",
            "/v1/metrics",
        ],
        "{internal:?}"
    );
}

/// The public document is a *subset*, and the internal one is everything.
///
/// The second half matters as much as the first: an internal document that
/// omitted the public routes would make our own tooling read two files and
/// merge them, and a counter is only useful beside the operation it explains.
#[test]
fn the_public_document_hides_the_diagnostics_and_the_internal_one_hides_nothing() {
    let public = openapi(Audience::Public, 1);
    let internal = openapi(Audience::Internal, 1);
    let public_paths = public["paths"].as_object().unwrap();
    let internal_paths = internal["paths"].as_object().unwrap();

    for route in ROUTES {
        assert!(
            internal_paths.contains_key(route.path),
            "{} is missing from the internal document",
            route.path
        );
        match route.audience {
            Audience::Public => assert!(
                public_paths.contains_key(route.path),
                "{} is missing from the public document",
                route.path
            ),
            Audience::Internal => assert!(
                !public_paths.contains_key(route.path),
                "{} is internal but published",
                route.path
            ),
        }
    }
    assert!(public_paths.len() < internal_paths.len());
}

/// And the internal document says out loud that it is not the contract, so
/// somebody who finds it does not build against it by accident.
#[test]
fn the_internal_document_points_at_the_public_one() {
    let internal = openapi(Audience::Internal, 1);
    let title = internal["info"]["title"].as_str().unwrap();
    let description = internal["info"]["description"].as_str().unwrap();
    assert!(title.contains("internal"), "{title}");
    assert!(description.contains("/v1/openapi.json"), "{description}");
    assert!(
        description.contains("no compatibility promise"),
        "{description}"
    );
}

// ------------------------------------------------------------------ the document

#[test]
fn the_document_covers_every_route() {
    let document = openapi(Audience::Internal, 1);
    let paths = document["paths"].as_object().expect("paths");
    for route in ROUTES {
        let entry = paths
            .get(route.path)
            .unwrap_or_else(|| panic!("{} is not in the document", route.path));
        assert!(
            entry.get(route.method.to_lowercase()).is_some(),
            "{} {} is not in the document",
            route.method,
            route.path
        );
    }
}

#[test]
fn the_document_describes_nothing_that_is_not_a_route() {
    let document = openapi(Audience::Internal, 1);
    let paths = document["paths"].as_object().expect("paths");
    for (path, operations) in paths {
        for method in operations.as_object().expect("operations").keys() {
            assert!(
                ROUTES
                    .iter()
                    .any(|r| r.path == path && r.method.to_lowercase() == *method),
                "the document describes {method} {path}, which does not exist"
            );
        }
    }
}

#[test]
fn the_document_says_every_route_needs_a_bearer_token() {
    let document = openapi(Audience::Internal, 1);
    assert!(document["components"]["securitySchemes"]["bearerAuth"].is_object());
    let paths = document["paths"].as_object().unwrap();
    for (path, operations) in paths {
        for (method, operation) in operations.as_object().unwrap() {
            assert!(
                operation["security"][0]["bearerAuth"].is_array(),
                "{method} {path} does not say it needs a token"
            );
        }
    }
}

#[test]
fn the_document_names_the_scope_each_route_needs() {
    let document = openapi(Audience::Internal, 1);
    for route in ROUTES.iter().filter(|r| r.scope.is_some()) {
        let scopes =
            document["paths"][route.path][route.method.to_lowercase()]["security"][0]["bearerAuth"]
                .as_array()
                .unwrap();
        assert_eq!(
            scopes[0].as_str(),
            route.scope.map(|s| s.as_str()),
            "{} {}",
            route.method,
            route.path
        );
    }
}

#[test]
fn the_document_describes_the_error_shape_clients_branch_on() {
    let document = openapi(Audience::Internal, 1);
    let failure = &document["components"]["schemas"]["Failure"];
    assert!(failure["properties"]["code"].is_object());
    assert!(failure["properties"]["message"].is_object());
    // And it says out loud that the message is not for a person, because that
    // is the mistake an integrator would otherwise make.
    let note = failure["properties"]["message"]["description"]
        .as_str()
        .unwrap();
    assert!(note.contains("never shown"), "{note}");
}

#[test]
fn the_document_says_the_store_is_loopback_only() {
    let document = openapi(Audience::Internal, 1);
    let server = document["servers"][0]["url"].as_str().unwrap();
    assert!(server.contains("127.0.0.1"), "{server}");
}

/// A third party never needs to learn that SQLite is underneath, and telling
/// them makes it a compatibility surface we would have to keep.
///
/// **Generated schemas made this test much larger than it looks.** It used to
/// see a handful of hand-written sentences; it now sees every field name and
/// every doc comment of every type on the wire, because `schemars` publishes
/// them as `description`. Two leaked immediately — `Timestamp` explaining that
/// it is signed "because that is what SQLite stores", and `DocumentEntry`
/// saying it is mirrored "into SQL" — both written for whoever maintains the
/// code and both entirely correct as Rust docs.
///
/// The fix is not to reword the Rust. It is `#[schemars(description = "…")]`
/// beside the type, so the maintainer's paragraph and the caller's stay
/// separate. When this fails, that is the tool to reach for.
///
/// **Public only.** The internal document says `database_bytes` and means it.
#[test]
fn no_route_description_leaks_the_physical_layout() {
    let document = openapi(Audience::Public, 1).to_string().to_lowercase();
    for leak in [
        "sqlite",
        "catalog.db",
        "workspace.db",
        "loro_updates",
        "loro_checkpoints",
        "appdata",
        "select ",
        " sql ",
        "lorodoc",
    ] {
        assert!(!document.contains(leak), "the document mentions {leak}");
    }
}

/// Every `$ref` points at a schema that exists, and every schema is pointed at.
///
/// The second half is what catches a type left behind when a route is deleted:
/// a definition nothing references is a shape we are still publishing and no
/// longer serving.
#[test]
fn every_schema_is_referenced_and_every_reference_resolves() {
    for audience in [Audience::Public, Audience::Internal] {
        let document = openapi(audience, 1);
        let defined: HashSet<String> = document["components"]["schemas"]
            .as_object()
            .expect("schemas")
            .keys()
            .cloned()
            .collect();

        let mut referenced = HashSet::new();
        collect_refs(&document, &mut referenced);

        for name in &referenced {
            assert!(
                defined.contains(name),
                "{audience:?}: $ref to {name}, which is not defined"
            );
        }
        for name in &defined {
            assert!(
                referenced.contains(name),
                "{audience:?}: {name} is defined and referenced by nothing"
            );
        }
    }
}

fn collect_refs(value: &serde_json::Value, into: &mut HashSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref"
                    && let Some(name) = child
                        .as_str()
                        .and_then(|r| r.strip_prefix("#/components/schemas/"))
                {
                    into.insert(name.to_string());
                }
                collect_refs(child, into);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_refs(item, into);
            }
        }
        _ => {}
    }
}

/// **The document describes shapes now, not just URLs.**
///
/// The thing this stage was for: a customer could not previously learn that a
/// workspace has an `icon` field without reading our Rust.
#[test]
fn a_caller_can_read_a_field_name_out_of_the_document() {
    let document = openapi(Audience::Public, 1);
    let schemas = &document["components"]["schemas"];
    let appearance = &schemas["WorkspaceAppearance"]["properties"];
    for field in ["icon", "color", "pinned", "archived"] {
        assert!(
            appearance.get(field).is_some(),
            "{field} is not in the published schema"
        );
    }
    // And a request body points at one, rather than describing it in prose.
    let patch = &document["paths"]["/v1/workspaces/{id}/appearance"]["patch"];
    assert_eq!(
        patch["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/WorkspaceAppearance"
    );
}

#[test]
fn the_document_carries_the_api_version_it_was_asked_for() {
    assert_eq!(openapi(Audience::Internal, 1)["info"]["version"], "1");
    assert_eq!(openapi(Audience::Internal, 7)["info"]["version"], "7");
}
