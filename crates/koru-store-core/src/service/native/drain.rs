// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{NativeEffectBinding, NativeEffectDrainReceipt, NativeEffectKind};

impl<C: Catalog, B: Backend> Store<C, B> {
    fn native_effect_owner(&self, binding: &NativeEffectBinding) -> Result<()> {
        if !binding.valid() {
            return Err(Error::bare(Code::InvalidRequest));
        }
        if self.catalog.local_user_scope()? != binding.user_scope {
            return Err(Error::bare(Code::Forbidden));
        }
        Ok(())
    }

    /// Internal native adapter preflight, before placement or availability reads.
    /// Effects must still use the identity-bound wrappers under this same owner.
    pub fn check_native_effect(&self, binding: &NativeEffectBinding) -> Result<()> {
        self.native_effect_owner(binding)?;
        if self.catalog.native_effect_fence(binding)?.is_some() {
            return Err(Error::new(Code::Forbidden, "native operation is fenced"));
        }
        Ok(())
    }

    /// Commit a permanent deny fence, settle only already-loaded affected
    /// windows, then acknowledge liveness. This never proves a write's result.
    /// The caller holds the existing serialized Store owner throughout.
    pub fn drain_native_effect(
        &mut self,
        binding: &NativeEffectBinding,
        at: Timestamp,
    ) -> Result<NativeEffectDrainReceipt> {
        self.native_effect_owner(binding)?;
        let fence = self.catalog.fence_native_effect(binding, at)?;
        if let Some(drained_at) = fence.drained_at {
            return Ok(NativeEffectDrainReceipt {
                binding: fence.binding,
                fenced_at: fence.fenced_at,
                drained_at,
            });
        }
        // A failed flush retains the commit window and the durable deny row.
        // No workspace/page is opened or reconstructed to establish liveness.
        if self.open.contains_key(&binding.workspace) {
            self.flush(binding.workspace, binding.document, at)?;
        }
        if binding.kind == NativeEffectKind::CreateDocument {
            self.flush_workspace_manifest(binding.workspace, at)?;
        }
        // A move can remove the loaded document while an earlier failed
        // commit window remains. flush's absent-position Ok(None) is not a
        // drain proof. Never discard that window or reconstruct its target.
        if let Some(open) = self.open.get(&binding.workspace) {
            let pending = |id| {
                open.windows
                    .get(&id)
                    .is_some_and(|window| !window.is_empty())
            };
            if pending(binding.document)
                || (binding.kind == NativeEffectKind::CreateDocument && pending(open.manifest_id))
            {
                return Err(Error::new(
                    Code::StorageFailed,
                    "native effect window remains pending",
                ));
            }
        }
        self.catalog.complete_native_effect_drain(binding, at)
    }

    /// Root-page creation bound to the trusted operation's permanent fence.
    /// Ordinary seeded creation and its expiring retry keys remain unchanged.
    pub fn create_native_document(
        &mut self,
        binding: &NativeEffectBinding,
        title: &str,
        initial_update: Vec<u8>,
        attribution: Option<&str>,
        at: Timestamp,
    ) -> std::result::Result<ProvenSeededCreation, NativeUpdateError> {
        self.check_native_effect(binding)?;
        if binding.kind != NativeEffectKind::CreateDocument {
            return Err(Error::bare(Code::InvalidRequest).into());
        }
        let hash = binding
            .request_fingerprint
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.create_seeded_document_with_origin(
            binding.workspace,
            title,
            "page",
            None,
            None,
            &binding.scope,
            &binding.request_key.to_string(),
            &hash,
            binding.document,
            initial_update,
            attribution,
            at,
        )
        .map_err(NativeUpdateError::Unconfirmed)
    }

    /// The permanent deny check and conditional effect share one Store owner
    /// boundary. A late caller cannot bypass the fence by waiting to enqueue.
    pub fn apply_native_update_at_version(
        &mut self,
        binding: &NativeEffectBinding,
        expected_version: &[u8],
        update: Vec<u8>,
        at: Timestamp,
        origin: crate::ports::UpdateOrigin,
    ) -> std::result::Result<DocumentVersion, NativeUpdateError> {
        self.check_native_effect(binding)?;
        if binding.kind != NativeEffectKind::EditDocument {
            return Err(Error::bare(Code::InvalidRequest).into());
        }
        self.apply_update_at_version(
            binding.workspace,
            binding.document,
            expected_version,
            update,
            at,
            origin,
        )
    }
}
