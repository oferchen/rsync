//! Iconv charset specification parsing for `--iconv` and `--no-iconv` arguments.

use std::ffi::OsStr;

use core::{
    client::{IconvParseError, IconvSetting},
    message::{Message, Role},
    rsync_error,
};

/// Resolves the iconv setting from `--iconv` and `--no-iconv` arguments.
///
/// - If `spec` is `Some`, parses the charset specification.
/// - If `disable` is `true` and no spec is given, returns `Disabled`.
/// - Otherwise returns `Unspecified`.
///
/// When the `iconv` cargo feature is disabled and the user supplies an
/// explicit `--iconv=LOCAL,REMOTE` (anything other than `--no-iconv` or
/// absence), this function returns a hard error rather than silently
/// no-opping. Without this guard, the parsed setting would flow through
/// `IconvSetting::resolve_converter` and produce `None`, causing
/// filenames containing non-ASCII bytes to be passed through verbatim
/// despite the user's explicit conversion request.
///
/// # Upstream Reference
///
/// - `options.c::recv_iconv_settings` - parses `--iconv=LOCAL,REMOTE`
/// - `flist.c::iconv_for_local` - applies the converter on the local side
pub(crate) fn resolve_iconv_setting(
    spec: Option<&OsStr>,
    disable: bool,
) -> Result<IconvSetting, Message> {
    if let Some(value) = spec {
        let text = value.to_string_lossy();
        match IconvSetting::parse(text.as_ref()) {
            Ok(setting) => accept_parsed_setting(setting),
            Err(error) => {
                let detail = match error {
                    IconvParseError::EmptySpecification => {
                        "--iconv value must not be empty".to_owned()
                    }
                    IconvParseError::MissingLocalCharset => {
                        "--iconv specification is missing the local charset".to_owned()
                    }
                    IconvParseError::MissingRemoteCharset => {
                        "--iconv specification is missing the remote charset".to_owned()
                    }
                };
                Err(rsync_error!(1, detail).with_role(Role::Client))
            }
        }
    } else if disable {
        Ok(IconvSetting::Disabled)
    } else {
        Ok(IconvSetting::Unspecified)
    }
}

/// Accepts a parsed setting when the `iconv` feature is enabled, after
/// checking that every charset it names can actually be opened.
///
/// Upstream refuses to start when `iconv_open` fails, exiting
/// `RERR_UNSUPPORTED` (4) from `setup_iconv()` (`rsync.c:130-140`). oc used
/// to accept the option, resolve the converter to `None`, and then complete
/// the transfer with filenames untranscoded and nothing on stderr - the same
/// silent passthrough this function's `#[cfg(not(feature = "iconv"))]` twin
/// below already refuses for a build-time-disabled iconv. The two have
/// identical consequences for the operator, so they are treated alike; only
/// the exit code differs, each matching its own upstream site.
#[cfg(feature = "iconv")]
fn accept_parsed_setting(setting: IconvSetting) -> Result<IconvSetting, Message> {
    match setting.validate_charsets() {
        Ok(()) => Ok(setting),
        // upstream: rsync.c:134 - `exit_cleanup(RERR_UNSUPPORTED)`.
        Err(detail) => Err(rsync_error!(4, detail).with_role(Role::Client)),
    }
}

/// Rejects an explicit iconv setting with a hard error when the `iconv`
/// feature was disabled at build time. Closes #1915.
#[cfg(not(feature = "iconv"))]
fn accept_parsed_setting(_setting: IconvSetting) -> Result<IconvSetting, Message> {
    Err(rsync_error!(
        1,
        "--iconv requires the iconv feature, which was disabled at build time".to_owned()
    )
    .with_role(Role::Client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn resolve_iconv_setting_none_not_disabled() {
        let result = resolve_iconv_setting(None, false).unwrap();
        assert_eq!(result, IconvSetting::Unspecified);
    }

    /// An unopenable charset must ABORT with upstream's exit code and text,
    /// never resolve to "no conversion".
    ///
    /// Upstream `setup_iconv()` calls `exit_cleanup(RERR_UNSUPPORTED)` when
    /// `iconv_open` fails (`rsync.c:130-140`) and prints
    /// `iconv_open("UTF-8", "<charset>") failed`. Measured against a built
    /// rsync 3.5.0: `--iconv=NOSUCHCHARSET` gives exit 4 and transfers
    /// nothing, where oc used to give exit 0 having copied every file with
    /// the conversion silently dropped.
    ///
    /// Asserts the exit code AND the message, because on a path whose whole
    /// purpose is telling the operator what went wrong, exit-4-with-other-
    /// wording is a half-fix.
    #[cfg(feature = "iconv")]
    #[test]
    fn unopenable_charset_exits_4_with_upstream_text() {
        let error = resolve_iconv_setting(Some(&os("NOSUCHCHARSET")), false)
            .expect_err("an unopenable charset must not resolve to a setting");

        assert_eq!(
            error.code(),
            Some(4),
            "upstream: rsync.c:134 RERR_UNSUPPORTED"
        );
        assert!(
            error
                .to_string()
                .contains(r#"iconv_open("UTF-8", "NOSUCHCHARSET") failed"#),
            "message must match upstream rsync.c:131-132, got: {error}"
        );
    }

    /// The post-comma half names the peer's charset and is validated too:
    /// upstream reaches the same `exit_cleanup` in the peer process, and a
    /// local copy runs both halves here.
    #[cfg(feature = "iconv")]
    #[test]
    fn unopenable_remote_charset_is_rejected_too() {
        let error = resolve_iconv_setting(Some(&os("UTF-8,NOSUCHCHARSET")), false)
            .expect_err("an unopenable remote charset must not resolve to a setting");

        assert_eq!(error.code(), Some(4));
        assert!(
            error
                .to_string()
                .contains(r#"iconv_open("UTF-8", "NOSUCHCHARSET") failed"#),
            "the failing charset must be named, got: {error}"
        );
    }

    /// Non-vacuity: a charset that CAN be opened still resolves, so the two
    /// assertions above are detecting the rejection rather than a parser
    /// that rejects every explicit spec.
    ///
    /// Deliberately `UTF-8,UTF-8` rather than a legacy name. The charset
    /// NAMESPACE is under review: oc resolves WHATWG labels via
    /// `encoding_rs::Encoding::for_label`, upstream uses POSIX iconv, and
    /// names like `ISO-8859-1` or `ascii` are precisely the ones that work
    /// may change - WHATWG currently folds both onto windows-1252. Pinning
    /// this control to a name under review would make it fail on a CORRECT
    /// namespace fix and read as a regression. UTF-8 resolves identically
    /// in both systems and under every candidate outcome.
    #[cfg(feature = "iconv")]
    #[test]
    fn openable_charset_still_resolves() {
        resolve_iconv_setting(Some(&os("UTF-8,UTF-8")), false)
            .expect("a charset pair that opens must be accepted");
    }

    #[test]
    fn resolve_iconv_setting_none_disabled() {
        let result = resolve_iconv_setting(None, true).unwrap();
        assert_eq!(result, IconvSetting::Disabled);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_iconv_setting_valid_spec() {
        let result = resolve_iconv_setting(Some(&os("UTF-8")), false).unwrap();
        assert_eq!(
            result,
            IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: None,
            }
        );
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_iconv_setting_both_charsets() {
        let result = resolve_iconv_setting(Some(&os("UTF-8,ISO-8859-1")), false).unwrap();
        assert_eq!(
            result,
            IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            }
        );
    }

    #[test]
    fn resolve_iconv_setting_empty() {
        let result = resolve_iconv_setting(Some(&os("")), false);
        assert!(result.is_err());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn resolve_iconv_setting_locale_default() {
        let result = resolve_iconv_setting(Some(&os(".")), false).unwrap();
        assert_eq!(result, IconvSetting::LocaleDefault);
    }

    #[cfg(not(feature = "iconv"))]
    #[test]
    fn resolve_iconv_setting_rejects_explicit_when_feature_off() {
        // Closes #1915 - when the iconv feature is compiled out, an
        // explicit --iconv=LOCAL,REMOTE must produce a hard error rather
        // than silently no-opping.
        let result = resolve_iconv_setting(Some(&os("UTF-8,ISO-8859-1")), false);
        assert!(result.is_err());
    }

    #[cfg(not(feature = "iconv"))]
    #[test]
    fn resolve_iconv_setting_rejects_locale_default_when_feature_off() {
        let result = resolve_iconv_setting(Some(&os(".")), false);
        assert!(result.is_err());
    }

    #[cfg(not(feature = "iconv"))]
    #[test]
    fn resolve_iconv_setting_accepts_no_iconv_when_feature_off() {
        // --no-iconv must always succeed regardless of feature gating.
        let result = resolve_iconv_setting(None, true).unwrap();
        assert_eq!(result, IconvSetting::Disabled);
    }

    #[cfg(not(feature = "iconv"))]
    #[test]
    fn resolve_iconv_setting_accepts_absence_when_feature_off() {
        let result = resolve_iconv_setting(None, false).unwrap();
        assert_eq!(result, IconvSetting::Unspecified);
    }
}
