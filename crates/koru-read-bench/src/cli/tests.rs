// SPDX-License-Identifier: MPL-2.0

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

#[test]
fn a_corpus_alone_reads_text_layers_with_the_product_defaults() {
    let Parsed::Run(options) = parse(args(&["corpus"])).unwrap() else {
        panic!("a run");
    };
    assert_eq!(
        options,
        Options {
            corpus: PathBuf::from("corpus"),
            model: None,
            truth: None,
            threads: 4,
            max_read_pages: Some(200),
            json: None,
        }
    );
}

#[test]
fn every_option_is_read() {
    let Parsed::Run(options) = parse(args(&[
        "--model",
        "m",
        "fixtures",
        "--truth",
        "t.json",
        "--threads",
        "2",
        "--max-read-pages",
        "0",
        "--json",
        "out.json",
    ]))
    .unwrap() else {
        panic!("a run");
    };
    assert_eq!(options.corpus, PathBuf::from("fixtures"));
    assert_eq!(options.model, Some(PathBuf::from("m")));
    assert_eq!(options.truth, Some(PathBuf::from("t.json")));
    assert_eq!(options.threads, 2);
    assert_eq!(options.max_read_pages, None, "0 lifts the cap");
    assert_eq!(options.json, Some(PathBuf::from("out.json")));
}

/// A mistyped flag would measure the wrong thing; it is refused.
#[test]
fn an_unknown_flag_a_missing_value_and_no_corpus_are_refused() {
    assert!(parse(args(&["corpus", "--modle", "m"])).is_err());
    assert!(parse(args(&["corpus", "--model"])).is_err());
    assert!(parse(args(&["--threads", "0", "corpus"])).is_err());
    assert!(parse(args(&[])).is_err());
    assert!(parse(args(&["a", "b"])).is_err());
    assert_eq!(parse(args(&["--help"])).unwrap(), Parsed::Help);
}

#[test]
fn the_help_names_every_flag() {
    for flag in [
        "--model",
        "--truth",
        "--threads",
        "--max-read-pages",
        "--json",
        "--help",
    ] {
        assert!(HELP.contains(flag), "{flag}");
    }
}
