// SPDX-License-Identifier: MPL-2.0

use super::content::ProfileContent;
use super::*;
use crate::reminders::ReminderSettings;

pub struct PrivateManifest {
    pub(super) inner: UnitDoc<ProfileUnitRef>,
}
impl PrivateManifest {
    pub fn create(profile: ProfileId) -> Result<Self> {
        let unit = ProfileUnitRef::Private { profile };
        Ok(Self {
            inner: UnitDoc::fresh(unit, |doc| unit.write_header(doc))?,
        })
    }
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            inner: load_profile_unit(bytes, |profile, workspace| {
                if workspace.is_some() {
                    return Err(scope_mismatch());
                }
                Ok(ProfileUnitRef::Private { profile })
            })?,
        })
    }
    pub fn profile(&self) -> ProfileId {
        self.inner.unit.profile()
    }
    pub fn profile_id(&self) -> ProfileId {
        self.inner.unit.profile()
    }
    pub fn unit(&self) -> ProfileUnitRef {
        self.inner.unit
    }
    unit_methods!();
    fn content(&self) -> ProfileContent<'_> {
        ProfileContent {
            doc: &self.inner.doc,
        }
    }

    pub fn add_dictionary_word(&self, language: &str, word: &str) -> Result<()> {
        self.inner.require_writable()?;
        self.content().add_dictionary_word(language, word)
    }

    pub fn update_reminder_settings(
        &self,
        end_of_day: Option<Option<crate::reminders::LocalTime>>,
        default_tone: Option<crate::reminders::ToneId>,
        week_start: Option<crate::reminders::WeekStartPreference>,
    ) -> Result<bool> {
        self.inner.require_writable()?;
        self.content()
            .update_reminder_settings(end_of_day, default_tone, week_start)
    }

    pub fn remove_dictionary_word(&self, language: &str, word: &str) -> Result<bool> {
        self.inner.require_writable()?;
        self.content().remove_dictionary_word(language, word)
    }

    pub fn clear_dictionary(&self) -> Result<usize> {
        self.inner.require_writable()?;
        self.content().clear_dictionary()
    }

    pub fn reminder_settings(&self) -> ReminderSettings {
        self.content().reminder_settings()
    }

    pub fn dictionary(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.content().dictionary()
    }
}
