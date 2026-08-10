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

/// Accepts a parsed setting when the `iconv` feature is enabled.
#[cfg(feature = "iconv")]
fn accept_parsed_setting(setting: IconvSetting) -> Result<IconvSetting, Message> {
    Ok(setting)
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
