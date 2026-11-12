// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_dictionary_of_one_character_per_line_is_accepted_with_or_without_a_final_newline() {
    assert_eq!(parse_dictionary("a\nb\nc\n"), Ok(3));
    assert_eq!(parse_dictionary("a\nb\nc"), Ok(3));
    // CRLF, as a checkout on Windows may write it: `str::lines` reads the
    // same characters.
    assert_eq!(parse_dictionary("a\r\nb\r\n"), Ok(2));
}

/// Plan 0.1's dictionary holds an ideographic space and characters outside
/// the Basic Multilingual Plane; each is one character.
#[test]
fn white_space_and_astral_characters_are_characters() {
    assert_eq!(parse_dictionary("!\n\u{3000}\n\u{20164}\n日\n"), Ok(4));
    assert_eq!(parse_dictionary(" \n"), Ok(1));
}

/// **A blank line would shift every index after it**: refused, with where.
#[test]
fn a_blank_line_anywhere_is_refused() {
    assert_eq!(
        parse_dictionary("a\n\nb\n"),
        Err(DictionaryError::BlankLine(2))
    );
    assert_eq!(
        parse_dictionary("\na\n"),
        Err(DictionaryError::BlankLine(1))
    );
    assert_eq!(
        parse_dictionary("a\n\n"),
        Err(DictionaryError::BlankLine(2))
    );
    assert_eq!(
        parse_dictionary("a\r\n\r\nb"),
        Err(DictionaryError::BlankLine(2))
    );
}

#[test]
fn a_line_of_two_characters_an_empty_file_and_a_byte_order_mark_are_refused() {
    assert_eq!(
        parse_dictionary("a\nbc\n"),
        Err(DictionaryError::NotOneCharacter(2))
    );
    assert_eq!(parse_dictionary(""), Err(DictionaryError::Empty));
    assert_eq!(parse_dictionary("\n"), Err(DictionaryError::Empty));
    assert_eq!(
        parse_dictionary("\u{FEFF}a\nb\n"),
        Err(DictionaryError::ByteOrderMark)
    );
}

#[test]
fn the_three_files_have_the_conversions_names() {
    let model = ModelDir::new("models");
    assert_eq!(model.detector(), Path::new("models").join("det.onnx"));
    assert_eq!(model.recognizer(), Path::new("models").join("rec.onnx"));
    assert_eq!(model.dictionary(), Path::new("models").join("rec_dict.txt"));
}

/// An error names a file of the model, never anything of a person's.
#[test]
fn errors_name_the_models_files() {
    assert_eq!(
        ModelError::Missing(DETECTOR).to_string(),
        "the model has no det.onnx"
    );
    assert_eq!(
        ModelError::Dictionary(DictionaryError::BlankLine(7)).to_string(),
        "the model's rec_dict.txt: line 7 is blank"
    );
}
