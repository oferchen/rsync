#![deny(unsafe_code)]

//! Token definitions backing `--out-format` parsing and rendering.

/// Parsed representation of an `--out-format` specification.
#[derive(Clone, Debug)]
pub(crate) struct OutFormat {
    tokens: Vec<OutFormatToken>,
}

impl OutFormat {
    /// Constructs a new [`OutFormat`] from parsed tokens.
    pub(super) fn new(tokens: Vec<OutFormatToken>) -> Self {
        Self { tokens }
    }

    /// Returns an iterator over the parsed tokens.
    pub(super) fn tokens(&self) -> impl Iterator<Item = &OutFormatToken> {
        self.tokens.iter()
    }

    /// Returns `true` when no tokens were parsed from the format string.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[derive(Clone, Debug)]
pub(super) enum OutFormatToken {
    Literal(String),
    Placeholder(OutFormatPlaceholder),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum OutFormatPlaceholder {
    FileName,
    FileNameWithSymlinkTarget,
    FullPath,
    ItemizedChanges,
    FileLength,
    BytesTransferred,
    ChecksumBytes,
    Operation,
    ModifyTime,
    PermissionString,
    CurrentTime,
    SymlinkTarget,
    OwnerName,
    GroupName,
    OwnerUid,
    OwnerGid,
    ProcessId,
    RemoteHost,
    RemoteAddress,
    ModuleName,
    ModulePath,
    FullChecksum,
}

/// Context values used when rendering `--out-format` placeholders.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutFormatContext {
    pub(super) remote_host: Option<String>,
    pub(super) remote_address: Option<String>,
    pub(super) module_name: Option<String>,
    pub(super) module_path: Option<String>,
}
