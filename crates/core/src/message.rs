use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as FmtWrite};
use std::io::{self, IoSlice, Write as IoWrite};
use std::path::Path;
use std::str::{self, FromStr};
use std::sync::OnceLock;

pub mod strings;

/// Version tag appended to message trailers.
pub const VERSION_SUFFIX: &str = "3.4.1-rust";

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
    /// the formatting helpers implemented by [`Message`]. Exposing the label keeps
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

/// Role used in the trailer portion of an rsync message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Role {
    /// Sender role (`[sender=…]`).
    Sender,
    /// Receiver role (`[receiver=…]`).
    Receiver,
    /// Generator role (`[generator=…]`).
    Generator,
    /// Server role (`[server=…]`).
    Server,
    /// Client role (`[client=…]`).
    Client,
    /// Daemon role (`[daemon=…]`).
    Daemon,
}

impl Role {
    /// Returns the lowercase trailer identifier used when formatting messages.
    ///
    /// The returned string matches the suffix rendered by upstream rsync. Keeping the
    /// mapping here allows higher layers to reuse the canonical spelling when
    /// constructing out-of-band logs or telemetry derived from [`Message`] instances.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Role;
    ///
    /// assert_eq!(Role::Sender.as_str(), "sender");
    /// assert_eq!(Role::Receiver.as_str(), "receiver");
    /// assert_eq!(Role::Daemon.as_str(), "daemon");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
            Self::Generator => "generator",
            Self::Server => "server",
            Self::Client => "client",
            Self::Daemon => "daemon",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a [`Role`] from a string fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRoleError {
    _private: (),
}

impl fmt::Display for ParseRoleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unrecognised rsync message role")
    }
}

impl std::error::Error for ParseRoleError {}

impl FromStr for Role {
    type Err = ParseRoleError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "sender" => Ok(Self::Sender),
            "receiver" => Ok(Self::Receiver),
            "generator" => Ok(Self::Generator),
            "server" => Ok(Self::Server),
            "client" => Ok(Self::Client),
            "daemon" => Ok(Self::Daemon),
            _ => Err(ParseRoleError { _private: () }),
        }
    }
}

/// Source location associated with a message.
///
/// # Examples
///
/// ```
/// use rsync_core::message::SourceLocation;
///
/// let location = SourceLocation::from_parts(
///     env!("CARGO_MANIFEST_DIR"),
///     "src/lib.rs",
///     120,
/// );
///
/// assert_eq!(location.line(), 120);
/// assert!(location.path().ends_with("src/lib.rs"));
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SourceLocation {
    path: Cow<'static, str>,
    line: u32,
}

impl SourceLocation {
    /// Creates a source location from workspace paths.
    #[must_use]
    pub fn from_parts(manifest_dir: &'static str, file: &'static str, line: u32) -> Self {
        let manifest_path = Path::new(manifest_dir);
        let file_path = Path::new(file);

        let absolute = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else if let Some(root) = option_env!("RSYNC_WORKSPACE_ROOT") {
            let workspace_path = Path::new(root);
            if let Ok(manifest_relative) = manifest_path.strip_prefix(workspace_path) {
                if manifest_relative.as_os_str().is_empty() {
                    manifest_path.join(file_path)
                } else if file_path.starts_with(manifest_relative) {
                    workspace_path.join(file_path)
                } else {
                    manifest_path.join(file_path)
                }
            } else {
                manifest_path.join(file_path)
            }
        } else {
            manifest_path.join(file_path)
        };

        let normalized = normalize_path(&absolute);
        let repo_relative = strip_workspace_prefix_owned(normalized);

        Self {
            path: Cow::Owned(repo_relative),
            line,
        }
    }

    /// Returns the repo-relative path stored in the source location.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Reports whether the stored path is relative to the workspace root.
    ///
    /// Paths pointing to files within the repository are normalised to a
    /// workspace-relative representation, matching upstream rsync's practice of
    /// omitting redundant prefixes in diagnostics. When the path escapes the
    /// workspace (for example when the caller provides an absolute path outside
    /// the repository), the method returns `false` to signal that the location is
    /// already absolute.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::SourceLocation;
    ///
    /// let inside = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), "src/lib.rs", 12);
    /// assert!(inside.is_workspace_relative());
    ///
    /// let outside = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), "/tmp/outside.rs", 7);
    /// assert!(!outside.is_workspace_relative());
    /// ```
    #[must_use]
    pub fn is_workspace_relative(&self) -> bool {
        let path = Path::new(self.path());
        !path.has_root()
    }

    /// Returns the line number recorded for the message.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }
}

impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.path, self.line)
    }
}

/// Macro helper that captures the current source location.
///
/// This macro expands at the call-site and therefore records the location of the
/// expansion. When the caller is a helper annotated with `#[track_caller]`,
/// consider using [`tracked_message_source!`] to surface the original
/// invocation location instead.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::SourceLocation, message_source};
///
/// let location: SourceLocation = message_source!();
/// assert!(location.path().ends_with(".rs"));
/// assert!(location.line() > 0);
/// ```
#[macro_export]
macro_rules! message_source {
    () => {
        $crate::message::SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), file!(), line!())
    };
}

/// Builds a [`SourceLocation`] from an explicit [`std::panic::Location`].
///
/// This macro is useful when the caller already captured a location through
/// `#[track_caller]` and wishes to convert it into the repo-relative form used
/// by the message subsystem.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::SourceLocation, message_source_from};
///
/// let caller = std::panic::Location::caller();
/// let location: SourceLocation = message_source_from!(caller);
/// assert_eq!(location.line(), caller.line());
/// ```
#[macro_export]
macro_rules! message_source_from {
    ($location:expr) => {{
        let location = $location;
        $crate::message::SourceLocation::from_parts(
            env!("CARGO_MANIFEST_DIR"),
            location.file(),
            location.line(),
        )
    }};
}

/// Captures a [`SourceLocation`] that honours `#[track_caller]` propagation.
///
/// Unlike [`macro@message_source`], this macro calls [`std::panic::Location::caller`]
/// so that helper functions annotated with `#[track_caller]` report the
/// location of their caller rather than their own body.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::SourceLocation, tracked_message_source};
///
/// #[track_caller]
/// fn helper() -> SourceLocation {
///     tracked_message_source!()
/// }
///
/// let location = helper();
/// assert!(location.path().ends_with(".rs"));
/// ```
#[macro_export]
macro_rules! tracked_message_source {
    () => {{ $crate::message_source_from!(::std::panic::Location::caller()) }};
}

/// Constructs an error [`Message`] with the provided exit code and payload.
///
/// The macro mirrors upstream diagnostics by automatically attaching the
/// call-site [`SourceLocation`] using [`macro@message_source`]. It accepts either a
/// string literal/`Cow<'static, str>` payload or a [`format!`] style template.
/// Callers can further customise the returned [`Message`] by chaining helpers
/// such as [`Message::with_role`].
///
/// # Examples
///
/// ```
/// use rsync_core::{message::Role, rsync_error};
///
/// let rendered = rsync_error!(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .to_string();
///
/// assert!(rendered.contains("rsync error: delta-transfer failure (code 23)"));
/// assert!(rendered.contains("[sender=3.4.1-rust]"));
/// ```
///
/// Formatting arguments are forwarded to [`format!`]:
///
/// ```
/// use rsync_core::rsync_error;
///
/// let path = "src/lib.rs";
/// let rendered = rsync_error!(11, "failed to read {path}", path = path).to_string();
///
/// assert!(rendered.contains("failed to read src/lib.rs"));
/// ```
#[macro_export]
macro_rules! rsync_error {
    ($code:expr, $text:expr $(,)?) => {{
        $crate::message::Message::error($code, $text)
            .with_source($crate::message_source!())
    }};
    ($code:expr, $fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::error($code, format!($fmt, $($arg)+))
            .with_source($crate::message_source!())
    }};
}

/// Constructs a warning [`Message`] from the provided payload.
///
/// Like [`macro@rsync_error`], the macro records the call-site source location so
/// diagnostics include repo-relative paths. Additional context, such as exit
/// codes, can be attached using [`Message::with_code`].
///
/// # Examples
///
/// ```
/// use rsync_core::rsync_warning;
///
/// let rendered = rsync_warning!("some files vanished")
///     .with_code(24)
///     .to_string();
///
/// assert!(rendered.starts_with("rsync warning:"));
/// assert!(rendered.contains("(code 24)"));
/// ```
#[macro_export]
macro_rules! rsync_warning {
    ($text:expr $(,)?) => {{
        $crate::message::Message::warning($text)
            .with_source($crate::message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::warning(format!($fmt, $($arg)+))
            .with_source($crate::message_source!())
    }};
}

/// Constructs an informational [`Message`] from the provided payload.
///
/// The macro is a convenience wrapper around [`Message::info`] that automatically
/// attaches the call-site source location. Upstream rsync typically omits source
/// locations for informational output, but retaining the metadata simplifies
/// debugging and keeps the API consistent across severities.
///
/// # Examples
///
/// ```
/// use rsync_core::rsync_info;
///
/// let rendered = rsync_info!("negotiation complete").to_string();
///
/// assert!(rendered.starts_with("rsync info:"));
/// ```
#[macro_export]
macro_rules! rsync_info {
    ($text:expr $(,)?) => {{
        $crate::message::Message::info($text)
            .with_source($crate::message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::info(format!($fmt, $($arg)+))
            .with_source($crate::message_source!())
    }};
}

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
/// assert!(rendered.contains("[sender=3.4.1-rust]"));
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
    /// Creates an informational message.
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn info<T: Into<Cow<'static, str>>>(text: T) -> Self {
        Self {
            severity: Severity::Info,
            code: None,
            text: text.into(),
            role: None,
            source: None,
        }
    }

    /// Creates a warning message.
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn warning<T: Into<Cow<'static, str>>>(text: T) -> Self {
        Self {
            severity: Severity::Warning,
            code: None,
            text: text.into(),
            role: None,
            source: None,
        }
    }

    /// Creates an error message with the provided exit code.
    #[must_use = "constructed messages must be emitted to reach users"]
    pub fn error<T: Into<Cow<'static, str>>>(code: i32, text: T) -> Self {
        Self {
            severity: Severity::Error,
            code: Some(code),
            text: text.into(),
            role: None,
            source: None,
        }
    }

    /// Returns the message severity.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        self.severity
    }

    /// Returns the exit code associated with the message if present.
    #[must_use]
    pub const fn code(&self) -> Option<i32> {
        self.code
    }

    /// Returns the message payload text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the role used in the trailer, if any.
    #[must_use]
    pub const fn role(&self) -> Option<Role> {
        self.role
    }

    /// Returns the recorded source location, if any.
    #[must_use]
    pub fn source(&self) -> Option<&SourceLocation> {
        self.source.as_ref()
    }

    /// Attaches a role trailer to the message.
    #[must_use = "the updated message must be emitted to retain the attached role"]
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Attaches a source location to the message.
    #[must_use = "the updated message must be emitted to retain the attached source"]
    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = Some(source);
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
    pub fn with_code(mut self, code: i32) -> Self {
        self.code = Some(code);
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
    /// assert!(rendered.contains("[sender=3.4.1-rust]"));
    /// ```
    pub fn render_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
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
        self.render_to_writer_inner(&mut adapter, false)
            .map_err(|_| fmt::Error)
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
    /// assert!(rendered.contains("[generator=3.4.1-rust]"));
    /// ```
    pub fn render_line_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        self.render_to(writer)?;
        FmtWrite::write_char(writer, '\n')
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
    /// assert!(rendered.contains("[sender=3.4.1-rust]"));
    /// ```
    pub fn render_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        self.render_to_writer_inner(writer, false)
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
    /// assert!(rendered.contains("[receiver=3.4.1-rust]"));
    /// ```
    pub fn render_line_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        self.render_to_writer_inner(writer, true)
    }

    fn render_to_writer_inner<W: IoWrite>(
        &self,
        writer: &mut W,
        include_newline: bool,
    ) -> io::Result<()> {
        const MAX_SEGMENTS: usize = 18;

        let mut segments: [IoSlice<'_>; MAX_SEGMENTS] = [IoSlice::new(&[]); MAX_SEGMENTS];
        let mut segment_count = 0usize;
        let mut total_len = 0usize;

        macro_rules! push_segment {
            ($expr:expr) => {{
                let slice: &[u8] = $expr;
                segments[segment_count] = IoSlice::new(slice);
                segment_count += 1;
                total_len += slice.len();
            }};
        }

        let mut code_buffer = [0u8; 20];
        let mut line_buffer = [0u8; 20];

        push_segment!(b"rsync ");
        push_segment!(self.severity.as_str().as_bytes());
        push_segment!(b": ");
        push_segment!(self.text.as_bytes());

        if let Some(code) = self.code {
            push_segment!(b" (code ");
            let digits = encode_signed_decimal(i64::from(code), &mut code_buffer);
            push_segment!(digits.as_bytes());
            push_segment!(b")");
        }

        if let Some(source) = &self.source {
            push_segment!(b" at ");
            push_segment!(source.path().as_bytes());
            push_segment!(b":");
            let digits = encode_unsigned_decimal(u64::from(source.line()), &mut line_buffer);
            push_segment!(digits.as_bytes());
        }

        if let Some(role) = self.role {
            push_segment!(b" [");
            push_segment!(role.as_str().as_bytes());
            push_segment!(b"=");
            push_segment!(VERSION_SUFFIX.as_bytes());
            push_segment!(b"]");
        }

        if include_newline {
            push_segment!(b"\n");
        }

        if segment_count > 1 {
            loop {
                match writer.write_vectored(&segments[..segment_count]) {
                    Ok(written) if written == total_len => return Ok(()),
                    Ok(written) => {
                        let mut remaining = written;
                        for slice in &segments[..segment_count] {
                            let data = slice.as_ref();
                            if remaining >= data.len() {
                                remaining -= data.len();
                                continue;
                            }

                            writer.write_all(&data[remaining..])?;
                            remaining = 0;
                        }

                        return Ok(());
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
        }

        for slice in &segments[..segment_count] {
            writer.write_all(slice.as_ref())?;
        }

        Ok(())
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render_to(f)
    }
}

/// Removes the workspace root prefix from a normalized path when possible.
///
/// The input string must already be normalised via [`normalize_path`]. When the path lives outside
/// the workspace root (or the root is unknown), the original string is returned unchanged.
fn strip_workspace_prefix_owned(normalized_path: String) -> String {
    if let Some(root) = normalized_workspace_root()
        && let Some(stripped) = strip_normalized_workspace_prefix(&normalized_path, root)
    {
        return stripped;
    }

    normalized_path
}

/// Returns the workspace-relative representation of `path` when it shares the provided root.
///
/// Both arguments must use forward slashes, matching the representation produced by
/// [`normalize_path`]. The helper enforces segment boundaries to avoid stripping prefixes from
/// directories that merely share the same leading byte sequence.
fn strip_normalized_workspace_prefix(path: &str, root: &str) -> Option<String> {
    if !path.starts_with(root) {
        return None;
    }

    let mut suffix = &path[root.len()..];

    if suffix.is_empty() {
        return Some(String::from("."));
    }

    if !root.ends_with('/') {
        if !suffix.starts_with('/') {
            return None;
        }

        suffix = &suffix[1..];

        if suffix.is_empty() {
            return Some(String::from("."));
        }
    }

    Some(suffix.to_owned())
}

/// Lazily computes the normalized workspace root used for source remapping.
fn normalized_workspace_root() -> Option<&'static str> {
    static NORMALIZED: OnceLock<Option<String>> = OnceLock::new();

    NORMALIZED
        .get_or_init(|| {
            option_env!("RSYNC_WORKSPACE_ROOT").map(|root| normalize_path(Path::new(root)))
        })
        .as_deref()
}

fn normalize_path(path: &Path) -> String {
    use std::path::Component;

    let mut prefix: Option<OsString> = None;
    let is_absolute = path.is_absolute();
    let mut segments: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
            Component::RootDir => {
                // Root components are handled via the `is_absolute` flag to avoid
                // reintroducing platform-specific separators when reconstructing the path.
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if segments.last().is_some_and(|last| last != "..") {
                    segments.pop();
                    continue;
                }

                if !is_absolute {
                    segments.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => segments.push(value.to_os_string()),
        }
    }

    let mut normalized = String::new();

    if let Some(prefix) = prefix {
        append_normalized_os_str(&mut normalized, prefix.as_os_str());
    }

    if is_absolute && !normalized.ends_with('/') {
        normalized.push('/');
    }

    for (index, segment) in segments.iter().enumerate() {
        if !(normalized.is_empty()
            || normalized.ends_with('/')
            || (index == 0 && normalized.ends_with(':')))
        {
            normalized.push('/');
        }

        append_normalized_os_str(&mut normalized, segment);
    }

    if normalized.is_empty() {
        String::from(".")
    } else {
        normalized
    }
}

fn encode_unsigned_decimal(value: u64, buf: &mut [u8]) -> &str {
    let start = encode_unsigned_decimal_into(value, buf);
    str::from_utf8(&buf[start..]).expect("decimal digits are valid ASCII")
}

/// Appends an [`OsStr`] to the destination string while normalising separators.
///
/// Windows paths frequently use backslashes while downstream consumers expect
/// the canonical forward-slash representation that upstream rsync emits. This
/// helper avoids allocating intermediate [`String`] values by copying the
/// decoded [`OsStr`] directly into the target buffer and rewriting any
/// backslashes in-place. The behaviour matches [`Path::to_string_lossy`],
/// ensuring unpaired surrogate pairs or other lossy conversions degrade in the
/// same manner as upstream.
fn append_normalized_os_str(target: &mut String, value: &OsStr) {
    let lossy = value.to_string_lossy();

    if lossy.as_ref().bytes().any(|byte| byte == b'\\') {
        target.extend(lossy.chars().map(|ch| if ch == '\\' { '/' } else { ch }));
    } else {
        target.push_str(lossy.as_ref());
    }
}

fn encode_signed_decimal(value: i64, buf: &mut [u8]) -> &str {
    if value < 0 {
        assert!(
            buf.len() >= 2,
            "buffer must include capacity for a sign and at least one digit"
        );

        let start = encode_unsigned_decimal_into(value.unsigned_abs(), buf);
        assert!(
            start > 0,
            "buffer must retain one byte to prefix the minus sign"
        );

        let sign_index = start - 1;
        buf[sign_index] = b'-';
        str::from_utf8(&buf[sign_index..]).expect("decimal digits are valid ASCII")
    } else {
        encode_unsigned_decimal(value as u64, buf)
    }
}

fn encode_unsigned_decimal_into(mut value: u64, buf: &mut [u8]) -> usize {
    assert!(
        !buf.is_empty(),
        "buffer must have capacity for at least one digit"
    );

    let mut index = buf.len();
    loop {
        assert!(
            index > 0,
            "decimal representation does not fit in the provided buffer"
        );

        index -= 1;
        buf[index] = b'0' + (value % 10) as u8;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rsync_error, rsync_info, rsync_warning};
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::io::{self, IoSlice};
    use std::str::FromStr;

    #[track_caller]
    fn tracked_source() -> SourceLocation {
        tracked_message_source!()
    }

    #[track_caller]
    fn untracked_source() -> SourceLocation {
        message_source!()
    }

    #[test]
    fn formats_error_with_code_role_and_source() {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let formatted = message.to_string();

        assert!(formatted.starts_with("rsync error: delta-transfer failure (code 23) at "));
        assert!(formatted.contains("[sender=3.4.1-rust]"));
        assert!(formatted.contains("src/message.rs"));
    }

    #[test]
    fn formats_warning_without_role_or_source() {
        let message = Message::warning("soft limit reached");
        let formatted = message.to_string();

        assert_eq!(formatted, "rsync warning: soft limit reached");
    }

    #[test]
    fn warnings_with_code_render_code_suffix() {
        let formatted = Message::warning("some files vanished before they could be transferred")
            .with_code(24)
            .to_string();

        assert!(formatted.starts_with("rsync warning: some files vanished"));
        assert!(formatted.contains("(code 24)"));
    }

    #[test]
    fn info_messages_omit_code_suffix() {
        let message = Message::info("protocol handshake complete").with_source(message_source!());
        let formatted = message.to_string();

        assert!(formatted.starts_with("rsync info: protocol handshake complete at "));
        assert!(!formatted.contains("(code"));
    }

    #[test]
    fn source_location_is_repo_relative() {
        let source = message_source!();
        let path = source.path();
        assert_eq!(path, "crates/core/src/message.rs");
        assert!(!path.contains('\\'));
        assert!(source.line() > 0);
        assert!(source.is_workspace_relative());
    }

    #[test]
    fn normalizes_redundant_segments() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let source = SourceLocation::from_parts(manifest_dir, "src/../src/message.rs", 7);
        assert_eq!(source.path(), "crates/core/src/message.rs");
    }

    #[test]
    fn retains_absolute_paths_outside_workspace() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let source = SourceLocation::from_parts(manifest_dir, "/tmp/outside.rs", 42);

        assert!(std::path::Path::new(source.path()).is_absolute());
        assert!(!source.is_workspace_relative());
    }

    #[test]
    fn strips_workspace_prefix_after_normalization() {
        let workspace_root = std::path::Path::new(env!("RSYNC_WORKSPACE_ROOT"));
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        let crate_relative = manifest_dir
            .strip_prefix(workspace_root)
            .expect("manifest directory must live within the workspace root");

        let redundant_root = workspace_root.join("..").join(
            workspace_root
                .file_name()
                .expect("workspace root should have a terminal component"),
        );

        let redundant_path = redundant_root.join(crate_relative).join("src/message.rs");

        let leaked: &'static str = Box::leak(
            redundant_path
                .to_string_lossy()
                .into_owned()
                .into_boxed_str(),
        );

        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 7);
        assert_eq!(source.path(), "crates/core/src/message.rs");
    }

    #[test]
    fn workspace_prefix_match_requires_separator_boundary() {
        let workspace_root = Path::new(env!("RSYNC_WORKSPACE_ROOT"));

        let Some(root_name) = workspace_root.file_name() else {
            // When the workspace lives at the filesystem root (e.g. `/`), every absolute path
            // is a descendant. The existing behaviour already strips the prefix, so there is no
            // partial-prefix scenario to validate.
            return;
        };

        let sibling_name = format!("{}-fork", root_name.to_string_lossy());
        let sibling = workspace_root
            .parent()
            .unwrap_or(workspace_root)
            .join(&sibling_name)
            .join("src/lib.rs");

        let leaked: &'static str =
            Box::leak(sibling.to_string_lossy().into_owned().into_boxed_str());

        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 11);
        let expected = normalize_path(Path::new(leaked));

        assert_eq!(source.path(), expected);
        assert!(Path::new(source.path()).is_absolute());
    }

    #[test]
    fn escaping_workspace_root_renders_absolute_path() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let escape = Path::new("../../../../outside.rs");
        let absolute = manifest_dir.join(escape);

        let leaked: &'static str =
            Box::leak(escape.to_string_lossy().into_owned().into_boxed_str());

        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), leaked, 13);

        assert!(Path::new(source.path()).is_absolute());
        assert_eq!(source.path(), normalize_path(&absolute));
    }

    #[test]
    fn workspace_root_path_is_marked_relative() {
        let source = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), ".", 5);

        assert_eq!(source.path(), "crates/core");
        assert!(source.is_workspace_relative());
    }

    #[test]
    fn normalize_preserves_relative_parent_segments() {
        let normalized = normalize_path(Path::new("../shared/src/lib.rs"));
        assert_eq!(normalized, "../shared/src/lib.rs");
    }

    #[test]
    fn normalize_empty_path_defaults_to_current_dir() {
        let normalized = normalize_path(Path::new(""));
        assert_eq!(normalized, ".");
    }

    #[test]
    fn normalize_windows_drive_paths_standardizes_separators() {
        let normalized = normalize_path(Path::new(r"C:\foo\bar\baz.txt"));
        assert_eq!(normalized, "C:/foo/bar/baz.txt");
    }

    #[test]
    fn normalize_windows_drive_roots_include_trailing_separator() {
        let normalized = normalize_path(Path::new(r"C:\"));
        assert_eq!(normalized, "C:/");
    }

    #[test]
    fn normalize_unc_like_paths_retains_server_share_structure() {
        let normalized = normalize_path(Path::new(r"\\server\share\dir\file"));
        assert_eq!(normalized, "//server/share/dir/file");
    }

    #[test]
    fn message_source_from_accepts_explicit_location() {
        let caller = std::panic::Location::caller();
        let location = message_source_from!(caller);

        assert_eq!(location.line(), caller.line());
        assert!(location.path().ends_with("crates/core/src/message.rs"));
    }

    #[test]
    fn tracked_message_source_propagates_caller_location() {
        let expected_line = line!() + 1;
        let location = tracked_source();

        assert_eq!(location.line(), expected_line);
        assert!(location.path().ends_with("crates/core/src/message.rs"));

        let helper_location = untracked_source();
        assert_ne!(helper_location.line(), expected_line);
        assert_eq!(helper_location.path(), location.path());
    }

    #[test]
    fn message_is_hashable() {
        let mut dedupe = HashSet::new();
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        assert!(dedupe.insert(message.clone()));
        assert!(!dedupe.insert(message));
    }

    #[test]
    fn render_to_matches_display_output() {
        let message = Message::error(35, "timeout in data send")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let mut rendered = String::new();
        message
            .render_to(&mut rendered)
            .expect("rendering into a string never fails");

        assert_eq!(rendered, message.to_string());
    }

    #[test]
    fn render_to_writer_matches_render_to() {
        let message = Message::warning("soft limit reached")
            .with_role(Role::Daemon)
            .with_source(message_source!());

        let mut buffer = Vec::new();
        message
            .render_to_writer(&mut buffer)
            .expect("writing into a vector never fails");

        assert_eq!(buffer, message.to_string().into_bytes());
    }

    #[test]
    fn render_line_to_appends_newline() {
        let message = Message::warning("soft limit reached");

        let mut rendered = String::new();
        message
            .render_line_to(&mut rendered)
            .expect("rendering into a string never fails");

        assert_eq!(rendered, format!("{}\n", message));
    }

    #[test]
    fn render_to_writer_matches_render_to_for_negative_codes() {
        let message = Message::error(-35, "timeout in data send")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let mut buffer = Vec::new();
        message
            .render_to_writer(&mut buffer)
            .expect("writing into a vector never fails");

        assert_eq!(buffer, message.to_string().into_bytes());
    }

    #[test]
    fn render_line_to_writer_appends_newline() {
        let message = Message::info("protocol handshake complete");

        let mut buffer = Vec::new();
        message
            .render_line_to_writer(&mut buffer)
            .expect("writing into a vector never fails");

        assert_eq!(buffer, format!("{}\n", message).into_bytes());
    }

    struct FailingWriter;

    impl io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("sink error"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn render_to_writer_propagates_io_error() {
        let mut writer = FailingWriter;
        let message = Message::info("protocol handshake complete");

        let err = message
            .render_to_writer(&mut writer)
            .expect_err("writer error should propagate");

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "sink error");
    }

    struct NewlineFailingWriter;

    impl io::Write for NewlineFailingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf == b"\n" {
                Err(io::Error::other("newline sink error"))
            } else {
                Ok(buf.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn render_line_to_writer_propagates_newline_error() {
        let mut writer = NewlineFailingWriter;
        let message = Message::warning("soft limit reached");

        let err = message
            .render_line_to_writer(&mut writer)
            .expect_err("newline error should propagate");

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "newline sink error");
    }

    #[derive(Default)]
    struct InterruptingVectoredWriter {
        buffer: Vec<u8>,
        remaining_interrupts: usize,
    }

    impl InterruptingVectoredWriter {
        fn new(interruptions: usize) -> Self {
            Self {
                remaining_interrupts: interruptions,
                ..Self::default()
            }
        }
    }

    impl io::Write for InterruptingVectoredWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            if self.remaining_interrupts > 0 {
                self.remaining_interrupts -= 1;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }

            let mut written = 0usize;
            for slice in bufs {
                self.buffer.extend_from_slice(slice.as_ref());
                written += slice.len();
            }

            Ok(written)
        }
    }

    #[test]
    fn render_to_writer_retries_after_interrupted_vectored_write() {
        let message = Message::info("protocol negotiation complete");
        let mut writer = InterruptingVectoredWriter::new(1);

        message
            .render_to_writer(&mut writer)
            .expect("interrupted writes should be retried");

        assert_eq!(writer.remaining_interrupts, 0);
        assert_eq!(writer.buffer, message.to_string().into_bytes());
    }

    #[test]
    fn render_to_writer_coalesces_segments_for_vectored_writer() {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .with_source(untracked_source());

        let expected = message.to_string();

        let mut writer = RecordingWriter::new();
        message
            .render_to_writer(&mut writer)
            .expect("vectored write succeeds");

        assert_eq!(writer.vectored_calls, 1, "single vectored write expected");
        assert_eq!(
            writer.write_calls, 0,
            "sequential fallback should be unused"
        );
        assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
    }

    #[test]
    fn render_to_writer_falls_back_when_vectored_partial() {
        let message = Message::error(30, "timeout in data send/receive")
            .with_role(Role::Receiver)
            .with_source(untracked_source());

        let expected = message.to_string();

        let mut writer = RecordingWriter::with_vectored_limit(5);
        message
            .render_to_writer(&mut writer)
            .expect("fallback write succeeds");

        assert_eq!(
            writer.vectored_calls, 1,
            "vectored path should be attempted once"
        );
        assert!(
            writer.write_calls > 0,
            "sequential fallback must finish the message"
        );
        assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
    }

    #[derive(Default)]
    struct RecordingWriter {
        buffer: Vec<u8>,
        vectored_calls: usize,
        write_calls: usize,
        vectored_limit: Option<usize>,
    }

    impl RecordingWriter {
        fn new() -> Self {
            Self::default()
        }

        fn with_vectored_limit(limit: usize) -> Self {
            Self {
                vectored_limit: Some(limit),
                ..Self::default()
            }
        }
    }

    impl super::IoWrite for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_calls += 1;

            let mut to_write: usize = bufs.iter().map(|slice| slice.len()).sum();
            if let Some(limit) = self.vectored_limit {
                let capped = to_write.min(limit);
                self.vectored_limit = Some(limit.saturating_sub(capped));
                to_write = capped;
            }

            let mut remaining = to_write;
            for slice in bufs {
                if remaining == 0 {
                    break;
                }

                let data = slice.as_ref();
                let portion = data.len().min(remaining);
                self.buffer.extend_from_slice(&data[..portion]);
                remaining -= portion;
            }

            Ok(to_write)
        }
    }

    #[test]
    fn severity_as_str_matches_expected_labels() {
        assert_eq!(Severity::Info.as_str(), "info");
        assert_eq!(Severity::Warning.as_str(), "warning");
        assert_eq!(Severity::Error.as_str(), "error");
    }

    #[test]
    fn severity_display_matches_as_str() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[test]
    fn severity_from_str_parses_known_labels() {
        assert_eq!(Severity::from_str("info"), Ok(Severity::Info));
        assert_eq!(Severity::from_str("warning"), Ok(Severity::Warning));
        assert_eq!(Severity::from_str("error"), Ok(Severity::Error));
    }

    #[test]
    fn severity_from_str_rejects_unknown_labels() {
        assert!(Severity::from_str("verbose").is_err());
    }

    #[test]
    fn role_as_str_matches_expected_labels() {
        assert_eq!(Role::Sender.as_str(), "sender");
        assert_eq!(Role::Receiver.as_str(), "receiver");
        assert_eq!(Role::Generator.as_str(), "generator");
        assert_eq!(Role::Server.as_str(), "server");
        assert_eq!(Role::Client.as_str(), "client");
        assert_eq!(Role::Daemon.as_str(), "daemon");
    }

    #[test]
    fn role_display_matches_as_str() {
        assert_eq!(Role::Sender.to_string(), "sender");
        assert_eq!(Role::Daemon.to_string(), "daemon");
    }

    #[test]
    fn role_from_str_parses_known_labels() {
        assert_eq!(Role::from_str("sender"), Ok(Role::Sender));
        assert_eq!(Role::from_str("receiver"), Ok(Role::Receiver));
        assert_eq!(Role::from_str("generator"), Ok(Role::Generator));
        assert_eq!(Role::from_str("server"), Ok(Role::Server));
        assert_eq!(Role::from_str("client"), Ok(Role::Client));
        assert_eq!(Role::from_str("daemon"), Ok(Role::Daemon));
    }

    #[test]
    fn role_from_str_rejects_unknown_labels() {
        assert!(Role::from_str("observer").is_err());
    }

    #[test]
    fn encode_unsigned_decimal_formats_expected_values() {
        let mut buf = [0u8; 8];
        assert_eq!(super::encode_unsigned_decimal(0, &mut buf), "0");
        assert_eq!(super::encode_unsigned_decimal(42, &mut buf), "42");
        assert_eq!(
            super::encode_unsigned_decimal(12_345_678, &mut buf),
            "12345678"
        );
    }

    #[test]
    fn encode_signed_decimal_handles_positive_and_negative_values() {
        let mut buf = [0u8; 12];
        assert_eq!(super::encode_signed_decimal(0, &mut buf), "0");
        assert_eq!(super::encode_signed_decimal(123, &mut buf), "123");
        assert_eq!(super::encode_signed_decimal(-456, &mut buf), "-456");
    }

    #[test]
    fn encode_signed_decimal_formats_i64_minimum_value() {
        let mut buf = [0u8; 32];
        assert_eq!(
            super::encode_signed_decimal(i64::MIN, &mut buf),
            "-9223372036854775808"
        );
    }

    #[test]
    fn render_to_writer_formats_minimum_exit_code() {
        let message = Message::error(i32::MIN, "integrity check failure")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut buffer = Vec::new();
        message
            .render_to_writer(&mut buffer)
            .expect("rendering into a vector never fails");

        let rendered = String::from_utf8(buffer).expect("message renders as UTF-8");
        assert!(rendered.contains("(code -2147483648)"));
    }

    #[test]
    fn rsync_error_macro_attaches_source_and_code() {
        let message = rsync_error!(23, "delta-transfer failure");

        assert_eq!(message.severity(), Severity::Error);
        assert_eq!(message.code(), Some(23));
        let source = message.source().expect("macro records source location");
        assert!(source.path().ends_with("crates/core/src/message.rs"));
    }

    #[test]
    fn rsync_warning_macro_supports_format_arguments() {
        let message = rsync_warning!("vanished {count} files", count = 2).with_code(24);

        assert_eq!(message.severity(), Severity::Warning);
        assert_eq!(message.code(), Some(24));
        assert_eq!(message.text(), "vanished 2 files");
    }

    #[test]
    fn rsync_info_macro_attaches_source() {
        let message = rsync_info!("protocol {version} negotiated", version = 32);

        assert_eq!(message.severity(), Severity::Info);
        assert_eq!(message.code(), None);
        assert_eq!(message.text(), "protocol 32 negotiated");
        assert!(message.source().is_some());
    }

    #[test]
    fn append_normalized_os_str_rewrites_backslashes() {
        let mut rendered = String::from("prefix/");
        append_normalized_os_str(&mut rendered, OsStr::new(r"dir\file.txt"));

        assert_eq!(rendered, "prefix/dir/file.txt");
    }

    #[test]
    fn append_normalized_os_str_preserves_existing_forward_slashes() {
        let mut rendered = String::new();
        append_normalized_os_str(&mut rendered, OsStr::new("dir/sub"));

        assert_eq!(rendered, "dir/sub");
    }
}
