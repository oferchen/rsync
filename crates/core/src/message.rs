use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

/// Version tag appended to message trailers.
pub const VERSION_SUFFIX: &str = "3.4.1-rust";

/// Severity of a user-visible message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Informational message.
    Info,
    /// Warning message.
    Warning,
    /// Error message.
    Error,
}

impl Severity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Role used in the trailer portion of an rsync message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    const fn as_str(self) -> &'static str {
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
#[derive(Clone, Debug, Eq, PartialEq)]
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

        let normalized_absolute = PathBuf::from(normalize_path(&absolute));
        let repo_relative = strip_workspace_prefix(&normalized_absolute);
        let normalized = normalize_path(&repo_relative);

        Self {
            path: Cow::Owned(normalized),
            line,
        }
    }

    /// Returns the repo-relative path stored in the source location.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
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
/// Unlike [`message_source!`], this macro calls [`std::panic::Location::caller`]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Attaches a source location to the message.
    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = Some(source);
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
        writer.write_str("rsync ")?;
        writer.write_str(self.severity.as_str())?;
        writer.write_str(": ")?;
        writer.write_str(&self.text)?;

        if let (Severity::Error, Some(code)) = (self.severity, self.code) {
            write!(writer, " (code {code})")?;
        }

        if let Some(source) = &self.source {
            write!(writer, " at {source}")?;
        }

        if let Some(role) = self.role {
            write!(writer, " [{}={VERSION_SUFFIX}]", role.as_str())?;
        }

        Ok(())
    }

    /// Writes the rendered message into an [`io::Write`] implementor.
    ///
    /// This helper mirrors [`Self::render_to`] but operates on byte writers. It avoids allocating
    /// intermediate [`String`] values by streaming the formatted payload directly into the provided
    /// writer. Any encountered I/O error is propagated unchanged, ensuring callers can surface the
    /// original failure context in user-facing diagnostics.
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
        struct Adapter<'a, W: IoWrite> {
            inner: &'a mut W,
            error: Option<io::Error>,
        }

        impl<'a, W: IoWrite> fmt::Write for Adapter<'a, W> {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                match self.inner.write_all(s.as_bytes()) {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        self.error = Some(err);
                        Err(fmt::Error)
                    }
                }
            }

            fn write_char(&mut self, ch: char) -> fmt::Result {
                let mut buf = [0u8; 4];
                let encoded = ch.encode_utf8(&mut buf);
                self.write_str(encoded)
            }
        }

        impl<'a, W: IoWrite> Adapter<'a, W> {
            fn finish(self) -> io::Result<()> {
                if let Some(err) = self.error {
                    Err(err)
                } else {
                    Ok(())
                }
            }
        }

        let mut adapter = Adapter {
            inner: writer,
            error: None,
        };

        let render_result = self.render_to(&mut adapter);
        let finish_result = adapter.finish();

        match (render_result, finish_result) {
            (Ok(()), outcome) => outcome,
            (Err(_), Err(err)) => Err(err),
            (Err(_), Ok(())) => Err(io::Error::other(
                "rendering message failed without capturing the I/O error",
            )),
        }
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
        self.render_to_writer(writer)?;
        writer.write_all(b"\n")
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.render_to(f)
    }
}

fn strip_workspace_prefix(path: &Path) -> PathBuf {
    if let Some(root) = option_env!("RSYNC_WORKSPACE_ROOT") {
        let workspace_path = Path::new(root);
        if let Ok(relative) = path.strip_prefix(workspace_path) {
            return relative.to_path_buf();
        }
    }

    path.to_path_buf()
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
        normalized.push_str(&prefix.to_string_lossy().replace('\\', "/"));
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

        normalized.push_str(&segment.to_string_lossy().replace('\\', "/"));
    }

    if normalized.is_empty() {
        String::from(".")
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

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
}
