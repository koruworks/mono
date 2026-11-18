// SPDX-License-Identifier: MPL-2.0
//! Finite creation effects under fresh Store authority. Every intent/advance
//! is committed by the engine; the gateway owns neither database nor history.
use super::*;
use koru_ai_core::bots::{BotAuthoritySnapshot, BotOwner, BotRolePlacement, BotRoleSeed};
use koru_bots_engine::{
    creation::{self as c, Failure, Phase, Record, Step},
    folders,
};
use uuid::Uuid;

impl Gateway {
    /// A concurrent poll or abandonment may advance the same intent. Report
    /// its current progress; that CAS conflict is not changed person input.
    pub(super) fn creation_progress(
        &self,
        owner: BotOwner,
        record: Record,
    ) -> Result<Record, Error> {
        let request = record.request;
        match self.drive_creation(owner, record) {
            Err(Error::Conflict) => self.creation_read(owner, request)?.ok_or(Error::NotAllowed),
            result => result,
        }
    }
    pub(super) fn creation_call(
        &self,
        owner: BotOwner,
        command: c::Command,
    ) -> Result<c::Reply, Error> {
        let scope = owner.user_scope.to_storage();
        (self.creation)(c::Request {
            owner: scope.clone(),
            sender: format!("person:{scope}"),
            command,
        })
    }
    pub(super) fn creation_read(
        &self,
        owner: BotOwner,
        request: Uuid,
    ) -> Result<Option<Record>, Error> {
        match self.creation_call(owner, c::Command::Read { request })? {
            c::Reply::Record(record) => Ok(record),
            _ => Err(Error::EngineUnavailable),
        }
    }
    pub(super) fn creation_advance(
        &self,
        owner: BotOwner,
        record: &Record,
        step: Step,
    ) -> Result<Record, Error> {
        match self.creation_call(
            owner,
            c::Command::Advance {
                request: record.request,
                revision: record.revision,
                step,
            },
        )? {
            c::Reply::Record(Some(record)) => Ok(record),
            _ => Err(Error::EngineUnavailable),
        }
    }
    pub(super) fn creation_authority(
        &self,
        owner: BotOwner,
        record: &Record,
    ) -> Result<BotAuthoritySnapshot, Error> {
        let d = &record.definition;
        let workspace = d.access.workspace;
        let needs_role = record.seed.is_none() || record.phase == Phase::Ready;
        let pages: Vec<_> = d
            .knowledge
            .iter()
            .chain(d.memory.iter())
            .copied()
            .chain(d.role.iter().copied().filter(|_| needs_role))
            .map(koru_ai_core::DocumentId::from_uuid)
            .chain(d.access.selected_pages.iter().copied())
            .map(|document| BotPage {
                workspace,
                document,
            })
            .collect();
        let snapshot = self
            .authority
            .inspect(&[workspace], &pages)
            .map_err(authority_error)?;
        if snapshot.owner != owner {
            return Err(Error::NotAllowed);
        }
        snapshot.workspace(workspace).map_err(authority_error)?;
        for page in pages {
            snapshot
                .pages
                .get(&page)
                .ok_or(Error::NotAllowed)?
                .as_ref()
                .map_err(|e| authority_error(*e))?;
        }
        Ok(snapshot)
    }
    /// Runs on a bounded background job. On a lost reply callers read progress
    /// again; they never replace the candidate, seed or effect request identity.
    pub(super) fn drive_creation(
        &self,
        owner: BotOwner,
        mut record: Record,
    ) -> Result<Record, Error> {
        for _ in 0..4 {
            if matches!(record.phase, Phase::Completed | Phase::Abandoned)
                || record.failure.is_some()
            {
                return Ok(record);
            }
            let authority = match self.creation_authority(owner, &record) {
                Ok(authority) => authority,
                Err(_) => {
                    return self.creation_advance(
                        owner,
                        &record,
                        Step::Failed {
                            reason: Failure::AuthorityUnavailable,
                        },
                    );
                }
            };
            match record.phase {
                Phase::Intent | Phase::FolderPending => {
                    let first = record.phase == Phase::Intent;
                    if first {
                        record = self.creation_advance(owner, &record, Step::StartFolder)?;
                    }
                    let folder = if let Some(path) = &record.definition.configuration.working_folder
                    {
                        folders::selected(
                            self.folders.as_ref(),
                            std::path::Path::new(path),
                            std::slice::from_ref(&self.root),
                        )
                        .and_then(|bound| {
                            if bound == std::path::Path::new(path) {
                                Ok(bound)
                            } else {
                                Err(folders::Failure::Mismatch)
                            }
                        })
                    } else {
                        folders::managed(
                            self.folders.as_ref(),
                            &self.root,
                            record.bot,
                            if first {
                                folders::Purpose::Create
                            } else {
                                folders::Purpose::RecoverCreation
                            },
                        )
                    };
                    let step = match folder {
                        Ok(folder) => match folder.to_str() {
                            Some(binding) => Step::FolderReady {
                                binding: binding.to_owned(),
                            },
                            None => Step::Failed {
                                reason: Failure::FolderInvalid,
                            },
                        },
                        Err(error) => Step::Failed {
                            reason: match error {
                                folders::Failure::Occupied => Failure::FolderOccupied,
                                folders::Failure::Mismatch => Failure::FolderMismatch,
                                folders::Failure::Invalid | folders::Failure::Reserved => {
                                    Failure::FolderInvalid
                                }
                                folders::Failure::Unavailable => Failure::FolderUnavailable,
                            },
                        },
                    };
                    record = self.creation_advance(owner, &record, step)?;
                }
                Phase::FolderReady | Phase::RolePending => {
                    if record.phase == Phase::FolderReady {
                        record = self.creation_advance(owner, &record, Step::StartRole)?;
                    }
                    let seed = record.seed.as_ref().ok_or(Error::InvalidRequest)?;
                    let workspace = record.definition.access.workspace;
                    let seed = BotRoleSeed {
                        owner,
                        workspace,
                        profile: authority.workspace(workspace).map_err(authority_error)?,
                        title: seed.title.clone(),
                        candidate: koru_ai_core::DocumentId::from_uuid(seed.candidate),
                        request_key: seed.request.to_string(),
                        request_hash: seed.hash.clone(),
                        seed: seed.bytes.clone(),
                    };
                    let step = match self.authority.create_role_page(&seed) {
                        Ok(proof) if !proof.matches(&seed) => Step::Failed {
                            reason: Failure::RoleConflict,
                        },
                        Ok(proof) if proof.placement == BotRolePlacement::Trashed => Step::Failed {
                            reason: Failure::RoleUnavailable,
                        },
                        Ok(proof) if proof.placement == BotRolePlacement::Missing => Step::Failed {
                            reason: Failure::RoleMissing,
                        },
                        Ok(_) => Step::RoleVerified,
                        Err(error) => Step::Failed {
                            reason: match error {
                                BotError::Conflict => Failure::RoleConflict,
                                BotError::Invalid => Failure::StoreRefused,
                                BotError::Forbidden => Failure::AuthorityUnavailable,
                                _ => Failure::RoleUnavailable,
                            },
                        },
                    };
                    record = self.creation_advance(owner, &record, step)?;
                }
                Phase::Ready => record = self.creation_advance(owner, &record, Step::Complete)?,
                Phase::Completed | Phase::Abandoned => return Ok(record),
            }
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests;
