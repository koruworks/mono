// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::model::{Profile, ProfileKind, Timestamp};
use crate::profile_account::ProfileAccountBinding;
use crate::text::Locale;

/// Public identity fields only, authored from current values into a fresh log.
pub struct IdentityManifest {
    pub(super) inner: UnitDoc<ProfileUnitRef>,
}

impl IdentityManifest {
    pub fn create(profile: &Profile) -> Result<Self> {
        let unit = ProfileUnitRef::Identity {
            profile: profile.id,
        };
        let inner = UnitDoc::fresh(unit, |doc| {
            unit.write_header(doc)?;
            let meta = doc.get_map("meta");
            meta.insert("name", profile.name.as_str())
                .map_err(loro_failed)?;
            meta.insert("locale", profile.locale.as_str())
                .map_err(loro_failed)?;
            meta.insert("created_at", profile.created_at.millis())
                .map_err(loro_failed)?;
            meta.insert("kind", profile.kind.as_str())
                .map_err(loro_failed)?;
            for (key, value) in [
                ("purpose", profile.purpose.as_deref()),
                ("avatar", profile.avatar.as_deref()),
                ("icon", profile.icon.as_deref()),
                ("color", profile.color.as_deref()),
                ("account", profile.account.as_deref()),
            ] {
                optional(&meta, key, value)?;
            }
            Ok(())
        })?;
        validation::validate(&inner.doc, inner.unit)?;
        Ok(Self { inner })
    }

    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            inner: load_profile_unit(bytes, |profile, workspace| {
                if workspace.is_some() {
                    return Err(scope_mismatch());
                }
                Ok(ProfileUnitRef::Identity { profile })
            })?,
        })
    }

    pub fn unit(&self) -> ProfileUnitRef {
        self.inner.unit
    }
    unit_methods!();

    pub fn profile(&self) -> Result<Profile> {
        read_profile(&self.inner.doc)
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        self.inner.require_writable()?;
        let mut profile = self.profile()?;
        profile.rename(name)?;
        self.inner
            .doc
            .get_map("meta")
            .insert("name", profile.name)
            .map_err(loro_failed)?;
        self.inner.doc.commit();
        Ok(())
    }
    pub fn set_appearance(&self, icon: Option<&str>, color: Option<&str>) -> Result<()> {
        self.inner.require_writable()?;
        let profile = self.profile()?.with_appearance(icon, color);
        let meta = self.inner.doc.get_map("meta");
        optional(&meta, "icon", profile.icon.as_deref())?;
        optional(&meta, "color", profile.color.as_deref())?;
        self.inner.doc.commit();
        Ok(())
    }
    pub fn set_account(&self, account: &str) -> Result<()> {
        self.inner.require_writable()?;
        ProfileAccountBinding::parse_storage(account)?;
        self.inner
            .doc
            .get_map("meta")
            .insert("account", account)
            .map_err(loro_failed)?;
        self.inner.doc.commit();
        Ok(())
    }
    pub fn clear_account(&self) -> Result<()> {
        self.inner.require_writable()?;
        optional(&self.inner.doc.get_map("meta"), "account", None)?;
        self.inner.doc.commit();
        Ok(())
    }
}

pub(super) fn read_profile(doc: &LoroDoc) -> Result<Profile> {
    let meta = doc.get_map("meta");
    let required =
        |key| string(&meta, key).ok_or_else(|| corrupt("identity field is absent or invalid"));
    let id = ProfileId::parse(&required("profile_id")?)
        .ok_or_else(|| corrupt("invalid identity Profile id"))?;
    let name = required("name")?;
    let locale =
        Locale::parse(&required("locale")?).ok_or_else(|| corrupt("invalid identity locale"))?;
    let created_at = integer(&meta, "created_at")
        .map(Timestamp::from_millis)
        .ok_or_else(|| corrupt("invalid identity creation time"))?;
    let kind = match required("kind")?.as_str() {
        "normal" => ProfileKind::Normal,
        "holder" => ProfileKind::Holder,
        _ => return Err(corrupt("invalid identity kind")),
    };
    let mut profile = Profile {
        id,
        name: name.clone(),
        purpose: string(&meta, "purpose"),
        avatar: string(&meta, "avatar"),
        icon: string(&meta, "icon"),
        color: string(&meta, "color"),
        account: string(&meta, "account"),
        locale,
        created_at,
        kind,
    };
    profile
        .rename(name.clone())
        .map_err(|_| corrupt("invalid identity name"))?;
    if profile.name != name {
        return Err(corrupt("identity name is not canonical"));
    }
    if let Some(account) = &profile.account {
        ProfileAccountBinding::parse_storage(account)?;
    }
    Ok(profile)
}
