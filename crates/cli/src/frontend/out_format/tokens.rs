#![deny(unsafe_code)]

//! Token definitions backing `--out-format` parsing and rendering.

use core::client::StrongChecksumAlgorithm;

/// Maximum width accepted for placeholder formatting.
pub(super) const MAX_PLACEHOLDER_WIDTH: usize = 4096;

/// Parsed representation of an `--out-format` specification.
#[derive(Clone, Debug)]
pub(crate) struct OutFormat {
    tokens: Vec<OutFormatToken>,
}

impl OutFormat {
    /// Constructs a new [`OutFormat`] from parsed tokens.
    pub(super) const fn new(tokens: Vec<OutFormatToken>) -> Self {
        Self { tokens }
    }

    /// Returns an iterator over the parsed tokens.
    pub(super) fn tokens(&self) -> impl Iterator<Item = &OutFormatToken> {
        self.tokens.iter()
    }

    /// Returns `true` when no tokens were parsed from the format string.
    #[cfg(test)]
    pub(crate) const fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// A single element of a parsed `--out-format` specification.
#[derive(Clone, Debug)]
pub(super) enum OutFormatToken {
    /// Literal text copied to the output verbatim.
    Literal(String),
    /// A `%`-placeholder resolved at render time.
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
    /// Whether the local side is the sender (push transfer).
    ///
    /// Controls the direction indicator in itemized output:
    /// `<` for sender (push), `>` for receiver (pull).
    /// (upstream: log.c:704 - uses `<` when am_sender && !am_server)
    pub(super) is_sender: bool,
    /// Whether `INFO_GTE(NAME, 2)` is in effect (i.e. `-vv` or
    /// `--info=name2`).
    ///
    /// Upstream `generator.c:582-583` keeps emitting itemize lines for
    /// entries whose `iflags == 0` when this is set, so unchanged dirs,
    /// files, and symlinks still surface as all-dot rows. The render path
    /// uses this to bypass the empty-change-set suppression that mirrors
    /// the default upstream gate.
    pub(super) emit_unchanged: bool,
    /// Whether `-ii` (the `-i` flag repeated) is in effect, i.e. upstream
    /// `stdout_format_has_i > 1`.
    ///
    /// Upstream `generator.c:582-583` ORs `stdout_format_has_i > 1` into the
    /// itemize emit gate as a term separate from `INFO_GTE(NAME, 2)`. Two
    /// `-i` flags therefore surface unchanged (`iflags == 0`) entries as
    /// all-dot rows even without `-vv`. The render path uses this to bypass
    /// the empty-change-set suppression independently of `emit_unchanged`.
    pub(super) itemize_repeated: bool,
    /// `--8-bit-output` / `-8`: when true, high-bit characters pass through
    /// without octal escaping. Only control characters below 0x20 (except
    /// tab) are escaped. Matches upstream `allow_8bit_chars`.
    pub(super) eight_bit_output: bool,
    /// `--links` / `-l` (also set by `-a`): whether symbolic links are
    /// preserved.
    ///
    /// Controls whether `--list-only` output appends ` -> <target>` to a
    /// symlink row. Upstream `generator.c:1183` only sets the arrow when
    /// `preserve_links && S_ISLNK(f->mode)`; without `-l` the symlink is
    /// still listed (with its target-length size) but no target string.
    pub(super) preserve_links: bool,
    /// Negotiated strong-checksum algorithm applied to the `%C` placeholder.
    ///
    /// Upstream `log.c:687-690` renders `%C` from the negotiated checksum
    /// (`file_sum_nni` under `--checksum`, otherwise `xfer_sum_nni`). `None`
    /// means the default negotiated algorithm (auto, i.e. xxh128 for a modern
    /// protocol-31+ transfer - `checksum.c` `negotiate_the_strings`).
    pub(super) full_checksum_algorithm: Option<StrongChecksumAlgorithm>,
    /// `--checksum` / `-c` (upstream `always_checksum`).
    ///
    /// Upstream `log.c:687-688` renders `%C` from the file-list checksum
    /// (`F_SUM`) for every regular file when this is set; otherwise `%C` is
    /// only populated for a transferred file (`ITEM_TRANSFER`).
    pub(super) always_checksum: bool,
}

impl OutFormatContext {
    /// Builds a context with the supplied sender flag, leaving other fields default.
    ///
    /// Used by the transfer driver to thread the sender role from the parsed
    /// `ClientConfig` through to the itemize renderer so the direction arrow
    /// matches upstream `log.c:701-704`.
    #[must_use]
    pub(crate) fn with_is_sender(is_sender: bool) -> Self {
        Self {
            is_sender,
            ..Self::default()
        }
    }

    /// Sets the `INFO_GTE(NAME, 2)` flag (`-vv` / `--info=name2`).
    ///
    /// Upstream `generator.c:582-583` ORs `INFO_GTE(NAME, 2)` into the
    /// itemize emit gate; mirroring that semantic locally requires the
    /// renderer to skip the "no change set, no creation" suppression so
    /// unchanged entries still print as all-dot rows.
    #[must_use]
    pub(crate) fn with_emit_unchanged(mut self, emit_unchanged: bool) -> Self {
        self.emit_unchanged = emit_unchanged;
        self
    }

    /// Returns whether the renderer should bypass empty-change-set
    /// suppression to mirror upstream `INFO_GTE(NAME, 2)` semantics.
    #[must_use]
    pub(crate) const fn emit_unchanged(&self) -> bool {
        self.emit_unchanged
    }

    /// Sets the `-ii` flag (`stdout_format_has_i > 1`).
    ///
    /// Upstream `generator.c:582-583` ORs `stdout_format_has_i > 1` into the
    /// itemize emit gate; mirroring it locally makes the renderer skip the
    /// "no change set, no creation" suppression for `-ii` even without `-vv`.
    #[must_use]
    pub(crate) fn with_itemize_repeated(mut self, itemize_repeated: bool) -> Self {
        self.itemize_repeated = itemize_repeated;
        self
    }

    /// Returns whether the renderer should bypass empty-change-set
    /// suppression to mirror upstream `stdout_format_has_i > 1` (`-ii`).
    #[must_use]
    pub(crate) const fn itemize_repeated(&self) -> bool {
        self.itemize_repeated
    }

    /// Sets the `--8-bit-output` flag for filename escaping in the
    /// out-format renderer.
    #[must_use]
    pub(crate) fn with_eight_bit_output(mut self, eight_bit_output: bool) -> Self {
        self.eight_bit_output = eight_bit_output;
        self
    }

    /// Sets the `--links` / `-l` (preserve-symlinks) flag.
    ///
    /// upstream: generator.c:1183 - the ` -> <target>` arrow in `--list-only`
    /// output is gated on `preserve_links`.
    #[must_use]
    pub(crate) fn with_preserve_links(mut self, preserve_links: bool) -> Self {
        self.preserve_links = preserve_links;
        self
    }

    /// Returns whether symbolic links are preserved (`-l` / `-a`), which the
    /// `--list-only` renderer uses to decide whether to append the symlink
    /// target arrow.
    #[must_use]
    pub(crate) const fn preserve_links(&self) -> bool {
        self.preserve_links
    }

    /// Sets the negotiated `%C` checksum algorithm and the `--checksum` flag.
    ///
    /// upstream: log.c:687-690 - `%C` renders the negotiated file/transfer
    /// checksum, not a hardcoded MD5. The algorithm selects the digest length
    /// and byte order (`util2.c:sum_as_hex`), and `always_checksum` decides
    /// whether an untransferred regular file still shows its file-list sum.
    #[must_use]
    pub(crate) fn with_full_checksum(
        mut self,
        algorithm: StrongChecksumAlgorithm,
        always_checksum: bool,
    ) -> Self {
        self.full_checksum_algorithm = Some(algorithm);
        self.always_checksum = always_checksum;
        self
    }

    /// Returns the negotiated `%C` checksum algorithm, resolving the default to
    /// [`Auto`](StrongChecksumAlgorithm::Auto) (xxh128 for a modern transfer).
    #[must_use]
    pub(super) const fn full_checksum_algorithm(&self) -> StrongChecksumAlgorithm {
        match self.full_checksum_algorithm {
            Some(algorithm) => algorithm,
            None => StrongChecksumAlgorithm::Auto,
        }
    }

    /// Returns whether `--checksum` (`always_checksum`) is in effect.
    #[must_use]
    pub(super) const fn always_checksum(&self) -> bool {
        self.always_checksum
    }
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
        let tokens = vec![OutFormatToken::Literal("hello".to_owned())];
        let format = OutFormat::new(tokens);
        assert!(!format.is_empty());
    }

    #[test]
    fn out_format_tokens_iterator() {
        let tokens = vec![
            OutFormatToken::Literal("a".to_owned()),
            OutFormatToken::Literal("b".to_owned()),
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
        let _ = token.kind;
        let _ = token.format;
    }

    #[test]
    fn out_format_token_literal() {
        let token = OutFormatToken::Literal("test".to_owned());
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
            remote_host: Some("server.example.com".to_owned()),
            remote_address: Some("192.168.1.1".to_owned()),
            module_name: Some("backup".to_owned()),
            module_path: Some("/var/backup".to_owned()),
            is_sender: false,
            emit_unchanged: false,
            itemize_repeated: false,
            eight_bit_output: false,
            preserve_links: false,
            full_checksum_algorithm: None,
            always_checksum: false,
        };
        assert_eq!(ctx.remote_host.as_deref(), Some("server.example.com"));
        assert_eq!(ctx.remote_address.as_deref(), Some("192.168.1.1"));
        assert_eq!(ctx.module_name.as_deref(), Some("backup"));
        assert_eq!(ctx.module_path.as_deref(), Some("/var/backup"));
    }

    #[test]
    fn out_format_clone() {
        let format = OutFormat::new(vec![OutFormatToken::Literal("x".to_owned())]);
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
        assert!(MAX_PLACEHOLDER_WIDTH > 0);
        assert!(MAX_PLACEHOLDER_WIDTH >= 4096);
    }
}
