// SPDX-License-Identifier: MPL-2.0

//! **The reader's zip: the three converted files, the licence, and the notice
//! that says what Koru changed.**
//!
//! The model store reads one archive format, a zip extracted flat into
//! `<id>/<version>/<digest>/`, so the reader's worker finds `det.onnx`,
//! `rec.onnx` and `rec_dict.txt` directly beneath the directory it is handed.
//! The licence and notice travel inside the archive, beside the weights, which
//! is exactly what voice does for a downloaded model (`voice/repack.rs`): the
//! weights are not in a Koru release package, so neither are their notices.
//!
//! **Apache-2.0 asks for two things here.** A copy of the licence (§4(a)), and
//! "prominent notices stating that You changed the files" (§4(b)) — and a
//! conversion to ONNX is a change. So `NOTICE.txt` names every upstream file by
//! URL, commit and digest, the converter, and the files it produced. The
//! upstream repositories carry no `NOTICE` file of their own to retain (checked
//! at both pinned commits: `README.md`, the three inference files and
//! `.gitattributes`, nothing else).
//!
//! **Deterministic**, through `voice::repack::zip_entries`: the same three
//! files always make the same zip, so the digest pinned in the table is a key
//! that can be computed without building anything.

use super::table::{CONVERTER, ReadModel};
use crate::voice::repack::{self, LICENCE_FILE, NOTICE_FILE, Packaged};
use std::path::Path;

/// `NOTICE.txt`, from the table alone. A pure function so its every line is
/// testable, and because its bytes are inside the pinned digest: anything
/// read from the clock or the machine would make the archive unreproducible.
pub fn notice(model: &ReadModel) -> String {
    let mut out = String::new();
    out.push_str("This directory contains a machine-learning model redistributed by Koru.\n");
    out.push_str("The model is not Koru's work. Its licence is reproduced in ");
    out.push_str(LICENCE_FILE);
    out.push_str(".\n\n");
    for line in model.origin_notice {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\n---\n\nRedistribution by Koru\n\n");
    out.push_str(&format!("SPDX licence identifier: {}\n", model.licence));
    out.push_str(&format!("Attribution: {}\n\n", model.attribution));
    out.push_str("Upstream files (PaddlePaddle inference format), each verified by SHA-256:\n");
    for weight in model.weights {
        out.push_str(&format!("  {}\n    {}\n", weight.url(), weight.sha256));
    }
    out.push_str(&format!(
        "\nChanges made: det.onnx and rec.onnx were converted from the files above\n\
         to ONNX by Koru with {CONVERTER}, using its default options (the opset is\n\
         raised automatically to the lowest the graph needs, the ONNX checker is on,\n\
         and constants are folded). rec_dict.txt is the recognizer's character list,\n\
         taken from rec/inference.yml (PostProcess.character_dict) and written one\n\
         character per line. No weight was retrained, quantised or otherwise altered.\n"
    ));
    out.push_str("\nFiles in this directory produced by that conversion:\n");
    for output in model.outputs {
        out.push_str(&format!(
            "  {}  {} bytes  {}\n",
            output.name, output.size, output.sha256
        ));
    }
    out
}

/// **Build the zip from the converted files in `models`.**
///
/// Every file is checked against its pin first, and an unpinned one is
/// refused: packaging is what turns a conversion into something publishable,
/// and a file nobody recorded must not get a published key by being zipped.
pub fn build(model: &ReadModel, models: &Path, destination: &Path) -> Result<Packaged, String> {
    let mut contents: Vec<(&str, Vec<u8>)> = Vec::new();
    for output in model.outputs {
        let path = models.join(output.name);
        verify_output(output.name, output.size, output.sha256, &path)?;
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        contents.push((output.name, bytes));
    }
    let licence = repack::licence_text(model.licence_file)?;
    let notice = notice(model);
    let mut entries: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect();
    entries.push((LICENCE_FILE, licence.as_bytes()));
    entries.push((NOTICE_FILE, notice.as_bytes()));
    repack::zip_entries(destination, &entries)
}

/// **One converted file against its pin**, or the reason it is refused.
///
/// The same check `convert` runs after converting and `package` runs before
/// zipping, so the two cannot accept different files.
pub fn verify_output(name: &str, size: u64, sha256: &str, path: &Path) -> Result<(), String> {
    if !super::table::is_digest(sha256) {
        return Err(format!(
            "{name} has no pinned SHA-256 in read_model/table.rs, so it will not be \
             packaged. Run `koru-release read-model convert`, check the digests it \
             prints against a second clean run, and paste them into the table."
        ));
    }
    if !path.is_file() {
        return Err(format!(
            "{} is missing. Run `koru-release read-model convert` (Windows x64) first.",
            path.display()
        ));
    }
    let (have_size, have) = repack::hash_file(path)?;
    if have_size != size || have != sha256 {
        return Err(format!(
            "{} is {have_size} bytes, sha256 {have}; the table pins {size} bytes, sha256 \
             {sha256}. Refusing it: either the upstream weights, the converter or the \
             conversion changed, and a changed model needs a new version in the table, \
             not a silent new digest.",
            path.display()
        ));
    }
    Ok(())
}
