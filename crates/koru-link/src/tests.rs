// SPDX-License-Identifier: MPL-2.0

//! What a Koru link is, and — more of this file — what it is not.

use super::*;

#[test]
fn selected_agent_links_encode_and_decode_valid_names() {
    for name in ["review-pr", "issue#42", "docs:docs", "route/a?b", "café"] {
        let skill = skill_link(name).unwrap();
        let command = command_link(name).unwrap();
        assert_eq!(parse(&skill), Ok(KoruLink::Skill { name: name.into() }));
        assert_eq!(parse(&command), Ok(KoruLink::Command { name: name.into() }));
        assert_eq!(wrapped(&format!("before [[{skill}]] after")).len(), 1);
    }
    assert_eq!(
        skill_link("route/a?b").unwrap(),
        "koru://skill/route%2Fa%3Fb"
    );
    assert_eq!(command_link("café").unwrap(), "koru://command/caf%C3%A9");
}

#[test]
fn malformed_or_untransportable_agent_names_remain_text() {
    for name in ["", "two words", "line\nbreak"] {
        assert_eq!(skill_link(name), Err(LinkError::BadName));
    }
    for url in [
        "koru://skill/two%20words",
        "koru://skill/bad%ZZ",
        "koru://skill/%FF",
        "koru://command/a/b",
        "koru://command/name#fragment",
    ] {
        assert!(parse(url).is_err(), "{url}");
        assert!(wrapped(&format!("[[{url}]]")).is_empty(), "{url}");
    }
}

#[test]
fn literal_link_escape_prevents_pill_recognition() {
    let text = "plain [[koru://skill/review-pr]] and [[koru://command/issue%2342]]";
    let escaped = escape_literal_links(text);
    assert_eq!(wrapped(&escaped), Vec::new());
    assert_eq!(unescape_literal_links(&escaped), text);
    let with_slash = "literal \\[[koru://skill/review-pr]]";
    let escaped = escape_literal_links(with_slash);
    assert_eq!(wrapped(&escaped), Vec::new());
    assert_eq!(unescape_literal_links(&escaped), with_slash);
}

fn uuid() -> Uuid {
    Uuid::new_v4()
}

#[test]
fn a_document_link_parses_into_a_document_and_no_node() {
    let document = uuid();
    assert_eq!(
        parse(&format!("koru://doc/{document}")),
        Ok(KoruLink::Document {
            document,
            node: None
        })
    );
}

#[test]
fn a_document_link_with_a_fragment_carries_the_node() {
    let (document, node) = (uuid(), uuid());
    assert_eq!(
        parse(&format!("koru://doc/{document}#{node}")),
        Ok(KoruLink::Document {
            document,
            node: Some(node)
        })
    );
}

#[test]
fn a_chat_link_carries_its_optional_immutable_block() {
    let (chat, block) = (uuid(), uuid());
    assert_eq!(
        parse(&chat_link(chat, Some(block))),
        Ok(KoruLink::Chat {
            chat,
            block: Some(block)
        })
    );
    assert_eq!(
        parse(&chat_link(chat, None)),
        Ok(KoruLink::Chat { chat, block: None })
    );
}

#[test]
fn a_bare_node_link_is_a_node_reference_not_a_document() {
    let node = uuid();
    assert_eq!(parse(&format!("koru://{node}")), Ok(KoruLink::Node(node)));
}

/// **The rule this file exists for.** A form this build does not know is
/// refused, so that adding one later cannot retroactively change what an
/// existing link meant.
#[test]
fn an_unknown_form_is_refused_rather_than_guessed() {
    let id = uuid();
    for text in [
        format!("koru://doc/{id}/history"),
        format!("koru://workspace/{id}"),
        format!("koru://doc/{id}/{id}"),
        // The MCP resource namespace, which is a different thing entirely and
        // must not be mistaken for a document.
        "koru://docs/model".to_owned(),
        "koru://doc/".to_owned(),
        "koru://doc".to_owned(),
        "koru://".to_owned(),
    ] {
        assert!(
            matches!(parse(&text), Err(LinkError::UnknownForm | LinkError::BadId)),
            "{text} was not refused"
        );
    }
}

/// A bare node has nothing to be inside, so a fragment on one is not a deep
/// link — it is a shape nobody wrote.
#[test]
fn a_fragment_on_a_bare_node_is_refused() {
    let (node, other) = (uuid(), uuid());
    assert_eq!(
        parse(&format!("koru://{node}#{other}")),
        Err(LinkError::UnknownForm)
    );
}

#[test]
fn a_non_uuid_id_is_refused() {
    assert_eq!(parse("koru://doc/not-a-uuid"), Err(LinkError::BadId));
    assert_eq!(
        parse(&format!("koru://doc/{}#nope", uuid())),
        Err(LinkError::BadId)
    );
}

#[test]
fn a_scheme_that_is_not_koru_is_refused() {
    let id = uuid();
    for text in [
        format!("https://doc/{id}"),
        format!("file://doc/{id}"),
        // One slash: a valid URI, and not this one.
        format!("koru:/doc/{id}"),
        format!("doc/{id}"),
    ] {
        assert_eq!(parse(&text), Err(LinkError::NotKoru), "{text}");
    }
}

/// The scheme is case-insensitive (RFC 3986 §3.1) and so is a UUID. The path
/// is not: `Doc` being `doc` would be a guess.
#[test]
fn the_scheme_is_case_insensitive_and_the_path_is_not() {
    let document = uuid();
    assert_eq!(
        parse(&format!("KORU://doc/{document}")),
        Ok(KoruLink::Document {
            document,
            node: None
        })
    );
    assert_eq!(
        parse(&format!("koru://DOC/{document}")),
        Err(LinkError::UnknownForm)
    );
}

/// A URL arrives from a browser, through a Windows command line, with one
/// click of intent behind it. Everything that could end an argument or start
/// another is refused before the shape is even looked at.
#[test]
fn a_link_carrying_a_quote_or_a_control_character_is_refused() {
    let id = uuid();
    for text in [
        format!("koru://doc/{id}\" & calc"),
        format!("koru://doc/{id}\n"),
        format!("koru://doc/{id} extra"),
        format!("koru://doc/{id}%00"),
        format!("koru://doc/{id}|x"),
    ] {
        assert_eq!(parse(&text), Err(LinkError::Illegal), "{text:?}");
    }
}

#[test]
fn an_over_long_link_is_refused_before_it_is_parsed() {
    let text = format!("koru://doc/{}", "a".repeat(MAX_LEN));
    assert_eq!(parse(&text), Err(LinkError::Illegal));
}

/// **The two halves are pinned to each other.** A writer and a reader in one
/// crate can still disagree; this is what stops them.
#[test]
fn what_the_writer_writes_is_what_the_parser_accepts() {
    let (document, node) = (uuid(), uuid());

    assert_eq!(
        parse(&document_link(document, None)),
        Ok(KoruLink::Document {
            document,
            node: None
        })
    );
    assert_eq!(
        parse(&document_link(document, Some(node))),
        Ok(KoruLink::Document {
            document,
            node: Some(node)
        })
    );
    assert_eq!(parse(&node_link(node)), Ok(KoruLink::Node(node)));
    assert_eq!(
        parse(&chat_link(document, Some(node))),
        Ok(KoruLink::Chat {
            chat: document,
            block: Some(node)
        })
    );
}

/// A pill's exported token is found where it sits, with its link; brackets
/// around anything else stay text.
#[test]
fn wrapped_finds_every_exported_token_and_nothing_else() {
    let (a, b) = (uuid(), uuid());
    let text = format!(
        "see [[{}]] and [[not a link]] then [[{}]]",
        document_link(a, None),
        document_link(a, Some(b))
    );
    let found = wrapped(&text);
    assert_eq!(found.len(), 2);
    assert_eq!(
        found[0].link,
        KoruLink::Document {
            document: a,
            node: None
        }
    );
    assert_eq!(&text[found[0].range.clone()], format!("[[koru://doc/{a}]]"));
    assert_eq!(
        found[1].link,
        KoruLink::Document {
            document: a,
            node: Some(b)
        }
    );
    assert!(wrapped("[[koru://doc/unclosed").is_empty());
}

/// **A mention of a Bot round-trips**: the id is what identifies it, and the
/// name it was picked under travels with it, whatever it is written in.
#[test]
fn bot_mention_link_round_trips_the_id_and_the_name() {
    let bot = uuid();
    for name in [
        "CFO",
        "Head of Sales",
        "café ☕",
        "財務担当",
        "a/b?c#d %",
        "R&D \"lead\"",
    ] {
        let link = bot_link(bot, name);
        assert!(link.starts_with(&format!("koru://bot/{bot}#")), "{link}");
        assert_eq!(
            parse(&link),
            Ok(KoruLink::Bot {
                bot,
                name: name.into()
            }),
            "{link}"
        );
        let text = format!("ask [[{link}]] please");
        let found = wrapped(&text);
        assert_eq!(found.len(), 1, "{link}");
        assert_eq!(&text[found[0].range.clone()], format!("[[{link}]]"));
    }
    assert_eq!(
        bot_link(bot, "Head of Sales"),
        format!("koru://bot/{bot}#Head%20of%20Sales")
    );
}

/// The name is cleaned before it is written: one line, single spaces, no
/// control characters, cut at a character when it is too long for a link.
#[test]
fn bot_mention_link_cleans_and_bounds_the_name() {
    let bot = uuid();
    assert_eq!(
        parse(&bot_link(bot, "  two\n\tlines\u{7}  here ")),
        Ok(KoruLink::Bot {
            bot,
            name: "two lines here".into()
        })
    );
    // Nothing left of the name: the bare link, which reads back nameless.
    assert_eq!(bot_link(bot, " \n "), format!("koru://bot/{bot}"));
    assert_eq!(
        parse(&format!("koru://bot/{bot}")),
        Ok(KoruLink::Bot {
            bot,
            name: String::new()
        })
    );
    for long in ["x".repeat(500), "財".repeat(500), "🙂 ".repeat(300)] {
        let link = bot_link(bot, &long);
        assert!(link.len() <= 512, "{}", link.len());
        let Ok(KoruLink::Bot { name, .. }) = parse(&link) else {
            panic!("a long name still writes a link that reads back: {link}");
        };
        assert!(!name.is_empty());
        assert!(name.chars().count() <= BOT_NAME_CHARS);
        assert!(long.starts_with(&name), "cut at a character, never changed");
    }
}

/// **An unknown or malformed `koru://bot/…` is left as text**: never guessed
/// at, never half a mention.
#[test]
fn bot_mention_link_unknown_or_malformed_stays_text() {
    let bot = uuid();
    for url in [
        "koru://bot/".to_owned(),
        "koru://bot/not-a-uuid".to_owned(),
        "koru://bot/not-a-uuid#CFO".to_owned(),
        format!("koru://bot/{bot}/history"),
        format!("koru://bot/{bot}/history#CFO"),
        format!("koru://bots/{bot}"),
        format!("koru://Bot/{bot}"),
        format!("koru://bot/{bot}#bad%ZZ"),
        format!("koru://bot/{bot}#%FF"),
        format!("koru://bot/{bot}#half%2"),
        // A name this writer never writes: a control character, a doubled
        // or leading space, a raw reserved character, more than the bound.
        format!("koru://bot/{bot}#line%0Abreak"),
        format!("koru://bot/{bot}#two%20%20spaces"),
        format!("koru://bot/{bot}#%20lead"),
        format!("koru://bot/{bot}#a/b"),
        format!("koru://bot/{bot}#a#b"),
        format!("koru://bot/{bot}#{}", "x".repeat(BOT_NAME_CHARS + 1)),
    ] {
        assert!(parse(&url).is_err(), "{url}");
        let text = format!("ask [[{url}]] please");
        assert!(wrapped(&text).is_empty(), "{url}");
    }
    // Words that look like a mention, typed by hand, are escaped like any
    // other link-shaped words and come back as the words they were.
    let typed = format!("see [[{}]]", bot_link(bot, "CFO"));
    let escaped = escape_literal_links(&typed);
    assert!(wrapped(&escaped).is_empty());
    assert_eq!(unescape_literal_links(&escaped), typed);
}
