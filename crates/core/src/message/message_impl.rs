use std::borrow::Cow;
use std::fmt::{self, Write as FmtWrite};
use std::io::{self, IoSlice, Write as IoWrite};
use std::str;

use super::{
    MAX_MESSAGE_SEGMENTS, MessageScratch, MessageSegments, Role, Severity, SourceLocation,
    VERSION_SUFFIX,
    numbers::{encode_signed_decimal, encode_unsigned_decimal},
    strings,
};

/// Structured representation of an rsync user-visible message.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::{Message, Role}, message_source};
///
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
///
/// let rendered = message.to_string();
/// assert!(rendered.contains("delta-transfer failure"));
/// assert!(rendered.contains(&format!(
///     "[sender={}]",
///     rsync_core::version::RUST_VERSION
/// )));
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[must_use = "messages must be formatted or emitted to reach users"]
pub struct Message {
    severity: Severity,
    code: Option<i32>,
    text: Cow<'static, str>,
    role: Option<Role>,
    source: Option<SourceLocation>,
}

impl Message {
    /// Returns the vectored representation of the rendered message.
    ///
    /// The helper exposes the same slices used internally when emitting the message into an
    /// [`io::Write`] implementor. Callers that need to integrate with custom buffered pipelines can
    /// reuse the returned segments with [`std::io::Write::write_vectored`],
    /// avoiding redundant allocations or
    /// per-segment formatting logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    /// use std::io::{self, Write};
    ///
    /// struct VecWriter(Vec<u8>);
    ///
    /// impl Write for VecWriter {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.0.extend_from_slice(buf);
    ///         Ok(buf.len())
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         Ok(())
    ///     }
    ///
    ///     fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
    ///         for slice in bufs {
    ///             self.0.extend_from_slice(slice);
    ///         }
    ///         Ok(bufs.iter().map(|slice| slice.len()).sum())
    ///     }
    /// }
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, true);
    ///
    /// let mut writer = VecWriter(Vec::new());
    /// writer.write_vectored(segments.as_slices()).unwrap();
    ///
    /// assert_eq!(writer.0, message.to_line_bytes().unwrap());
    /// ```
    #[must_use]
    pub fn as_segments<'a>(
        &'a self,
        scratch: &'a mut MessageScratch,
        include_newline: bool,
    ) -> MessageSegments<'a> {
        let mut segments: [IoSlice<'a>; MAX_MESSAGE_SEGMENTS] =
            [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS];
        let mut count = 0usize;
        let mut total_len = 0usize;

        let mut push = |slice: &'a [u8]| {
            if slice.is_empty() {
                return;
            }

            debug_assert!(
                count < segments.len(),
                "message segments exceeded allocation"
            );
            segments[count] = IoSlice::new(slice);
            count += 1;
            total_len += slice.len();
        };

        push(self.severity.prefix().as_bytes());
        push(self.text.as_bytes());

        if let Some(code) = self.code {
            push(b" (code ");
            let digits = encode_signed_decimal(i64::from(code), &mut scratch.code_digits);
            push(digits.as_bytes());
            push(b")");
        }

        if let Some(source) = &self.source {
            push(b" at ");
            push(source.path().as_bytes());
            push(b":");
            let digits =
                encode_unsigned_decimal(u64::from(source.line()), &mut scratch.line_digits);
            push(digits.as_bytes());
        }

        if let Some(role) = self.role {
            push(b" [");
            push(role.as_str().as_bytes());
            push(b"=");
            push(VERSION_SUFFIX.as_bytes());
            push(b"]");
        }

        if include_newline {
            push(b"\n");
        }

        MessageSegments {
            segments,
            count,
            total_len,
        }
    }

    /// Invokes the provided closure with the vectored representation of the message.
    ///
    /// The helper borrows the internal thread-local [`MessageScratch`] and renders the message
    /// exactly once before handing the [`MessageSegments`] view to the supplied closure. This keeps
    /// call sites lightweight when they only need transient access to the slices—for example when
    /// forwarding diagnostics to a sink that expects [`IoSlice`] values. The closure must not store
    /// the provided reference because it borrows scratch space owned by the thread-local buffer.
    ///
    /// # Examples
    ///
    /// Write the rendered message using vectored I/O without manually managing scratch buffers.
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, Role},
    ///     message_source,
    /// };
    /// use std::io::{self, IoSlice, Write};
    ///
    /// struct Collector(Vec<u8>);
    ///
    /// impl Write for Collector {
    ///     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    ///         self.0.extend_from_slice(buf);
    ///         Ok(buf.len())
    ///     }
    ///
    ///     fn flush(&mut self) -> io::Result<()> {
    ///         Ok(())
    ///     }
    ///
    ///     fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
    ///         let mut total = 0;
    ///         for slice in bufs {
    ///             self.0.extend_from_slice(slice);
    ///             total += slice.len();
    ///         }
    ///         Ok(total)
    ///     }
    /// }
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut collector = Collector(Vec::new());
    ///
    /// message.with_segments(true, |segments| {
    ///     collector.write_vectored(segments.as_ref()).unwrap();
    /// });
    ///
    /// assert_eq!(collector.0, message.to_line_bytes().unwrap());
    /// ```
    pub fn with_segments<R>(
        &self,
        include_newline: bool,
        f: impl FnOnce(&MessageSegments<'_>) -> R,
    ) -> R {
        MessageScratch::with_thread_local(|scratch| {
            let segments = self.as_segments(scratch, include_newline);
            f(&segments)
        })
    }

    /// Returns the number of bytes in the rendered message without a trailing newline.
    ///
    /// The helper renders the message into thread-local scratch space and reports the total
    /// length through [`MessageSegments::len`]. This allows call sites to reserve precise buffer
    /// capacity before invoking [`Self::append_to_vec`] or [`Self::render_to_writer`], matching
    /// upstream rsync's allocation discipline.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let mut buffer = Vec::with_capacity(message.byte_len());
    /// let appended = message.append_to_vec(&mut buffer).unwrap();
    ///
    /// assert_eq!(buffer.len(), message.byte_len());
    /// assert_eq!(appended, message.byte_len());
    /// ```
    #[inline]
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.with_segments(false, |segments| segments.len())
    }

    /// Returns the number of bytes in the rendered message including the trailing newline.
    ///
    /// The helper mirrors [`Self::byte_len`] but counts the additional byte appended by
    /// [`Self::append_line_to_vec`] and [`Self::render_line_to_writer`]. Consumers that batch
    /// diagnostics into shared buffers can therefore pre-reserve exactly enough capacity to avoid
    /// incremental reallocations.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let message = Message::warning("vanished").with_code(24);
    /// let expected = message.to_line_bytes().unwrap();
    ///
    /// assert_eq!(message.line_byte_len(), expected.len());
    /// ```
    #[inline]
    #[must_use]
    pub fn line_byte_len(&self) -> usize {
        self.with_segments(true, |segments| segments.len())
    }

    /// Creates a message with the provided severity and payload.
    ///
    /// Higher layers typically construct diagnostics through the severity-specific helpers such as
    /// [`Message::info`], [`Message::warning`], or [`Message::error`]. This constructor allows callers to
    /// generate messages dynamically when the severity is only known at runtime—for example when
    /// mapping upstream exit-code tables. The message starts without an associated exit code or
    /// source location so additional context can be layered on afterwards.
    ///
    /// # Examples
    ///
    /// Build a warning message and attach an exit code once additional context becomes available.
    ///
    /// ```
    /// use rsync_core::message::{Message, Severity};
    ///
    /// let message = Message::new(Severity::Warning, "some files vanished").with_code(24);
    ///
    /// assert_eq!(message.severity(), Severity::Warning);
    /// assert_eq!(message.code(), Some(24));
    /// assert_eq!(message.text(), "some files vanished");
    /// ```
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
    ///
    /// The helper consults [`strings::exit_code_message`] to reproduce the severity and
    /// wording that upstream rsync associates with well-known exit codes. When the table
    /// contains an entry the returned [`Message`] already includes the `(code N)` suffix,
    /// leaving callers to optionally attach roles or source locations before emitting the
    /// diagnostic. Unknown codes yield `None`, allowing higher layers to surface bespoke
    /// explanations when necessary.
    ///
    /// # Examples
    ///
    /// Look up exit code 23 and render the canonical error message:
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let message = Message::from_exit_code(23).expect("code 23 is defined by upstream");
    /// assert!(message.is_error());
    /// assert_eq!(message.code(), Some(23));
    /// assert_eq!(
    ///     message.text(),
    ///     "some files/attrs were not transferred (see previous errors)"
    /// );
    /// ```
    ///
    /// Exit code 24 is downgraded to a warning by upstream rsync:
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let message = Message::from_exit_code(24).expect("code 24 is defined by upstream");
    /// assert!(message.is_warning());
    /// assert!(message
    ///     .to_string()
    ///     .starts_with("rsync warning: some files vanished before they could be transferred"));
    /// ```
    #[doc(alias = "rerr_names")]
    #[must_use]
    pub fn from_exit_code(code: i32) -> Option<Self> {
        strings::exit_code_message(code).map(|template| {
            Self::new(template.severity(), template.text()).with_code(template.code())
        })
    }

    /// Returns the message severity.
    #[inline]
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    /// Returns `true` when the message severity is [`Severity::Info`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let info = Message::info("probe");
    /// assert!(info.is_info());
    /// assert!(!info.is_warning());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_info(&self) -> bool {
        self.severity.is_info()
    }

    /// Returns `true` when the message severity is [`Severity::Warning`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let warning = Message::warning("vanished");
    /// assert!(warning.is_warning());
    /// assert!(!warning.is_error());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_warning(&self) -> bool {
        self.severity.is_warning()
    }

    /// Returns `true` when the message severity is [`Severity::Error`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let error = Message::error(11, "io");
    /// assert!(error.is_error());
    /// assert!(!error.is_info());
    /// ```
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
    ///
    /// The returned tuple provides copies of the scalar fields together with
    /// references to the textual payload and optional source location. This is
    /// useful when call sites need to inspect or branch on the contents of a
    /// [`Message`] while retaining ownership so it can still be emitted. The
    /// helper avoids cloning the payload: the returned string slice borrows the
    /// existing [`Cow`] storage in place.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role, Severity}, message_source};
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let (severity, code, text, role, source) = message.parts();
    ///
    /// assert_eq!(severity, Severity::Error);
    /// assert_eq!(code, Some(23));
    /// assert_eq!(text, "delta-transfer failure");
    /// assert_eq!(role, Some(Role::Sender));
    /// assert!(source.is_some());
    ///
    /// // The original message is still available for emission.
    /// assert!(message.to_string().contains("rsync error:"));
    /// ```
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
    ///
    /// Unlike [`parts`](Self::parts), this helper transfers ownership of the
    /// textual payload and optional [`SourceLocation`], making it convenient to
    /// persist diagnostics or feed them into structured logging sinks that take
    /// ownership. The returned [`Cow`] retains its original borrowing mode, so
    /// zero-copy payloads remain borrowed unless the caller subsequently needs a
    /// mutable string.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role, Severity}, message_source};
    ///
    /// let message = Message::warning("vanished files detected")
    ///     .with_code(24)
    ///     .with_role(Role::Receiver)
    ///     .with_source(message_source!());
    /// let (severity, code, text, role, source) = message.into_parts();
    ///
    /// assert_eq!(severity, Severity::Warning);
    /// assert_eq!(code, Some(24));
    /// assert_eq!(text.as_ref(), "vanished files detected");
    /// assert_eq!(role, Some(Role::Receiver));
    /// assert!(source.is_some());
    /// ```
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

    /// Replaces the message payload with the provided text.
    ///
    /// The helper keeps the message's severity, exit code, role, and source location untouched,
    /// mirroring upstream rsync's habit of enriching diagnostics with additional wording as more
    /// context becomes available. Accepting any type that converts into a [`Cow<'static, str>`]
    /// keeps allocations to a minimum when callers promote string literals or reuse existing
    /// buffers.
    ///
    /// # Examples
    ///
    /// Update the payload on a cloned message without disturbing its metadata:
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, Role},
    ///     message_source,
    /// };
    ///
    /// let original = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let updated = original.clone().with_text("retry scheduled for delta-transfer");
    ///
    /// assert_eq!(updated.text(), "retry scheduled for delta-transfer");
    /// assert_eq!(updated.code(), Some(23));
    /// assert_eq!(updated.role(), Some(Role::Sender));
    /// assert_eq!(updated.source(), original.source());
    /// ```
    #[inline]
    #[must_use = "the updated message must be emitted to observe the new text"]
    pub fn with_text<T: Into<Cow<'static, str>>>(mut self, text: T) -> Self {
        self.text = text.into();
        self
    }

    /// Adjusts the message severity while keeping all other metadata intact.
    ///
    /// The helper mirrors upstream rsync's practice of reclassifying diagnostics without
    /// rebuilding them from scratch. It is particularly handy when cloning message templates that
    /// default to `error` but need to be downgraded to `warning` or `info` depending on runtime
    /// conditions. The exit code, role trailer, source location, and payload remain unchanged so
    /// the caller only needs to emit the returned [`Message`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::{Message, Severity};
    ///
    /// let template = Message::error(23, "delta-transfer failure");
    /// let downgraded = template.clone().with_severity(Severity::Warning);
    ///
    /// assert_eq!(downgraded.severity(), Severity::Warning);
    /// assert_eq!(downgraded.code(), template.code());
    /// assert_eq!(downgraded.text(), template.text());
    /// ```
    #[inline]
    #[must_use = "the updated message must be emitted to observe the new severity"]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
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

    /// Attaches a role trailer to the message.
    #[inline]
    #[must_use = "the updated message must be emitted to retain the attached role"]
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Removes any role trailer from the message.
    ///
    /// This helper is useful when higher layers clone a templated [`Message`] and need to emit the
    /// diagnostic without associating it with a specific sender/receiver role. Clearing the role
    /// mirrors upstream behaviour where certain warnings are rendered without a trailer even if the
    /// original template attached one.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::{Message, Role};
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .without_role();
    ///
    /// assert!(message.role().is_none());
    /// let rendered = message.to_string();
    /// assert!(!rendered.contains("[sender="));
    /// ```
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

    /// Removes any source location metadata from the message.
    ///
    /// Messages cloned from templates sometimes need to suppress the originating Rust file when
    /// relayed to users—for example when reproducing upstream diagnostics that omit source
    /// locations. Calling this helper clears the recorded [`SourceLocation`] while leaving the
    /// payload and severity untouched.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::Message, message_source};
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_source(message_source!())
    ///     .without_source();
    ///
    /// assert!(message.source().is_none());
    /// let rendered = message.to_string();
    /// assert!(!rendered.contains(" at "));
    /// ```
    #[inline]
    #[must_use = "the updated message must be emitted to observe the cleared source"]
    pub fn without_source(mut self) -> Self {
        self.source = None;
        self
    }

    /// Overrides the exit code associated with the message.
    ///
    /// The helper is primarily used by warning templates that mirror upstream rsync's
    /// behaviour of emitting `(code N)` even for warning severities (for example when files
    /// vanish on the sender side). It can also adjust informational messages when higher
    /// layers need to bubble up a numeric status that differs from the default.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let rendered = Message::warning("some files vanished before transfer")
    ///     .with_code(24)
    ///     .to_string();
    ///
    /// assert!(rendered.contains("rsync warning:"));
    /// assert!(rendered.contains("(code 24)"));
    /// ```
    #[must_use = "the updated message must be emitted to retain the attached code"]
    #[inline]
    pub fn with_code(mut self, code: i32) -> Self {
        self.code = Some(code);
        self
    }

    /// Removes any exit code annotation from the message.
    ///
    /// This mirrors upstream rsync's behaviour where informational diagnostics often omit `(code
    /// N)` even if a template initially provided one. Clearing the code is cheaper than rebuilding
    /// the [`Message`] from scratch when only the numeric suffix needs to be stripped.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let message = Message::error(23, "delta-transfer failure").without_code();
    ///
    /// assert!(message.code().is_none());
    /// let rendered = message.to_string();
    /// assert!(!rendered.contains("(code"));
    /// ```
    #[inline]
    #[must_use = "the updated message must be emitted to observe the cleared code"]
    pub fn without_code(mut self) -> Self {
        self.code = None;
        self
    }

    /// Renders the message into an arbitrary [`fmt::Write`] implementation.
    ///
    /// This helper mirrors the [`Display`](fmt::Display) representation while
    /// allowing callers to avoid allocating intermediate [`String`] values.
    /// Higher layers can stream messages directly into log buffers or I/O
    /// adaptors without cloning the payload.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let mut rendered = String::new();
    /// Message::error(12, "example")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!())
    ///     .render_to(&mut rendered)
    ///     .unwrap();
    ///
    /// assert!(rendered.contains("rsync error: example (code 12)"));
    /// assert!(rendered.contains(&format!(
    ///     "[sender={}]",
    ///     rsync_core::version::RUST_VERSION
    /// )));
    /// ```
    #[inline]
    #[must_use = "formatter writes can fail; propagate errors to preserve upstream diagnostics"]
    pub fn render_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        MessageScratch::with_thread_local(|scratch| self.render_to_with_scratch(scratch, writer))
    }

    /// Renders the message followed by a newline into an arbitrary [`fmt::Write`] implementor.
    ///
    /// This helper mirrors [`Self::render_line_to_writer`] but operates on string-based writers.
    /// It avoids cloning intermediate [`String`] values by streaming the payload directly into the
    /// provided formatter, making it convenient for unit tests and diagnostic buffers that operate
    /// on UTF-8 text rather than byte streams.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let mut rendered = String::new();
    /// Message::error(42, "permission denied")
    ///     .with_role(Role::Generator)
    ///     .with_source(message_source!())
    ///     .render_line_to(&mut rendered)
    ///     .unwrap();
    ///
    /// assert!(rendered.ends_with('\n'));
    /// assert!(rendered.contains(&format!(
    ///     "[generator={}]",
    ///     rsync_core::version::RUST_VERSION
    /// )));
    /// ```
    #[inline]
    #[must_use = "newline rendering can fail; handle formatting errors to retain diagnostics"]
    pub fn render_line_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        MessageScratch::with_thread_local(|scratch| {
            self.render_line_to_with_scratch(scratch, writer)
        })
    }

    /// Returns the rendered message as a [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::render_to_writer`] but collects the output into an owned
    /// buffer. It pre-allocates exactly enough capacity for the rendered message, avoiding
    /// repeated reallocations even when exit codes, source locations, and role trailers are
    /// attached.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let bytes = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!())
    ///     .to_bytes()
    ///     .expect("Vec<u8> writes are infallible");
    ///
    /// assert!(bytes.starts_with(b"rsync error:"));
    /// let trailer = format!("[sender={}]", rsync_core::version::RUST_VERSION);
    /// assert!(bytes.ends_with(trailer.as_bytes()));
    /// ```
    #[inline]
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        MessageScratch::with_thread_local(|scratch| self.to_bytes_with_scratch(scratch))
    }

    /// Returns the rendered message followed by a newline as a [`Vec<u8>`].
    ///
    /// This convenience API mirrors [`Self::render_line_to_writer`] and is primarily intended for
    /// tests and logging adapters that prefer to work with owned byte buffers.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let rendered = Message::warning("some files vanished")
    ///     .with_code(24)
    ///     .to_line_bytes()
    ///     .expect("Vec<u8> writes are infallible");
    ///
    /// assert!(rendered.ends_with(b"\n"));
    /// ```
    #[inline]
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_line_bytes(&self) -> io::Result<Vec<u8>> {
        MessageScratch::with_thread_local(|scratch| self.to_line_bytes_with_scratch(scratch))
    }

    /// Writes the rendered message into an [`io::Write`] implementor.
    ///
    /// This helper mirrors [`Self::render_to`] but operates on byte writers. It
    /// avoids allocating intermediate [`String`] values by streaming the
    /// constituent byte slices directly into the provided writer. Implementors
    /// that advertise vectored-write support receive the full message in a
    /// single [`write_vectored`](IoWrite::write_vectored) call; others fall back
    /// to sequential [`write_all`](IoWrite::write_all) operations. Any
    /// encountered I/O error is propagated unchanged, ensuring callers can
    /// surface the original failure context in user-facing diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let mut output = Vec::new();
    /// Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!())
    ///     .render_to_writer(&mut output)
    ///     .unwrap();
    ///
    /// let rendered = String::from_utf8(output).unwrap();
    /// assert!(rendered.contains("rsync error: delta-transfer failure (code 23)"));
    /// assert!(rendered.contains(&format!(
    ///     "[sender={}]",
    ///     rsync_core::version::RUST_VERSION
    /// )));
    /// ```
    #[inline]
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        MessageScratch::with_thread_local(|scratch| {
            self.render_to_writer_with_scratch(scratch, writer)
        })
    }

    /// Writes the rendered message followed by a newline into an [`io::Write`] implementor.
    ///
    /// This helper mirrors the behaviour of upstream rsync, which emits a newline terminator for
    /// user-visible diagnostics. Callers that need to stream multiple messages into the same
    /// output can therefore avoid handling line termination manually.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let mut output = Vec::new();
    /// Message::error(30, "timeout in data send")
    ///     .with_role(Role::Receiver)
    ///     .with_source(message_source!())
    ///     .render_line_to_writer(&mut output)
    ///     .unwrap();
    ///
    /// let rendered = String::from_utf8(output).unwrap();
    /// assert!(rendered.ends_with('\n'));
    /// assert!(rendered.contains(&format!(
    ///     "[receiver={}]",
    ///     rsync_core::version::RUST_VERSION
    /// )));
    /// ```
    #[inline]
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_line_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        MessageScratch::with_thread_local(|scratch| {
            self.render_line_to_writer_with_scratch(scratch, writer)
        })
    }

    /// Appends the rendered message into the provided byte buffer.
    ///
    /// The helper mirrors [`Self::render_to_writer`] but avoids the overhead of
    /// constructing a temporary [`Vec`] or going through the
    /// [`std::io::Write`] trait for
    /// `Vec<u8>`. Callers that batch multiple diagnostics can therefore reuse
    /// an output buffer across invocations without repeated allocations or
    /// trait-object dispatch. The buffer is grown exactly enough to hold the
    /// rendered message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, Role}, message_source};
    ///
    /// let mut buffer = Vec::new();
    /// let appended = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!())
    ///     .append_to_vec(&mut buffer)?;
    ///
    /// assert!(buffer.starts_with(b"rsync error:"));
    /// let trailer = format!("[sender={}]", rsync_core::version::RUST_VERSION);
    /// assert!(buffer.ends_with(trailer.as_bytes()));
    /// assert_eq!(appended, buffer.len());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        MessageScratch::with_thread_local(|scratch| {
            self.append_to_vec_with_scratch(scratch, buffer)
        })
    }

    /// Appends the rendered message followed by a newline into the provided buffer.
    ///
    /// This convenience function mirrors [`Self::render_line_to_writer`] while
    /// avoiding intermediate allocations. It is particularly useful for
    /// snapshot-style tests or logging sinks that keep diagnostics in an
    /// in-memory byte buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    ///
    /// let mut buffer = Vec::new();
    /// let appended = Message::warning("vanished file")
    ///     .with_code(24)
    ///     .append_line_to_vec(&mut buffer)?;
    ///
    /// assert!(buffer.ends_with(b"\n"));
    /// assert_eq!(appended, buffer.len());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_line_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        MessageScratch::with_thread_local(|scratch| {
            self.append_line_to_vec_with_scratch(scratch, buffer)
        })
    }

    /// Streams the rendered message into an [`io::Write`] implementor using caller-provided scratch buffers.
    ///
    /// This variant avoids reinitialising [`MessageScratch`] storage for callers that need to emit
    /// a high volume of diagnostics. Reusing the buffer removes the repeated zeroing performed by
    /// [`MessageScratch::new`], matching upstream rsync's strategy of recycling stack-allocated
    /// arrays when formatting error messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{message::{Message, MessageScratch}, rsync_error};
    ///
    /// let mut scratch = MessageScratch::new();
    /// let mut output = Vec::new();
    ///
    /// for code in [11, 12] {
    ///     rsync_error!(code, "i/o failure")
    ///         .render_to_writer_with_scratch(&mut scratch, &mut output)
    ///         .unwrap();
    /// }
    ///
    /// let rendered = String::from_utf8(output).unwrap();
    /// assert!(rendered.contains("rsync error: i/o failure (code 11)"));
    /// assert!(rendered.contains("rsync error: i/o failure (code 12)"));
    /// ```
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_to_writer_with_scratch<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> io::Result<()> {
        self.render_to_writer_inner(scratch, writer, false)
    }

    /// Writes the rendered message followed by a newline while reusing caller-provided scratch buffers.
    ///
    /// The helper mirrors [`Self::render_line_to_writer`] but avoids repeated allocation or
    /// zero-initialisation by accepting an existing [`MessageScratch`]. The newline terminator is
    /// appended after the main message segments using the same buffer.
    #[must_use = "rsync diagnostics must report I/O failures when streaming to writers"]
    pub fn render_line_to_writer_with_scratch<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> io::Result<()> {
        self.render_to_writer_inner(scratch, writer, true)
    }

    /// Appends the rendered message into the provided buffer while reusing caller-supplied scratch space.
    ///
    /// The helper mirrors [`Self::append_to_vec`] but avoids reinitialising the
    /// thread-local scratch storage when the caller already maintains a
    /// reusable [`MessageScratch`]. The buffer is extended in place using the
    /// vectored slices emitted by [`MessageSegments`].
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<usize> {
        self.append_to_vec_with_scratch_inner(scratch, buffer, false)
    }

    /// Appends the rendered message followed by a newline into the provided buffer while reusing scratch space.
    #[must_use = "buffer growth can fail; handle allocation or I/O errors when appending diagnostics"]
    pub fn append_line_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<usize> {
        self.append_to_vec_with_scratch_inner(scratch, buffer, true)
    }

    fn render_to_writer_inner<W: IoWrite>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
        include_newline: bool,
    ) -> io::Result<()> {
        let segments = self.as_segments(scratch, include_newline);
        segments.write_to(writer)
    }

    fn to_bytes_with_scratch_inner(
        &self,
        scratch: &mut MessageScratch,
        include_newline: bool,
    ) -> io::Result<Vec<u8>> {
        let segments = self.as_segments(scratch, include_newline);
        let mut buffer = Vec::new();
        let _ = segments.extend_vec(&mut buffer)?;
        Ok(buffer)
    }

    fn append_to_vec_with_scratch_inner(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
        include_newline: bool,
    ) -> io::Result<usize> {
        let segments = self.as_segments(scratch, include_newline);
        segments.extend_vec(buffer)
    }
}

impl Message {
    /// Renders the message into an arbitrary [`fmt::Write`] implementor while reusing scratch buffers.
    ///
    /// The helper mirrors [`Self::render_to`] but accepts an explicit [`MessageScratch`], allowing
    /// callers that emit multiple diagnostics to amortise the buffer initialisation cost.
    #[must_use = "formatter writes can fail; propagate errors to preserve upstream diagnostics"]
    pub fn render_to_with_scratch<W: fmt::Write>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> fmt::Result {
        struct Adapter<'a, W>(&'a mut W);

        impl<W: fmt::Write> IoWrite for Adapter<'_, W> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let text = str::from_utf8(buf)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

                self.0
                    .write_str(text)
                    .map_err(|_| io::Error::other("formatter error"))?;

                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                let mut written = 0usize;

                for buf in bufs {
                    self.write(buf.as_ref())?;
                    written += buf.len();
                }

                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }

            fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
                self.0
                    .write_fmt(fmt)
                    .map_err(|_| io::Error::other("formatter error"))
            }
        }

        let mut adapter = Adapter(writer);
        self.render_to_writer_inner(scratch, &mut adapter, false)
            .map_err(|_| fmt::Error)
    }

    /// Renders the message followed by a newline into an [`fmt::Write`] implementor while reusing scratch buffers.
    #[must_use = "newline rendering can fail; handle formatting errors to retain diagnostics"]
    pub fn render_line_to_with_scratch<W: fmt::Write>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> fmt::Result {
        self.render_to_with_scratch(scratch, writer)?;
        FmtWrite::write_char(writer, '\n')
    }

    /// Collects the rendered message into a [`Vec<u8>`] while reusing caller-provided scratch buffers.
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, false)
    }

    /// Collects the rendered message and a trailing newline into a [`Vec<u8>`] while reusing scratch buffers.
    #[must_use = "collecting rendered bytes allocates; handle potential I/O or allocation failures"]
    pub fn to_line_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, true)
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render_to(f)
    }
}
