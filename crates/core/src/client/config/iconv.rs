use protocol::iconv::{
    FilenameConverter, IconvRole, converter_from_locale, trace_peer_charset,
};
use thiserror::Error;

/// Describes the requested iconv charset conversion behaviour.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub enum IconvSetting {
    /// No explicit iconv request was provided.
    #[default]
    Unspecified,
    /// Charset conversion is explicitly disabled (`--no-iconv` or `--iconv=-`).
    Disabled,
    /// Charset conversion should use the locale defaults (`--iconv=.`).
    LocaleDefault,
    /// Charset conversion should use the provided local charset and optional remote charset.
    Explicit {
        /// Charset used for local filenames.
        local: String,
        /// Optional charset requested for the remote side; defaults to locale when absent.
        remote: Option<String>,
    },
}

impl IconvSetting {
    /// Parses an iconv specification as accepted by `--iconv`.
    pub fn parse(spec: &str) -> Result<Self, IconvParseError> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(IconvParseError::EmptySpecification);
        }

        if trimmed == "-" {
            return Ok(Self::Disabled);
        }

        if trimmed == "." {
            return Ok(Self::LocaleDefault);
        }

        let mut parts = trimmed.splitn(2, ',');
        let local = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(IconvParseError::MissingLocalCharset)?
            .to_owned();

        match parts.next() {
            Some(remainder) => {
                let remote = remainder.trim();
                if remote.is_empty() {
                    return Err(IconvParseError::MissingRemoteCharset);
                }
                Ok(Self::Explicit {
                    local,
                    remote: Some(remote.to_owned()),
                })
            }
            None => Ok(Self::Explicit {
                local,
                remote: None,
            }),
        }
    }

    /// Returns whether the setting explicitly disables iconv conversion.
    #[must_use]
    pub const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Returns whether the setting was not explicitly configured.
    #[must_use]
    pub const fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    /// Returns the CLI value that should be forwarded to downstream invocations.
    pub fn cli_value(&self) -> Option<String> {
        match self {
            Self::Unspecified | Self::Disabled => None,
            Self::LocaleDefault => Some(".".to_owned()),
            Self::Explicit { local, remote } => {
                if let Some(remote) = remote {
                    Some(format!("{local},{remote}"))
                } else {
                    Some(local.clone())
                }
            }
        }
    }

    /// Resolves the CLI-side iconv setting into a transfer-side
    /// [`FilenameConverter`].
    ///
    /// This is the bridge from the parsed user request
    /// ([`IconvSetting`] in `core::client::config`) to the in-process
    /// converter consumed by the transfer crate's file-list reader and
    /// writer hooks. Without this bridge, `--iconv` parses, validates,
    /// and is forwarded to the remote peer over SSH, but the local
    /// process silently passes raw bytes through file-list ingest,
    /// file-list emit, and filter matching.
    ///
    /// Mapping:
    ///
    /// - [`IconvSetting::Unspecified`] / [`IconvSetting::Disabled`]
    ///   resolve to `None`. The receiver and generator skip the iconv
    ///   hook and operate on raw bytes, matching upstream rsync's
    ///   behaviour when `--iconv` is absent or `--no-iconv` is supplied.
    /// - [`IconvSetting::LocaleDefault`] (`--iconv=.`) resolves to
    ///   [`converter_from_locale`], mirroring upstream
    ///   `options.c::recv_iconv_settings` which uses the locale's
    ///   `nl_langinfo(CODESET)` for the local side and UTF-8 for the
    ///   remote side.
    /// - [`IconvSetting::Explicit`] resolves via
    ///   [`FilenameConverter::new`] with the local charset paired against
    ///   the literal wire charset `"UTF-8"`. Upstream rsync always uses
    ///   UTF-8 on the wire (`rsync.c:130-140` opens
    ///   `ic_send = iconv_open(UTF8_CHARSET, charset)` and
    ///   `ic_recv = iconv_open(charset, UTF8_CHARSET)` where `charset` is
    ///   each peer's local charset); the post-comma half of
    ///   `--iconv=LOCAL,REMOTE` describes the *peer's* local charset and
    ///   is forwarded to the remote CLI by the invocation builder, not
    ///   used for transcoding on this side. When the local charset is not
    ///   recognised by `encoding_rs`, this returns `None` and emits a
    ///   `tracing::warn!` (gated on the `tracing` feature) so the
    ///   transfer falls back to verbatim bytes rather than aborting in
    ///   the middle of a file list. The CLI parser already validates
    ///   that the spec is non-empty, so reaching this fallback
    ///   indicates an unsupported encoding label.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:87-140` `setup_iconv()` - wire is always UTF-8.
    /// - `flist.c::iconv_for_local` (file-list path entry transcode)
    /// - `options.c::recv_iconv_settings` (parse `--iconv=LOCAL,REMOTE`)
    /// - `compat.c:716-718` (gates `CF_SYMLINK_ICONV` advertisement on
    ///   whether iconv is configured)
    pub fn resolve_converter(&self) -> Option<FilenameConverter> {
        match self {
            Self::Unspecified | Self::Disabled => None,
            Self::LocaleDefault => {
                // upstream: rsync.c:142-145 - the level-1 emission fires
                // after the iconv contexts open. For --iconv=. the local
                // charset is locale-default, rendered as "[LOCALE]" via
                // None.
                trace_peer_charset(IconvRole::Client, None);
                Some(converter_from_locale())
            }
            Self::Explicit { local, .. } => {
                // upstream: rsync.c:130-140 - wire is always UTF-8; only the
                // local-side charset matters for this peer's transcoding. The
                // remote half of --iconv=LOCAL,REMOTE is forwarded to the
                // remote CLI by remote::invocation::builder so the peer opens
                // its own iconv context.
                match FilenameConverter::new(local, "UTF-8") {
                    Ok(converter) => {
                        // upstream: rsync.c:142-145 - `[%s] charset: %s`
                        // emits once after both ic_send/ic_recv succeed.
                        trace_peer_charset(IconvRole::Client, Some(local));
                        Some(converter)
                    }
                    Err(_error) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            local = %local,
                            error = %_error,
                            "--iconv: unsupported local charset; filenames will not be transcoded locally",
                        );
                        None
                    }
                }
            }
        }
    }
}

/// Errors raised while parsing an iconv specification.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum IconvParseError {
    /// The specification was empty.
    #[error("iconv specification must not be empty")]
    EmptySpecification,
    /// The local charset was omitted before the comma.
    #[error("iconv specification is missing a local charset")]
    MissingLocalCharset,
    /// The remote charset component after the comma was empty.
    #[error("iconv specification is missing a remote charset")]
    MissingRemoteCharset,
}

#[cfg(test)]
mod tests {
    use super::{IconvParseError, IconvSetting};

    #[test]
    fn parse_locale_default() {
        let setting = IconvSetting::parse(".").expect("parse");
        assert_eq!(setting, IconvSetting::LocaleDefault);
        assert_eq!(setting.cli_value().as_deref(), Some("."));
    }

    #[test]
    fn parse_disabled() {
        let setting = IconvSetting::parse("-").expect("parse");
        assert!(setting.is_disabled());
        assert!(setting.cli_value().is_none());
    }

    #[test]
    fn parse_explicit_pair() {
        let setting = IconvSetting::parse("utf8,iso88591").expect("parse");
        assert_eq!(
            setting,
            IconvSetting::Explicit {
                local: "utf8".to_owned(),
                remote: Some("iso88591".to_owned()),
            }
        );
        assert_eq!(setting.cli_value().as_deref(), Some("utf8,iso88591"));
    }

    #[test]
    fn parse_single_charset() {
        let setting = IconvSetting::parse("utf8").expect("parse");
        assert_eq!(
            setting,
            IconvSetting::Explicit {
                local: "utf8".to_owned(),
                remote: None,
            }
        );
        assert_eq!(setting.cli_value().as_deref(), Some("utf8"));
    }

    #[test]
    fn parse_trims_whitespace() {
        let setting = IconvSetting::parse("  utf8 ,  iso88591  ").expect("parse");
        assert_eq!(
            setting,
            IconvSetting::Explicit {
                local: "utf8".to_owned(),
                remote: Some("iso88591".to_owned()),
            }
        );
    }

    #[test]
    fn parse_errors_for_missing_local_charset() {
        let error = IconvSetting::parse(",remote").expect_err("reject");
        assert_eq!(error, IconvParseError::MissingLocalCharset);
    }

    #[test]
    fn parse_errors_for_missing_remote_charset() {
        let error = IconvSetting::parse("utf8,").expect_err("reject");
        assert_eq!(error, IconvParseError::MissingRemoteCharset);
    }

    #[test]
    fn parse_errors_for_empty_value() {
        let error = IconvSetting::parse("").expect_err("reject");
        assert_eq!(error, IconvParseError::EmptySpecification);
    }

    #[test]
    fn resolve_converter_unspecified_is_none() {
        assert!(IconvSetting::Unspecified.resolve_converter().is_none());
    }

    #[test]
    fn resolve_converter_disabled_is_none() {
        assert!(IconvSetting::Disabled.resolve_converter().is_none());
    }

    #[test]
    fn resolve_converter_locale_default_is_some() {
        let converter = IconvSetting::LocaleDefault
            .resolve_converter()
            .expect("locale-default should produce a converter");
        // Locale-default maps to UTF-8/UTF-8 on most modern systems and
        // is therefore an identity converter; the only contract here is
        // that we produce *some* converter (rather than None).
        assert!(converter.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_converter_explicit_pair_local_utf8_is_identity() {
        // upstream: rsync.c:130-140 - wire is always UTF-8. When the local
        // charset matches the wire charset, no transcoding is needed on
        // this peer (the remote half of LOCAL,REMOTE is the *peer's* local
        // charset, forwarded to the remote CLI separately).
        let setting = IconvSetting::Explicit {
            local: "UTF-8".to_owned(),
            remote: Some("ISO-8859-1".to_owned()),
        };
        let converter = setting
            .resolve_converter()
            .expect("explicit pair should resolve to a converter");
        assert!(
            converter.is_identity(),
            "local=UTF-8 against UTF-8 wire is identity"
        );
        assert_eq!(converter.local_encoding_name(), "UTF-8");
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_converter_explicit_pair_non_utf8_local_transcodes() {
        // Local Latin-1 against the UTF-8 wire requires real transcoding.
        let setting = IconvSetting::Explicit {
            local: "ISO-8859-1".to_owned(),
            remote: Some("UTF-8".to_owned()),
        };
        let converter = setting
            .resolve_converter()
            .expect("explicit pair should resolve to a converter");
        assert!(!converter.is_identity());
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_converter_explicit_single_uses_local_charset() {
        // A single charset means "local only"; the wire is always UTF-8
        // (upstream rsync.c:130-140), so a Latin-1 local charset still
        // requires transcoding to and from the UTF-8 wire.
        let setting = IconvSetting::Explicit {
            local: "ISO-8859-1".to_owned(),
            remote: None,
        };
        let converter = setting
            .resolve_converter()
            .expect("single charset should resolve to a converter");
        assert!(!converter.is_identity());
        assert_eq!(converter.remote_encoding_name(), "UTF-8");
    }

    #[test]
    fn resolve_converter_malformed_local_falls_back_to_none() {
        let setting = IconvSetting::Explicit {
            local: "definitely-not-a-real-charset".to_owned(),
            remote: Some("UTF-8".to_owned()),
        };
        assert!(setting.resolve_converter().is_none());
    }

    mod debug_iconv_emissions {
        //! Pinning tests for `--debug=ICONV,1` producer wiring at
        //! converter-construction time. The emission shape and gating
        //! tests live in `protocol::iconv::trace`; this module verifies
        //! that `IconvSetting::resolve_converter` invokes the producer
        //! once per successful converter.

        use super::IconvSetting;
        use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

        fn init_iconv(level: u8) {
            let mut cfg = VerbosityConfig::default();
            cfg.debug.iconv = level;
            init(cfg);
            let _ = drain_events();
        }

        fn iconv_messages() -> Vec<String> {
            drain_events()
                .into_iter()
                .filter_map(|event| match event {
                    DiagnosticEvent::Debug {
                        flag: DebugFlag::Iconv,
                        message,
                        ..
                    } => Some(message),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn locale_default_emits_locale_placeholder() {
            // upstream: rsync.c:142-145 - `--iconv=.` renders charset as
            // "[LOCALE]" because *charset evaluates to NUL after
            // setup_iconv resolves the default.
            init_iconv(1);
            let _ = IconvSetting::LocaleDefault.resolve_converter();
            let m = iconv_messages();
            assert!(
                m.iter().any(|s| s == "[client] charset: [LOCALE]"),
                "missing locale-default emission: {m:?}"
            );
        }

        #[cfg(feature = "iconv")]
        #[test]
        fn explicit_charset_emits_charset_name() {
            // upstream: rsync.c:142-145 - explicit `--iconv=LOCAL` (or
            // LOCAL,REMOTE) names the charset directly.
            init_iconv(1);
            let setting = IconvSetting::Explicit {
                local: "ISO-8859-1".to_owned(),
                remote: Some("UTF-8".to_owned()),
            };
            let _ = setting.resolve_converter();
            let m = iconv_messages();
            assert!(
                m.iter().any(|s| s == "[client] charset: ISO-8859-1"),
                "missing explicit-charset emission: {m:?}"
            );
        }

        #[test]
        fn unspecified_and_disabled_do_not_emit() {
            // upstream: setup_iconv early-returns when iconv_opt is unset
            // (rsync.c:115-116); no DEBUG_GTE(ICONV, 1) fires.
            init_iconv(2);
            let _ = IconvSetting::Unspecified.resolve_converter();
            let _ = IconvSetting::Disabled.resolve_converter();
            assert!(
                iconv_messages().is_empty(),
                "unspecified/disabled must not emit"
            );
        }

        #[test]
        fn level0_suppresses_resolve_converter_emission() {
            // upstream: DEBUG_GTE(ICONV, 1) is false when --debug=ICONV is off.
            init_iconv(0);
            let _ = IconvSetting::LocaleDefault.resolve_converter();
            assert!(
                iconv_messages().is_empty(),
                "level-0 must gate the resolve_converter emission"
            );
        }

        #[test]
        fn malformed_charset_does_not_emit() {
            // upstream: rsync.c:130-134 - iconv_open failure aborts via
            // exit_cleanup before the DEBUG_GTE(ICONV, 1) emission. We
            // mirror the same ordering by skipping the trace when
            // FilenameConverter::new errors out.
            init_iconv(1);
            let setting = IconvSetting::Explicit {
                local: "definitely-not-a-real-charset".to_owned(),
                remote: Some("UTF-8".to_owned()),
            };
            assert!(setting.resolve_converter().is_none());
            assert!(
                iconv_messages().is_empty(),
                "failed conversion must not emit the success-path level-1 line"
            );
        }
    }
}
