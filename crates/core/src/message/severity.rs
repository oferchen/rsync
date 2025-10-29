use std::fmt;
use std::str::FromStr;

/// Severity of a user-visible message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Severity {
    /// Informational message.
    Info,
    /// Warning message.
    Warning,
    /// Error message.
    Error,
}

impl Severity {
    /// Returns the lowercase label used when rendering the severity.
    ///
    /// The strings mirror upstream rsync's diagnostics and therefore feed directly into
    /// the formatting helpers implemented by [`Message`](crate::message::Message). Exposing the label keeps
    /// external crates from duplicating the canonical wording while still allowing
    /// call sites to branch on the textual representation when building structured
    /// logs or integration tests.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert_eq!(Severity::Info.as_str(), "info");
    /// assert_eq!(Severity::Warning.as_str(), "warning");
    /// assert_eq!(Severity::Error.as_str(), "error");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Returns the canonical prefix rendered at the start of diagnostics.
    ///
    /// The string mirrors upstream rsync's output, combining the constant
    /// `"rsync"` banner with the lowercase severity label and trailing
    /// colon. Centralising the prefix ensures
    /// [`Message::as_segments`](crate::message::Message::as_segments)
    /// doesn't need to assemble the pieces manually, which avoids
    /// additional vectored segments and keeps rendering logic in sync with
    /// upstream expectations.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert_eq!(Severity::Info.prefix(), "rsync info: ");
    /// assert_eq!(Severity::Warning.prefix(), "rsync warning: ");
    /// assert_eq!(Severity::Error.prefix(), "rsync error: ");
    /// ```
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Info => "rsync info: ",
            Self::Warning => "rsync warning: ",
            Self::Error => "rsync error: ",
        }
    }

    /// Reports whether this severity represents an informational message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Info.is_info());
    /// assert!(!Severity::Error.is_info());
    /// ```
    #[must_use]
    pub const fn is_info(self) -> bool {
        matches!(self, Self::Info)
    }

    /// Reports whether this severity represents a warning message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Warning.is_warning());
    /// assert!(!Severity::Info.is_warning());
    /// ```
    #[must_use]
    pub const fn is_warning(self) -> bool {
        matches!(self, Self::Warning)
    }

    /// Reports whether this severity represents an error message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Error.is_error());
    /// assert!(!Severity::Warning.is_error());
    /// ```
    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(self, Self::Error)
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a [`Severity`] from a string fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSeverityError {
    _private: (),
}

impl fmt::Display for ParseSeverityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised rsync message severity")
    }
}

impl std::error::Error for ParseSeverityError {}

impl FromStr for Severity {
    type Err = ParseSeverityError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            _ => Err(ParseSeverityError { _private: () }),
        }
    }
}
