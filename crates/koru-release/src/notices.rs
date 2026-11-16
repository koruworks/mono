// SPDX-License-Identifier: MPL-2.0

//! **`THIRD-PARTY-NOTICES.txt`: what a package owes the people whose work it
//! carries.**
//!
//! Koru is MPL-2.0 (`LICENSE`, `NOTICE`), but the binaries it ships contain
//! about a thousand crates and a dozen native libraries. Nearly all their
//! licenses ask the same thing of a *binary* distribution: carry the license
//! text and the copyright notice with it. Without this file, every package we
//! published was out of compliance, and the open-source launch would have made
//! that public.
//!
//! The file has two parts, and they come from two places:
//!
//! - **Rust crates**, found by walking `cargo metadata` from the packages this
//!   release builds through their *normal* dependencies, for the host's
//!   platform only. Proc-macro crates are left out because they run in the
//!   compiler and none of their code is in a binary. Build-only dependencies
//!   are excluded for the same reason. Each crate's own license files
//!   (`LICENSE*`, `COPYING*`, `NOTICE*`, …) are copied verbatim. A crate that
//!   ships none gets the text recorded for it in `licences/crates/`: the
//!   license file from its repository at the published revision, or, where
//!   upstream has none, the standard text of the license it declares under
//!   our recorded judgment. A crate with neither is a refusal to package.
//! - **Everything Cargo cannot see**: fonts, the vendored TypeScript grammar,
//!   and the native libraries statically linked through `sherpa-onnx-sys` and
//!   `whisper-rs-sys`. Those are kept by hand in `licences/bundled/`, one file
//!   per component, each with a small header saying what it is, where it came
//!   from, and which shipped files contain it.
//!
//! The Android app adds a third part, its Maven libraries, which only Gradle
//! can resolve: see [`android`]. The macOS package and the iOS app are in
//! [`apple`].
//!
//! **A crate whose license cannot be satisfied is a refusal to package**, in
//! the same spirit as a missing font: a shipped crate with no license file and
//! no standard text for its license, or a proprietary workspace package in the
//! dependency graph of a binary the open-source app ships.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The file's name in the package's data root, beside `LICENSE` and `NOTICE`.
pub const FILE: &str = "THIRD-PARTY-NOTICES.txt";

/// Where the hand-kept and standard texts live, relative to the repository.
const BUNDLED_DIR: &str = "crates/koru-release/licences/bundled";
const SPDX_DIR: &str = "crates/koru-release/licences/spdx";
/// One file per crate that ships no license file of its own.
const CRATES_DIR: &str = "crates/koru-release/licences/crates";

/// The license a package declares when it must never ship in the open-source
/// app: the hosted Koru Cloud services, which are not in this repository.
/// **Zero such packages in a shipped graph is a release condition**, so a
/// workspace package marked with it that a shipped binary links stops the
/// release.
const PROPRIETARY: &str = "LicenseRef-Koru-Proprietary";

/// For a crate with no license file of its own, which of the licenses it
/// offers to reproduce. Permissive first, and never a copyleft option when the
/// crate offers anything else.
const PREFERENCE: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "Zlib",
    "BSL-1.0",
    "Unlicense",
    "CC0-1.0",
    "MPL-2.0",
];

#[derive(Debug, Deserialize)]
pub(crate) struct Metadata {
    pub packages: Vec<Package>,
    pub workspace_members: Vec<String>,
    pub resolve: Resolve,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub license_file: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    pub repository: Option<String>,
    pub source: Option<String>,
    pub manifest_path: PathBuf,
    #[serde(default)]
    pub targets: Vec<Target>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Target {
    #[serde(default)]
    pub kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Resolve {
    pub nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Node {
    pub id: String,
    #[serde(default)]
    pub deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeDep {
    pub pkg: String,
    #[serde(default)]
    pub dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DepKind {
    pub kind: Option<String>,
}

/// One crate as the notices file describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrateNotice {
    pub name: String,
    pub version: String,
    pub license: String,
    pub location: String,
    /// `(file name, text)` — the crate's own license files, or the one
    /// standard text chosen for it.
    pub texts: Vec<(String, String)>,
    /// The text is a reconstruction nobody checked: no license file in the
    /// package and no record in `licences/crates/`.
    pub inferred: bool,
}

/// One hand-kept component from `licences/bundled/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bundled {
    pub header: Vec<(String, String)>,
    pub text: String,
}

impl Bundled {
    pub fn field(&self, key: &str) -> Option<&str> {
        self.header
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Whether this component is in a build for `os` (`std::env::consts::OS`).
    /// A file without a `Platforms` line is in every build; one with it is in
    /// the builds it names: the Android-only fonts, and the desktop-only
    /// native libraries.
    pub fn applies_to(&self, os: &str) -> bool {
        self.field("Platforms")
            .is_none_or(|platforms| platforms.split(", ").any(|platform| platform == os))
    }
}

/// Generates the notices for `packages` (workspace package names), as built
/// for this host, and writes them to `destination`. Every crate without a
/// license file of its own must have a record in `licences/crates/`.
pub fn write(root: &Path, packages: &[&str], destination: &Path) -> Result<(), String> {
    let build = Build {
        packages,
        triples: vec![host_triple()?],
        os: std::env::consts::OS,
        unrecorded: Unrecorded::Refuse,
        source: None,
    };
    let text = generate(root, &build, &[])?;
    fs::write(destination, text).map_err(|error| error.to_string())
}

/// What one artifact's notices are generated for.
pub(crate) struct Build<'a> {
    /// The workspace packages whose binaries the artifact carries.
    pub packages: &'a [&'a str],
    /// Every target triple the artifact holds code for. Android ships two
    /// ABIs, so its graph is the union of both.
    pub triples: Vec<String>,
    /// The `std::env::consts::OS` spelling a bundled component's `Platforms`
    /// line is matched against.
    pub os: &'a str,
    pub unrecorded: Unrecorded,
    /// `https://github.com/koruworks/mono/tree/<commit>`, printed at the top
    /// of the file when the build knows it (MPL-2.0 §3.2).
    pub source: Option<&'a str>,
}

/// **A crate that ships no license file and has no record in
/// `licences/crates/`.** A release refuses it: its notice would be a
/// reconstruction nobody checked against upstream. An Android *debug* build
/// labels it `INFERRED` instead, so a dependency bump does not stop a
/// developer's app from building; the release build of the same tree refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unrecorded {
    Refuse,
    Label,
}

/// The whole notices file for `build`, with `libraries` (the Android app's
/// Maven dependencies) as a third part when there are any.
pub(crate) fn generate(
    root: &Path,
    build: &Build,
    libraries: &[android::Library],
) -> Result<String, String> {
    // The same features the package build turns on, so a crate only
    // recognition brings is attributed exactly when it is shipped.
    let features = crate::read_runtime::features(build.packages, &build.triples);
    let metadata = metadata(root, &build.triples, features)?;
    let shipped = shipped(&metadata, build.packages)?;
    let records = read_records(&root.join(CRATES_DIR))?;
    let mut crates = Vec::with_capacity(shipped.len());
    let mut unlicensed = Vec::new();
    let mut unrecorded = Vec::new();
    for package in shipped {
        match crate_notice(root, package, &records)? {
            Some(notice) => {
                if notice.inferred {
                    unrecorded.push(format!("{} {}", package.name, package.version));
                }
                crates.push(notice);
            }
            None => unlicensed.push(format!(
                "{} {} ({})",
                package.name,
                package.version,
                package.license.as_deref().unwrap_or("no license declared")
            )),
        }
    }
    if !unlicensed.is_empty() {
        return Err(format!(
            "these shipped crates have no license file and no standard text in {SPDX_DIR}: {}",
            unlicensed.join(", ")
        ));
    }
    if build.unrecorded == Unrecorded::Refuse && !unrecorded.is_empty() {
        return Err(format!(
            "these shipped crates carry no license file and have no record in {CRATES_DIR}: {}. \
             Record each one's license from its repository at the published revision \
             (licensing/README.md, \"A crate without a license file\")",
            unrecorded.join(", ")
        ));
    }
    let mut bundled = read_bundled(&root.join(BUNDLED_DIR))?;
    bundled.retain(|component| component.applies_to(build.os));
    let libraries = android::notices(root, libraries)?;
    Ok(render(&crates, &bundled, &libraries, build.source))
}

fn metadata(root: &Path, triples: &[String], features: Option<&str>) -> Result<Metadata, String> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--locked"]);
    for triple in triples {
        command.args(["--filter-platform", triple]);
    }
    if let Some(features) = features {
        command.args(["--features", features]);
    }
    let output = command
        .output()
        .map_err(|error| format!("cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("cargo metadata: {error}"))
}

/// The triple this release is built for: koru-release always builds for its
/// own host.
fn host_triple() -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|error| format!("rustc -vV: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc -vV printed no host triple".to_owned())
}

/// Every third-party package whose code can end up in one of `roots`'
/// binaries, sorted by name and version.
pub(crate) fn shipped<'a>(
    metadata: &'a Metadata,
    roots: &[&str],
) -> Result<Vec<&'a Package>, String> {
    let packages: HashMap<&str, &Package> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let nodes: HashMap<&str, &Node> = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let members: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();

    let mut stack = Vec::new();
    for root in roots {
        let id = metadata
            .workspace_members
            .iter()
            .find(|id| packages.get(id.as_str()).is_some_and(|p| p.name == *root))
            .ok_or_else(|| format!("{root} is not a workspace package"))?;
        stack.push(id.as_str());
    }

    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in &node.deps {
            // `kind: null` is a normal dependency; "dev" and "build" never
            // put code in the binary.
            if dep.dep_kinds.iter().any(|kind| kind.kind.is_none()) {
                stack.push(dep.pkg.as_str());
            }
        }
    }

    let mut out = Vec::new();
    for id in seen {
        let Some(package) = packages.get(id) else {
            continue;
        };
        if members.contains(id) {
            if package.license.as_deref() == Some(PROPRIETARY) {
                return Err(format!(
                    "{} is proprietary ({PROPRIETARY}) and must not be linked by a binary the open-source app ships",
                    package.name
                ));
            }
            continue;
        }
        let proc_macro = package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(|kind| kind == "proc-macro"));
        if !proc_macro {
            out.push(*package);
        }
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(out)
}

/// The notice for one crate, or `None` when neither the crate, its record nor
/// `licences/spdx/` can supply a text.
fn crate_notice(
    root: &Path,
    package: &Package,
    records: &BTreeMap<String, Bundled>,
) -> Result<Option<CrateNotice>, String> {
    let dir = package
        .manifest_path
        .parent()
        .ok_or_else(|| format!("{} has no manifest directory", package.name))?;
    let mut texts = license_files(dir)?;
    if let Some(file) = &package.license_file {
        let path = dir.join(file);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.clone());
        if !texts.iter().any(|(existing, _)| *existing == name) {
            let text = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            texts.push((name, String::from_utf8_lossy(&text).into_owned()));
        }
    }
    let license = package.license.clone().unwrap_or_default();
    if texts.is_empty()
        && let Some(record) = records.get(&package.name)
    {
        texts.push(recorded_text(package, &license, record)?);
        return Ok(Some(CrateNotice {
            name: package.name.clone(),
            version: package.version.clone(),
            license,
            location: location(package),
            texts,
            inferred: false,
        }));
    }
    let inferred = texts.is_empty();
    if texts.is_empty() {
        let Some(id) = choose(&license) else {
            return Ok(None);
        };
        let path = root.join(SPDX_DIR).join(format!("{id}.txt"));
        let Ok(standard) = fs::read_to_string(&path) else {
            return Ok(None);
        };
        texts.push((
            // Said in the file itself: this notice is reconstructed, not
            // copied, because the crate ships no license file of its own.
            format!("{id} — INFERRED: standard text, copyright line from Cargo.toml authors"),
            fill_copyright(&standard, &package.name, &package.authors),
        ));
    }
    Ok(Some(CrateNotice {
        name: package.name.clone(),
        version: package.version.clone(),
        license,
        location: location(package),
        texts,
        inferred,
    }))
}

/// **The text recorded for a crate that ships no license file**, and the
/// label that says where it came from. A record is checked against the crate
/// it describes: it must name a license the crate declares, so a crate that
/// changes license is not silently attributed under the old one.
///
/// - `Disposition: upstream`: the file is the crate repository's own, fetched
///   at the revision the package was published from (`Source`).
/// - `Disposition: judgment`: upstream has no license file. The text is the
///   standard one with the Cargo authors as the holders, which is our recorded
///   judgment of the best evidence, and an issue asking upstream for a license
///   file is drafted in licensing/UPSTREAM-ISSUES.md.
pub(crate) fn recorded_text(
    package: &Package,
    license: &str,
    record: &Bundled,
) -> Result<(String, String), String> {
    let field = |key: &str| {
        record
            .field(key)
            .ok_or_else(|| format!("{CRATES_DIR}/{}.txt has no `{key}`", package.name))
    };
    let id = field("License")?;
    let declared = license
        .split(|c: char| c == '(' || c == ')' || c == '/' || c.is_whitespace())
        .any(|token| token.trim_end_matches('+') == id);
    if !declared {
        return Err(format!(
            "{CRATES_DIR}/{}.txt records {id}, but {} {} declares `{license}`",
            package.name, package.name, package.version
        ));
    }
    let checked = field("Checked")?;
    let label = match field("Disposition")? {
        "upstream" => format!(
            "{} — from upstream at {} (checked at {checked})",
            field("File")?,
            field("Source")?
        ),
        "judgment" => format!(
            "{id} — standard text; upstream has no license file (checked at {checked}), \
             our recorded judgment: the declared license and Cargo.toml authors"
        ),
        other => {
            return Err(format!(
                "{CRATES_DIR}/{}.txt: unknown disposition `{other}`",
                package.name
            ));
        }
    };
    Ok((label, record.text.clone()))
}

/// Reads every `<crate>.txt` in `licences/crates/`, keyed by crate name.
fn read_records(dir: &Path) -> Result<BTreeMap<String, Bundled>, String> {
    let mut out = BTreeMap::new();
    for path in txt_files(dir)? {
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let record = parse_record(&text, RECORD_KEYS)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let name = record.field("Crate").unwrap_or_default().to_owned();
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned());
        if stem.as_deref() != Some(name.as_str()) {
            return Err(format!(
                "{}: the file is named for another crate than `{name}`",
                path.display()
            ));
        }
        out.insert(name, record);
    }
    Ok(out)
}

/// The header keys every `licences/crates/` record carries.
pub(crate) const RECORD_KEYS: &[&str] = &["Crate", "Checked", "License", "Source", "Disposition"];

/// A crate's own license files: the files at its root whose names say they
/// are one. Sorted, so the output does not depend on directory order.
fn license_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_license_file(&name) || !entry.path().is_file() {
            continue;
        }
        let bytes = fs::read(entry.path()).map_err(|error| format!("{name}: {error}"))?;
        out.push((name, String::from_utf8_lossy(&bytes).into_owned()));
    }
    out.sort();
    Ok(out)
}

pub(crate) fn is_license_file(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "UNLICENSE",
        "COPYRIGHT",
        "NOTICE",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// Which of the licenses in an SPDX expression to reproduce for a crate that
/// ships no text of its own. Handles `OR` and the old `/` spelling; `AND` and
/// `WITH` are refused (see below). An expression it cannot satisfy returns `None`.
pub(crate) fn choose(expression: &str) -> Option<&'static str> {
    // **Only a plain choice is ours to make.** `A OR B` lets us pick one
    // license; `A AND B` requires both texts, and `A WITH exception` requires
    // the exception's text too. Collapsing either into one license would print
    // an incomplete notice, so an expression with `AND` or `WITH` gets no
    // standard text, and the release refuses the crate until it is handled.
    // No crate that needs this fallback uses either today.
    //
    // **One tokenizer for both decisions.** Parentheses delimit operators in
    // SPDX (`(MIT)AND(BSD-3-Clause)` is valid), so the refusal must see the
    // same tokens the selection does. Splitting on whitespace alone let an
    // operator next to a parenthesis slip past the refusal. Operators are
    // matched case-insensitively, which is stricter than SPDX requires.
    let tokens: Vec<&str> = expression
        .split(|c: char| c == '(' || c == ')' || c == '/' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens
        .iter()
        .any(|token| token.eq_ignore_ascii_case("AND") || token.eq_ignore_ascii_case("WITH"))
    {
        return None;
    }
    let ids: Vec<&str> = tokens
        .into_iter()
        .filter(|token| !token.eq_ignore_ascii_case("OR"))
        .map(|token| token.trim_end_matches('+'))
        .collect();
    PREFERENCE.iter().copied().find(|id| ids.contains(id))
}

/// Puts a copyright line into a standard text's placeholder. MIT's is the
/// only one in `licences/spdx/` that has one; the others are returned as is.
pub(crate) fn fill_copyright(standard: &str, name: &str, authors: &[String]) -> String {
    let holders = if authors.is_empty() {
        format!("the {name} authors")
    } else {
        authors
            .iter()
            .map(|author| strip_email(author))
            .collect::<Vec<_>>()
            .join(", ")
    };
    standard.replace("<year> <copyright holders>", &holders)
}

fn strip_email(author: &str) -> String {
    match author.find('<') {
        Some(at) => author[..at].trim().to_owned(),
        None => author.trim().to_owned(),
    }
}

fn location(package: &Package) -> String {
    match &package.source {
        Some(source) if source.starts_with("registry+") => format!(
            "https://crates.io/crates/{}/{}",
            package.name, package.version
        ),
        _ => package
            .repository
            .clone()
            .unwrap_or_else(|| "vendored in the Koru repository".to_owned()),
    }
}

/// Reads every `*.txt` in `licences/bundled/`: a header of `Key: value`
/// lines, a `---` line, then the license text.
fn read_bundled(dir: &Path) -> Result<Vec<Bundled>, String> {
    txt_files(dir)?
        .iter()
        .map(|path| {
            let text =
                fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
            parse_bundled(&text).map_err(|error| format!("{}: {error}", path.display()))
        })
        .collect()
}

/// Every `*.txt` directly in `dir`, sorted.
fn txt_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    paths.sort();
    Ok(paths)
}

pub(crate) fn parse_bundled(text: &str) -> Result<Bundled, String> {
    parse_record(text, &["Component", "Source", "Files", "License"])
}

/// A `Key: value` header, a `---` line, then the text: the shape of every
/// file in `licences/bundled/` and `licences/crates/`.
pub(crate) fn parse_record(text: &str, required: &[&str]) -> Result<Bundled, String> {
    let (head, body) = text
        .split_once("\n---\n")
        .ok_or("no `---` line between the header and the text")?;
    let header = head
        .lines()
        .map(|line| {
            line.split_once(": ")
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| format!("header line `{line}` is not `Key: value`"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bundled = Bundled {
        header,
        text: body.to_owned(),
    };
    for key in required {
        if bundled.field(key).is_none() {
            return Err(format!("the header has no `{key}`"));
        }
    }
    Ok(bundled)
}

/// The whole file. Identical license texts are printed once, under the list of
/// every crate that uses them, so a thousand copies of Apache-2.0 cost one.
pub(crate) fn render(
    crates: &[CrateNotice],
    bundled: &[Bundled],
    libraries: &[CrateNotice],
    source: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str(
        "Koru — third-party notices\n\
         ==========================\n\n\
         Koru is free software under the Mozilla Public License 2.0 (see LICENSE and\n\
         NOTICE beside this file). It is built with the work of many other people.\n\
         This file reproduces the license and copyright notices their licenses ask\n\
         a binary distribution to carry. It is generated by koru-release for this\n\
         build; it is not hand-edited.\n\n",
    );
    if let Some(source) = source {
        out.push_str(&format!("The source code of this build: {source}\n\n"));
    }

    out.push_str("Part 1 — Fonts, native libraries and other bundled components\n");
    out.push_str("--------------------------------------------------------------\n\n");
    for component in bundled {
        for (key, value) in &component.header {
            out.push_str(&format!("{key}: {value}\n"));
        }
        out.push('\n');
        out.push_str(component.text.trim_end());
        out.push_str("\n\n");
        out.push_str(&"=".repeat(78));
        out.push_str("\n\n");
    }

    out.push_str("Part 2 — Rust crates\n");
    out.push_str("--------------------\n\n");
    render_notices(&mut out, crates);
    if !libraries.is_empty() {
        out.push_str("Part 3 — Android libraries\n");
        out.push_str("--------------------------\n\n");
        render_notices(&mut out, libraries);
    }
    out
}

/// A list of components, then every distinct text under the components that
/// use it.
fn render_notices(out: &mut String, crates: &[CrateNotice]) {
    for notice in crates {
        out.push_str(&format!(
            "{} {} — {} — {}\n",
            notice.name,
            notice.version,
            if notice.license.is_empty() {
                "see text"
            } else {
                &notice.license
            },
            notice.location
        ));
    }
    out.push('\n');

    // Group by normalized text, keeping first-seen order for stable output.
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for notice in crates {
        for (file, text) in &notice.texts {
            let key = normalize(text);
            let user = format!("{} {} ({file})", notice.name, notice.version);
            match groups.get_mut(&key) {
                Some((_, users)) => users.push(user),
                None => {
                    order.push(key.clone());
                    groups.insert(key, (text.clone(), vec![user]));
                }
            }
        }
    }
    for key in order {
        let (text, users) = &groups[&key];
        out.push_str(&"=".repeat(78));
        out.push('\n');
        out.push_str("Used by: ");
        out.push_str(&users.join(", "));
        out.push_str("\n\n");
        out.push_str(text.replace("\r\n", "\n").trim_end());
        out.push_str("\n\n");
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `koru-release notices <android|ios|macos> …`: the notices for a build that
/// `package::build` does not package, or for a host it does not run on.
pub(crate) fn run(root: &Path, args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut args = args.peekable();
    match args.peek().map(String::as_str) {
        Some("android") => android::run(root, args),
        Some("ios") => apple::run_ios(root, args.skip(1)),
        Some("macos") => apple::run_macos(root, args.skip(1)),
        _ => Err("usage: koru-release notices <android|ios|macos> …".into()),
    }
}

pub(crate) mod android;
pub(crate) mod apple;

#[cfg(test)]
mod tests;
