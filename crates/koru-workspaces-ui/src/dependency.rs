// SPDX-License-Identifier: MPL-2.0

//! **The fence around this controller's dependencies, in both directions.**
//!
//! Down: a workspace controller that linked `koru-core`, `koru-workspace`,
//! `koru-editor` or `koru-workspace-host` could name a `WorkspaceDoc`, and a
//! controller that could name one would be tempted to read one — the
//! charter's own sentence. The catalog's authority is the Store behind the
//! Gateway, and this crate only asks it.
//!
//! Up: nothing may treat the controller's projection of the catalog as the
//! catalog. It is read to *draw* it — the Manage tab's Profile pages do — and
//! for nothing else.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// The manifest is an exact allowlist, and the prose comment above each line is
/// the human half of the same fence.
#[test]
fn the_manifest_names_only_what_a_neutral_controller_may_link() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let section = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    let actual: BTreeSet<_> = section
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .map(|name| name.split('.').next().unwrap())
        .collect();
    assert_eq!(
        actual,
        BTreeSet::from([
            "koru-api-client",
            "koru-api-wire",
            "koru-i18n",
            "koru-store-core",
            "koru-ui",
            // The link worker's log. A facade that writes lines and reads
            // nothing, already in the graph through koru-ui.
            "tracing",
        ]),
        "the workspace controller holds rows and talks to the typed client; it \
         draws nothing, and a dependency beyond this list is one it has no use \
         for"
    );
}

/// The transitive graph, with a reason per entry.
#[test]
fn the_normal_graph_holds_no_document_workspace_or_session() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--manifest-path"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .args(["-e", "normal", "--prefix", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    let packages: BTreeSet<_> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    for forbidden in [
        // The model, and everything that projects it. The charter's own
        // sentence: this controller names no `WorkspaceDoc`.
        "koru-core",
        "koru-workspace",
        "koru-editor",
        "koru-document",
        "koru-blocks",
        // Koru's window host. Reaching this for a workspace is exactly what
        // kept the old manager inside `apps/koru`.
        "koru-workspace-host",
        // The store itself. This controller opens no database and runs no query;
        // it asks the Gateway, like every caller since SP-6b.
        "koru-store-server",
        "rusqlite",
        // The session host and its client. **The most important row here.**
        // This controller's whole destructive story is that terminating a
        // workspace's terminals belongs to the Gateway operation, over a
        // private bridge, under a supervised authority (ADR-267). A manager
        // that could reach a session client could close a terminal directly,
        // and the five-step ordering would become a convention instead of a
        // contract.
        "koru-session-client",
        "koru-session-wire",
        // The authority's own host. Consuming the *client* is the contract.
        "koru-api-gateway-core",
    ] {
        assert!(
            !packages.contains(forbidden),
            "forbidden dependency {forbidden}: the workspace controller names \
             no document, no workspace host, no store and no session"
        );
    }
}

/// **The controller's projection is read only to be drawn.**
///
/// The controller holds a projection of the catalog so a page can draw it;
/// that projection is not an authority and must not become one by being
/// convenient. Its one reader is the Manage tab's *Workspaces* on a Profile's
/// page, which lists `rows_of` the Profile it shows. A caller that wants the
/// workspace list for anything else asks
/// `koru_api_client::workspaces().catalog()`.
#[test]
fn only_the_manage_tab_reads_the_projection_and_only_to_draw_it() {
    let root = workspace_root();
    let drawing = root
        .join("apps")
        .join("koru")
        .join("src")
        .join("manage")
        .join("workspaces.rs");
    let offenders: Vec<String> = rust_sources(&root)
        .into_iter()
        .filter(|file| !file.starts_with(root.join("crates").join("koru-workspaces-ui")))
        .filter(|file| *file != drawing)
        .filter(|file| {
            std::fs::read_to_string(file)
                .is_ok_and(|text| text.contains("koru_workspaces_ui") && text.contains(".rows_of("))
        })
        .map(|file| relative(&root, &file))
        .collect();
    assert!(
        offenders.is_empty(),
        "a caller is reading the workspace catalog out of the controller: {offenders:?}"
    );
}

/// **There is exactly one workspace manager.**
///
/// SP-8b retired the app's own row-painting manager when this crate's surface
/// replaced it; ADR-449-onemanagetab retired the Workspaces tab and that
/// surface in turn. The one drawing left is a Profile page's *Workspaces*. A
/// test rather than a note, because "we meant to remove it" is how two managers
/// happen.
#[test]
fn no_second_workspace_manager_survives() {
    let root = workspace_root();
    let src = root.join("apps").join("koru").join("src");
    for retired in [
        src.join("workspaces").join("draw.rs"),
        src.join("workspaces.rs"),
        root.join("crates")
            .join("koru-workspaces-ui")
            .join("src")
            .join("column.rs"),
    ] {
        assert!(
            !retired.exists(),
            "a retired workspace manager is back: {}",
            retired.display()
        );
    }
}

fn rust_sources(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for area in ["crates", "apps"] {
        walk(&root.join(area), &mut out);
    }
    out
}

fn walk(directory: &Path, into: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            walk(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/koru-workspaces-ui sits two levels under the workspace root")
        .to_path_buf()
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}
