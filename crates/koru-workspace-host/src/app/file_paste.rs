// SPDX-License-Identifier: MPL-2.0

//! Build ordinary attachment rows in memory for the existing document planting
//! path. File import owns the bytes; the rows contain references only.

use koru_core::schema::{Cutting, Parent};
use koru_core::{
    Attr, AttrValue, DocRef, DocumentId, Node, NodeId, Ref, WorkspaceDoc, WorkspaceId,
};

pub(super) fn attachment_cuttings(
    workspace: WorkspaceId,
    files: &[(DocumentId, String)],
) -> Result<Vec<(Cutting, Option<koru_core::ListStyle>)>, koru_core::StoreError> {
    let mut doc = WorkspaceDoc::create(workspace, "")?;
    let mut cuttings = Vec::with_capacity(files.len());
    for (file, label) in files {
        let row = doc.insert(Parent::Root, Node::paragraph("")?)?;
        let attr = Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                NodeId::from_uuid(file.as_uuid()),
                DocRef::Document(*file),
            )),
            label,
        );
        doc.insert_pill(row, 0, &attr)?;
        cuttings.push((doc.cutting(row)?, None));
    }
    Ok(cuttings)
}

#[cfg(test)]
mod tests;
