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

#[cfg(test)]
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn out_format_new_empty() {
        let format = OutFormat::new(vec![]);
        assert!(format.is_empty());
    }

    #[test]
    fn out_format_new_with_tokens() {
        let tokens = vec![OutFormatToken::Literal("hello".to_string())];
        let format = OutFormat::new(tokens);
        assert!(!format.is_empty());
    }

    #[test]
    fn out_format_tokens_iterator() {
        let tokens = vec![
            OutFormatToken::Literal("a".to_string()),
            OutFormatToken::Literal("b".to_string()),
        ];
        let format = OutFormat::new(tokens);
        let count = format.tokens().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn placeholder_format_new_defaults() {
        let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(format.width(), None);
        assert_eq!(format.align(), PlaceholderAlignment::Right);
        assert_eq!(format.humanize(), HumanizeMode::None);
    }

    #[test]
    fn placeholder_format_with_width() {
        let format = PlaceholderFormat::new(
            Some(20),
            PlaceholderAlignment::Left,
            HumanizeMode::Separator,
        );
        assert_eq!(format.width(), Some(20));
        assert_eq!(format.align(), PlaceholderAlignment::Left);
        assert_eq!(format.humanize(), HumanizeMode::Separator);
    }

    #[test]
    fn placeholder_format_decimal_units() {
        let format = PlaceholderFormat::new(
            Some(10),
            PlaceholderAlignment::Right,
            HumanizeMode::DecimalUnits,
        );
        assert_eq!(format.humanize(), HumanizeMode::DecimalUnits);
    }

    #[test]
    fn placeholder_format_binary_units() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
        assert_eq!(format.humanize(), HumanizeMode::BinaryUnits);
    }

    #[test]
    fn placeholder_alignment_default() {
        let align: PlaceholderAlignment = Default::default();
        assert_eq!(align, PlaceholderAlignment::Right);
    }

    #[test]
    fn humanize_mode_default() {
        let mode: HumanizeMode = Default::default();
        assert_eq!(mode, HumanizeMode::None);
    }

    #[test]
    fn placeholder_token_new() {
        let token = PlaceholderToken::new(
            OutFormatPlaceholder::FileName,
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None),
        );
        // Verify it can be accessed
        let _ = token.kind;
        let _ = token.format;
    }

    #[test]
    fn out_format_token_literal() {
        let token = OutFormatToken::Literal("test".to_string());
        if let OutFormatToken::Literal(s) = token {
            assert_eq!(s, "test");
        } else {
            panic!("Expected Literal variant");
        }
    }

    #[test]
    fn out_format_token_placeholder() {
        let inner = PlaceholderToken::new(
            OutFormatPlaceholder::FileLength,
            PlaceholderFormat::new(
                Some(15),
                PlaceholderAlignment::Left,
                HumanizeMode::Separator,
            ),
        );
        let token = OutFormatToken::Placeholder(inner);
        if let OutFormatToken::Placeholder(p) = token {
            assert_eq!(p.format.width(), Some(15));
        } else {
            panic!("Expected Placeholder variant");
        }
    }

    #[test]
    fn out_format_context_default() {
        let ctx: OutFormatContext = Default::default();
        assert!(ctx.remote_host.is_none());
        assert!(ctx.remote_address.is_none());
        assert!(ctx.module_name.is_none());
        assert!(ctx.module_path.is_none());
    }

    #[test]
    fn out_format_context_with_values() {
        let ctx = OutFormatContext {
            remote_host: Some("server.example.com".to_string()),
            remote_address: Some("192.168.1.1".to_string()),
            module_name: Some("backup".to_string()),
            module_path: Some("/var/backup".to_string()),
        };
        assert_eq!(ctx.remote_host.as_deref(), Some("server.example.com"));
        assert_eq!(ctx.remote_address.as_deref(), Some("192.168.1.1"));
        assert_eq!(ctx.module_name.as_deref(), Some("backup"));
        assert_eq!(ctx.module_path.as_deref(), Some("/var/backup"));
    }

    #[test]
    fn out_format_clone() {
        let format = OutFormat::new(vec![OutFormatToken::Literal("x".to_string())]);
        let cloned = format;
        assert!(!cloned.is_empty());
    }

    #[test]
    fn placeholder_format_clone() {
        let format =
            PlaceholderFormat::new(Some(5), PlaceholderAlignment::Left, HumanizeMode::Separator);
        let cloned = format;
        assert_eq!(cloned.width(), Some(5));
    }

    #[test]
    fn placeholder_alignment_eq() {
        assert_eq!(PlaceholderAlignment::Left, PlaceholderAlignment::Left);
        assert_eq!(PlaceholderAlignment::Right, PlaceholderAlignment::Right);
        assert_ne!(PlaceholderAlignment::Left, PlaceholderAlignment::Right);
    }

    #[test]
    fn humanize_mode_eq() {
        assert_eq!(HumanizeMode::None, HumanizeMode::None);
        assert_eq!(HumanizeMode::Separator, HumanizeMode::Separator);
        assert_eq!(HumanizeMode::DecimalUnits, HumanizeMode::DecimalUnits);
        assert_eq!(HumanizeMode::BinaryUnits, HumanizeMode::BinaryUnits);
        assert_ne!(HumanizeMode::None, HumanizeMode::Separator);
    }

    #[test]
    fn max_placeholder_width_constant() {
        // Verify the constant is accessible and reasonable
        assert!(MAX_PLACEHOLDER_WIDTH > 0);
        assert!(MAX_PLACEHOLDER_WIDTH >= 4096);
    }
}
