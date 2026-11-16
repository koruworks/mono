// SPDX-License-Identifier: MPL-2.0

//! **The native platforms, held to their source**: every Kotlin and Swift
//! file a native record is written from, compiled in as text, and read in
//! its own syntax. The Rust checks beside this file read `pub fn` and
//! `struct`; these read `fun Name(`, `class`, `object`, an `@Composable`,
//! `struct Name: View` and the members of a Kotlin or Swift `enum`.
//!
//! **What a new native file does here.** A file that is renamed or deleted
//! stops this compiling (`include_str!`). A file that is *added* is not seen
//! directly: Gradle lists no sources and Xcode's synchronized folders list
//! no files, and the suite reads nothing at run time. So the checks hold the
//! doors a new native surface has to come through — Android's manifest
//! components, its navigation routes and its Gradle modules; Xcode's
//! targets, the iOS root's routes and what the macOS packager compiles —
//! and every composable and view the compiled-in files declare. A new
//! screen behind an existing route, in a new file, is the manifest's hand
//! review.

use super::*;

/// Every native file a record is written from, from the repository root.
pub(super) const SOURCES: &[(&str, &str)] = &[
    (
        "apps/android/app/src/main/java/app/koru/mobile/BrowserSignInReturnActivity.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/BrowserSignInReturnActivity.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/MainActivity.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/MainActivity.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ShareActivity.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ShareActivity.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/services/AndroidGoogleAccountDialog.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/services/AndroidGoogleAccountDialog.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/services/AndroidQrScanner.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/services/AndroidQrScanner.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/services/AppUpdates.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/services/AppUpdates.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/services/BrowserSignIn.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/services/BrowserSignIn.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/AccountScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/AccountScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/BlobTransferPanel.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/BlobTransferPanel.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/CloudInventoryScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/CloudInventoryScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/CloudProgressScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/CloudProgressScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/CloudStorageSection.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/CloudStorageSection.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/EntityTileColors.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/EntityTileColors.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/KoruApp.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/KoruApp.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/OpenSourceLicensesScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/OpenSourceLicensesScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/QrScanButton.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/QrScanButton.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/SettingsScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/SettingsScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/editor/CloudCopyBody.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/editor/CloudCopyBody.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/editor/CloudCopyScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/editor/CloudCopyScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/editor/DocumentEditor.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/editor/DocumentEditor.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/editor/NativeRichEditor.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/editor/NativeRichEditor.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/run/RunPresentation.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/run/RunPresentation.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/run/RunScreen.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/run/RunScreen.kt"
        ),
    ),
    (
        "apps/android/app/src/main/java/app/koru/mobile/ui/theme/Theme.kt",
        include_str!(
            "../../../../../../../apps/android/app/src/main/java/app/koru/mobile/ui/theme/Theme.kt"
        ),
    ),
    (
        "apps/ios/KoruMobile/DevicesScreen.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/DevicesScreen.swift"),
    ),
    (
        "apps/ios/KoruMobile/DocumentRendering.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/DocumentRendering.swift"),
    ),
    (
        "apps/ios/KoruMobile/KoruMobileApp.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/KoruMobileApp.swift"),
    ),
    (
        "apps/ios/KoruMobile/NativeRichTextEditor.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/NativeRichTextEditor.swift"),
    ),
    (
        "apps/ios/KoruMobile/OpenSourceLicensesScreen.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/OpenSourceLicensesScreen.swift"),
    ),
    (
        "apps/ios/KoruMobile/SettingsScreen.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/SettingsScreen.swift"),
    ),
    (
        "apps/ios/KoruMobile/ShareDestinationScreen.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/ShareDestinationScreen.swift"),
    ),
    (
        "apps/ios/KoruMobile/Theme.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/Theme.swift"),
    ),
    (
        "apps/ios/KoruMobile/Views.swift",
        include_str!("../../../../../../../apps/ios/KoruMobile/Views.swift"),
    ),
    (
        "apps/ios/KoruShare/ShareViewController.swift",
        include_str!("../../../../../../../apps/ios/KoruShare/ShareViewController.swift"),
    ),
    (
        "apps/macos/KoruShare/ShareViewController.swift",
        include_str!("../../../../../../../apps/macos/KoruShare/ShareViewController.swift"),
    ),
];

/// The Android app's manifest: every component a launcher, a share sheet, a
/// link, a widget host or the notification system could open.
const ANDROID_MANIFEST: &str =
    include_str!("../../../../../../../apps/android/app/src/main/AndroidManifest.xml");

/// The Android build's module list.
const ANDROID_SETTINGS: &str =
    include_str!("../../../../../../../apps/android/settings.gradle.kts");

/// The iOS project: its native targets, each a product a person can open
/// (the app, an extension) or a test bundle.
const IOS_PROJECT: &str =
    include_str!("../../../../../../../apps/ios/KoruMobile.xcodeproj/project.pbxproj");

/// The release packager, which compiles the macOS share extension's Swift
/// with `swiftc`: the only list of the Mac's native sources.
const MAC_PACKAGER: &str = include_str!("../../../../../../../crates/koru-release/src/package.rs");

/// The language a native owner is written in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Language {
    Kotlin,
    Swift,
}

/// The native owners: the name a record writes before `::`, its language,
/// and the folder its symbols resolve in.
const OWNERS: &[(&str, Language, &str)] = &[
    (
        "app.koru.mobile",
        Language::Kotlin,
        "apps/android/app/src/main/java/app/koru/mobile/",
    ),
    ("ios/KoruMobile", Language::Swift, "apps/ios/KoruMobile/"),
    ("ios/KoruShare", Language::Swift, "apps/ios/KoruShare/"),
    ("macos/KoruShare", Language::Swift, "apps/macos/KoruShare/"),
];

/// Whether a record's owner is a native platform's.
pub(super) fn owns(owner: &str) -> bool {
    OWNERS.iter().any(|(name, _, _)| *name == owner)
}

/// Whether a file is a compiled-in native source.
pub(super) fn is_native_file(file: &str) -> bool {
    SOURCES.iter().any(|(path, _)| *path == file)
}

fn native_source(file: &str) -> &'static str {
    SOURCES
        .iter()
        .find(|(path, _)| *path == file)
        .map(|(_, text)| *text)
        .unwrap_or_else(|| panic!("{file} is not compiled into the native checks"))
}

/// The owner a native file belongs to, and its path below the owner's
/// folder.
fn owner_of(file: &'static str) -> (&'static str, Language, &'static str) {
    OWNERS
        .iter()
        .filter_map(|(owner, language, folder)| {
            file.strip_prefix(*folder)
                .map(|rest| (*owner, *language, rest))
        })
        .next()
        .unwrap_or_else(|| panic!("{file} belongs to no native owner"))
}

/// The code of a Kotlin or Swift file: comment lines dropped, so a name a
/// comment mentions is never mistaken for one the code declares.
fn code_lines(text: &'static str) -> impl Iterator<Item = &'static str> {
    text.lines().filter(|line| {
        let line = line.trim_start();
        !(line.starts_with("//") || line.starts_with("/*") || line.starts_with('*'))
    })
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The identifier at the start of `text`.
fn ident(text: &str) -> &str {
    let end = text.find(|c: char| !is_ident(c)).unwrap_or(text.len());
    &text[..end]
}

/// `text` with a leading balanced `<…>` (a generic list) and whitespace
/// skipped.
fn skip_generics(text: &str) -> &str {
    let text = text.trim_start();
    if !text.starts_with('<') {
        return text;
    }
    let mut depth = 0;
    for (at, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return text[at + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    ""
}

/// Every name `line` declares after one of `keywords`, each keyword a whole
/// word: `private fun <T> EntityListScreen(` declares `EntityListScreen`, a
/// Kotlin extension `fun Receiver.name(` declares `name`.
fn declared_names<'a>(line: &'a str, keywords: &[&str]) -> Vec<&'a str> {
    let mut names = Vec::new();
    for keyword in keywords {
        for (at, _) in line.match_indices(keyword) {
            if line[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let rest = skip_generics(&line[at + keyword.len()..]);
            let name = ident(rest);
            let after = &rest[name.len()..];
            let name = match after.strip_prefix('.') {
                Some(member) if *keyword == "fun " => ident(member),
                _ => name,
            };
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// Whether `text`, read as `language`, declares `item`.
fn declares(text: &'static str, language: Language, item: &str) -> bool {
    code_lines(text).any(|line| match language {
        Language::Kotlin => declared_names(line, &["fun ", "class ", "object ", "interface "])
            .contains(&item),
        Language::Swift => {
            declared_names(line, &["struct ", "class ", "enum ", "func ", "protocol ", "actor "])
                .contains(&item)
                // A top-level constant, at the start of its line.
                || ["let ", "var "]
                    .iter()
                    .any(|keyword| line.strip_prefix(keyword).is_some_and(|rest| ident(rest) == item))
        }
    })
}

/// The files a native symbol may be declared in, and the declaration's name:
/// a Kotlin symbol's package is the folder below the owner's, a Swift one's
/// target is the owner's folder and everything in it.
fn files_of(symbol: &str) -> Option<(Language, Vec<&'static str>, &str)> {
    let (owner, path) = symbol.split_once("::")?;
    let &(_, language, folder) = OWNERS.iter().find(|(name, _, _)| *name == owner)?;
    match language {
        Language::Kotlin => {
            let (package, item) = path.rsplit_once('.').unwrap_or(("", path));
            let dir = if package.is_empty() {
                folder.to_owned()
            } else {
                format!("{folder}{}/", package.replace('.', "/"))
            };
            let declaration = if package.is_empty() {
                format!("package {owner}")
            } else {
                format!("package {owner}.{package}")
            };
            let files = SOURCES
                .iter()
                .filter(|(file, text)| {
                    file.strip_prefix(dir.as_str())
                        .is_some_and(|rest| !rest.contains('/'))
                        && text.lines().any(|line| line.trim_end() == declaration)
                })
                .map(|(_, text)| *text)
                .collect();
            Some((language, files, item))
        }
        Language::Swift => {
            let files = SOURCES
                .iter()
                .filter(|(file, _)| file.starts_with(folder))
                .map(|(_, text)| *text)
                .collect();
            Some((language, files, path))
        }
    }
}

/// Whether a native symbol is declared where its path says.
pub(super) fn declared(symbol: &str) -> bool {
    files_of(symbol).is_some_and(|(language, files, item)| {
        !item.contains('.') && files.iter().any(|text| declares(text, language, item))
    })
}

/// The body of the first `{ … }` block in `text`: what lies between the
/// braces, with the depth of each character (1 is the block's own level;
/// brackets, parentheses and nested braces go deeper).
fn braced(text: &str) -> Vec<(char, usize)> {
    let Some(open) = text.find('{') else {
        return Vec::new();
    };
    let mut depth = 1;
    let mut body = Vec::new();
    for c in text[open + 1..].chars() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        body.push((c, depth));
    }
    body
}

/// The members of `enum class <name>` (Kotlin) or `enum <name>` (Swift) in a
/// compiled-in native file, in declaration order. A Kotlin enum's
/// constructor parameters sit in parentheses before its body and hold no
/// brace, so the first brace after the name opens the entries.
pub(super) fn enum_members(file: &str, name: &str) -> Vec<String> {
    let code: String = code_lines(native_source(file))
        .collect::<Vec<_>>()
        .join("\n");
    let language = if file.ends_with(".kt") {
        Language::Kotlin
    } else {
        Language::Swift
    };
    let keyword = match language {
        Language::Kotlin => "enum class ",
        Language::Swift => "enum ",
    };
    let opening = code
        .match_indices(keyword)
        .map(|(at, _)| at + keyword.len())
        .find(|at| ident(&code[*at..]) == name)
        .unwrap_or_else(|| panic!("{file} no longer declares the enum {name}"));
    // The entries' own level: arguments, bodies and nested types are deeper.
    let body: String = braced(&code[opening + name.len()..])
        .into_iter()
        .filter(|(_, depth)| *depth == 1)
        .map(|(c, _)| c)
        .collect();
    match language {
        // Entries up to the first `;`, separated by commas.
        Language::Kotlin => body
            .split(';')
            .next()
            .unwrap_or("")
            .split(',')
            .map(|entry| ident(entry.trim()).to_owned())
            .filter(|entry| !entry.is_empty())
            .collect(),
        // Every `case a, b = …` statement.
        Language::Swift => body
            .split(['\n', ';'])
            .filter_map(|statement| statement.trim().strip_prefix("case "))
            .flat_map(|cases| cases.split(','))
            .map(|case| ident(case.trim()).to_owned())
            .filter(|case| !case.is_empty())
            .collect(),
    }
}

/// The composables (Kotlin) or views (Swift) a native file declares: what a
/// person can see drawn by that file's own code.
fn ui_declarations(file: &'static str) -> Vec<&'static str> {
    let text = native_source(file);
    let lines: Vec<&'static str> = code_lines(text).collect();
    let mut names = Vec::new();
    match owner_of(file).1 {
        Language::Kotlin => {
            for (at, line) in lines.iter().enumerate() {
                let Some(rest) = line.trim_start().strip_prefix("@Composable") else {
                    continue;
                };
                // The function the annotation sits on: on its own line, or
                // a line or two below, past other annotations.
                let name = std::iter::once(rest)
                    .chain(lines[at + 1..].iter().take(3).copied())
                    .find_map(|candidate| declared_names(candidate, &["fun "]).first().copied())
                    .unwrap_or_else(|| panic!("{file}: an @Composable with no function: {line}"));
                names.push(name);
            }
        }
        Language::Swift => {
            const UI: [&str; 10] = [
                "View",
                "App",
                "ViewModifier",
                "UIViewRepresentable",
                "UIViewControllerRepresentable",
                "NSViewRepresentable",
                "UIViewController",
                "NSViewController",
                "UIView",
                "NSView",
            ];
            for line in lines {
                for keyword in ["struct ", "class "] {
                    for (at, _) in line.match_indices(keyword) {
                        if line[..at].chars().next_back().is_some_and(is_ident) {
                            continue;
                        }
                        let rest = &line[at + keyword.len()..];
                        let name = ident(rest);
                        let after = skip_generics(&rest[name.len()..]);
                        let Some(conformances) = after.strip_prefix(':') else {
                            continue;
                        };
                        let conformances = conformances.split('{').next().unwrap_or("");
                        if conformances
                            .split(|c: char| c == ',' || c == '&' || c.is_whitespace())
                            .any(|conformance| UI.contains(&conformance))
                        {
                            names.push(name);
                        }
                    }
                }
            }
        }
    }
    names
}

/// A native file's symbol for a name it declares, as a record writes it.
fn symbol_of(file: &'static str, name: &str) -> String {
    let (owner, language, rest) = owner_of(file);
    match language {
        Language::Kotlin => match rest.rsplit_once('/') {
            Some((package, _)) => format!("{owner}::{}.{name}", package.replace('/', ".")),
            None => format!("{owner}::{name}"),
        },
        Language::Swift => format!("{owner}::{name}"),
    }
}

fn native_records() -> impl Iterator<Item = &'static Entry> {
    catalog().filter(|e| owns(e.source.owner))
}

fn native_symbols() -> Vec<&'static str> {
    native_records()
        .flat_map(symbols)
        .map(|(symbol, _, _)| symbol)
        .collect()
}

/// Every composable and view a compiled-in native file declares is named by
/// a record, and every compiled-in file holds at least one recorded symbol.
#[test]
fn every_native_composable_and_view_has_a_record() {
    let recorded = native_symbols();
    for (file, _) in SOURCES {
        for name in ui_declarations(file) {
            let symbol = symbol_of(file, name);
            assert!(
                recorded.contains(&symbol.as_str()),
                "{file} declares {name}, which no native record names ({symbol})"
            );
        }
        let (owner, language, _) = owner_of(file);
        assert!(
            recorded.iter().any(|symbol| {
                symbol.starts_with(&format!("{owner}::"))
                    && files_of(symbol).is_some_and(|(_, files, item)| {
                        files.contains(&native_source(file))
                            && declares(native_source(file), language, item)
                    })
            }),
            "{file} is compiled in but no record names anything it declares"
        );
    }
}

/// A native record is a native adapter with a native file, claims no
/// inspector identity and no A2UI support, and each symbol is drawn only on
/// its platform, drawn by the system, or draws nothing. Every native adapter
/// is a native platform's.
#[test]
fn every_native_record_is_a_platform_adapter() {
    for entry in native_records() {
        assert_eq!(entry.kind, Kind::NativeAdapter, "{}", entry.catalog_id);
        assert!(entry.identities.is_empty(), "{}", entry.catalog_id);
        assert_eq!(
            entry.source.module, entry.source.owner,
            "{}",
            entry.catalog_id
        );
        for (symbol, a2ui, story) in symbols(entry) {
            assert_eq!(a2ui, A2ui::NotSupported, "{symbol}");
            assert!(
                matches!(
                    story,
                    Story::PlatformOnly | Story::Boundary | Story::Nonvisual
                ),
                "{symbol} is {story:?}"
            );
        }
        // Drawn on its own platform, and nowhere else.
        let shipped: Vec<Platform> = entry
            .platforms
            .iter()
            .filter(|(_, availability)| {
                matches!(
                    availability,
                    Availability::Existing | Availability::NativeOwned
                )
            })
            .map(|(platform, _)| *platform)
            .collect();
        assert_eq!(shipped.len(), 1, "{}", entry.catalog_id);
    }
    for entry in catalog().filter(|e| e.kind == Kind::NativeAdapter) {
        assert!(owns(entry.source.owner), "{}", entry.catalog_id);
    }
}

/// The opening tags of a manifest's components, with their `android:name`,
/// in document order; XML comments are not components.
fn android_components() -> Vec<String> {
    const TAGS: [&str; 6] = [
        "application",
        "activity",
        "activity-alias",
        "service",
        "receiver",
        "provider",
    ];
    let mut text = ANDROID_MANIFEST;
    let mut code = String::new();
    while let Some(start) = text.find("<!--") {
        code.push_str(&text[..start]);
        let end = text[start..].find("-->").expect("an XML comment ends") + start + 3;
        text = &text[end..];
    }
    code.push_str(text);
    let mut found = Vec::new();
    let mut rest = code.as_str();
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        let tag = &rest[..rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len())];
        if TAGS.contains(&tag) {
            let open = &rest[..rest.find('>').expect("a tag closes")];
            let name = open
                .split("android:name=\"")
                .nth(1)
                .and_then(|value| value.split('"').next())
                .unwrap_or_else(|| panic!("a <{tag}> names nothing"));
            found.push(format!("{tag} {name}"));
        }
    }
    found
}

/// The Android manifest's components, by tag and `android:name`, each a
/// record or excluded.
const ANDROID_COMPONENTS: &[(&str, Module)] = &[
    (
        "application .KoruApplication",
        Module::Excluded(
            "the process's Application: builds the service container once; draws nothing",
        ),
    ),
    (
        "activity .MainActivity",
        Module::Records(&["C-ANDROID-APP"]),
    ),
    (
        "activity .BrowserSignInReturnActivity",
        Module::Records(&["C-ANDROID-SYSTEM-SURFACES"]),
    ),
    (
        "activity .ShareActivity",
        Module::Records(&["C-ANDROID-SHARE"]),
    ),
    (
        "provider androidx.core.content.FileProvider",
        Module::Excluded(
            "hands one exported file to another app under a one-shot read grant; the chooser it feeds is C-ANDROID-SYSTEM-SURFACES",
        ),
    ),
];

/// The Android app's navigation routes, by the constant `Routes` declares,
/// and the records each opens.
const ANDROID_ROUTES: &[(&str, Module)] = &[
    ("Devices", Module::Records(&["C-ANDROID-LINK-DEVICE"])),
    ("Profiles", Module::Records(&["C-ANDROID-PROFILES"])),
    ("Workspaces", Module::Records(&["C-ANDROID-WORKSPACES"])),
    ("Documents", Module::Records(&["C-ANDROID-DOCUMENTS"])),
    ("Document", Module::Records(&["C-ANDROID-DOCUMENT-VIEWER"])),
    ("Run", Module::Records(&["C-ANDROID-RUN"])),
    ("Diagnostics", Module::Records(&["C-ANDROID-SETTINGS"])),
    ("Settings", Module::Records(&["C-ANDROID-SETTINGS"])),
    ("Account", Module::Records(&["C-ANDROID-ACCOUNT"])),
    (
        "CloudFiles",
        Module::Records(&["C-ANDROID-CLOUD-INVENTORY", "C-ANDROID-CLOUD-COPY"]),
    ),
    (
        "CloudProgress",
        Module::Records(&["C-ANDROID-CLOUD-PROGRESS"]),
    ),
    ("Licenses", Module::Records(&["C-ANDROID-SETTINGS"])),
    ("License", Module::Records(&["C-ANDROID-SETTINGS"])),
];

/// The Android build's Gradle modules.
const ANDROID_MODULES: &[(&str, Module)] = &[(":app", Module::Records(&["C-ANDROID-APP"]))];

/// A list of doors against its baseline: name for name and in order, each
/// door's records real or its exclusion explained.
fn check_entry_points(what: &str, declared: Vec<String>, baseline: &[(&str, Module)]) {
    let listed: Vec<&str> = baseline.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        declared, listed,
        "{what} changed: classify the new one or drop the removed one"
    );
    for (name, module) in baseline {
        match module {
            Module::Records(records) => {
                assert!(!records.is_empty(), "{what}: {name} names no record");
                for id in *records {
                    entry(id);
                }
            }
            Module::Excluded(reason) => assert!(!reason.is_empty(), "{what}: {name}"),
        }
    }
}

/// Every door into the Android UI — a manifest component, a navigation
/// route, a Gradle module — is classified, so a new activity, share target,
/// widget receiver, notification service, screen route or module fails until
/// it has a record or a reason.
#[test]
fn the_android_entry_points_are_classified() {
    check_entry_points(
        "the Android manifest's components",
        android_components(),
        ANDROID_COMPONENTS,
    );

    let app = native_source("apps/android/app/src/main/java/app/koru/mobile/ui/KoruApp.kt");
    let start = app
        .find("private object Routes {")
        .expect("KoruApp.kt declares its routes");
    let routes = &app[start..start + app[start..].find("\n}").expect("the routes close")];
    let declared: Vec<String> = routes
        .lines()
        .filter_map(|line| line.trim().strip_prefix("const val "))
        .map(|rest| ident(rest).to_owned())
        .collect();
    check_entry_points("the Android routes", declared, ANDROID_ROUTES);
    for (name, _) in ANDROID_ROUTES {
        assert!(
            app.contains(&format!("composable(Routes.{name}")),
            "the navigation host no longer opens Routes.{name}"
        );
    }

    let declared: Vec<String> = code_lines(ANDROID_SETTINGS)
        .filter_map(|line| line.trim().strip_prefix("include(\""))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect();
    check_entry_points("the Android Gradle modules", declared, ANDROID_MODULES);
}

/// The iOS project's native targets, by name.
const IOS_TARGETS: &[(&str, Module)] = &[
    ("KoruMobile", Module::Records(&["C-IOS-APP"])),
    (
        "KoruMobileTests",
        Module::Excluded("the app's unit-test bundle; draws nothing a person sees"),
    ),
    ("KoruShare", Module::Records(&["C-IOS-SHARE-EXTENSION"])),
];

/// The iOS root's navigation routes, by the case `Route` declares, and the
/// records each opens.
const IOS_ROUTES: &[(&str, Module)] = &[
    ("workspaces", Module::Records(&["C-IOS-WORKSPACES"])),
    ("documents", Module::Records(&["C-IOS-DOCUMENTS"])),
    ("document", Module::Records(&["C-IOS-DOCUMENT-VIEW"])),
    ("devices", Module::Records(&["C-IOS-DEVICES"])),
    ("settings", Module::Records(&["C-IOS-SETTINGS"])),
    ("diagnostics", Module::Records(&["C-IOS-SETTINGS"])),
];

/// The Swift the release packager compiles into the macOS share extension,
/// by the path it names.
const MAC_PACKAGED: &[(&str, Module)] = &[
    (
        "apps/ios/KoruShared/ShareInbox.swift",
        Module::Excluded(
            "the inbox format the iOS and macOS extensions and the iOS app compile; draws nothing",
        ),
    ),
    (
        "apps/macos/KoruShare/ShareViewController.swift",
        Module::Records(&["C-MACOS-SHARE-EXTENSION"]),
    ),
];

/// Every door into the Apple UI — an Xcode target (an app, an extension, a
/// widget, a notification service would each be one), a route of the iOS
/// root's navigation stack, and the Swift the macOS packager compiles — is
/// classified.
#[test]
fn the_apple_entry_points_are_classified() {
    // Every string literal that names a native Swift source, each once.
    let mut packaged: Vec<String> = Vec::new();
    for (at, _) in MAC_PACKAGER.match_indices("\"apps/") {
        let rest = &MAC_PACKAGER[at + 1..];
        let literal = &rest[..rest.find('"').expect("a path literal closes")];
        if literal.ends_with(".swift") && !packaged.iter().any(|known| known == literal) {
            packaged.push(literal.to_owned());
        }
    }
    check_entry_points("the macOS packager's Swift", packaged, MAC_PACKAGED);

    let section = IOS_PROJECT
        .split("/* Begin PBXNativeTarget section */")
        .nth(1)
        .and_then(|rest| rest.split("/* End PBXNativeTarget section */").next())
        .expect("the iOS project lists its native targets");
    let targets: Vec<String> = section
        .lines()
        .filter_map(|line| line.trim().strip_prefix("name = "))
        .filter_map(|rest| rest.strip_suffix(';'))
        .map(str::to_owned)
        .collect();
    check_entry_points("the iOS project's targets", targets, IOS_TARGETS);

    let routes = enum_members("apps/ios/KoruMobile/Views.swift", "Route");
    check_entry_points("the iOS routes", routes, IOS_ROUTES);
    let views = native_source("apps/ios/KoruMobile/Views.swift");
    for (name, _) in IOS_ROUTES {
        assert!(
            views.contains(&format!("case .{name}:"))
                || views.contains(&format!("case let .{name}(")),
            "the root's navigation destination no longer opens Route.{name}"
        );
    }
}

/// What the native counts add up to, for the manifest's table.
pub(super) fn counts() -> Vec<(&'static str, usize)> {
    let files = |prefix: &str| {
        SOURCES
            .iter()
            .filter(|(file, _)| file.starts_with(prefix))
            .count()
    };
    let declarations = |language: Language| -> usize {
        SOURCES
            .iter()
            .filter(|(file, _)| owner_of(file).1 == language)
            .map(|(file, _)| ui_declarations(file).len())
            .sum()
    };
    vec![
        ("native files: Android", files("apps/android/")),
        ("native files: iOS", files("apps/ios/")),
        ("native files: macOS", files("apps/macos/")),
        ("native composables", declarations(Language::Kotlin)),
        ("native views", declarations(Language::Swift)),
        (
            "native entry points baselined",
            ANDROID_COMPONENTS.len()
                + ANDROID_ROUTES.len()
                + ANDROID_MODULES.len()
                + IOS_TARGETS.len()
                + IOS_ROUTES.len()
                + MAC_PACKAGED.len(),
        ),
        ("records: native platforms", native_records().count()),
    ]
}
