use std::borrow::Cow;

use super::{Message, Role, Severity, SourceLocation};

impl Message {
    /// Returns the message severity.
    #[inline]
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    /// Returns `true` when the message severity is [`Severity::Info`].
    #[inline]
    #[must_use]
    pub const fn is_info(&self) -> bool {
        self.severity.is_info()
    }

    /// Returns `true` when the message severity is [`Severity::Warning`].
    #[inline]
    #[must_use]
    pub const fn is_warning(&self) -> bool {
        self.severity.is_warning()
    }

    /// Returns `true` when the message severity is [`Severity::Error`].
    #[inline]
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.severity.is_error()
    }

    /// Returns the exit code associated with the message if present.
    #[inline]
    #[must_use]
    pub const fn code(&self) -> Option<i32> {
        self.code
    }

    /// Returns the message payload text.
    #[inline]
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Exposes borrowed views over the message components without consuming `self`.
    #[inline]
    #[must_use]
    pub fn parts(
        &self,
    ) -> (
        Severity,
        Option<i32>,
        &str,
        Option<Role>,
        Option<&SourceLocation>,
    ) {
        (
            self.severity,
            self.code,
            self.text.as_ref(),
            self.role,
            self.source.as_ref(),
        )
    }

    /// Consumes the message and returns owned components.
    #[inline]
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Severity,
        Option<i32>,
        Cow<'static, str>,
        Option<Role>,
        Option<SourceLocation>,
    ) {
        (self.severity, self.code, self.text, self.role, self.source)
    }

    /// Returns the role used in the trailer, if any.
    #[inline]
    #[must_use]
    pub const fn role(&self) -> Option<Role> {
        self.role
    }

    /// Returns the recorded source location, if any.
    #[inline]
    #[must_use]
    pub fn source(&self) -> Option<&SourceLocation> {
        self.source.as_ref()
    }
}
