// SPDX-License-Identifier: MPL-2.0
//! The working folder stays visible and can be changed directly.
use super::*;

impl NewBotDialog {
    pub(super) fn folder_controls(&self, locked: bool) -> El {
        let summary = self.summary();
        let folder = row("New bot working folder")
            .gap(Space::S2)
            .items_center()
            .child(
                leaf(
                    "New bot change folder",
                    IconLeaf {
                        glyph: icons::FOLDER,
                        disabled: locked,
                    },
                )
                .semantic()
                .id(CHANGE_FOLDER)
                .radius(Radius::M)
                .states(&QUIET_STATES)
                .hit()
                .focusable()
                .disabled(locked)
                .action(Action::Click)
                .action(Action::Focus)
                .semantics(Role::Button)
                .name(Words::BotsChangeFolder),
            )
            .child(
                text(summary.clone())
                    .named("New bot folder")
                    .id(FOLDER)
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .fill()
                    .truncate_middle()
                    .semantics(Role::Group)
                    .name_text(summary),
            );
        let mut actions = row("New bot folder actions").gap(Space::S2);
        if self
            .form()
            .is_some_and(|form| form.draft().folder.is_some())
        {
            actions = actions.child(self.settings_link(
                "New bot managed folder",
                MANAGED_FOLDER,
                Words::BotsFolderUseManaged,
                locked,
            ));
        }
        col("New bot folder controls")
            .gap(Space::S2)
            .child(folder)
            .child(actions.child(spacer()).child(self.settings_link(
                "New bot full form",
                FULL_FORM,
                Words::BotsNewBotFullForm,
                false,
            )))
    }
}
