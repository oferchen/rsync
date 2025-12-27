use std::borrow::Cow;

use super::{Message, Severity};
use crate::{branding::Brand, message::strings};

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
            brand: Brand::Upstream,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_message_with_severity_and_text() {
        let msg = Message::new(Severity::Info, "test message");
        assert!(msg.is_info());
        assert_eq!(msg.text(), "test message");
    }

    #[test]
    fn new_without_code() {
        let msg = Message::new(Severity::Error, "error");
        assert_eq!(msg.code(), None);
    }

    #[test]
    fn new_without_role() {
        let msg = Message::new(Severity::Warning, "warn");
        assert_eq!(msg.role(), None);
    }

    #[test]
    fn new_without_source() {
        let msg = Message::new(Severity::Info, "info");
        assert!(msg.source().is_none());
    }

    #[test]
    fn info_creates_info_severity() {
        let msg = Message::info("informational");
        assert!(msg.is_info());
        assert_eq!(msg.severity(), Severity::Info);
    }

    #[test]
    fn warning_creates_warning_severity() {
        let msg = Message::warning("warning text");
        assert!(msg.is_warning());
        assert_eq!(msg.severity(), Severity::Warning);
    }

    #[test]
    fn error_creates_error_severity_with_code() {
        let msg = Message::error(23, "error text");
        assert!(msg.is_error());
        assert_eq!(msg.severity(), Severity::Error);
        assert_eq!(msg.code(), Some(23));
    }

    #[test]
    fn from_exit_code_known_code() {
        let msg = Message::from_exit_code(1);
        assert!(msg.is_some());
    }

    #[test]
    fn from_exit_code_unknown_code() {
        let msg = Message::from_exit_code(999);
        assert!(msg.is_none());
    }

    #[test]
    fn new_with_owned_string() {
        let text = String::from("owned string");
        let msg = Message::new(Severity::Info, text);
        assert_eq!(msg.text(), "owned string");
    }

    #[test]
    fn new_with_static_str() {
        let msg = Message::new(Severity::Info, "static string");
        assert_eq!(msg.text(), "static string");
    }

    #[test]
    fn new_default_brand_is_upstream() {
        let msg = Message::new(Severity::Info, "test");
        assert_eq!(msg.brand(), Brand::Upstream);
    }
}
