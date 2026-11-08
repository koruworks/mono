// SPDX-License-Identifier: MPL-2.0

//! **Every crate a document names is a crate in this workspace** (techdebt 90).
//!
//! A rename swept `docs/` by hand twice, and each sweep left references to
//! crates that no longer existed under that name. This test scans
//! `guide/**/*.md`, `docs/**/*.md`, `reference/**/*.md`, `licensing/**/*.md`, `AGENTS.md` and
//! `README.md` for a `koru-*` name used as a
//! crate reference and fails when the name is not a workspace member's package
//! (or one of its binaries), so the next rename or deletion is caught here
//! rather than by a reader who cannot find the crate.
//!
//! It lives in `koru-release` because this is the crate whose tests already
//! hold the repository to itself (`every_platform_carries_the_workspace_version`
//! reads the root manifest and the mobile projects the same way).
//!
//! **What counts as a crate reference** — prose that only *mentions* a
//! `koru-*` token (a kind such as `koru-system`, a Docker volume, a host name)
//! is not one:
//!
//! - a path segment `crates/<name>` or `apps/<name>`, including inside a
//!   relative link (`../../crates/<name>/src/lib.rs`);
//! - `` `<name>` crate `` or `` crate `<name>` `` / `` crates `<name>` ``;
//! - `cargo … -p <name>`.
//!
//! **What is history and not scanned:** a document that records what existed
//! when it was written — `archive/` and `implemented/` folders, plan and
//! roadmap revisions (`*.revN.md`, `*.revN.notes.md`) and signed-off plans
//! (`*.final.md`). ADRs *are* scanned: their crate names follow the crate
//! (QUESTIONS #13), and the few that name a deleted crate are listed in
//! [`FORMER`].
//!
//! **It runs on whatever part of `docs/` the tree has.** A document that is
//! absent asks for no allowlist entry, and the names of the hosted Koru Cloud
//! services ([`HOSTED`]), which are not in this repository, are accepted.
//! Every document that is present is still checked in full.
//!
//! **`docs/` is a separate, private checkout** (a git submodule) that an
//! outside contributor does not have. Without it — missing, or present and
//! empty — the test says so on stderr and checks the documents this
//! repository carries itself; with it, every document is checked.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// **Names that look like crates and are not, anywhere they appear.** Each
/// says why, and names the documents that make it needed: the entry must be
/// used while any of them is in the tree (see [`present`]).
const NOT_CRATES: &[(&str, &str, &[&str])] = &[
    // PRINCIPLES.md and ADR-101-125 describe the rejected alternative of a
    // leftovers crate under this name; it was never created.
    (
        "koru-desktop",
        "a rejected proposal, never created",
        &[
            "guide/contributing/PRINCIPLES.md",
            "docs/decisions/ADR-101-125.md",
        ],
    ),
    // ADR-151-175 rejects a platform crate for the system reader.
    (
        "koru-platform",
        "a rejected proposal, never created",
        &["docs/decisions/ADR-151-175.md"],
    ),
    // ADR-351-375 puts the Delegate surface in the app rather than in a
    // `-ui` crate of this name.
    (
        "koru-delegate-ui",
        "a rejected proposal, never created",
        &["docs/decisions/ADR-351-375.md"],
    ),
    // PRODUCT_METRICS.md recommends a first implementation under this name;
    // outbound product telemetry is unimplemented and the hosted ingest is
    // `koru-metrics-server`.
    (
        "koru-metrics",
        "a proposed crate, never created",
        &["docs/features/koru-ai/PRODUCT_METRICS.md"],
    ),
];

/// **The hosted Koru Cloud services.** They are proprietary and not in this
/// repository, while documents may still name them — the licensing pages say
/// which packages are proprietary. A name here must not be a workspace member:
/// the test fails if one becomes one, so the list cannot hide a real crate.
const HOSTED: &[&str] = &[
    "koru-public-api",
    "koru-metrics-server",
    "koru-account-api",
    "koru-account-apple",
    "koru-account-google",
    "koru-account-mail",
    "koru-account-observability",
    "koru-account-postgres",
    "koru-object-worker",
    "koru-cloud-storage",
    "koru-replica-host",
    "koru-metrics-posthog",
    "koru-server-env",
];

/// **Crates that existed and were deleted or never built, and the documents
/// allowed to name them** — each one records that history (or the plan that
/// proposed the crate) rather than describing today's code. A new reference
/// anywhere else fails: describe today's code by its current crate instead.
const FORMER: &[(&str, &[&str])] = &[
    // The standalone Koru Terminal app, deleted by ADR-279; these record its
    // removal or decisions taken while it existed, and techdebt 60-62 are
    // entries about its code.
    (
        "koru-terminal",
        &[
            "docs/architecture/CRATES.md",
            "docs/features/macos/CATCHUP.md",
            "docs/contributing/techdebt/README.md",
        ],
    ),
    // Deleted in the pre-release gateway cutover; ADR-105-ipc records what it
    // held while it existed.
    ("koru-store-discovery", &["docs/decisions/ADR-101-125.md"]),
    // The standalone Koru AI window, retired 2026-11-03 (ADR-191's
    // amendment). The ADRs record decisions taken while it existed and its
    // retirement; ICONS.md is the dated 2026-09-13 icon scan; the refactor
    // results and the icon plan's execution log are history.
    (
        "koru-ai-chat",
        &[
            "docs/decisions/ADR-176-200.md",
            "docs/decisions/ADR-251-275.md",
            "docs/features/koruui/ICONS.md",
            "docs/contributing/refactor-ui/ELEMENTS-RESULTS.md",
            "docs/koru-icons/EXECUTION.md",
        ],
    ),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// **Whether a document is in this tree.** `docs/` is a separate checkout
/// that may be absent, so an allowlist entry whose documents are all absent
/// cannot be used and is not stale; every document that *is* present is
/// still scanned in full.
fn present(root: &Path, doc: &str) -> bool {
    root.join(doc).is_file()
}

/// Every workspace member's package name and binary names, read from the
/// root manifest's `members` (a trailing `/*` expands to every directory with
/// a `Cargo.toml`) less its `exclude`.
fn workspace_names(root: &Path) -> BTreeSet<String> {
    let manifest: toml::Table = std::fs::read_to_string(root.join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let workspace = manifest["workspace"].as_table().unwrap();
    let list = |key: &str| -> Vec<String> {
        workspace
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let excluded: BTreeSet<PathBuf> = list("exclude").iter().map(|e| root.join(e)).collect();
    let mut dirs = Vec::new();
    for member in list("members") {
        match member.strip_suffix("/*") {
            Some(parent) => {
                for entry in std::fs::read_dir(root.join(parent)).unwrap() {
                    dirs.push(entry.unwrap().path());
                }
            }
            None => dirs.push(root.join(member)),
        }
    }
    let mut names = BTreeSet::new();
    for dir in dirs {
        let path = dir.join("Cargo.toml");
        if excluded.contains(&dir) || !path.is_file() {
            continue;
        }
        let crate_manifest: toml::Table = std::fs::read_to_string(&path)
            .unwrap()
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let Some(package) = crate_manifest
            .get("package")
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        names.insert(package["name"].as_str().unwrap().to_owned());
        for bin in crate_manifest
            .get("bin")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = bin.get("name").and_then(toml::Value::as_str) {
                names.insert(name.to_owned());
            }
        }
        if let Ok(bins) = std::fs::read_dir(dir.join("src/bin")) {
            for entry in bins.flatten() {
                let path = entry.path();
                let stem = if path.is_dir() {
                    path.file_name()
                } else {
                    path.file_stem()
                };
                if let Some(stem) = stem.and_then(|s| s.to_str()) {
                    names.insert(stem.to_owned());
                }
            }
        }
    }
    names
}

/// Whether `relative` (forward slashes) is a history document this test
/// does not scan — see the module comment.
fn is_history(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let revision = name
        .split(".rev")
        .skip(1)
        .any(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    relative.contains("/archive/")
        || relative.contains("/implemented/")
        || revision
        || name.ends_with(".final.md")
}

/// Whether the `docs/` checkout is here: the directory exists and holds
/// anything at all. An uninitialised submodule is an empty directory.
fn docs_checkout_present(docs: &Path) -> bool {
    std::fs::read_dir(docs).is_ok_and(|mut entries| entries.next().is_some())
}

fn markdown_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// A `koru-*` name starting at `text`, or `None`: lowercase ASCII letters,
/// digits and single hyphens, never ending in a hyphen.
fn koru_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("koru-")?;
    let len = rest
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    let name = text[..5 + len].trim_end_matches('-');
    (name.len() > 5).then_some(name)
}

/// The crate references on one line, by the rules in the module comment.
fn crate_references(line: &str) -> Vec<&str> {
    let mut found = Vec::new();
    // Path segments: `crates/<name>` and `apps/<name>`, not glued to a word.
    for prefix in ["crates/", "apps/"] {
        for (at, _) in line.match_indices(prefix) {
            let glued = line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if glued {
                continue;
            }
            let rest = &line[at + prefix.len()..];
            if let Some(name) = koru_name(rest) {
                // `crates/koru-store-*` names a family, not a crate.
                if !rest[name.len()..].starts_with(['-', '*']) {
                    found.push(name);
                }
            }
        }
    }
    // `cargo … -p <name>`.
    for (at, _) in line.match_indices("-p ") {
        if let Some(name) = koru_name(line[at + 3..].trim_start()) {
            found.push(name);
        }
    }
    // A backticked name beside the word "crate".
    for (at, _) in line.match_indices("`koru-") {
        let body = &line[at + 1..];
        let Some(name) = koru_name(body) else {
            continue;
        };
        if !body[name.len()..].starts_with('`') {
            continue;
        }
        let after = &body[name.len() + 1..];
        let before = line[..at].trim_end();
        let crate_after = after
            .strip_prefix(" crate")
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() && c != 's'));
        let crate_before = before.ends_with(" crate") || before.ends_with(" crates");
        if crate_after || crate_before {
            found.push(name);
        }
    }
    found
}

#[test]
fn every_crate_a_document_names_is_a_workspace_member() {
    let root = root();
    let names = workspace_names(&root);
    assert!(
        names.contains("koru-core") && names.contains("koru-agent") && names.contains("koru"),
        "the workspace members were not read: {names:?}"
    );
    let hosted_members: Vec<&str> = HOSTED
        .iter()
        .copied()
        .filter(|name| names.contains(*name))
        .collect();
    assert!(
        hosted_members.is_empty(),
        "HOSTED in crates/koru-release/src/docs_crates.rs lists workspace members: \
         {hosted_members:?}. List only the hosted services, which are not in this repository."
    );
    let mut files = vec![root.join("AGENTS.md"), root.join("README.md")];
    for dir in ["guide", "reference", "licensing"] {
        markdown_under(&root.join(dir), &mut files);
    }
    let docs = root.join("docs");
    if docs_checkout_present(&docs) {
        markdown_under(&docs, &mut files);
    } else {
        eprintln!(
            "note: docs/ is missing or empty (the private koru-docs submodule is not checked \
             out); skipping the docs/ part of this check and scanning only AGENTS.md, \
             README.md, guide/, reference/ and licensing/"
        );
    }

    let mut stale = Vec::new();
    let mut used = BTreeSet::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_history(&relative) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for name in crate_references(line) {
                if names.contains(name) || HOSTED.contains(&name) {
                    continue;
                }
                if NOT_CRATES.iter().any(|(not, _, _)| *not == name) {
                    used.insert(name.to_owned());
                } else if FORMER
                    .iter()
                    .any(|(former, docs)| *former == name && docs.contains(&relative.as_str()))
                {
                    used.insert(format!("{name} in {relative}"));
                } else {
                    stale.push(format!("{relative}:{}: `{name}`", n + 1));
                }
            }
        }
    }
    assert!(
        stale.is_empty(),
        "these documents name a crate that is not a workspace member — point them at \
         the crate that exists now, or, if the name is history or not a crate, add it \
         with a reason to FORMER or NOT_CRATES in crates/koru-release/src/docs_crates.rs:\n{}",
        stale.join("\n")
    );
    // **The allowlists shrink with the docs.** An entry nothing needs any
    // more would quietly admit the name back. An entry whose documents are
    // not in this tree (no `docs/` checkout) is not asked for.
    let unused: Vec<String> = NOT_CRATES
        .iter()
        .filter(|(_, _, docs)| docs.iter().any(|doc| present(&root, doc)))
        .map(|(name, _, _)| (*name).to_owned())
        .chain(FORMER.iter().flat_map(|(name, docs)| {
            docs.iter()
                .filter(|doc| present(&root, doc))
                .map(move |doc| format!("{name} in {doc}"))
        }))
        .filter(|entry| !used.contains(entry))
        .collect();
    assert!(
        unused.is_empty(),
        "no document needs these allowlist entries any more — remove them: {unused:?}"
    );
}

#[test]
fn the_reference_rules_find_crates_and_ignore_mentions() {
    assert_eq!(
        crate_references("see [lib](../../crates/koru-core/src/lib.rs) and `apps/koru-agent`"),
        ["koru-core", "koru-agent"]
    );
    assert_eq!(
        crate_references("the `koru-search` crate, crate `koru-env`, crates `koru-keys`"),
        ["koru-search", "koru-env", "koru-keys"]
    );
    assert_eq!(
        crate_references("cargo test -p koru-workspace-host"),
        ["koru-workspace-host"]
    );
    // Mentions that are not crate references.
    assert!(crate_references("a `koru-system` kind; the `koru-relay-1` volume").is_empty());
    assert!(crate_references("my-repo/crates is not a path; sub-crates/koru-x either").is_empty());
    assert!(crate_references("the `koru-x` crateless thing").is_empty());
    assert!(crate_references("`crates/koru-store-*` are the replacement").is_empty());
    // History documents are skipped.
    assert!(is_history("docs/x/ROADMAP.rev3.md"));
    assert!(is_history("docs/x/ROADMAP.rev1.notes.md"));
    assert!(is_history("docs/x/PLAN.final.md"));
    assert!(is_history("docs/archive/old.md"));
    assert!(is_history("docs/v/plans/implemented/R.md"));
    assert!(!is_history("docs/decisions/ADR-450-productflags.md"));
    assert!(!is_history("docs/x/revision-notes.md"));
}
