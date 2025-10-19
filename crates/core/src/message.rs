use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::TryReserveError;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as FmtWrite};
use std::fs;
use std::io::{self, IoSlice, Write as IoWrite};
use std::path::{Path, PathBuf, PrefixComponent};
use std::slice;
use std::str::{self, FromStr};
use std::sync::OnceLock;

pub mod strings;

const MAX_MESSAGE_SEGMENTS: usize = 18;

#[derive(Debug)]
struct MessageBufferReserveError {
    inner: TryReserveError,
}

impl MessageBufferReserveError {
    #[inline]
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }

    #[inline]
    fn inner(&self) -> &TryReserveError {
        &self.inner
    }
}

impl fmt::Display for MessageBufferReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to reserve memory while rendering rsync message: {}",
            self.inner
        )
    }
}

impl std::error::Error for MessageBufferReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.inner())
    }
}

#[inline]
fn map_message_reserve_error(err: TryReserveError) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        MessageBufferReserveError::new(err),
    )
}

/// Scratch buffers used when producing vectored message segments.
///
/// Instances of this type are supplied to [`Message::as_segments`] so the helper can encode
/// decimal exit codes and line numbers without allocating temporary [`String`] values. The
/// buffers are stack-allocated and reusable, making it cheap for higher layers to render
/// multiple messages in succession without paying repeated allocation costs.
///
/// # Examples
///
/// ```
/// use rsync_core::{message::{Message, Role, MessageScratch}, message_source};
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
/// let segments = message.as_segments(&mut scratch, false);
///
/// assert_eq!(segments.len(), message.to_bytes().unwrap().len());
/// ```
#[derive(Clone, Debug)]
pub struct MessageScratch {
    code_digits: [u8; 20],
    line_digits: [u8; 20],
}

impl MessageScratch {
    /// Creates a new scratch buffer with zeroed storage.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            code_digits: [0; 20],
            line_digits: [0; 20],
        }
    }
}

impl Default for MessageScratch {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static THREAD_LOCAL_SCRATCH: RefCell<MessageScratch> = const { RefCell::new(MessageScratch::new()) };
}

fn call_with_scratch<F, R>(closure: &mut Option<F>, scratch: &mut MessageScratch) -> R
where
    F: FnOnce(&mut MessageScratch) -> R,
{
    let f = closure
        .take()
        .expect("message scratch closure invoked multiple times");
    f(scratch)
}

fn with_thread_local_scratch<F, R>(f: F) -> R
where
    F: FnOnce(&mut MessageScratch) -> R,
{
    let mut closure = Some(f);

    if let Ok(Some(result)) = THREAD_LOCAL_SCRATCH.try_with(|scratch| {
        if let Ok(mut guard) = scratch.try_borrow_mut() {
            return Some(call_with_scratch(&mut closure, &mut guard));
        }

        None
    }) {
        return result;
    }

    let mut scratch = MessageScratch::new();
    call_with_scratch(&mut closure, &mut scratch)
}

/// Collection of slices that jointly render an [`Message`].
///
/// The segments reference the message payload together with optional exit codes, source
/// locations, and role trailers. Callers obtain the structure through [`Message::as_segments`]
/// and can then stream the slices into vectored writers, aggregate statistics, or reuse the
/// layout when constructing custom buffers. `MessageSegments` implements [`AsRef`] so the
/// collected [`IoSlice`] values can be passed directly to APIs such as
/// [`write_vectored`](IoWrite::write_vectored) without requiring an intermediate allocation.
///
/// # Examples
///
/// Convert the segments into a slice suitable for [`write_vectored`](IoWrite::write_vectored).
///
/// ```
/// use rsync_core::{
///     message::{Message, MessageScratch, Role},
///     message_source,
/// };
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(11, "error in file IO")
///     .with_role(Role::Receiver)
///     .with_source(message_source!());
/// let segments = message.as_segments(&mut scratch, false);
///
/// let slices: &[std::io::IoSlice<'_>] = segments.as_ref();
/// assert_eq!(slices.len(), segments.segment_count());
/// ```
///
/// Consume the segments to collect the rendered message into a contiguous buffer.
///
/// ```
/// use rsync_core::{
///     message::{Message, MessageScratch, Role},
///     message_source,
/// };
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
///
/// let segments = message.as_segments(&mut scratch, false);
/// let mut flattened = Vec::new();
/// segments.extend_vec(&mut flattened)?;
///
/// assert_eq!(flattened, message.to_bytes().unwrap());
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct MessageSegments<'a> {
    segments: [IoSlice<'a>; MAX_MESSAGE_SEGMENTS],
    count: usize,
    total_len: usize,
}

impl<'a> MessageSegments<'a> {
    /// Returns the populated slice view over the underlying [`IoSlice`] array.
    #[inline]
    #[must_use]
    pub fn as_slices(&self) -> &[IoSlice<'a>] {
        &self.segments[..self.count]
    }

    #[inline]
    fn as_slices_mut(&mut self) -> &mut [IoSlice<'a>] {
        &mut self.segments[..self.count]
    }

    /// Returns the total number of bytes covered by the message segments.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.total_len
    }

    /// Reports the number of populated segments.
    #[inline]
    #[must_use]
    pub const fn segment_count(&self) -> usize {
        self.count
    }

    /// Returns an iterator over the populated [`IoSlice`] values.
    ///
    /// The iterator traverses the same slices that [`Self::as_slices`] exposes, preserving their
    /// original ordering so call sites can stream the message into custom sinks without allocating
    /// intermediate buffers. This mirrors upstream rsync's behaviour where formatted messages are
    /// emitted sequentially. The iterator borrows the segments, meaning the caller must keep the
    /// [`MessageSegments`] instance alive for the duration of the iteration.
    ///
    /// # Examples
    ///
    /// Iterate over the segments to compute their cumulative length.
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let total: usize = segments.iter().map(|slice| slice.len()).sum();
    ///
    /// assert_eq!(total, segments.len());
    /// ```
    #[inline]
    pub fn iter(&self) -> slice::Iter<'_, IoSlice<'a>> {
        self.as_slices().iter()
    }

    /// Reports whether any slices were produced or contain bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0 || self.total_len == 0
    }

    /// Returns a mutable iterator over the populated vectored slices.
    ///
    /// This mirrors [`Self::iter`] but yields mutable references so callers can
    /// adjust slice boundaries prior to issuing writes. The iterator maintains
    /// the original ordering so diagnostics remain byte-identical to upstream
    /// rsync.
    ///
    /// # Examples
    ///
    /// Iterate mutably over the slices and confirm they are all non-empty.
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut segments = message.as_segments(&mut scratch, false);
    ///
    /// for slice in &mut segments {
    ///     assert!(!slice.as_ref().is_empty());
    /// }
    /// ```
    #[inline]
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, IoSlice<'a>> {
        self.as_slices_mut().iter_mut()
    }

    /// Streams the message segments into the provided writer.
    ///
    /// The helper prefers vectored writes when the message spans multiple
    /// segments so downstream sinks receive the payload in a single
    /// [`write_vectored`](IoWrite::write_vectored) call. When the writer reports
    /// that vectored I/O is unsupported or performs a partial write, the
    /// remaining bytes are flushed sequentially to mirror upstream rsync's
    /// formatting logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(12, "example")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = Vec::new();
    /// segments.write_to(&mut buffer).unwrap();
    ///
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// ```
    pub fn write_to<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        if self.count == 1 {
            let bytes = self.segments[0].as_ref();

            if bytes.is_empty() {
                return Ok(());
            }

            writer.write_all(bytes)?;
            return Ok(());
        }

        let mut storage = self.segments;
        let mut view: &mut [IoSlice<'a>] = &mut storage[..self.count];
        let mut remaining = self.total_len;

        while !view.is_empty() && remaining != 0 {
            match writer.write_vectored(view) {
                Ok(0) => {
                    return Err(io::Error::from(io::ErrorKind::WriteZero));
                }
                Ok(written) => {
                    debug_assert!(written <= remaining);
                    remaining = remaining.saturating_sub(written);

                    if remaining == 0 {
                        return Ok(());
                    }

                    IoSlice::advance_slices(&mut view, written);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::Unsupported => break,
                Err(err) => return Err(err),
            }
        }

        for slice in view.iter() {
            let bytes = slice.as_ref();

            if bytes.is_empty() {
                continue;
            }

            writer.write_all(bytes)?;
            debug_assert!(bytes.len() <= remaining);
            remaining = remaining.saturating_sub(bytes.len());
        }

        if remaining != 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }

        Ok(())
    }

    /// Extends the provided buffer with the rendered message bytes.
    ///
    /// The method ensures enough capacity for the rendered message by using
    /// [`Vec::try_reserve_exact`], avoiding the exponential growth strategy of
    /// [`Vec::try_reserve`]. It then copies each segment into the buffer. This
    /// keeps allocations tight for call sites that accumulate multiple
    /// diagnostics into a single [`Vec<u8>`] without going through the
    /// [`Write`](IoWrite) trait.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = b"prefix: ".to_vec();
    /// let prefix_len = buffer.len();
    /// segments.extend_vec(&mut buffer)?;
    ///
    /// assert_eq!(&buffer[..prefix_len], b"prefix: ");
    /// assert_eq!(&buffer[prefix_len..], message.to_bytes().unwrap().as_slice());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn extend_vec(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        let required = self.len();
        let spare = buffer.capacity().saturating_sub(buffer.len());
        if spare < required {
            buffer
                .try_reserve_exact(required - spare)
                .map_err(map_message_reserve_error)?;
        }

        for slice in self.iter() {
            buffer.extend_from_slice(slice.as_ref());
        }
        Ok(())
    }

    /// Collects the message segments into a freshly allocated [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::extend_vec`] but manages the buffer lifecycle
    /// internally, returning the rendered bytes directly. This keeps call sites
    /// concise when they only need an owned copy of the message without
    /// providing scratch storage up front. Allocation failures propagate as
    /// [`io::ErrorKind::OutOfMemory`] so diagnostics remain actionable.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let collected = segments.to_vec().expect("allocation succeeds");
    ///
    /// assert_eq!(collected, message.to_bytes().unwrap());
    /// ```
    pub fn to_vec(&self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::with_capacity(self.len());
        self.extend_vec(&mut buffer)?;
        Ok(buffer)
    }
}

impl<'a> AsRef<[IoSlice<'a>]> for MessageSegments<'a> {
    #[inline]
    fn as_ref(&self) -> &[IoSlice<'a>] {
        self.as_slices()
    }
}

impl<'a> IntoIterator for &'a MessageSegments<'a> {
    type Item = &'a IoSlice<'a>;
    type IntoIter = slice::Iter<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut MessageSegments<'a> {
    type Item = &'a mut IoSlice<'a>;
    type IntoIter = slice::IterMut<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<'a> IntoIterator for MessageSegments<'a> {
    type Item = IoSlice<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<IoSlice<'a>, MAX_MESSAGE_SEGMENTS>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter().take(self.count)
    }
}

/// Version tag appended to message trailers.
pub const VERSION_SUFFIX: &str = crate::version::RUST_VERSION;

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

    /// Returns the canonical prefix rendered at the start of diagnostics.
    ///
    /// The string mirrors upstream rsync's output, combining the constant
    /// `"rsync"` banner with the lowercase severity label and trailing
    /// colon. Centralising the prefix ensures [`Message::as_segments`]
    /// doesn't need to assemble the pieces manually, which avoids
    /// additional vectored segments and keeps rendering logic in sync with
    /// upstream expectations.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert_eq!(Severity::Info.prefix(), "rsync info: ");
    /// assert_eq!(Severity::Warning.prefix(), "rsync warning: ");
    /// assert_eq!(Severity::Error.prefix(), "rsync error: ");
    /// ```
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Info => "rsync info: ",
            Self::Warning => "rsync warning: ",
            Self::Error => "rsync error: ",
        }
    }

    /// Reports whether this severity represents an informational message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Info.is_info());
    /// assert!(!Severity::Error.is_info());
    /// ```
    #[must_use]
    pub const fn is_info(self) -> bool {
        matches!(self, Self::Info)
    }

    /// Reports whether this severity represents a warning message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Warning.is_warning());
    /// assert!(!Severity::Info.is_warning());
    /// ```
    #[must_use]
    pub const fn is_warning(self) -> bool {
        matches!(self, Self::Warning)
    }

    /// Reports whether this severity represents an error message.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Severity;
    ///
    /// assert!(Severity::Error.is_error());
    /// assert!(!Severity::Warning.is_error());
    /// ```
    #[must_use]
    pub const fn is_error(self) -> bool {
        matches!(self, Self::Error)
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
/// call-site [`SourceLocation`] using [`macro@tracked_message_source`]. It accepts either a
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
            .with_source($crate::tracked_message_source!())
    }};
    ($code:expr, $fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::error($code, format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
    }};
}

/// Constructs a warning [`Message`] from the provided payload.
///
/// Like [`macro@rsync_error`], the macro records the call-site source location so
/// diagnostics include repo-relative paths. The macro relies on
/// [`macro@tracked_message_source`], meaning callers annotated with
/// `#[track_caller]` automatically propagate their invocation site. Additional
/// context, such as exit codes, can be attached using [`Message::with_code`].
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
            .with_source($crate::tracked_message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::warning(format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
    }};
}

/// Constructs an informational [`Message`] from the provided payload.
///
/// The macro is a convenience wrapper around [`Message::info`] that automatically
/// attaches the call-site source location. Upstream rsync typically omits source
/// locations for informational output, but retaining the metadata simplifies
/// debugging and keeps the API consistent across severities. As with the other
/// message macros, [`macro@tracked_message_source`] ensures `#[track_caller]`
/// annotations propagate the original invocation site into diagnostics.
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
            .with_source($crate::tracked_message_source!())
    }};
    ($fmt:expr, $($arg:tt)+ $(,)?) => {{
        $crate::message::Message::info(format!($fmt, $($arg)+))
            .with_source($crate::tracked_message_source!())
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
    /// Returns the vectored representation of the rendered message.
    ///
    /// The helper exposes the same slices used internally when emitting the message into an
    /// [`io::Write`] implementor. Callers that need to integrate with custom buffered pipelines can
    /// reuse the returned segments with [`Write::write_vectored`], avoiding redundant allocations or
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
        with_thread_local_scratch(|scratch| {
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
    /// message.append_to_vec(&mut buffer).unwrap();
    ///
    /// assert_eq!(buffer.len(), message.byte_len());
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
    /// assert!(rendered.contains("[sender=3.4.1-rust]"));
    /// ```
    #[inline]
    pub fn render_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        with_thread_local_scratch(|scratch| self.render_to_with_scratch(scratch, writer))
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
    #[inline]
    pub fn render_line_to<W: fmt::Write>(&self, writer: &mut W) -> fmt::Result {
        with_thread_local_scratch(|scratch| self.render_line_to_with_scratch(scratch, writer))
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
    /// assert!(bytes.ends_with(b"[sender=3.4.1-rust]"));
    /// ```
    #[inline]
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        with_thread_local_scratch(|scratch| self.to_bytes_with_scratch(scratch))
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
    pub fn to_line_bytes(&self) -> io::Result<Vec<u8>> {
        with_thread_local_scratch(|scratch| self.to_line_bytes_with_scratch(scratch))
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
    #[inline]
    pub fn render_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        with_thread_local_scratch(|scratch| self.render_to_writer_with_scratch(scratch, writer))
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
    #[inline]
    pub fn render_line_to_writer<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        with_thread_local_scratch(|scratch| {
            self.render_line_to_writer_with_scratch(scratch, writer)
        })
    }

    /// Appends the rendered message into the provided byte buffer.
    ///
    /// The helper mirrors [`Self::render_to_writer`] but avoids the overhead of
    /// constructing a temporary [`Vec`] or going through the [`Write`] trait for
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
    /// Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!())
    ///     .append_to_vec(&mut buffer)?;
    ///
    /// assert!(buffer.starts_with(b"rsync error:"));
    /// assert!(buffer.ends_with(b"[sender=3.4.1-rust]"));
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn append_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        with_thread_local_scratch(|scratch| self.append_to_vec_with_scratch(scratch, buffer))
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
    /// Message::warning("vanished file")
    ///     .with_code(24)
    ///     .append_line_to_vec(&mut buffer)?;
    ///
    /// assert!(buffer.ends_with(b"\n"));
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn append_line_to_vec(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        with_thread_local_scratch(|scratch| self.append_line_to_vec_with_scratch(scratch, buffer))
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
    pub fn append_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<()> {
        self.append_to_vec_with_scratch_inner(scratch, buffer, false)
    }

    /// Appends the rendered message followed by a newline into the provided buffer while reusing scratch space.
    pub fn append_line_to_vec_with_scratch(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
    ) -> io::Result<()> {
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
        segments.extend_vec(&mut buffer)?;
        Ok(buffer)
    }

    fn append_to_vec_with_scratch_inner(
        &self,
        scratch: &mut MessageScratch,
        buffer: &mut Vec<u8>,
        include_newline: bool,
    ) -> io::Result<()> {
        let segments = self.as_segments(scratch, include_newline);
        segments.extend_vec(buffer)
    }
}

impl Message {
    /// Renders the message into an arbitrary [`fmt::Write`] implementor while reusing scratch buffers.
    ///
    /// The helper mirrors [`Self::render_to`] but accepts an explicit [`MessageScratch`], allowing
    /// callers that emit multiple diagnostics to amortise the buffer initialisation cost.
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
    pub fn render_line_to_with_scratch<W: fmt::Write>(
        &self,
        scratch: &mut MessageScratch,
        writer: &mut W,
    ) -> fmt::Result {
        self.render_to_with_scratch(scratch, writer)?;
        FmtWrite::write_char(writer, '\n')
    }

    /// Collects the rendered message into a [`Vec<u8>`] while reusing caller-provided scratch buffers.
    pub fn to_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, false)
    }

    /// Collects the rendered message and a trailing newline into a [`Vec<u8>`] while reusing scratch buffers.
    pub fn to_line_bytes_with_scratch(&self, scratch: &mut MessageScratch) -> io::Result<Vec<u8>> {
        self.to_bytes_with_scratch_inner(scratch, true)
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
        .get_or_init(|| workspace_root_path().map(normalize_path))
        .as_deref()
}

/// Returns the absolute workspace root configured at build time, if available.
fn workspace_root_path() -> Option<&'static Path> {
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

fn compute_workspace_root(
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

fn canonicalize_or_fallback(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_path(path: &Path) -> String {
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

    #[track_caller]
    fn tracked_rsync_error_macro() -> Message {
        rsync_error!(23, "delta-transfer failure")
    }

    #[track_caller]
    fn tracked_rsync_warning_macro() -> Message {
        rsync_warning!("some files vanished")
    }

    #[track_caller]
    fn tracked_rsync_info_macro() -> Message {
        rsync_info!("negotiation complete")
    }

    #[test]
    fn message_new_allows_dynamic_severity() {
        let warning = Message::new(Severity::Warning, "dynamic warning");
        assert_eq!(warning.severity(), Severity::Warning);
        assert_eq!(warning.code(), None);
        assert_eq!(warning.text(), "dynamic warning");

        let error = Message::new(Severity::Error, "dynamic error").with_code(23);
        assert_eq!(error.severity(), Severity::Error);
        assert_eq!(error.code(), Some(23));
        assert_eq!(error.text(), "dynamic error");
    }

    #[test]
    fn message_predicates_forward_to_severity() {
        let info = Message::info("probe");
        assert!(info.is_info());
        assert!(!info.is_warning());
        assert!(!info.is_error());

        let warning = Message::warning("vanished");
        assert!(warning.is_warning());
        assert!(!warning.is_info());
        assert!(!warning.is_error());

        let error = Message::error(11, "io failure");
        assert!(error.is_error());
        assert!(!error.is_info());
        assert!(!error.is_warning());
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
    fn message_without_role_clears_trailer() {
        let formatted = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .without_role()
            .to_string();

        assert!(!formatted.contains("[sender="));
    }

    #[test]
    fn message_without_source_clears_location() {
        let formatted = Message::error(23, "delta-transfer failure")
            .with_source(message_source!())
            .without_source()
            .to_string();

        assert!(!formatted.contains(" at "));
    }

    #[test]
    fn message_without_code_clears_suffix() {
        let formatted = Message::error(23, "delta-transfer failure")
            .without_code()
            .to_string();

        assert!(!formatted.contains("(code"));
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
    fn strip_normalized_workspace_prefix_returns_current_dir_for_exact_match() {
        let root = "/workspace/project";
        let stripped = super::strip_normalized_workspace_prefix(root, root)
            .expect("identical paths should collapse to the current directory");

        assert_eq!(stripped, ".");
    }

    #[test]
    fn strip_normalized_workspace_prefix_accepts_trailing_separator_on_root() {
        let root = "/workspace/project/";
        let path = "/workspace/project/crates/core/src/lib.rs";
        let stripped = super::strip_normalized_workspace_prefix(path, root)
            .expect("child paths should remain accessible when the root ends with a separator");

        assert_eq!(stripped, "crates/core/src/lib.rs");
    }

    #[test]
    fn strip_normalized_workspace_prefix_rejects_partial_component_matches() {
        let root = "/workspace/project";
        let path = "/workspace/project-old/src/lib.rs";

        assert!(
            super::strip_normalized_workspace_prefix(path, root).is_none(),
            "differing path segments must not be treated as the same workspace",
        );
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
    fn compute_workspace_root_prefers_explicit_env() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let computed = super::compute_workspace_root(Some(manifest), Some("ignored"))
            .expect("explicit manifest directory should be accepted");

        assert_eq!(computed, PathBuf::from(manifest));
    }

    #[test]
    fn compute_workspace_root_falls_back_to_manifest_ancestors() {
        let expected = super::canonicalize_or_fallback(Path::new(env!("RSYNC_WORKSPACE_ROOT")));
        let computed = super::compute_workspace_root(None, None)
            .expect("ancestor scan should locate the workspace root");

        assert_eq!(computed, expected);
    }

    #[test]
    fn compute_workspace_root_handles_relative_workspace_dir() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = Path::new(env!("RSYNC_WORKSPACE_ROOT"));

        let relative_from_root = match manifest_dir.strip_prefix(workspace_root) {
            Ok(relative) => relative,
            Err(_) => Path::new("."),
        };

        let mut relative_to_root = PathBuf::new();
        for component in relative_from_root.components() {
            if matches!(component, std::path::Component::Normal(_)) {
                relative_to_root.push("..");
            }
        }

        if relative_to_root.as_os_str().is_empty() {
            relative_to_root.push(".");
        }

        let relative_owned = relative_to_root.to_string_lossy().into_owned();
        let computed = super::compute_workspace_root(None, Some(relative_owned.as_str()))
            .expect("relative workspace dir should resolve");
        let expected = super::canonicalize_or_fallback(workspace_root);

        assert_eq!(computed, expected);
    }

    #[test]
    fn source_location_clone_preserves_path_and_line() {
        let original = SourceLocation::from_parts(env!("CARGO_MANIFEST_DIR"), "src/lib.rs", 42);
        let cloned = original.clone();

        assert_eq!(original, cloned);
        assert_eq!(cloned.path(), "crates/core/src/lib.rs");
        assert_eq!(cloned.line(), 42);
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

    #[cfg(windows)]
    #[test]
    fn normalize_verbatim_disk_paths_drop_unc_prefix() {
        let normalized = normalize_path(Path::new(r"\\?\C:\foo\bar"));
        assert_eq!(normalized, "C:/foo/bar");
    }

    #[cfg(windows)]
    #[test]
    fn normalize_verbatim_unc_paths_match_standard_unc_rendering() {
        let normalized = normalize_path(Path::new(r"\\?\UNC\server\share\dir"));
        assert_eq!(normalized, "//server/share/dir");
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
    fn message_clone_preserves_rendering_and_metadata() {
        let original = Message::error(12, "protocol error")
            .with_role(Role::Sender)
            .with_source(message_source!());
        let cloned = original.clone();

        assert_eq!(original, cloned);
        assert_eq!(cloned.to_string(), original.to_string());
        assert_eq!(cloned.code(), Some(12));
        assert_eq!(cloned.role(), Some(Role::Sender));
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
    fn with_segments_invokes_closure_with_rendered_bytes() {
        let message = Message::error(35, "timeout in data send")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let expected = message.to_bytes().unwrap();
        let mut collected = Vec::new();

        let value = message.with_segments(false, |segments| {
            for slice in segments {
                collected.extend_from_slice(slice.as_ref());
            }

            0xdead_beefu64
        });

        assert_eq!(value, 0xdead_beefu64);
        assert_eq!(collected, expected);
    }

    #[test]
    fn with_segments_supports_newline_variants() {
        let message = Message::warning("vanished files detected").with_code(24);

        let mut collected = Vec::new();
        message.with_segments(true, |segments| {
            for slice in segments {
                collected.extend_from_slice(slice.as_ref());
            }
        });

        assert_eq!(collected, message.to_line_bytes().unwrap());
    }

    #[test]
    fn with_segments_supports_reentrant_rendering() {
        let message = Message::warning("vanished files detected").with_code(24);
        let expected = message.to_bytes().expect("rendering into Vec never fails");

        message.with_segments(false, |segments| {
            let nested = message
                .to_bytes()
                .expect("rendering inside closure should not panic");
            assert_eq!(nested, expected);

            let flattened = segments.to_vec().expect("collecting segments never fails");
            assert_eq!(flattened, expected);
        });
    }

    #[test]
    fn render_to_writer_with_scratch_matches_fresh_scratch() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut reused = Vec::new();
        message
            .render_to_writer_with_scratch(&mut scratch, &mut reused)
            .expect("writing into a vector never fails");

        let mut baseline = Vec::new();
        message
            .render_to_writer(&mut baseline)
            .expect("writing into a vector never fails");

        assert_eq!(reused, baseline);
    }

    #[test]
    fn scratch_supports_sequential_messages() {
        let mut scratch = MessageScratch::new();
        let mut output = Vec::new();

        rsync_error!(23, "delta-transfer failure")
            .render_line_to_writer_with_scratch(&mut scratch, &mut output)
            .expect("writing into a vector never fails");

        rsync_warning!("some files vanished")
            .with_code(24)
            .render_line_to_writer_with_scratch(&mut scratch, &mut output)
            .expect("writing into a vector never fails");

        let rendered = String::from_utf8(output).expect("messages are UTF-8");
        assert!(rendered.lines().any(|line| line.contains("(code 23)")));
        assert!(rendered.lines().any(|line| line.contains("(code 24)")));
    }

    #[test]
    fn message_segments_iterator_covers_all_bytes() {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Receiver)
            .with_source(message_source!());
        let mut scratch = MessageScratch::new();

        let collected: Vec<u8> = {
            let segments = message.as_segments(&mut scratch, true);
            segments
                .iter()
                .flat_map(|slice| slice.as_ref().iter().copied())
                .collect()
        };

        assert_eq!(collected, message.to_line_bytes().unwrap());
    }

    #[test]
    fn message_segments_into_iterator_matches_iter() {
        let message = Message::error(12, "example failure")
            .with_role(Role::Sender)
            .with_source(message_source!());
        let mut scratch = MessageScratch::new();

        let segments = message.as_segments(&mut scratch, true);
        let via_method: Vec<usize> = segments.iter().map(|slice| slice.len()).collect();
        let via_into: Vec<usize> = (&segments).into_iter().map(|slice| slice.len()).collect();

        assert_eq!(via_method, via_into);
    }

    #[test]
    fn message_segments_mut_iterator_covers_all_bytes() {
        let message = Message::error(24, "partial transfer").with_source(message_source!());
        let mut scratch = MessageScratch::new();

        let mut segments = message.as_segments(&mut scratch, false);
        let mut total_len = 0;

        for slice in &mut segments {
            total_len += slice.as_ref().len();
        }

        assert_eq!(total_len, message.to_bytes().unwrap().len());
    }

    #[test]
    fn message_segments_extend_vec_appends_bytes() {
        let message = Message::error(12, "example failure")
            .with_role(Role::Server)
            .with_source(message_source!());
        let mut scratch = MessageScratch::new();

        let segments = message.as_segments(&mut scratch, false);
        let mut buffer = b"prefix: ".to_vec();
        let prefix_len = buffer.len();
        segments
            .extend_vec(&mut buffer)
            .expect("Vec<u8> growth should succeed for small messages");

        assert_eq!(&buffer[..prefix_len], b"prefix: ");
        assert_eq!(
            &buffer[prefix_len..],
            message.to_bytes().unwrap().as_slice()
        );
    }

    #[test]
    fn message_segments_extend_vec_noop_for_empty_segments() {
        let segments = MessageSegments {
            segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
            count: 0,
            total_len: 0,
        };

        let mut buffer = b"static prefix".to_vec();
        let expected = buffer.clone();
        let capacity = buffer.capacity();

        segments
            .extend_vec(&mut buffer)
            .expect("empty segments should not alter the buffer");

        assert_eq!(buffer, expected);
        assert_eq!(buffer.capacity(), capacity);
    }

    #[test]
    fn message_segments_is_empty_accounts_for_zero_length_segments() {
        let mut scratch = MessageScratch::new();
        let message = Message::info("ready");
        let populated = message.as_segments(&mut scratch, false);
        assert!(!populated.is_empty());

        let empty = MessageSegments {
            segments: [IoSlice::new(&[]); MAX_MESSAGE_SEGMENTS],
            count: 1,
            total_len: 0,
        };

        assert!(empty.is_empty());
    }

    #[test]
    fn message_segments_to_vec_collects_bytes() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Receiver)
            .with_source(message_source!());
        let mut scratch = MessageScratch::new();

        let segments = message.as_segments(&mut scratch, false);
        let collected = segments
            .to_vec()
            .expect("allocating the rendered message succeeds");

        assert_eq!(collected, message.to_bytes().unwrap());
    }

    #[test]
    fn message_segments_to_vec_respects_newline_flag() {
        let message = Message::warning("vanished file").with_code(24);
        let mut scratch = MessageScratch::new();

        let segments = message.as_segments(&mut scratch, true);
        let collected = segments
            .to_vec()
            .expect("allocating the rendered message succeeds");

        assert_eq!(collected, message.to_line_bytes().unwrap());
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
    fn render_to_with_scratch_matches_standard_rendering() {
        let message = Message::warning("soft limit reached")
            .with_code(24)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut reused = String::new();
        message
            .render_to_with_scratch(&mut scratch, &mut reused)
            .expect("rendering into a string never fails");

        let mut baseline = String::new();
        message
            .render_to(&mut baseline)
            .expect("rendering into a string never fails");

        assert_eq!(reused, baseline);
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
    fn segments_match_rendered_output() {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let segments = message.as_segments(&mut scratch, true);

        let mut aggregated = Vec::new();
        for slice in segments.as_slices() {
            aggregated.extend_from_slice(slice.as_ref());
        }

        assert_eq!(aggregated, message.to_line_bytes().unwrap());
        assert_eq!(segments.len(), aggregated.len());
        assert!(segments.segment_count() > 1);
    }

    #[test]
    fn segments_handle_messages_without_optional_fields() {
        let message = Message::info("protocol handshake complete");
        let mut scratch = MessageScratch::new();
        let segments = message.as_segments(&mut scratch, false);

        let mut combined = Vec::new();
        for slice in segments.as_slices() {
            combined.extend_from_slice(slice.as_ref());
        }

        assert_eq!(combined, message.to_bytes().unwrap());
        assert_eq!(segments.segment_count(), segments.as_slices().len());
        assert!(!segments.is_empty());
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

    #[test]
    fn to_bytes_matches_display_output() {
        let message = Message::error(11, "read failure")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");
        let expected = message.to_string().into_bytes();

        assert_eq!(rendered, expected);
    }

    #[test]
    fn byte_len_matches_rendered_length() {
        let message = Message::error(35, "timeout waiting for daemon connection")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let rendered = message.to_bytes().expect("Vec<u8> writes are infallible");

        assert_eq!(message.byte_len(), rendered.len());
    }

    #[test]
    fn to_line_bytes_appends_newline() {
        let message = Message::warning("vanished")
            .with_code(24)
            .with_source(message_source!());

        let rendered = message
            .to_line_bytes()
            .expect("Vec<u8> writes are infallible");
        let expected = {
            let mut buf = message.to_string().into_bytes();
            buf.push(b'\n');
            buf
        };

        assert_eq!(rendered, expected);
    }

    #[test]
    fn line_byte_len_matches_rendered_length() {
        let message = Message::warning("some files vanished")
            .with_code(24)
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let rendered = message
            .to_line_bytes()
            .expect("Vec<u8> writes are infallible");

        assert_eq!(message.line_byte_len(), rendered.len());
    }

    #[test]
    fn append_to_vec_matches_to_bytes() {
        let message = Message::error(23, "delta-transfer failure")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut buffer = Vec::new();
        message
            .append_to_vec(&mut buffer)
            .expect("Vec<u8> growth should succeed for small messages");

        assert_eq!(buffer, message.to_bytes().unwrap());
    }

    #[test]
    fn append_line_to_vec_matches_to_line_bytes() {
        let message = Message::warning("vanished")
            .with_code(24)
            .with_source(message_source!());

        let mut buffer = Vec::new();
        message
            .append_line_to_vec(&mut buffer)
            .expect("Vec<u8> growth should succeed for small messages");

        assert_eq!(buffer, message.to_line_bytes().unwrap());
    }

    #[test]
    fn append_with_scratch_accumulates_messages() {
        let message = Message::error(11, "read failure")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut buffer = Vec::new();
        message
            .append_to_vec_with_scratch(&mut scratch, &mut buffer)
            .expect("Vec<u8> growth should succeed for small messages");
        let first_len = buffer.len();
        let without_newline = message.to_bytes().unwrap();

        message
            .append_line_to_vec_with_scratch(&mut scratch, &mut buffer)
            .expect("Vec<u8> growth should succeed for small messages");
        let with_newline = message
            .to_line_bytes()
            .expect("Vec<u8> writes are infallible");

        assert_eq!(&buffer[..first_len], without_newline.as_slice());
        assert_eq!(&buffer[first_len..], with_newline.as_slice());
    }

    #[test]
    fn to_bytes_with_scratch_matches_standard_rendering() {
        let message = Message::info("protocol handshake complete").with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let reused = message
            .to_line_bytes_with_scratch(&mut scratch)
            .expect("Vec<u8> writes are infallible");

        let baseline = message
            .to_line_bytes()
            .expect("Vec<u8> writes are infallible");

        assert_eq!(reused, baseline);
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
    fn render_to_writer_uses_thread_local_scratch_per_thread() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let message = Message::error(42, "per-thread scratch")
            .with_role(Role::Sender)
            .with_source(message_source!());
        let barrier = Arc::new(Barrier::new(4));

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let message = message.clone();

                thread::spawn(move || {
                    barrier.wait();
                    let expected = message.to_string().into_bytes();

                    for _ in 0..64 {
                        let mut buffer = Vec::new();
                        message
                            .render_to_writer(&mut buffer)
                            .expect("Vec<u8> writes are infallible");

                        assert_eq!(buffer, expected);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }
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
    fn render_to_writer_skips_vectored_when_writer_does_not_support_it() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Receiver)
            .with_source(untracked_source());

        let expected = message.to_string();

        let mut writer = RecordingWriter::without_vectored();
        message
            .render_to_writer(&mut writer)
            .expect("sequential write succeeds");

        assert_eq!(writer.vectored_calls, 0, "vectored writes must be skipped");
        assert!(
            writer.write_calls > 0,
            "sequential path should handle the message"
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

        assert!(
            writer.vectored_calls >= 1,
            "vectored path should be attempted at least once"
        );
        assert!(
            writer.write_calls > 0,
            "sequential fallback must finish the message"
        );
        assert_eq!(String::from_utf8(writer.buffer).unwrap(), expected);
    }

    #[test]
    fn segments_as_ref_exposes_slice_view() {
        let mut scratch = MessageScratch::new();
        let message = Message::error(35, "timeout waiting for daemon connection")
            .with_role(Role::Sender)
            .with_source(untracked_source());

        let segments = message.as_segments(&mut scratch, false);
        let slices = segments.as_ref();

        assert_eq!(slices.len(), segments.segment_count());

        let flattened: Vec<u8> = slices
            .iter()
            .flat_map(|slice| slice.as_ref())
            .copied()
            .collect();

        assert_eq!(flattened, message.to_bytes().unwrap());
    }

    #[test]
    fn segments_into_iter_collects_bytes() {
        let mut scratch = MessageScratch::new();
        let message = Message::warning("some files vanished")
            .with_code(24)
            .with_source(untracked_source());

        let segments = message.as_segments(&mut scratch, true);
        let mut flattened = Vec::new();

        for slice in segments.clone() {
            flattened.extend_from_slice(slice.as_ref());
        }

        assert_eq!(flattened, message.to_line_bytes().unwrap());
    }

    #[test]
    fn segments_into_iter_respects_segment_count() {
        let mut scratch = MessageScratch::new();
        let message = Message::info("protocol negotiation complete");

        let segments = message.as_segments(&mut scratch, false);
        let iter = segments.clone().into_iter();

        assert_eq!(iter.count(), segments.segment_count());
    }

    struct RecordingWriter {
        buffer: Vec<u8>,
        vectored_calls: usize,
        write_calls: usize,
        vectored_limit: Option<usize>,
        supports_vectored: bool,
    }

    impl RecordingWriter {
        fn new() -> Self {
            Self {
                buffer: Vec::new(),
                vectored_calls: 0,
                write_calls: 0,
                vectored_limit: None,
                supports_vectored: true,
            }
        }

        fn with_vectored_limit(limit: usize) -> Self {
            let mut writer = Self::new();
            writer.vectored_limit = Some(limit);
            writer
        }

        fn without_vectored() -> Self {
            let mut writer = Self::new();
            writer.supports_vectored = false;
            writer
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
            if !self.supports_vectored {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "vectored writes unsupported",
                ));
            }
            self.vectored_calls += 1;

            let mut to_write: usize = bufs.iter().map(|slice| slice.len()).sum();
            if let Some(limit) = self.vectored_limit {
                let capped = to_write.min(limit);
                self.vectored_limit = Some(limit.saturating_sub(capped));
                to_write = capped;

                if to_write == 0 {
                    self.supports_vectored = false;
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "vectored limit reached",
                    ));
                }
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
    fn severity_prefix_matches_expected_strings() {
        assert_eq!(Severity::Info.prefix(), "rsync info: ");
        assert_eq!(Severity::Warning.prefix(), "rsync warning: ");
        assert_eq!(Severity::Error.prefix(), "rsync error: ");
    }

    #[test]
    fn severity_display_matches_as_str() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[test]
    fn severity_predicates_match_variants() {
        assert!(Severity::Info.is_info());
        assert!(!Severity::Info.is_warning());
        assert!(!Severity::Info.is_error());

        assert!(Severity::Warning.is_warning());
        assert!(!Severity::Warning.is_info());
        assert!(!Severity::Warning.is_error());

        assert!(Severity::Error.is_error());
        assert!(!Severity::Error.is_info());
        assert!(!Severity::Error.is_warning());
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
    fn rsync_error_macro_honors_track_caller() {
        let expected_line = line!() + 1;
        let message = tracked_rsync_error_macro();
        let source = message.source().expect("macro records source location");

        assert_eq!(source.line(), expected_line);
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
    fn rsync_warning_macro_honors_track_caller() {
        let expected_line = line!() + 1;
        let message = tracked_rsync_warning_macro();
        let source = message.source().expect("macro records source location");

        assert_eq!(source.line(), expected_line);
        assert!(source.path().ends_with("crates/core/src/message.rs"));
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
    fn rsync_info_macro_honors_track_caller() {
        let expected_line = line!() + 1;
        let message = tracked_rsync_info_macro();
        let source = message.source().expect("macro records source location");

        assert_eq!(source.line(), expected_line);
        assert!(source.path().ends_with("crates/core/src/message.rs"));
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

    #[test]
    fn append_normalized_os_str_handles_unc_prefixes() {
        let mut rendered = String::new();
        append_normalized_os_str(&mut rendered, OsStr::new(r"\\server\share\path"));

        assert_eq!(rendered, "//server/share/path");
    }

    #[test]
    fn append_normalized_os_str_preserves_trailing_backslash() {
        let mut rendered = String::new();
        append_normalized_os_str(&mut rendered, OsStr::new(r#"C:\path\to\dir\"#));

        assert_eq!(rendered, "C:/path/to/dir/");
    }

    #[derive(Default)]
    struct TrackingWriter {
        written: Vec<u8>,
        vectored_calls: usize,
        unsupported_once: bool,
        always_unsupported: bool,
        vectored_limit: Option<usize>,
    }

    impl TrackingWriter {
        fn with_unsupported_once() -> Self {
            Self {
                unsupported_once: true,
                ..Self::default()
            }
        }

        fn with_always_unsupported() -> Self {
            Self {
                always_unsupported: true,
                ..Self::default()
            }
        }

        fn with_vectored_limit(limit: usize) -> Self {
            Self {
                vectored_limit: Some(limit),
                ..Self::default()
            }
        }
    }

    impl io::Write for TrackingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_calls += 1;

            if self.unsupported_once {
                self.unsupported_once = false;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no vectored support",
                ));
            }

            if self.always_unsupported {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no vectored support",
                ));
            }

            let mut limit = self.vectored_limit.unwrap_or(usize::MAX);
            let mut total = 0usize;
            for buf in bufs {
                if limit == 0 {
                    break;
                }

                let slice = buf.as_ref();
                let take = slice.len().min(limit);
                self.written.extend_from_slice(&slice[..take]);
                total += take;
                limit -= take;

                if take < slice.len() {
                    break;
                }
            }

            Ok(total)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct PartialThenUnsupportedWriter {
        written: Vec<u8>,
        vectored_calls: usize,
        fallback_writes: usize,
        limit: usize,
    }

    impl PartialThenUnsupportedWriter {
        fn new(limit: usize) -> Self {
            Self {
                limit,
                ..Self::default()
            }
        }
    }

    impl io::Write for PartialThenUnsupportedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.fallback_writes += 1;
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            self.vectored_calls += 1;

            if self.vectored_calls == 1 {
                let mut limit = self.limit;
                let mut total = 0usize;

                for buf in bufs {
                    if limit == 0 {
                        break;
                    }

                    let slice = buf.as_ref();
                    let take = slice.len().min(limit);
                    self.written.extend_from_slice(&slice[..take]);
                    total += take;
                    limit -= take;

                    if take < slice.len() {
                        break;
                    }
                }

                return Ok(total);
            }

            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "vectored disabled after first call",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct ZeroProgressWriter {
        write_calls: usize,
    }

    impl io::Write for ZeroProgressWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_calls += 1;
            Ok(buf.len())
        }

        fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn segments_write_to_prefers_vectored_io() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = TrackingWriter::default();

        {
            let segments = message.as_segments(&mut scratch, true);
            segments
                .write_to(&mut writer)
                .expect("writing into a vector never fails");
        }

        assert_eq!(writer.written, message.to_line_bytes().unwrap());
        assert!(writer.vectored_calls >= 1);
    }

    #[test]
    fn segments_write_to_skips_vectored_for_single_segment() {
        let message = Message::info("");
        let mut scratch = MessageScratch::new();
        let segments = message.as_segments(&mut scratch, false);

        assert_eq!(segments.segment_count(), 1);

        let mut writer = RecordingWriter::new();
        segments
            .write_to(&mut writer)
            .expect("single-segment writes succeed");

        assert_eq!(writer.vectored_calls, 0, "vectored path should be skipped");
        assert_eq!(writer.write_calls, 1, "single write_all call expected");
        assert_eq!(writer.buffer, message.to_bytes().unwrap());
    }

    #[test]
    fn segments_write_to_falls_back_after_unsupported_vectored_call() {
        let message = Message::error(30, "timeout in data send/receive")
            .with_role(Role::Receiver)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = TrackingWriter::with_unsupported_once();

        {
            let segments = message.as_segments(&mut scratch, false);
            segments
                .write_to(&mut writer)
                .expect("sequential fallback should succeed");
        }

        assert_eq!(writer.written, message.to_bytes().unwrap());
        assert_eq!(writer.vectored_calls, 1);
    }

    #[test]
    fn segments_write_to_handles_persistent_unsupported_vectored_calls() {
        let message = Message::error(124, "remote shell failed")
            .with_role(Role::Client)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = TrackingWriter::with_always_unsupported();

        {
            let segments = message.as_segments(&mut scratch, false);
            segments
                .write_to(&mut writer)
                .expect("sequential fallback should succeed");
        }

        assert_eq!(writer.written, message.to_bytes().unwrap());
        assert_eq!(writer.vectored_calls, 1);
    }

    #[test]
    fn segments_write_to_retries_after_partial_vectored_write() {
        let message = Message::error(35, "protocol generator aborted")
            .with_role(Role::Generator)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = TrackingWriter::with_vectored_limit(8);

        {
            let segments = message.as_segments(&mut scratch, true);
            segments
                .write_to(&mut writer)
                .expect("partial vectored writes should succeed");
        }

        assert_eq!(writer.written, message.to_line_bytes().unwrap());
        assert!(writer.vectored_calls >= 2);
    }

    #[test]
    fn segments_write_to_handles_partial_then_unsupported_vectored_call() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = PartialThenUnsupportedWriter::new(8);

        {
            let segments = message.as_segments(&mut scratch, false);
            segments
                .write_to(&mut writer)
                .expect("sequential fallback should succeed after partial vectored writes");
        }

        assert_eq!(writer.written, message.to_bytes().unwrap());
        assert_eq!(writer.vectored_calls, 2);
        assert!(writer.fallback_writes >= 1);
    }

    #[test]
    fn segments_write_to_handles_cross_slice_progress_before_unsupported_vectored_call() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = PartialThenUnsupportedWriter::new(18);

        {
            let segments = message.as_segments(&mut scratch, false);
            segments
                .write_to(&mut writer)
                .expect("sequential fallback should succeed after cross-slice progress");
        }

        assert_eq!(writer.written, message.to_bytes().unwrap());
        assert_eq!(writer.vectored_calls, 2);
        assert!(writer.fallback_writes >= 1);
    }

    #[test]
    fn segments_write_to_errors_when_vectored_makes_no_progress() {
        let message = Message::error(11, "error in file IO")
            .with_role(Role::Sender)
            .with_source(message_source!());

        let mut scratch = MessageScratch::new();
        let mut writer = ZeroProgressWriter::default();

        let err = {
            let segments = message.as_segments(&mut scratch, false);
            segments
                .write_to(&mut writer)
                .expect_err("zero-length vectored write must error")
        };

        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
        assert_eq!(writer.write_calls, 0, "sequential writes should not run");
    }
}
