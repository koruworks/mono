// SPDX-License-Identifier: MPL-2.0

//! The registration against the source it describes. Everything here is in
//! memory: the production file is compiled in as text, never read at run time.

use super::*;

/// The file the button entry names as its owner.
const BUTTON_SOURCE: &str = include_str!("../../../../../crates/koru-ui/src/button.rs");

/// The members of `pub enum <name>` in the button source, in declaration
/// order: every line of the body that is a bare identifier and a comma.
fn source_variants(name: &str) -> Vec<&'static str> {
    let opening = format!("pub enum {name} {{");
    let start = BUTTON_SOURCE
        .find(&opening)
        .unwrap_or_else(|| panic!("button.rs no longer declares `pub enum {name}`"))
        + opening.len();
    let body = &BUTTON_SOURCE[start..];
    let body = &body[..body.find("\n}").expect("the enum closes")];
    body.lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(','))
        .filter(|member| {
            !member.is_empty()
                && member
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .collect()
}

fn button() -> &'static Entry {
    entry(BUTTON).expect("the seed registers the shared button")
}

#[test]
fn the_button_entry_names_the_real_button() {
    let button = button();
    assert_eq!(button.public_name, "KoruButton");
    assert_eq!(button.family, Family::Controls);
    assert_eq!(button.source.file, "crates/koru-ui/src/button.rs");
    // The name the production painter itself reports to the inspector.
    assert!(
        BUTTON_SOURCE.contains(&format!("\"{}\"", button.public_name)),
        "button.rs no longer registers itself as {}",
        button.public_name
    );
    // Each symbol exists, under the registered path. `type_name` is the
    // compiler's own answer, so a moved or renamed item fails here.
    let symbols: Vec<&str> = button.recipes.iter().map(|recipe| recipe.symbol).collect();
    // The path only: how a lifetime parameter is printed is the compiler's
    // business, and the registration names the item, not an instantiation.
    let path = |name: &'static str| name.split('<').next().unwrap_or(name);
    assert_eq!(
        symbols,
        [
            path(std::any::type_name_of_val(&koru_ui::button::draw_button)),
            path(std::any::type_name::<koru_ui::button::ActionButton<'static>>()),
        ]
    );
    for recipe in button.recipes {
        assert!(
            recipe.symbol.starts_with(button.source.module),
            "{} is outside {}",
            recipe.symbol,
            button.source.module
        );
    }
}

#[test]
fn each_recipe_lists_its_own_production_type() {
    let [dialog, compact] = button().recipes else {
        panic!("the button has a dialog recipe and a compact one");
    };
    assert_eq!(
        dialog.styles.type_path(),
        std::any::type_name::<ButtonRole>()
    );
    assert_eq!(
        compact.styles.type_path(),
        std::any::type_name::<ActionStyle>()
    );
    assert_eq!(dialog.styles, Styles::Dialog(&DIALOG_STYLES));
    assert_eq!(compact.styles, Styles::Compact(&COMPACT_STYLES));
}

/// The listed styles are exactly the enum's members, in its order. The slot
/// functions match every member, so equal lengths and matching slots mean
/// nothing is missing and nothing is listed twice.
#[test]
fn the_listed_styles_are_the_production_enums_members() {
    for (index, role) in DIALOG_STYLES.into_iter().enumerate() {
        assert_eq!(dialog_slot(role), index, "{role:?}");
    }
    for (index, style) in COMPACT_STYLES.into_iter().enumerate() {
        assert_eq!(compact_slot(style), index, "{style:?}");
    }
    let dialog: Vec<String> = DIALOG_STYLES.iter().map(|r| format!("{r:?}")).collect();
    assert_eq!(dialog, source_variants("ButtonRole"));
    let compact: Vec<String> = COMPACT_STYLES.iter().map(|s| format!("{s:?}")).collect();
    assert_eq!(compact, source_variants("ActionStyle"));
    assert_eq!(DIALOG_STYLES[0], ButtonRole::default());
    assert!(COMPACT_STYLES.contains(&ActionStyle::default()));
}

#[test]
fn the_trial_story_resolves_to_the_dialog_recipe() {
    let (entry, recipe) = story(BUTTON_DIALOG_STORY).expect("the trial story is registered");
    assert_eq!(entry.catalog_id, BUTTON);
    assert_eq!(recipe.symbol, "koru_ui::button::draw_button");
    assert_eq!(recipe.styles.len(), 3);
    assert_eq!(recipe.a2ui, A2ui::NotSupported);
    assert_eq!(story("C-BUTTON/compact"), None);
    assert_eq!(story(""), None);
}

/// A compact action has no story yet, and says so rather than borrowing the
/// dialog one. It is the recipe the composition catalog's Button draws.
#[test]
fn a_recipe_without_a_story_is_listed_as_such() {
    let compact = button().recipes[1];
    assert_eq!(compact.story, Story::NoStoryYet);
    assert_eq!(compact.a2ui, A2ui::HostStyled);
}

#[test]
fn ids_are_unique_and_every_platform_has_an_answer() {
    for (index, entry) in ENTRIES.iter().enumerate() {
        assert!(
            ENTRIES[..index]
                .iter()
                .all(|earlier| earlier.catalog_id != entry.catalog_id),
            "{} is registered twice",
            entry.catalog_id
        );
        let listed: Vec<Platform> = entry.platforms.iter().map(|(p, _)| *p).collect();
        assert_eq!(listed, Platform::ALL, "{}", entry.catalog_id);
    }
    let mut stories: Vec<&str> = ENTRIES
        .iter()
        .flat_map(|entry| entry.recipes)
        .filter_map(|recipe| match recipe.story {
            Story::Available(id) => Some(id),
            Story::NoStoryYet => None,
        })
        .collect();
    let count = stories.len();
    stories.sort_unstable();
    stories.dedup();
    assert_eq!(stories.len(), count, "a story ID is registered twice");
    assert_eq!(
        button().availability(Platform::Windows),
        Availability::Available
    );
    assert_eq!(
        button().availability(Platform::Android),
        Availability::NotAudited
    );
}
