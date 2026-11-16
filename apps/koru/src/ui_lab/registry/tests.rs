// SPDX-License-Identifier: MPL-2.0

//! The registration against the source it describes. Everything here is in
//! memory: the production file is compiled in as text, never read at run time.

use super::*;

/// The file the button entry names as its owner.
const BUTTON_TEXT: &str = include_str!("../../../../../crates/koru-ui/src/button.rs");

/// The members of `pub enum <name>` in `source`, in declaration order: the
/// leading identifier of every line of the body that starts a variant, unit
/// or carrying data. Attributes, comments and the fields of a struct-like
/// variant are skipped by their indentation.
fn enum_members(source: &'static str, name: &str) -> Vec<&'static str> {
    let opening = source
        .find(&format!("pub enum {name} {{"))
        .or_else(|| source.find(&format!("pub enum {name}<")))
        .unwrap_or_else(|| panic!("the source no longer declares `pub enum {name}`"));
    let body = &source[opening..];
    let body = &body[body.find('{').expect("the enum opens") + 1..];
    let body = &body[..body.find("\n}").expect("the enum closes")];
    body.lines()
        .filter(|line| line.starts_with("    ") && !line.starts_with("     "))
        .map(str::trim)
        .filter(|line| !line.starts_with("//") && !line.starts_with('#'))
        .filter_map(|line| {
            let end = line
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(line.len());
            let ident = &line[..end];
            (!ident.is_empty() && ident.starts_with(|c: char| c.is_ascii_uppercase()))
                .then_some(ident)
        })
        .collect()
}

fn button() -> &'static Entry {
    catalog()
        .find(|entry| entry.catalog_id == BUTTON)
        .expect("the catalog registers the shared button")
}

/// Every symbol of an entry, story-driven or not, with what it says about
/// A2UI and stories.
fn symbols(entry: &Entry) -> impl Iterator<Item = (&'static str, A2ui, Story)> + use<> {
    let (recipes, apis) = (entry.recipes, entry.apis);
    recipes
        .iter()
        .map(|recipe| (recipe.symbol, recipe.a2ui, recipe.story))
        .chain(apis.iter().map(|api| (api.symbol, api.a2ui, api.story)))
}

#[test]
fn the_button_entry_names_the_real_button() {
    let button = button();
    assert_eq!(button.public_name, "KoruButton");
    assert_eq!(button.family, Family::Controls);
    assert_eq!(button.kind, Kind::Widget);
    assert_eq!(button.source.file, "crates/koru-ui/src/button.rs");
    // The name the production painter itself reports to the inspector.
    assert!(
        BUTTON_TEXT.contains(&format!("\"{}\"", button.public_name)),
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
    for (symbol, _, _) in self::symbols(button) {
        assert!(
            symbol.starts_with(button.source.module),
            "{symbol} is outside {}",
            button.source.module
        );
    }
}

/// Each recipe carries its styles as the production type's own values: the
/// dialog button's are `ButtonRole`, the compact action's `ActionStyle`, and
/// neither list is borrowed from the other.
#[test]
fn each_recipe_lists_its_own_production_type() {
    let [dialog, compact] = button().recipes else {
        panic!("the button has a dialog recipe and a compact one");
    };
    assert_eq!(dialog.styles, Styles::Dialog(&DIALOG_STYLES));
    assert_eq!(compact.styles, Styles::Compact(&COMPACT_STYLES));
}

/// The listed styles are exactly the enum's members, in the order the source
/// declares them. The registry's own constant assertion already holds each
/// list to its enum at compile time; this reads `button.rs` itself, so the
/// order shown in the Lab is the order a reader of the source sees. The
/// option sets the button's other symbols carry are the same two lists.
#[test]
fn the_listed_styles_are_the_production_enums_members() {
    for (index, role) in DIALOG_STYLES.into_iter().enumerate() {
        assert_eq!(dialog_slot(role), index, "{role:?}");
    }
    for (index, style) in COMPACT_STYLES.into_iter().enumerate() {
        assert_eq!(compact_slot(style), index, "{style:?}");
    }
    let dialog: Vec<String> = DIALOG_STYLES.iter().map(|r| format!("{r:?}")).collect();
    assert_eq!(dialog, enum_members(BUTTON_TEXT, "ButtonRole"));
    let compact: Vec<String> = COMPACT_STYLES.iter().map(|s| format!("{s:?}")).collect();
    assert_eq!(compact, enum_members(BUTTON_TEXT, "ActionStyle"));
    assert_eq!(DIALOG_STYLES[0], ButtonRole::default());
    assert!(COMPACT_STYLES.contains(&ActionStyle::default()));

    let idents = |set: OptionSet| -> Vec<String> {
        set.members.iter().map(|m| m.ident.to_owned()).collect()
    };
    assert_eq!(idents(DIALOG_OPTIONS), dialog);
    assert_eq!(idents(COMPACT_OPTIONS), compact);
    for (member, role) in DIALOG_OPTIONS.members.iter().zip(DIALOG_STYLES) {
        assert_eq!(member.label, Some(dialog_style_label(role)));
    }
}

#[test]
fn the_trial_story_resolves_to_the_dialog_recipe() {
    let (entry, recipe) = story(BUTTON_DIALOG_STORY).expect("the trial story is registered");
    assert_eq!(entry.catalog_id, BUTTON);
    assert_eq!(recipe.symbol, "koru_ui::button::draw_button");
    assert_eq!(recipe.styles, Styles::Dialog(&DIALOG_STYLES));
    assert_eq!(recipe.controls, [Control::Label, Control::Style]);
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
    assert_eq!(compact.a2ui, A2ui::HostStyled("Button"));
}

/// **The gallery is desktop only, and the registry is not.** What the
/// gallery shows drops every record written from the Android and iOS apps,
/// keeps the desktop-only native ones, and loses nothing else; the registry
/// itself still holds all of them.
#[test]
fn the_gallery_shows_every_desktop_record_and_no_phone_record() {
    let shown: Vec<&str> = desktop().map(|entry| entry.catalog_id).collect();
    let listed = |records: &[Entry]| -> Vec<bool> {
        records
            .iter()
            .map(|record| shown.contains(&record.catalog_id))
            .collect()
    };
    assert!(!native::ANDROID.is_empty() && !native::IOS.is_empty());
    assert!(
        listed(native::ANDROID).iter().all(|shown| !shown),
        "an Android record is in the gallery"
    );
    assert!(
        listed(native::IOS).iter().all(|shown| !shown),
        "an iOS record is in the gallery"
    );
    // Native, and on a desktop: the macOS share extension, the system
    // surfaces the desktop app asks the operating system for.
    assert!(!native::MACOS.is_empty() && !app::OS_SURFACES.is_empty());
    assert!(listed(native::MACOS).iter().all(|shown| *shown));
    assert!(listed(app::OS_SURFACES).iter().all(|shown| *shown));
    // Nothing else is dropped: the gallery is the catalog less the two
    // phones' own records, in the catalog's order.
    let phones: Vec<&str> = native::ANDROID
        .iter()
        .chain(native::IOS)
        .map(|record| record.catalog_id)
        .collect();
    let expected: Vec<&str> = catalog()
        .map(|entry| entry.catalog_id)
        .filter(|id| !phones.contains(id))
        .collect();
    assert_eq!(shown, expected);
    assert_eq!(catalog().count(), shown.len() + phones.len());
    // The filter reads the record's own platform cells, so a record with a
    // phone cell beside its desktop ones stays.
    assert!(button().on_desktop());
    assert_ne!(
        button().availability(Platform::Ios),
        Availability::NotApplicable
    );
    // A story is looked up through the gallery, never past it.
    for record in native::ANDROID.iter().chain(native::IOS) {
        assert!(!record.on_desktop(), "{}", record.catalog_id);
        for symbol in record.symbols() {
            assert!(!matches!(symbol.story, Story::Available(_)));
        }
    }
}

/// A record's page states one status, one kind and one answer a platform,
/// each in words: no status shares its wording with another, in any
/// language.
#[test]
fn every_status_a_page_states_reads_differently() {
    const KINDS: [Kind; 7] = [
        Kind::Token,
        Kind::Primitive,
        Kind::Widget,
        Kind::Support,
        Kind::DomainComposition,
        Kind::AppLocal,
        Kind::NativeAdapter,
    ];
    const AVAILABILITY: [Availability; 9] = [
        Availability::Available,
        Availability::Existing,
        Availability::SharedSourceUnverified,
        Availability::NativePartial,
        Availability::Planned,
        Availability::Unavailable,
        Availability::NotApplicable,
        Availability::NativeOwned,
        Availability::NotAudited,
    ];
    const STORIES: [Story; 5] = [
        Story::Available("x"),
        Story::NoStoryYet,
        Story::Nonvisual,
        Story::PlatformOnly,
        Story::Boundary,
    ];
    const A2UI: [A2ui; 5] = [
        A2ui::Component("Row"),
        A2ui::HostStyled("Button"),
        A2ui::Values {
            property: "layout.gap",
            mapped: &[],
        },
        A2ui::HostOnly,
        A2ui::NotSupported,
    ];
    fn distinct(lang: koru_i18n::Lang, mut words: Vec<String>) {
        let count = words.len();
        assert!(words.iter().all(|word| !word.trim().is_empty()));
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), count, "{lang:?}: {words:?}");
    }
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            let word = |key| koru_i18n::t(lang, key).to_owned();
            distinct(lang, KINDS.map(|kind| word(kind.label())).to_vec());
            distinct(lang, AVAILABILITY.map(|a| word(a.label())).to_vec());
            distinct(lang, STORIES.map(|story| word(story.label())).to_vec());
            let reasons: Vec<String> = STORIES
                .iter()
                .filter_map(|story| story.reason().map(word))
                .collect();
            assert_eq!(reasons.len(), STORIES.len() - 1, "a story is not explained");
            distinct(lang, reasons);
            let sentences: Vec<String> = A2UI.iter().map(|status| status.sentence()).collect();
            // The identifier is in the sentence as it is written.
            assert!(sentences[0].contains("Row"), "{lang:?}");
            assert!(sentences[1].contains("Button"), "{lang:?}");
            assert!(sentences[2].contains("layout.gap"), "{lang:?}");
            assert!(sentences.iter().all(|sentence| !sentence.contains("{}")));
            distinct(lang, sentences);
        });
    }
    let names: Vec<&str> = Platform::DESKTOP.map(Platform::name).to_vec();
    assert_eq!(names, ["Windows", "macOS", "Linux"]);
}

/// A search is its words, lower case; a record is what they ask for when
/// every one of them is in its name, its public name, its catalog ID or one
/// of its symbols, whatever the case.
#[test]
fn a_record_matches_when_every_word_is_found_in_what_it_is_called() {
    assert_eq!(search_words("  Split   BUTTON "), ["split", "button"]);
    assert!(search_words("   ").is_empty());
    koru_i18n::with_current(koru_i18n::Lang::En, || {
        let found = |query: &str| button().matches(&search_words(query));
        assert!(found("button"));
        assert!(found("KORUBUTTON"));
        assert!(found("c-button"));
        assert!(found("draw_help"));
        // Each word may be found somewhere different.
        assert!(found("koru draw_apple c-butt"));
        assert!(!found("button toggle"), "every word has to be found");
        assert!(!found("zzzz"));
        // No words is no search: everything is what was asked for.
        assert!(found(""));
        // The source file and the purpose are not searched.
        assert!(!found("button.rs"));
        assert!(!found(koru_i18n::t(koru_i18n::Lang::En, button().purpose)));
    });
    // The name searched is the one in the display language.
    for lang in koru_i18n::Lang::ALL {
        koru_i18n::with_current(lang, || {
            for entry in desktop() {
                let name = koru_i18n::t(lang, entry.name);
                assert!(
                    entry.matches(&search_words(name)),
                    "{lang:?}: `{name}` does not find {}",
                    entry.catalog_id
                );
            }
        });
    }
}

/// Each record with a story names the symbol the story drives as a recipe,
/// is drawn on Windows, and kept what was recorded about A2UI and its
/// values when it gained the story.
#[test]
fn every_storied_record_names_what_its_story_drives() {
    let recipe = |id: &str| story(id).unwrap_or_else(|| panic!("no story `{id}`"));
    let cases: [(&str, &str, &str, Styles, &[Control]); 9] = [
        (
            BUTTON_DIALOG_STORY,
            "C-BUTTON",
            "koru_ui::button::draw_button",
            Styles::Dialog(&DIALOG_STYLES),
            &[Control::Label, Control::Style],
        ),
        (
            TEXT_INPUT_STORY,
            "C-TEXT-INPUT",
            "koru_ui::text_input::TextInput",
            Styles::Field(&FIELD_STYLES),
            &[Control::Placeholder, Control::Style],
        ),
        (
            ROW_STORY,
            "P-ROW",
            "koru_draw::el::row",
            Styles::Unstyled,
            &[Control::Gap, Control::Padding],
        ),
        (
            COLUMN_STORY,
            "P-COLUMN",
            "koru_draw::el::col",
            Styles::Unstyled,
            &[Control::Gap, Control::Padding],
        ),
        (
            SPACE_STORY,
            "T-SPACE",
            "koru_draw::theme::Space",
            Styles::Unstyled,
            &[],
        ),
        (
            SURFACE_STORY,
            "T-SURFACE",
            "koru_draw::theme::Surface",
            Styles::Unstyled,
            &[],
        ),
        (
            INK_STORY,
            "T-INK",
            "koru_draw::theme::Ink",
            Styles::Unstyled,
            &[],
        ),
        (
            LINE_STORY,
            "T-LINE",
            "koru_draw::theme::Line",
            Styles::Unstyled,
            &[],
        ),
        (
            TEXT_ROLES_STORY,
            "T-TEXT-ROLES",
            "koru_draw::theme::roles",
            Styles::Unstyled,
            &[],
        ),
    ];
    for (id, catalog_id, symbol, styles, controls) in cases {
        let (entry, recipe) = recipe(id);
        assert_eq!(entry.catalog_id, catalog_id, "{id}");
        assert_eq!(recipe.symbol, symbol, "{id}");
        assert_eq!(recipe.styles, styles, "{id}");
        assert_eq!(recipe.controls, controls, "{id}");
        assert_eq!(entry.story_id(), Some(id));
        assert_eq!(
            entry.availability(Platform::Windows),
            Availability::Available,
            "{id}"
        );
        // The other cells are what they were recorded as.
        assert_eq!(
            entry.availability(Platform::MacOs),
            Availability::SharedSourceUnverified,
            "{id}"
        );
    }
    // Every story the catalog registers is one of these.
    let registered = catalog()
        .flat_map(|entry| entry.symbols())
        .filter(|symbol| matches!(symbol.story, Story::Available(_)))
        .count();
    assert_eq!(registered, cases.len());
    // What was recorded beside the symbol stayed with it.
    assert_eq!(recipe(ROW_STORY).1.a2ui, A2ui::Component("Row"));
    assert_eq!(recipe(COLUMN_STORY).1.a2ui, A2ui::Component("Column"));
    assert!(matches!(
        recipe(SPACE_STORY).1.a2ui,
        A2ui::Values {
            property: "layout.gap, layout.padding, Spacer.space",
            ..
        }
    ));
    assert_eq!(recipe(SPACE_STORY).1.options.len(), 1);
    assert_eq!(recipe(SPACE_STORY).1.options[0].members.len(), 15);
    assert_eq!(
        recipe(TEXT_INPUT_STORY).1.options[0].members[0].ident,
        "Reveal"
    );
    // The field styles are constructors of the production type, by name.
    let idents: Vec<&str> = FIELD_STYLES.iter().map(|style| style.ident()).collect();
    assert_eq!(idents, ["form", "search", "inline"]);
    assert_eq!(FIELD_STYLES[0], FieldStyle::default());
    let source = include_str!("../../../../../crates/koru-ui/src/text_input.rs");
    for ident in idents {
        assert!(
            source.contains(&format!("pub fn {ident}(")),
            "InputStyle no longer has `{ident}`"
        );
    }
    // Every input is text or a choice, named in every language.
    for control in [
        Control::Label,
        Control::Style,
        Control::Placeholder,
        Control::Gap,
        Control::Padding,
    ] {
        let _ = control.input();
        for lang in koru_i18n::Lang::ALL {
            assert!(!koru_i18n::t(lang, control.name()).trim().is_empty());
        }
    }
    let mut parts: Vec<&str> = [
        Control::Label,
        Control::Style,
        Control::Placeholder,
        Control::Gap,
        Control::Padding,
    ]
    .map(Control::part)
    .to_vec();
    parts.sort_unstable();
    parts.dedup();
    assert_eq!(parts.len(), 5);
}

/// A record's status is its symbols': the Button runs, a token table waits,
/// and what several symbols say about A2UI is listed once each.
#[test]
fn a_records_status_is_what_its_symbols_add_up_to() {
    assert_eq!(button().status(), Story::Available(BUTTON_DIALOG_STORY));
    assert_eq!(
        button().a2ui(),
        [
            A2ui::HostStyled("Button"),
            A2ui::HostOnly,
            A2ui::NotSupported
        ]
    );
    assert_eq!(button().symbols().count(), 11);
    for entry in catalog() {
        let stories: Vec<Story> = entry.symbols().map(|symbol| symbol.story).collect();
        let status = entry.status();
        assert!(stories.contains(&status), "{}", entry.catalog_id);
        assert!(!entry.a2ui().is_empty(), "{}", entry.catalog_id);
    }
}

#[test]
fn ids_are_unique_and_every_platform_has_an_answer() {
    const PLATFORMS: [Platform; 5] = [
        Platform::Windows,
        Platform::MacOs,
        Platform::Linux,
        Platform::Android,
        Platform::Ios,
    ];
    let entries: Vec<&Entry> = catalog().collect();
    for (index, entry) in entries.iter().enumerate() {
        assert!(
            entries[..index]
                .iter()
                .all(|earlier| earlier.catalog_id != entry.catalog_id),
            "{} is registered twice",
            entry.catalog_id
        );
        let listed: Vec<Platform> = entry.platforms.iter().map(|(p, _)| *p).collect();
        assert_eq!(listed, PLATFORMS, "{}", entry.catalog_id);
        // The Lab draws only what it has a story for.
        let drawn = entry
            .platforms
            .iter()
            .any(|(_, availability)| *availability == Availability::Available);
        let storied = symbols(entry).any(|(_, _, story)| matches!(story, Story::Available(_)));
        assert_eq!(drawn, storied, "{}", entry.catalog_id);
    }
    let mut stories: Vec<&str> = entries
        .iter()
        .flat_map(|entry| symbols(entry))
        .filter_map(|(_, _, story)| match story {
            Story::Available(id) => Some(id),
            _ => None,
        })
        .collect();
    let count = stories.len();
    stories.sort_unstable();
    stories.dedup();
    assert_eq!(stories.len(), count, "a story ID is registered twice");
    for id in &stories {
        let entry = entries
            .iter()
            .find(|entry| symbols(entry).any(|(_, _, s)| s == Story::Available(id)))
            .unwrap();
        assert!(
            id.starts_with(&format!("{}/", entry.catalog_id)),
            "{id} is not under {}",
            entry.catalog_id
        );
    }
    // The Lab draws the Windows implementation; nothing is claimed for a
    // platform nobody has looked at.
    assert_eq!(
        button().platforms,
        [
            (Platform::Windows, Availability::Available),
            (Platform::MacOs, Availability::SharedSourceUnverified),
            (Platform::Linux, Availability::SharedSourceUnverified),
            (Platform::Android, Availability::NotAudited),
            (Platform::Ios, Availability::NativePartial),
        ]
    );
}

/// Every record names at least one symbol, each symbol sits under the
/// record's module, and no symbol is registered by two records.
#[test]
fn every_record_names_its_symbols_under_its_own_module() {
    let mut all: Vec<&str> = Vec::new();
    for entry in catalog() {
        assert!(
            symbols(entry).next().is_some(),
            "{} names no symbol",
            entry.catalog_id
        );
        for (symbol, _, _) in symbols(entry) {
            assert!(
                symbol.starts_with(&format!("{}::", entry.source.module))
                    || symbol == entry.source.module,
                "{}: {symbol} is outside {}",
                entry.catalog_id,
                entry.source.module
            );
            all.push(symbol);
        }
        assert!(
            entry
                .source
                .module
                .starts_with(&entry.source.owner.replace('-', "_")),
            "{}: {} is not in {}",
            entry.catalog_id,
            entry.source.module,
            entry.source.owner
        );
        // A native adapter is written from Kotlin or Swift; everything else
        // from Rust.
        let native = entry.source.file.ends_with(".kt") || entry.source.file.ends_with(".swift");
        assert!(
            (entry.source.file.ends_with(".rs") || native && entry.kind == Kind::NativeAdapter)
                && !entry.source.file.contains('\\'),
            "{}",
            entry.catalog_id
        );
    }
    let count = all.len();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), count, "a symbol is registered by two records");
}

/// Every word the registration puts on screen — a name, a purpose, a group
/// heading, an input's name, an option set's name, an option's label — is a
/// translation key, so it is present in all eight languages by construction.
/// What a key cannot promise is that two things read differently, or that an
/// identifier stayed out.
#[test]
fn every_displayed_string_is_translated_and_identifiers_are_not() {
    for lang in koru_i18n::Lang::ALL {
        let word = |key| koru_i18n::t(lang, key);
        let mut names: Vec<&str> = Vec::new();
        for entry in catalog() {
            let name = word(entry.name);
            assert!(!name.trim().is_empty());
            assert!(!name.contains('\n'), "{lang:?}: {name}");
            names.push(name);
            let purpose = word(entry.purpose);
            assert!(
                !purpose.trim().is_empty() && !purpose.contains('\n'),
                "{lang:?}: a purpose is one line: {purpose}"
            );
            assert_ne!(name, purpose, "{lang:?}: {}", entry.catalog_id);
            // A technical identifier is shown as itself, never inside a
            // translated sentence where a translator could change it.
            for identifier in [entry.public_name, entry.catalog_id, entry.source.module] {
                assert!(
                    !purpose.contains(identifier) && !name.contains(identifier)
                        || identifier.chars().all(|c| c.is_ascii_alphabetic())
                            && name.eq_ignore_ascii_case(identifier),
                    "{lang:?}: {identifier} leaked into translated text"
                );
            }
            for recipe in entry.recipes {
                for control in recipe.controls {
                    assert!(!word(control.name()).trim().is_empty());
                }
            }
            for api in entry.apis {
                for set in api.options {
                    assert!(!word(set.name).trim().is_empty());
                    let mut labels: Vec<&str> = set
                        .members
                        .iter()
                        .filter_map(|member| member.label.map(word))
                        .collect();
                    let labelled = labels.len();
                    assert!(
                        labelled == 0 || labelled == set.members.len(),
                        "{lang:?}: {} labels some members and not others",
                        set.source
                    );
                    labels.sort_unstable();
                    labels.dedup();
                    assert_eq!(
                        labels.len(),
                        labelled,
                        "{lang:?}: two of {}'s options read the same",
                        set.source
                    );
                }
            }
        }
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "{lang:?}: two records share a name");
        let mut headings: Vec<&str> = Family::ALL.iter().map(|f| word(f.heading())).collect();
        headings.sort_unstable();
        headings.dedup();
        assert_eq!(headings.len(), Family::ALL.len(), "{lang:?}");
        let mut styles: Vec<&str> = DIALOG_STYLES
            .iter()
            .map(|style| word(dialog_style_label(*style)))
            .collect();
        styles.sort_unstable();
        styles.dedup();
        assert_eq!(
            styles.len(),
            DIALOG_STYLES.len(),
            "{lang:?}: two styles read the same"
        );
        assert_ne!(
            word(Control::Label.name()),
            word(Control::Style.name()),
            "{lang:?}"
        );
    }
    assert_ne!(Control::Label.part(), Control::Style.part());
}

mod drift;
