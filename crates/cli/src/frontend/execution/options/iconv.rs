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
pub(crate) fn resolve_iconv_setting(
    spec: Option<&OsStr>,
    disable: bool,
) -> Result<IconvSetting, Message> {
    if let Some(value) = spec {
        let text = value.to_string_lossy();
        match IconvSetting::parse(text.as_ref()) {
            Ok(setting) => Ok(setting),
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

    #[test]
    fn resolve_iconv_setting_locale_default() {
        let result = resolve_iconv_setting(Some(&os(".")), false).unwrap();
        assert_eq!(result, IconvSetting::LocaleDefault);
    }
}
