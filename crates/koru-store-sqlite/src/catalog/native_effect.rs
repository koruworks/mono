// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_store_core::{
    NativeEffectBinding, NativeEffectDrainReceipt, NativeEffectFence, NativeEffectKind,
};

fn validate(catalog: &SqliteCatalog, binding: &NativeEffectBinding) -> Result<()> {
    if !binding.valid() {
        return Err(Error::bare(Code::InvalidRequest));
    }
    if catalog.local_user_scope()? != binding.user_scope {
        return Err(Error::bare(Code::Forbidden));
    }
    Ok(())
}
fn token(kind: NativeEffectKind) -> &'static str {
    match kind {
        NativeEffectKind::CreateDocument => "create_document",
        NativeEffectKind::EditDocument => "edit_document",
    }
}
type Raw = (
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<u8>,
    i64,
    Option<i64>,
);
fn row(conn: &Connection, binding: &NativeEffectBinding) -> Result<Option<NativeEffectFence>> {
    let raw: Option<Raw> = conn.query_row("SELECT user_scope_id,scope,request_key,workspace_id,document_id,kind,request_fingerprint,fenced_at_ms,drained_at_ms FROM native_effect_fences WHERE user_scope_id=?1 AND scope=?2 AND request_key=?3", params![binding.user_scope.to_storage(),binding.scope,binding.request_key.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional().map_err(sql_failed)?;
    let Some((user, scope, key, workspace, document, kind, fingerprint, fenced, drained)) = raw
    else {
        return Ok(None);
    };
    let corrupt = || Error::new(Code::CorruptState, "invalid native effect fence");
    let parsed = NativeEffectBinding {
        user_scope: UserScopeId::parse(&user).ok_or_else(corrupt)?,
        scope,
        request_key: key.parse().map_err(|_| corrupt())?,
        workspace: WorkspaceId::parse(&workspace).ok_or_else(corrupt)?,
        document: DocumentId::parse(&document).ok_or_else(corrupt)?,
        kind: match kind.as_str() {
            "create_document" => NativeEffectKind::CreateDocument,
            "edit_document" => NativeEffectKind::EditDocument,
            _ => return Err(corrupt()),
        },
        request_fingerprint: fingerprint.try_into().map_err(|_| corrupt())?,
    };
    if !parsed.valid()
        || parsed.user_scope.to_storage() != user
        || parsed.request_key.to_string() != key
        || parsed.workspace.to_storage() != workspace
        || parsed.document.to_storage() != document
    {
        return Err(corrupt());
    }
    if &parsed != binding {
        return Err(Error::new(
            Code::IdempotencyConflict,
            "native effect binding changed",
        ));
    }
    Ok(Some(NativeEffectFence {
        binding: parsed,
        fenced_at: Timestamp::from_millis(fenced),
        drained_at: drained.map(Timestamp::from_millis),
    }))
}
pub(super) fn read(
    catalog: &SqliteCatalog,
    binding: &NativeEffectBinding,
) -> Result<Option<NativeEffectFence>> {
    validate(catalog, binding)?;
    row(&catalog.connection, binding)
}
pub(super) fn fence(
    catalog: &mut SqliteCatalog,
    binding: &NativeEffectBinding,
    at: Timestamp,
) -> Result<NativeEffectFence> {
    validate(catalog, binding)?;
    let tx = catalog
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_failed)?;
    if let Some(old) = row(&tx, binding)? {
        tx.commit().map_err(sql_failed)?;
        return Ok(old);
    }
    tx.execute("INSERT INTO native_effect_fences(user_scope_id,scope,request_key,workspace_id,document_id,kind,request_fingerprint,fenced_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![binding.user_scope.to_storage(),binding.scope,binding.request_key.to_string(),binding.workspace.to_storage(),binding.document.to_storage(),token(binding.kind),binding.request_fingerprint.as_slice(),at.millis()]).map_err(sql_failed)?;
    let result = row(&tx, binding)?.ok_or_else(|| Error::bare(Code::CorruptState))?;
    tx.commit().map_err(sql_failed)?;
    Ok(result)
}
pub(super) fn complete(
    catalog: &mut SqliteCatalog,
    binding: &NativeEffectBinding,
    at: Timestamp,
) -> Result<NativeEffectDrainReceipt> {
    validate(catalog, binding)?;
    let tx = catalog
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_failed)?;
    let existing = row(&tx, binding)?
        .ok_or_else(|| Error::new(Code::InvalidRequest, "native deny fence is missing"))?;
    let drained = existing.drained_at.unwrap_or(at);
    if existing.drained_at.is_none() {
        tx.execute("UPDATE native_effect_fences SET drained_at_ms=?4 WHERE user_scope_id=?1 AND scope=?2 AND request_key=?3 AND drained_at_ms IS NULL",params![binding.user_scope.to_storage(),binding.scope,binding.request_key.to_string(),at.millis()]).map_err(sql_failed)?;
    }
    tx.commit().map_err(sql_failed)?;
    Ok(NativeEffectDrainReceipt {
        binding: existing.binding,
        fenced_at: existing.fenced_at,
        drained_at: drained,
    })
}

#[cfg(test)]
mod tests;
