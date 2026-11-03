// SPDX-License-Identifier: MPL-2.0

//! **Migrated surfaces have no unapproved design exceptions**
//! (ELEMENTS.final §4, layer 3).
//!
//! Every `.rs` file on [`MIGRATED`] is read from the repository and checked
//! line by line for the forms that bypass the element layer's tokens: raw
//! colours and sizes, reads of the global palette, branches on the
//! appearance, scene paint outside a leaf adapter, and a literal handed to
//! `name_text` where a translation key belongs. A line that needs one of
//! them anyway is recorded in `crates/koru-draw/ui-allowances.txt` with its
//! scope, reason and owner, and an entry nothing matches any more fails too.
//!
//! What this cannot do is prove a builder chain or where a `name_text`
//! string came from; the types and the frame's validation hold those. It
//! catches the obvious forms in the files that promise to have none.

use std::path::{Path, PathBuf};

/// The files written on the element layer, by path from the repository root.
/// A surface joins when it moves over; it never leaves.
const MIGRATED: &[&str] = &[
    "apps/koru/src/panes/explore/popover_elements.rs",
    // ELEMENTS-PLAN P5: the Profiles page.
    "apps/koru/src/panes/explore/popover_elements/profiles.rs",
    // The chat's model picker (koru-ai D64, QUESTIONS M7): its tree and its
    // paint, every leaf a named adapter; its tokens are defined beside them
    // in `model_picker/tokens.rs`.
    "crates/koru-ai-ui/src/model_picker/elements.rs",
    "crates/koru-ai-ui/src/model_picker/paint.rs",
];

const ALLOWANCES: &str = include_str!("../../ui-allowances.txt");

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Rule {
    RawColour,
    RawSize,
    PaletteRead,
    AppearanceBranch,
    DirectPaint,
    UntranslatedName,
}

impl Rule {
    fn key(self) -> &'static str {
        match self {
            Self::RawColour => "raw-colour",
            Self::RawSize => "raw-size",
            Self::PaletteRead => "palette-read",
            Self::AppearanceBranch => "appearance-branch",
            Self::DirectPaint => "direct-paint",
            Self::UntranslatedName => "untranslated-name",
        }
    }

    fn advice(self) -> &'static str {
        match self {
            Self::RawColour => "use a colour role (Surface, Ink, Line, Accent)",
            Self::RawSize => {
                "use a Space step, a Radius, a metrics:: or alias:: token, or Length::Runtime for measured geometry"
            }
            Self::PaletteRead => {
                "read colours from the frame's context (cx.palette()), never theme::p()"
            }
            Self::AppearanceBranch => "let the theme decide; never branch on is_dark()",
            Self::DirectPaint => "paint through an element, or inside an `impl Leaf` adapter",
            Self::UntranslatedName => "name application chrome with .name(koru_i18n::Key::…)",
        }
    }
}

const FORMS: &[(Rule, &str)] = &[
    (Rule::RawColour, "Color::from_rgb8("),
    (Rule::RawColour, "Color::from_rgba8("),
    (Rule::RawColour, "Color::from_rgb("),
    (Rule::RawColour, "Color::from_rgba("),
    (Rule::RawColour, "Color::new("),
    (Rule::RawColour, "Color::WHITE"),
    (Rule::RawColour, "Color::BLACK"),
    (Rule::RawColour, "palette::css::"),
    (Rule::RawSize, "Space::Px("),
    (Rule::RawSize, "Radius::Px("),
    (Rule::RawSize, "Length::Px("),
    (Rule::RawSize, "Metric::new("),
    (Rule::RawSize, "Tagged::new("),
    (Rule::PaletteRead, "theme::p()"),
    (Rule::AppearanceBranch, "is_dark()"),
    (Rule::DirectPaint, "scene::fill("),
    (Rule::DirectPaint, "scene::stroke("),
    (Rule::DirectPaint, "scene::draw_layout"),
    (Rule::DirectPaint, ".push_layer("),
    (Rule::DirectPaint, ".push_clip_layer("),
    (Rule::UntranslatedName, ".name_text(\""),
];

#[derive(Debug, PartialEq)]
struct Finding {
    path: String,
    line: usize,
    rule: Rule,
    text: String,
}

/// One allowance: `path | rule | snippet | reason | owner | replacement`.
#[derive(Debug)]
struct Allowance {
    path: String,
    rule: String,
    snippet: String,
}

fn allowances(text: &str) -> Vec<Allowance> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split('|').map(str::trim).collect();
            assert!(
                fields.len() == 6 && fields[..5].iter().all(|f| !f.is_empty()),
                "ui-allowances.txt: `{line}` needs path | rule | snippet | reason | owner | replacement (replacement may be empty)"
            );
            assert!(
                FORMS.iter().any(|(rule, _)| rule.key() == fields[1]),
                "ui-allowances.txt: `{}` is not a rule",
                fields[1]
            );
            Allowance {
                path: fields[0].into(),
                rule: fields[1].into(),
                snippet: fields[2].into(),
            }
        })
        .collect()
}

/// The code on a line: its text before a `//` comment that is not inside a
/// string literal.
fn code(line: &str) -> &str {
    let mut quoted = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b'/' if !quoted && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn braces(code: &str) -> i32 {
    let mut quoted = false;
    let mut depth = 0;
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                chars.next();
            }
            '"' => quoted = !quoted,
            '\'' if !quoted => {
                // A char literal like '{': skip it whole.
                let rest: String = chars.clone().take(3).collect();
                if rest.chars().nth(1) == Some('\'') {
                    chars.next();
                    chars.next();
                }
            }
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Every finding in one source. Comments, `#[cfg(test)]` items and the body
/// of an `impl … Leaf for …` (where an adapter paints) are not held to the
/// paint rule; tests are not held to any.
fn scan(path: &str, source: &str) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut depth = 0;
    // The depth an exempt block opened at, and whether it exempts all rules
    // (a test item) or only direct paint (a leaf adapter).
    let mut exempt: Vec<(i32, bool)> = Vec::new();
    let mut test_item_next = false;
    for (n, line) in source.lines().enumerate() {
        let code = code(line);
        let trimmed = code.trim();
        if trimmed.starts_with("#[cfg(test)]") {
            test_item_next = true;
            continue;
        }
        let opening = depth;
        let delta = braces(code);
        if test_item_next && !trimmed.is_empty() && !trimmed.starts_with("#[") {
            test_item_next = false;
            if delta > 0 {
                exempt.push((opening, true));
            } else {
                // `mod tests;` — the item is another file, not on the list.
                depth += delta;
                continue;
            }
        } else if implements_leaf(trimmed) && delta > 0 {
            exempt.push((opening, false));
        }
        let all = exempt.iter().any(|(_, all)| *all);
        let in_leaf = exempt.iter().any(|(_, all)| !*all);
        if !all {
            for (rule, form) in FORMS {
                if *rule == Rule::DirectPaint && in_leaf {
                    continue;
                }
                let hit = if *rule == Rule::PaletteRead {
                    code.contains(form) || reads_p(code)
                } else {
                    code.contains(form)
                };
                if hit
                    && !found
                        .iter()
                        .any(|f: &Finding| f.line == n + 1 && f.rule == *rule)
                {
                    found.push(Finding {
                        path: path.into(),
                        line: n + 1,
                        rule: *rule,
                        text: line.trim().into(),
                    });
                }
            }
        }
        depth += delta;
        exempt.retain(|(at, _)| depth > *at);
    }
    found
}

/// `impl Leaf for X` or `impl el::Leaf for X`: the trait named is `Leaf`.
fn implements_leaf(code: &str) -> bool {
    code.starts_with("impl")
        && code.contains(" for ")
        && code
            .split(" for ")
            .next()
            .is_some_and(|head| head.rsplit([' ', ':', '>']).next() == Some("Leaf"))
}

/// A bare `p()` call: the global palette read under its short name.
fn reads_p(code: &str) -> bool {
    code.match_indices("p()").any(|(at, _)| {
        code[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.' || c == ':'))
    })
}

fn allowed(finding: &Finding, entries: &[Allowance]) -> Option<usize> {
    entries.iter().position(|a| {
        a.path == finding.path && a.rule == finding.rule.key() && finding.text.contains(&a.snippet)
    })
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn migrated_surfaces_have_no_unapproved_design_exceptions() {
    let entries = allowances(ALLOWANCES);
    let mut used = vec![false; entries.len()];
    let mut failures = Vec::new();
    for path in MIGRATED {
        let source = std::fs::read_to_string(repository().join(path))
            .unwrap_or_else(|e| panic!("{path} is on the migrated list but cannot be read: {e}"));
        for finding in scan(path, &source) {
            match allowed(&finding, &entries) {
                Some(i) => used[i] = true,
                None => failures.push(format!(
                    "{}:{}: {} — {}\n    {}",
                    finding.path,
                    finding.line,
                    finding.rule.key(),
                    finding.rule.advice(),
                    finding.text
                )),
            }
        }
    }
    for (entry, used) in entries.iter().zip(used) {
        if !used {
            failures.push(format!(
                "ui-allowances.txt: `{} | {} | {}` matches nothing any more; remove it",
                entry.path, entry.rule, entry.snippet
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "design exceptions without an allowance (crates/koru-draw/ui-allowances.txt):\n{}",
        failures.join("\n")
    );
}

const PATH: &str = "apps/example/src/surface.rs";

fn rules(source: &str) -> Vec<&'static str> {
    scan(PATH, source).iter().map(|f| f.rule.key()).collect()
}

/// A deliberate unapproved `Space::Px(7.0)` fails; recorded with a reason
/// and owner it passes.
#[test]
fn a_raw_spacing_fails_until_it_has_an_allowance() {
    let source = "row(\"Nudged\").pad_x(Space::Px(7.0))\n";
    let findings = scan(PATH, source);
    assert_eq!(rules(source), ["raw-size"]);
    let entries = allowances(
        "apps/example/src/surface.rs | raw-size | Space::Px(7.0) | optical nudge for the glyph | design review 2026-10-29 |\n",
    );
    assert_eq!(allowed(&findings[0], &entries), Some(0));
    let elsewhere = allowances(
        "apps/example/src/other.rs | raw-size | Space::Px(7.0) | optical nudge | design review |\n",
    );
    assert_eq!(allowed(&findings[0], &elsewhere), None, "scope is per file");
}

/// Tagged metrics, the gear's approved legacy values, measured geometry and
/// a model's name are normal inputs.
#[test]
fn tagged_metrics_runtime_geometry_and_model_names_pass() {
    let source = r#"
        row("Profile row")
            .min_h(metrics::PROFILE_ROW_MIN)
            .child(
                leaf("Manage gear", Gear)
                    .semantic()
                    .w(metrics::WORKSPACE_GEAR_PLATE)
                    .radius(Radius::Metric(metrics::WORKSPACE_GEAR_RADIUS)),
            )
            .w(Length::Runtime(width, "sidebar width"))
            .name_text(&workspace.name)
            .pad_x(alias::NAV_PAD)
    "#;
    assert!(rules(source).is_empty(), "{:?}", scan(PATH, source));
}

#[test]
fn each_bypass_is_named() {
    for (line, rule) in [
        ("let ink = Color::from_rgb8(1, 2, 3);", "raw-colour"),
        ("radius(Radius::Px(5.0))", "raw-size"),
        (".w(Length::Px(22.0))", "raw-size"),
        (
            "const MINE: Metric = Metric::new(\"MINE\", 3.0);",
            "raw-size",
        ),
        ("let text = theme::p().text;", "palette-read"),
        ("let fill = p().surface;", "palette-read"),
        ("if is_dark() { a } else { b }", "appearance-branch"),
        ("if system_is_dark() {", "appearance-branch"),
        ("scene::fill(scene, root, colour, &shape);", "direct-paint"),
        ("scene.push_clip_layer(fill, root, &shape);", "direct-paint"),
        (".name_text(\"Settings\")", "untranslated-name"),
    ] {
        assert_eq!(rules(line), [rule], "{line}");
    }
    // Not a palette read: a method or path that ends in p().
    assert!(rules("let t = self.p(); let u = map(); let v = theme::tokens::cap();").is_empty());
}

/// An adapter paints inside its `impl Leaf`; the same call outside fails.
/// Comments and test items are not source.
#[test]
fn leaf_adapters_comments_and_tests_are_not_held_to_the_paint_rule() {
    let source = r#"
impl Leaf for TileLeaf {
    fn paint(&self, scene: &mut Scene) {
        scene::fill(scene, root, cx.palette().surface, &rect);
    }
}

// The old page drew with scene::fill( and Space::Px(7.0); not any more.
fn draw(scene: &mut Scene) {
    scene::fill(scene, root, colour, &rect); // outside a leaf
}

#[cfg(test)]
mod tests {
    fn fixture() { let _ = Space::Px(7.0); let _ = theme::p(); }
}
"#;
    let found = scan(PATH, source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].rule, Rule::DirectPaint);
    assert_eq!(found[0].line, 10);
}

#[test]
fn an_allowance_names_its_scope_reason_and_owner() {
    let entries = allowances(
        "# comment\n\napps/a.rs | raw-colour | Color::from_rgb8( | brand artwork | design | replace with Accent::Brand\n",
    );
    assert_eq!(entries.len(), 1);
    let missing_owner = std::panic::catch_unwind(|| {
        allowances("apps/a.rs | raw-colour | Color:: | brand artwork |  | \n")
    });
    assert!(missing_owner.is_err());
    let unknown_rule =
        std::panic::catch_unwind(|| allowances("apps/a.rs | colours | Color:: | why | who | \n"));
    assert!(unknown_rule.is_err());
}

/// Developer names repeat freely; a key used twice in one frame is a
/// composition error the frame reports (§4, layer 2).
#[test]
fn a_repeated_label_passes_and_a_duplicate_key_fails() {
    use crate::el::{Key, col, row};
    use crate::geometry::Rect;
    use crate::text::TextEngine;
    use crate::theme::themes::{Contrast, LIGHT_THEME};

    let lay = |tree: crate::el::El| {
        tree.layout(
            &mut TextEngine::new(),
            &mut crate::el::TextCache::default(),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            crate::el::FrameCx::new(&LIGHT_THEME, Contrast::Standard),
        )
    };
    let repeated =
        lay(col("List").children((0..3).map(|i| row("Workspace row").id(Key::model(i, "root")))));
    assert!(
        repeated.diagnostics.is_empty(),
        "{:?}",
        repeated.diagnostics
    );
    let duplicate =
        lay(col("List").children((0..2).map(|_| row("Workspace row").id(Key::model(7, "root")))));
    assert!(
        duplicate
            .diagnostics
            .iter()
            .any(|d| d.message == "a key is used twice in one frame")
    );
}
