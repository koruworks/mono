// SPDX-License-Identifier: MPL-2.0

//! The hosted catalog, split by what an answer means.
//!
//! The names are the hosted server's rung 1
//! (`contracts/hosted-tools-v1.md` section 3, "The catalog is the capability
//! set"; `koru_mcp_server::RUNG1_TOOLS`). A test holds this list against that
//! constant, so a tool added to the rung fails here until somebody says
//! whether it reads or writes.
//!
//! **Two enums, so the split is a compile-time fact.** [`Client::read`] takes
//! a [`ReadTool`] and returns an answer or a refusal, and may be repeated
//! freely. [`Client::write`] takes a [`WriteTool`] and returns a
//! [`WriteOutcome`]. A write tool cannot be sent through the read path, where
//! a lost answer would look like a harmless failure.
//!
//! [`Client::read`]: crate::Client::read
//! [`Client::write`]: crate::Client::write
//! [`WriteOutcome`]: crate::WriteOutcome

/// A tool that changes nothing: rung 1's reads
/// (`koru_mcp_server::RUNG1_READ_TOOLS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadTool {
    GetDocumentation,
    ListWorkspaces,
    ListDocuments,
    SearchDocuments,
    ReadDocument,
    ListTasks,
}

/// A tool that can change something the hosted server holds.
///
/// Five of them write documents. The other three (`create_image_upload`,
/// `claim_document`, `release_document`) change no document, and are here
/// because a lost answer leaves the caller not knowing whether an upload was
/// minted or a claim taken or released, which is the same question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WriteTool {
    CreateDocument,
    EditDocument,
    ExtractToDocument,
    CreateImageUpload,
    AddImage,
    ReplaceImage,
    ClaimDocument,
    ReleaseDocument,
}

/// Any tool of the hosted catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostedTool {
    Read(ReadTool),
    Write(WriteTool),
}

impl ReadTool {
    pub const ALL: [ReadTool; 6] = [
        ReadTool::GetDocumentation,
        ReadTool::ListWorkspaces,
        ReadTool::ListDocuments,
        ReadTool::SearchDocuments,
        ReadTool::ReadDocument,
        ReadTool::ListTasks,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            ReadTool::GetDocumentation => "get_documentation",
            ReadTool::ListWorkspaces => "list_workspaces",
            ReadTool::ListDocuments => "list_documents",
            ReadTool::SearchDocuments => "search_documents",
            ReadTool::ReadDocument => "read_document",
            ReadTool::ListTasks => "list_tasks",
        }
    }
}

impl WriteTool {
    pub const ALL: [WriteTool; 8] = [
        WriteTool::CreateDocument,
        WriteTool::EditDocument,
        WriteTool::ExtractToDocument,
        WriteTool::CreateImageUpload,
        WriteTool::AddImage,
        WriteTool::ReplaceImage,
        WriteTool::ClaimDocument,
        WriteTool::ReleaseDocument,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            WriteTool::CreateDocument => "create_document",
            WriteTool::EditDocument => "edit_document",
            WriteTool::ExtractToDocument => "extract_to_document",
            WriteTool::CreateImageUpload => "create_image_upload",
            WriteTool::AddImage => "add_image",
            WriteTool::ReplaceImage => "replace_image",
            WriteTool::ClaimDocument => "claim_document",
            WriteTool::ReleaseDocument => "release_document",
        }
    }

    /// Whether a success of this tool must carry a document revision.
    ///
    /// `create_document` and `edit_document` answer `revision`, the image
    /// tools answer the edit's `revision`, and `extract_to_document` answers
    /// `document_revision` and `source_revision`
    /// (`crates/koru-mcp-server/MCP.md`, "Results"). The other three write no
    /// document.
    pub(crate) const fn writes_a_document(self) -> bool {
        matches!(
            self,
            WriteTool::CreateDocument
                | WriteTool::EditDocument
                | WriteTool::ExtractToDocument
                | WriteTool::AddImage
                | WriteTool::ReplaceImage
        )
    }
}

impl HostedTool {
    pub const fn name(self) -> &'static str {
        match self {
            HostedTool::Read(tool) => tool.name(),
            HostedTool::Write(tool) => tool.name(),
        }
    }

    /// The hosted tool of this name, if the catalog has one.
    pub fn from_name(name: &str) -> Option<Self> {
        ReadTool::ALL
            .into_iter()
            .map(HostedTool::Read)
            .chain(WriteTool::ALL.into_iter().map(HostedTool::Write))
            .find(|tool| tool.name() == name)
    }
}

/// Whether `name` could be a tool's name: a short token of lower-case
/// letters, digits and underscores. Every name in the catalog is one, so it
/// travels in a header as it is and is safe to print.
pub(crate) fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}
