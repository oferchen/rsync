use std::borrow::Cow;

use super::{Message, Role, Severity, SourceLocation};

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
}
