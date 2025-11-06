#![deny(unsafe_code)]

//! Token definitions backing `--out-format` parsing and rendering.

/// Maximum width accepted for placeholder formatting.
pub(super) const MAX_PLACEHOLDER_WIDTH: usize = 4096;

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
    Placeholder(PlaceholderToken),
}

/// Parsed placeholder together with formatting metadata.
#[derive(Clone, Copy, Debug)]
pub(super) struct PlaceholderToken {
    /// Kind of placeholder rendered by the format specification.
    pub(super) kind: OutFormatPlaceholder,
    /// Formatting controls applied to the rendered value.
    pub(super) format: PlaceholderFormat,
}

impl PlaceholderToken {
    /// Creates a new [`PlaceholderToken`] from the supplied components.
    pub(super) const fn new(kind: OutFormatPlaceholder, format: PlaceholderFormat) -> Self {
        Self { kind, format }
    }
}

/// Placeholder kinds supported by `--out-format`.
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

/// Formatting controls associated with a placeholder.
#[derive(Clone, Copy, Debug)]
pub(super) struct PlaceholderFormat {
    width: Option<usize>,
    align: PlaceholderAlignment,
    humanize: HumanizeMode,
}

impl PlaceholderFormat {
    /// Constructs a [`PlaceholderFormat`] from its individual components.
    pub(super) const fn new(
        width: Option<usize>,
        align: PlaceholderAlignment,
        humanize: HumanizeMode,
    ) -> Self {
        Self {
            width,
            align,
            humanize,
        }
    }

    /// Returns the configured width, when provided.
    #[must_use]
    pub(super) const fn width(&self) -> Option<usize> {
        self.width
    }

    /// Reports whether the rendered value should be left-aligned.
    #[must_use]
    pub(super) const fn align(&self) -> PlaceholderAlignment {
        self.align
    }

    /// Returns the humanisation strategy associated with the placeholder.
    #[must_use]
    pub(super) const fn humanize(&self) -> HumanizeMode {
        self.humanize
    }
}

/// Alignment applied to width-constrained placeholders.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum PlaceholderAlignment {
    /// Right-align the rendered value (default).
    #[default]
    Right,
    /// Left-align the rendered value.
    Left,
}

/// Human-readable formatting applied to numeric placeholders.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum HumanizeMode {
    /// Render the value without any additional formatting.
    #[default]
    None,
    /// Insert locale-style thousands separators.
    Separator,
    /// Render the value using decimal (base-1000) units.
    DecimalUnits,
    /// Render the value using binary (base-1024) units.
    BinaryUnits,
}

/// Context values used when rendering `--out-format` placeholders.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutFormatContext {
    pub(super) remote_host: Option<String>,
    pub(super) remote_address: Option<String>,
    pub(super) module_name: Option<String>,
    pub(super) module_path: Option<String>,
}
