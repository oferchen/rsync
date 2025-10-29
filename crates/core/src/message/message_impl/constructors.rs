use std::borrow::Cow;

use super::{Message, Severity};
use crate::message::strings;

impl Message {
    /// Creates a message with the provided severity and payload.
    ///
    /// Higher layers typically construct diagnostics through the
    /// severity-specific helpers such as [`Message::info`], [`Message::warning`],
    /// or [`Message::error`]. This constructor allows callers to generate
    /// messages dynamically when the severity is only known at runtime—for
    /// example when mapping upstream exit-code tables. The message starts without
    /// an associated exit code or source location so additional context can be
    /// layered on afterwards.
    #[inline]
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn new<T: Into<Cow<'static, str>>>(severity: Severity, text: T) -> Self {
        Self {
            severity,
            code: None,
            text: text.into(),
            role: None,
            source: None,
        }
    }

    /// Creates an informational message.
    #[inline]
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn info<T: Into<Cow<'static, str>>>(text: T) -> Self {
        Self::new(Severity::Info, text)
    }

    /// Creates a warning message.
    #[inline]
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn warning<T: Into<Cow<'static, str>>>(text: T) -> Self {
        Self::new(Severity::Warning, text)
    }

    /// Creates an error message with the provided exit code.
    #[inline]
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn error<T: Into<Cow<'static, str>>>(code: i32, text: T) -> Self {
        Self::new(Severity::Error, text).with_code(code)
    }

    /// Constructs the canonical message for a known rsync exit code.
    #[doc(alias = "rerr_names")]
    #[must_use]
    pub fn from_exit_code(code: i32) -> Option<Self> {
        strings::exit_code_message(code).map(|template| {
            Self::new(template.severity(), template.text()).with_code(template.code())
        })
    }
}
