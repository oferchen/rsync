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
            .ok_or(IconvParseError::MissingLocalCharset)?.to_owned();

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
    #[must_use]
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
}
