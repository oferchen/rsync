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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_returns_message_severity() {
        let msg = Message::info("test");
        assert_eq!(msg.severity(), Severity::Info);
    }

    #[test]
    fn is_info_true_for_info_message() {
        let msg = Message::info("info message");
        assert!(msg.is_info());
    }

    #[test]
    fn is_info_false_for_warning() {
        let msg = Message::warning("warning");
        assert!(!msg.is_info());
    }

    #[test]
    fn is_info_false_for_error() {
        let msg = Message::error(1, "error");
        assert!(!msg.is_info());
    }

    #[test]
    fn is_warning_true_for_warning_message() {
        let msg = Message::warning("warning");
        assert!(msg.is_warning());
    }

    #[test]
    fn is_warning_false_for_info() {
        let msg = Message::info("info");
        assert!(!msg.is_warning());
    }

    #[test]
    fn is_warning_false_for_error() {
        let msg = Message::error(1, "error");
        assert!(!msg.is_warning());
    }

    #[test]
    fn is_error_true_for_error_message() {
        let msg = Message::error(1, "error");
        assert!(msg.is_error());
    }

    #[test]
    fn is_error_false_for_info() {
        let msg = Message::info("info");
        assert!(!msg.is_error());
    }

    #[test]
    fn is_error_false_for_warning() {
        let msg = Message::warning("warning");
        assert!(!msg.is_error());
    }

    #[test]
    fn code_returns_none_for_info() {
        let msg = Message::info("no code");
        assert_eq!(msg.code(), None);
    }

    #[test]
    fn code_returns_some_for_error() {
        let msg = Message::error(23, "with code");
        assert_eq!(msg.code(), Some(23));
    }

    #[test]
    fn text_returns_message_text() {
        let msg = Message::info("the text");
        assert_eq!(msg.text(), "the text");
    }

    #[test]
    fn parts_returns_all_components() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), 10);
        let msg = Message::error(42, "error text")
            .with_role(Role::Sender)
            .with_source(source);

        let (severity, code, text, role, src) = msg.parts();
        assert_eq!(severity, Severity::Error);
        assert_eq!(code, Some(42));
        assert_eq!(text, "error text");
        assert_eq!(role, Some(Role::Sender));
        assert!(src.is_some());
    }

    #[test]
    fn into_parts_consumes_and_returns_owned() {
        let msg = Message::info("owned text");
        let (severity, code, text, role, source) = msg.into_parts();
        assert_eq!(severity, Severity::Info);
        assert_eq!(code, None);
        assert_eq!(text.as_ref(), "owned text");
        assert_eq!(role, None);
        assert!(source.is_none());
    }

    #[test]
    fn role_returns_none_by_default() {
        let msg = Message::info("test");
        assert_eq!(msg.role(), None);
    }

    #[test]
    fn role_returns_some_when_set() {
        let msg = Message::info("test").with_role(Role::Receiver);
        assert_eq!(msg.role(), Some(Role::Receiver));
    }

    #[test]
    fn source_returns_none_by_default() {
        let msg = Message::info("test");
        assert!(msg.source().is_none());
    }

    #[test]
    fn source_returns_some_when_set() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), 42);
        let msg = Message::info("test").with_source(source);
        let retrieved = msg.source().unwrap();
        assert_eq!(retrieved.line(), 42);
    }
}
