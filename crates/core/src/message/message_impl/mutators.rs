use std::borrow::Cow;

use super::{Message, Role, Severity, SourceLocation};
use crate::branding::Brand;

impl Message {
    /// Replaces the message payload with the provided text.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the new text"]
    pub fn with_text<T: Into<Cow<'static, str>>>(mut self, text: T) -> Self {
        self.text = text.into();
        self
    }

    /// Adjusts the message severity while keeping all other metadata intact.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the new severity"]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Attaches a role trailer to the message.
    #[inline]
    #[must_use = "the updated message must be emitted to retain the attached role"]
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Removes any role trailer from the message.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the cleared role"]
    pub fn without_role(mut self) -> Self {
        self.role = None;
        self
    }

    /// Attaches a source location to the message.
    #[inline]
    #[must_use = "the updated message must be emitted to retain the attached source"]
    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = Some(source);
        self
    }

    /// Removes any source location from the message.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the cleared source"]
    pub fn without_source(mut self) -> Self {
        self.source = None;
        self
    }

    /// Overrides the exit code associated with the message.
    #[inline]
    #[must_use = "the updated message must be emitted to retain the attached code"]
    pub fn with_code(mut self, code: i32) -> Self {
        self.code = Some(code);
        self
    }

    /// Removes any exit code annotation from the message.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the cleared code"]
    pub fn without_code(mut self) -> Self {
        self.code = None;
        self
    }

    /// Applies a brand to the message, adjusting rendered prefixes accordingly.
    #[inline]
    #[must_use = "the updated message must be emitted to observe the applied brand"]
    pub fn with_brand(mut self, brand: Brand) -> Self {
        self.brand = brand;
        self
    }

    /// Returns the brand associated with the message.
    #[inline]
    #[must_use]
    pub const fn brand(&self) -> Brand {
        self.brand
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_text_replaces_text() {
        let msg = Message::info("original").with_text("replaced");
        assert_eq!(msg.text(), "replaced");
    }

    #[test]
    fn with_text_owned_string() {
        let text = String::from("owned");
        let msg = Message::info("original").with_text(text);
        assert_eq!(msg.text(), "owned");
    }

    #[test]
    fn with_severity_changes_severity() {
        let msg = Message::info("test").with_severity(Severity::Error);
        assert!(msg.is_error());
    }

    #[test]
    fn with_severity_preserves_text() {
        let msg = Message::info("preserved").with_severity(Severity::Warning);
        assert_eq!(msg.text(), "preserved");
    }

    #[test]
    fn with_role_attaches_role() {
        let msg = Message::info("test").with_role(Role::Sender);
        assert_eq!(msg.role(), Some(Role::Sender));
    }

    #[test]
    fn with_role_replaces_existing() {
        let msg = Message::info("test")
            .with_role(Role::Sender)
            .with_role(Role::Receiver);
        assert_eq!(msg.role(), Some(Role::Receiver));
    }

    #[test]
    fn without_role_clears_role() {
        let msg = Message::info("test").with_role(Role::Sender).without_role();
        assert_eq!(msg.role(), None);
    }

    #[test]
    fn with_source_attaches_source() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), 42);
        let msg = Message::info("test").with_source(source);
        assert!(msg.source().is_some());
    }

    #[test]
    fn without_source_clears_source() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), 42);
        let msg = Message::info("test").with_source(source).without_source();
        assert!(msg.source().is_none());
    }

    #[test]
    fn with_code_attaches_code() {
        let msg = Message::info("test").with_code(23);
        assert_eq!(msg.code(), Some(23));
    }

    #[test]
    fn with_code_replaces_existing() {
        let msg = Message::error(1, "test").with_code(42);
        assert_eq!(msg.code(), Some(42));
    }

    #[test]
    fn without_code_clears_code() {
        let msg = Message::error(23, "test").without_code();
        assert_eq!(msg.code(), None);
    }

    #[test]
    fn with_brand_changes_brand() {
        let msg = Message::info("test").with_brand(Brand::Oc);
        assert_eq!(msg.brand(), Brand::Oc);
    }

    #[test]
    fn brand_returns_current_brand() {
        let msg = Message::info("test");
        assert_eq!(msg.brand(), Brand::Upstream);
    }

    #[test]
    fn chained_mutations_preserve_all_changes() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), 100);
        let msg = Message::info("original")
            .with_text("updated")
            .with_severity(Severity::Warning)
            .with_role(Role::Sender)
            .with_source(source)
            .with_code(42)
            .with_brand(Brand::Oc);

        assert_eq!(msg.text(), "updated");
        assert!(msg.is_warning());
        assert_eq!(msg.role(), Some(Role::Sender));
        assert!(msg.source().is_some());
        assert_eq!(msg.code(), Some(42));
        assert_eq!(msg.brand(), Brand::Oc);
    }
}
