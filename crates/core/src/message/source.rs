use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf, PrefixComponent};
use std::sync::OnceLock;

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
        } else if let Some(workspace_path) = workspace_root_path() {
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
        let repo_relative =
            canonicalize_virtual_test_path(strip_workspace_prefix_owned(normalized));

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

/// Removes the workspace root prefix from a normalized path when possible.
///
/// The input string must already be normalised via [`normalize_path`]. When the path lives outside
/// the workspace root (or the root is unknown), the original string is returned unchanged.
fn strip_workspace_prefix_owned(normalized_path: String) -> String {
    if let Some(stripped) = normalized_workspace_root()
        .and_then(|root| strip_normalized_workspace_prefix(&normalized_path, root))
    {
        return stripped;
    }

    normalized_path
}

fn canonicalize_virtual_test_path(path: String) -> String {
    if let Some(rewritten) = rewrite_test_shard(&path) {
        return rewritten;
    }

    path
}

fn rewrite_test_shard(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2 {
        return None;
    }

    let last = segments[segments.len() - 1];
    let Some(digits) = last.strip_prefix("part")?.strip_suffix(".rs") else {
        return None;
    };

    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    if segments[segments.len() - 2] != "tests" {
        return None;
    }

    let mut rewritten = segments[..segments.len() - 2].join("/");
    if !rewritten.is_empty() {
        rewritten.push('/');
    }
    rewritten.push_str("tests.rs");
    Some(rewritten)
}

/// Returns the workspace-relative representation of `path` when it shares the provided root.
///
/// Both arguments must use forward slashes, matching the representation produced by
/// [`normalize_path`]. The helper enforces segment boundaries to avoid stripping prefixes from
/// directories that merely share the same leading byte sequence.
pub(super) fn strip_normalized_workspace_prefix(path: &str, root: &str) -> Option<String> {
    if !path.starts_with(root) {
        return None;
    }

    let mut suffix = &path[root.len()..];

    if suffix.is_empty() {
        return Some(String::from("."));
    }

    if !root.ends_with('/') {
        let stripped_suffix = suffix.strip_prefix('/')?;
        if stripped_suffix.is_empty() {
            return Some(String::from("."));
        }

        suffix = stripped_suffix;
    }

    Some(suffix.to_owned())
}

/// Lazily computes the normalized workspace root used for source remapping.
pub(super) fn normalized_workspace_root() -> Option<&'static str> {
    static NORMALIZED: OnceLock<Option<String>> = OnceLock::new();

    NORMALIZED
        .get_or_init(|| workspace_root_path().map(normalize_path))
        .as_deref()
}

/// Returns the absolute workspace root configured at build time, if available.
pub(super) fn workspace_root_path() -> Option<&'static Path> {
    static WORKSPACE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

    WORKSPACE_ROOT
        .get_or_init(|| {
            compute_workspace_root(
                option_env!("RSYNC_WORKSPACE_ROOT"),
                option_env!("CARGO_WORKSPACE_DIR"),
            )
        })
        .as_deref()
}

pub(super) fn compute_workspace_root(
    explicit_root: Option<&str>,
    workspace_dir: Option<&str>,
) -> Option<PathBuf> {
    if let Some(root) = explicit_root {
        return Some(PathBuf::from(root));
    }

    fallback_workspace_root(workspace_dir)
}

fn fallback_workspace_root(workspace_dir: Option<&str>) -> Option<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    if let Some(dir) = workspace_dir {
        let candidate = Path::new(dir);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            manifest_dir.join(candidate)
        };

        if candidate.is_dir() {
            return Some(canonicalize_or_fallback(&candidate));
        }
    }

    for ancestor in manifest_dir.ancestors() {
        if ancestor.join("Cargo.lock").is_file() {
            return Some(canonicalize_or_fallback(ancestor));
        }
    }

    Some(canonicalize_or_fallback(manifest_dir))
}

pub(super) fn canonicalize_or_fallback(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn normalize_path(path: &Path) -> String {
    use std::path::Component;

    let mut prefix: Option<String> = None;
    let is_absolute = path.is_absolute();
    let mut segments: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => {
                prefix = Some(normalize_prefix_component(value));
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
        normalized.push_str(&prefix);
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

fn normalize_prefix_component(prefix: PrefixComponent<'_>) -> String {
    use std::path::Prefix;

    match prefix.kind() {
        Prefix::VerbatimDisk(disk) | Prefix::Disk(disk) => {
            let mut rendered = String::with_capacity(2);
            let letter = char::from(disk).to_ascii_uppercase();
            rendered.push(letter);
            rendered.push(':');
            rendered
        }
        Prefix::VerbatimUNC(server, share) | Prefix::UNC(server, share) => {
            let mut rendered = String::from("//");
            append_normalized_os_str(&mut rendered, server);
            rendered.push('/');
            append_normalized_os_str(&mut rendered, share);
            rendered
        }
        Prefix::DeviceNS(ns) => {
            let mut rendered = String::from("//./");
            append_normalized_os_str(&mut rendered, ns);
            rendered
        }
        Prefix::Verbatim(component) => {
            let mut rendered = String::new();
            append_normalized_os_str(&mut rendered, component);
            rendered
        }
    }
}

pub(super) fn append_normalized_os_str(target: &mut String, value: &OsStr) {
    let lossy = value.to_string_lossy();
    let text = lossy.as_ref();

    if let Some(first_backslash) = text.find('\\') {
        let (prefix, remainder) = text.split_at(first_backslash);
        target.push_str(prefix);

        for ch in remainder.chars() {
            target.push(if ch == '\\' { '/' } else { ch });
        }
    } else {
        target.push_str(text);
    }
}
