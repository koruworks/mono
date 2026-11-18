// SPDX-License-Identifier: MPL-2.0

//! Copied filesystem entries are an import gesture, with the destination chosen
//! before any file is read. Bitmap/text clipboard contents retain their routes.

use std::path::PathBuf;

use koru_clipboard::Backend as _;
use koru_i18n::{Key as Text, t};
use koru_ui::{Choice, ChoiceOutcome};
use koru_workspace_host::{ImportSourceSelection, PaneRequest};
use winit::keyboard::{Key, ModifiersState};

use super::{MainView, TabView};

pub(super) struct Offer {
    pub dialog: Choice,
    paths: Vec<PathBuf>,
    document: Option<koru_core::DocumentId>,
    draft: Option<u64>,
}

impl MainView {
    pub(super) fn offer_file_paste(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if !koru_keys::registry::get(koru_keys::ShortcutId::Paste)
            .chord
            .matches(key, text, mods)
            || self.tabs().active_tab().view.is_prompting()
            || self.tabs().active_tab().view.is_renaming()
            || !matches!(
                self.tabs().active_tab().view,
                TabView::Welcome(_) | TabView::Leaf(_)
            )
        {
            return false;
        }
        // Only the path list is copied here: no file opens, image decoding,
        // network work or polling on the UI thread.
        let Ok(paths) = koru_clipboard::system().backend().image_paths() else {
            return false;
        };
        self.offer_copied_files(paths)
    }

    pub(super) fn offer_copied_files(&mut self, paths: Vec<PathBuf>) -> bool {
        if paths.is_empty() {
            return false;
        }
        let lang = crate::format::lang();
        let document = matches!(self.tabs().active_tab().view, TabView::Leaf(_))
            .then_some(self.tabs().active_tab().document)
            .flatten();
        let draft = self
            .tabs()
            .active_tab()
            .draft
            .as_ref()
            .map(|_| self.tabs().active_tab().id);
        let mut dialog = Choice::new(t(lang, Text::PasteFilesTitle), file_list(&paths, lang));
        if document.is_some() || draft.is_some() {
            dialog = dialog.answer(t(lang, Text::PasteFilesAttach));
        }
        dialog = dialog
            .answer(t(
                lang,
                if document.is_some() || draft.is_some() {
                    Text::PasteFilesOnly
                } else {
                    Text::PasteFilesImport
                },
            ))
            .answer(t(lang, Text::WorkspacesCancel));
        self.file_paste = Some(Offer {
            dialog,
            paths,
            document,
            draft,
        });
        true
    }

    pub(super) fn answer_file_paste(&mut self, out: ChoiceOutcome) -> bool {
        if out == ChoiceOutcome::Keep {
            return false;
        }
        let Some(offer) = self.file_paste.take() else {
            return false;
        };
        if let Some(tab) = offer.draft {
            match out {
                ChoiceOutcome::Chosen(0) if self.tabs().active_tab().id == tab => {
                    self.pending_file_paste_draft = Some((tab, offer.paths));
                    self.promote_draft();
                }
                ChoiceOutcome::Chosen(1) => {
                    self.ask_pane(PaneRequest::ImportFiles(ImportSourceSelection::Files(
                        offer.paths,
                    )));
                }
                _ => {}
            }
            return true;
        }
        match (out, offer.document) {
            (ChoiceOutcome::Chosen(0), Some(document)) => {
                self.ask_pane(PaneRequest::ImportFileAttachments {
                    paths: offer.paths,
                    document,
                });
            }
            (ChoiceOutcome::Chosen(0), None) | (ChoiceOutcome::Chosen(1), Some(_)) => {
                self.ask_pane(PaneRequest::ImportFiles(ImportSourceSelection::Files(
                    offer.paths,
                )));
            }
            _ => {}
        }
        true
    }

    pub(super) fn finish_draft_file_paste(&mut self) {
        let Some((tab, paths)) = self.pending_file_paste_draft.take() else {
            return;
        };
        if self.tabs().active_tab().id == tab
            && let Some(document) = self.tabs().active_tab().document
        {
            self.ask_pane(PaneRequest::ImportFileAttachments { paths, document });
        }
    }
}

fn file_list(paths: &[PathBuf], lang: koru_i18n::Lang) -> String {
    // Choice does not scroll. Keep long names and bulk selections from hiding
    // its answers; the full original selection stays in Offer, never in logs.
    let mut names: Vec<String> = paths
        .iter()
        .take(3)
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            let mut short: String = name
                .chars()
                .take(36)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            if name.chars().count() > 36 {
                short.push('…');
            }
            short
        })
        .collect();
    if paths.len() > names.len() {
        names.push(koru_i18n::fill(
            t(lang, Text::PasteFilesMore),
            &[&koru_i18n::number::decimal(
                lang,
                (paths.len() - names.len()) as u64,
            )],
        ));
    }
    names.join("\n")
}

#[cfg(test)]
mod tests;
