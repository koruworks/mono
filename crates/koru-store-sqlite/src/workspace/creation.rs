// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(super) fn origin_at(
    connection: &Connection,
    document: DocumentId,
) -> Result<CreationOriginLookup> {
    let row: Option<(String,String,String,Vec<u8>,i64)> = connection.query_row(
        "SELECT scope,request_key,request_hash,initial_update,created_at FROM document_creation_origins WHERE document_id=?1",
        params![document.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).optional().map_err(sql_failed)?;
    if let Some((scope, request_key, request_hash, initial_update, created_at)) = row {
        if request_key.is_empty()
            || request_key.len() > 128
            || request_hash.is_empty()
            || request_hash.len() > 128
        {
            return Err(Error::new(
                Code::CorruptState,
                "invalid creation origin binding",
            ));
        }
        return Ok(CreationOriginLookup::Known(CreationOrigin {
            document,
            scope,
            request_key,
            request_hash,
            initial_update,
            created_at: Timestamp::from_millis(created_at),
        }));
    }
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM loro_documents WHERE id=?1)",
            params![document.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql_failed)?;
    Ok(if exists {
        CreationOriginLookup::Unknown
    } else {
        CreationOriginLookup::Missing
    })
}

impl SqliteWorkspace {
    pub(super) fn seed_document_inner(
        &mut self,
        seed: &SeedDocument,
        strict: bool,
    ) -> Result<SeededDocument> {
        let tx = self.connection.transaction().map_err(sql_failed)?;
        tx.execute(
            "DELETE FROM document_creations WHERE created_at < ?1",
            params![seed.expires_before.millis()],
        )
        .map_err(sql_failed)?;

        // A v2 caller-minted candidate is the create identity. Content was
        // committed before manifest publication, so finding this row can mean
        // the earlier call died in that safe gap. Reuse its original first
        // update and let Store finish publication; never replace those bytes
        // with a later retry body. This deliberately precedes receipt/hash
        // lookup because the transport receipt is allowed to expire while the
        // document identity is not.
        if strict {
            let receipt: Option<String> = tx
                .query_row(
                    "SELECT document_id FROM document_creations WHERE scope=?1 AND request_key=?2",
                    params![seed.scope, seed.key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_failed)?;
            if receipt
                .as_deref()
                .is_some_and(|id| id != seed.candidate.to_storage())
            {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "creation receipt names another candidate",
                ));
            }
            match origin_at(&tx, seed.candidate)? {
                CreationOriginLookup::Known(origin) => {
                    if origin.initial_update.is_empty()
                        || origin.initial_update.len() > 16 * 1024 * 1024
                    {
                        return Err(Error::new(
                            Code::InvalidRequest,
                            "original seed exceeds strict creation bounds",
                        ));
                    }
                    if origin.scope != seed.scope
                        || origin.request_key != seed.key
                        || origin.request_hash != seed.request_hash
                    {
                        return Err(Error::new(
                            Code::IdempotencyConflict,
                            "candidate creation binding differs",
                        ));
                    }
                    let kind: String = tx
                        .query_row(
                            "SELECT kind FROM loro_documents WHERE id=?1",
                            params![seed.candidate.to_storage()],
                            |r| r.get(0),
                        )
                        .map_err(sql_failed)?;
                    if kind != DocumentKind::Document.as_str() {
                        return Err(Error::new(
                            Code::IdempotencyConflict,
                            "candidate kind differs",
                        ));
                    }
                    tx.commit().map_err(sql_failed)?;
                    return Ok(SeededDocument {
                        document: seed.candidate,
                        initial_update: origin.initial_update,
                        replayed: true,
                    });
                }
                CreationOriginLookup::Unknown => {
                    return Err(Error::new(
                        Code::IdempotencyConflict,
                        "candidate creation origin is unknown",
                    ));
                }
                CreationOriginLookup::Missing => {}
            }
        }
        if seed.recover_candidate && !strict {
            let existing: Option<(String, Vec<u8>)> = tx
                .query_row(
                    "SELECT d.kind, u.update_bytes
                       FROM loro_documents d
                       JOIN loro_updates u ON u.document_id = d.id AND u.seq = 1
                      WHERE d.id = ?1",
                    params![seed.candidate.to_storage()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql_failed)?;
            if let Some((kind, initial_update)) = existing {
                if kind != DocumentKind::Document.as_str() {
                    return Err(Error::new(
                        Code::IdempotencyConflict,
                        "the candidate identity belongs to another document kind",
                    ));
                }
                tx.commit().map_err(sql_failed)?;
                return Ok(SeededDocument {
                    document: seed.candidate,
                    initial_update,
                    replayed: true,
                });
            }
        }

        let existing: Option<(String, String, Vec<u8>)> = tx
            .query_row(
                "SELECT request_hash, document_id, initial_update
                   FROM document_creations
                  WHERE scope = ?1 AND request_key = ?2",
                params![seed.scope, seed.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sql_failed)?;
        if let Some((request_hash, document, initial_update)) = existing {
            let document = DocumentId::parse(&document).ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    "an idempotency record contains an invalid document id",
                )
            })?;
            if (strict && document != seed.candidate) || request_hash != seed.request_hash {
                return Err(Error::new(
                    Code::IdempotencyConflict,
                    "that idempotency key was already used for a different request",
                ));
            }
            tx.commit().map_err(sql_failed)?;
            return Ok(SeededDocument {
                document,
                initial_update,
                replayed: true,
            });
        }

        let document = seed.candidate.to_storage();
        tx.execute(
            "INSERT INTO loro_documents
               (id, kind, latest_checkpoint_seq, version_vector, frontiers, updated_at)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
            params![
                document,
                DocumentKind::Document.as_str(),
                seed.version_vector,
                seed.frontiers,
                seed.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "INSERT INTO loro_updates
               (document_id, seq, update_bytes, received_at, from_client)
             VALUES (?1, 1, ?2, ?3, NULL)",
            params![document, seed.initial_update, seed.created_at.millis()],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "INSERT INTO document_creations
               (scope, request_key, request_hash, document_id, initial_update, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                seed.scope,
                seed.key,
                seed.request_hash,
                document,
                seed.initial_update,
                seed.created_at.millis()
            ],
        )
        .map_err(sql_failed)?;
        tx.execute(
            "INSERT INTO document_creation_origins (document_id,scope,request_key,request_hash,initial_update,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![document, seed.scope, seed.key, seed.request_hash, seed.initial_update, seed.created_at.millis()],
        ).map_err(sql_failed)?;
        tx.commit().map_err(sql_failed)?;
        Ok(SeededDocument {
            document: seed.candidate,
            initial_update: seed.initial_update.clone(),
            replayed: false,
        })
    }
}
