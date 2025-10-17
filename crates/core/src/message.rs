use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;
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

        let candidate = if let Some(root) = option_env!("RSYNC_WORKSPACE_ROOT") {
            let workspace_path = Path::new(root);
            if let Ok(manifest_relative) = manifest_path.strip_prefix(workspace_path) {
                let manifest_relative = manifest_relative.to_path_buf();
                let within_manifest = manifest_relative.as_os_str().is_empty()
                    || file_path.starts_with(&manifest_relative);

                if within_manifest {
                    file_path.to_path_buf()
                } else {
                    manifest_relative.join(file_path)
                }
            } else {
                manifest_path.join(file_path)
            }
        } else {
            manifest_path.join(file_path)
        };

        let normalized_candidate = if candidate.is_absolute() {
            PathBuf::from(normalize_path(&candidate))
        } else {
            candidate
        };

        let repo_relative = if normalized_candidate.is_absolute() {
            strip_workspace_prefix(&normalized_candidate)
        } else {
            normalized_candidate
        };

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
pub struct Message {
    severity: Severity,
    code: Option<i32>,
    text: Cow<'static, str>,
    role: Option<Role>,
    source: Option<SourceLocation>,
}

impl Message {
    /// Creates an informational message.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn with_role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Attaches a source location to the message.
    #[must_use]
    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = Some(source);
        self
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rsync {}: {}", self.severity.as_str(), self.text)?;

        if let (Severity::Error, Some(code)) = (self.severity, self.code) {
            write!(f, " (code {code})")?;
        }

        if let Some(source) = &self.source {
            write!(f, " at {source}")?;
        }

        if let Some(role) = self.role {
            write!(f, " [{}={VERSION_SUFFIX}]", role.as_str())?;
        }

        Ok(())
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
}
