#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `client` module exposes the orchestration entry points consumed by the
//! `oc-rsync` CLI binary. The current implementation focuses on providing a
//! deterministic, synchronous local copy engine that mirrors the high-level
//! behaviour of `rsync SOURCE DEST` when no remote shells or daemons are
//! involved. The API models the configuration and error structures that higher
//! layers will reuse once network transports and the full delta-transfer engine
//! land.
//!
//! # Design
//!
//! - [`ClientConfig`] encapsulates the caller-provided transfer arguments. A
//!   builder is offered so future options (e.g. logging verbosity) can be wired
//!   through without breaking call sites.
//! - [`run_client`] executes the client flow. For now the implementation mirrors
//!   a simplified subset of upstream behaviour by copying files, directories,
//!   and symbolic links on the local filesystem while preserving permissions and
//!   timestamps, but without delta compression or advanced metadata handling
//!   such as ownership, ACLs, or extended attributes.
//! - [`ClientError`] carries the exit code and fully formatted
//!   [`Message`](crate::message::Message) so binaries can surface diagnostics via
//!   the central rendering helpers.
//!
//! # Invariants
//!
//! - `ClientError::exit_code` always matches the exit code embedded in the
//!   [`Message`].
//! - `run_client` never panics and preserves the provided configuration even
//!   when reporting unsupported functionality.
//!
//! # Errors
//!
//! All failures are routed through [`ClientError`]. The structure implements
//! [`std::error::Error`], allowing integration with higher-level error handling
//! stacks without losing access to the formatted diagnostic.
//!
//! # Examples
//!
//! Running the client with a single source copies the file into the destination
//! path. The helper currently operates entirely on the local filesystem.
//!
//! ```
//! use rsync_core::client::{run_client, ClientConfig};
//! use std::fs;
//! use tempfile::tempdir;
//!
//! let temp = tempdir().unwrap();
//! let source = temp.path().join("source.txt");
//! let destination = temp.path().join("dest.txt");
//! fs::write(&source, b"example").unwrap();
//!
//! let config = ClientConfig::builder()
//!     .transfer_args([source.clone(), destination.clone()])
//!     .build();
//!
//! run_client(config).expect("local copy succeeds");
//! assert_eq!(fs::read(&destination).unwrap(), b"example");
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the formatting utilities reused by the client
//!   orchestration.
//! - [`crate::version`] for the canonical version banner shared with the CLI.

use std::cmp::Ordering;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rsync_meta::{
    MetadataError, apply_directory_metadata, apply_file_metadata, apply_symlink_metadata,
};

use crate::{
    message::{Message, Role},
    rsync_error,
};

/// Exit code returned when client functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code used when a copy partially or wholly fails.
const PARTIAL_TRANSFER_EXIT_CODE: i32 = 23;

/// Configuration describing the requested client operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfig {
    transfer_args: Vec<OsString>,
}

impl ClientConfig {
    /// Creates a new [`ClientConfigBuilder`].
    #[must_use]
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }
}

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
}

impl ClientConfigBuilder {
    /// Sets the transfer arguments that should be propagated to the engine.
    #[must_use]
    pub fn transfer_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.transfer_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
        }
    }
}

/// Error returned when the client orchestration fails.
#[derive(Clone, Debug)]
pub struct ClientError {
    exit_code: i32,
    message: Message,
}

impl ClientError {
    /// Creates a new [`ClientError`] from the supplied message.
    fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    #[must_use]
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for ClientError {}

/// Runs the client orchestration using the provided configuration.
///
/// The current implementation offers best-effort local copies covering
/// directories, regular files, and symbolic links. Metadata preservation, delta
/// compression, and remote transports remain unimplemented.
pub fn run_client(config: ClientConfig) -> Result<(), ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    let spec = TransferSpec::from_args(config.transfer_args())?;
    copy_sources(&spec)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn builder_collects_transfer_arguments() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("source"), OsString::from("dest")])
            .build();

        assert_eq!(
            config.transfer_args(),
            &[OsString::from("source"), OsString::from("dest")]
        );
        assert!(config.has_transfer_request());
    }

    #[test]
    fn run_client_reports_missing_operands() {
        let config = ClientConfig::builder().build();
        let error = run_client(config).expect_err("missing operands should error");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("missing source operands"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn run_client_copies_single_file() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        fs::write(&source, b"example").expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .build();

        run_client(config).expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), b"example");
    }

    #[test]
    fn read_directory_entries_sorted_orders_entries() {
        let tmp = tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir(root.join("b_dir")).expect("create b_dir");
        fs::create_dir(root.join("a_dir")).expect("create a_dir");
        fs::write(root.join("c.txt"), b"c").expect("write c");
        fs::write(root.join("a.txt"), b"a").expect("write a");

        let entries = read_directory_entries_sorted(root).expect("read entries");
        let names: Vec<OsString> = entries.into_iter().map(|entry| entry.file_name).collect();

        let expected = vec![
            OsString::from("a.txt"),
            OsString::from("a_dir"),
            OsString::from("b_dir"),
            OsString::from("c.txt"),
        ];

        assert_eq!(names, expected);
    }

    #[test]
    fn run_client_copies_directory_tree() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        let source_file = nested.join("file.txt");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(&source_file, b"tree").expect("write source file");

        let dest_root = tmp.path().join("destination");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .build();

        run_client(config).expect("directory copy succeeds");

        let copied_file = dest_root.join("nested").join("file.txt");
        assert_eq!(fs::read(copied_file).expect("read copied"), b"tree");
    }

    #[cfg(unix)]
    #[test]
    fn run_client_copies_symbolic_link() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let target_file = tmp.path().join("target.txt");
        fs::write(&target_file, b"symlink target").expect("write target");

        let source_link = tmp.path().join("source-link");
        symlink(&target_file, &source_link).expect("create source symlink");

        let destination_link = tmp.path().join("dest-link");
        let config = ClientConfig::builder()
            .transfer_args([source_link.clone(), destination_link.clone()])
            .build();

        run_client(config).expect("link copy succeeds");

        let copied = fs::read_link(destination_link).expect("read copied link");
        assert_eq!(copied, target_file);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_symbolic_links_in_directories() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");

        let target_file = tmp.path().join("target.txt");
        fs::write(&target_file, b"data").expect("write target");
        let link_path = nested.join("link");
        symlink(&target_file, &link_path).expect("create link");

        let dest_root = tmp.path().join("destination");
        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .build();

        run_client(config).expect("directory copy succeeds");

        let copied_link = dest_root.join("nested").join("link");
        let copied_target = fs::read_link(copied_link).expect("read copied link");
        assert_eq!(copied_target, target_file);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_file_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-metadata.txt");
        let destination = tmp.path().join("dest-metadata.txt");
        fs::write(&source, b"metadata").expect("write source");

        let mode = 0o640;
        fs::set_permissions(&source, PermissionsExt::from_mode(mode))
            .expect("set source permissions");
        let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
        set_file_times(&source, atime, mtime).expect("set source timestamps");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .build();

        run_client(config).expect("copy succeeds");

        let dest_metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_directory_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source-dir");
        fs::create_dir(&source_dir).expect("create source dir");

        let mode = 0o751;
        fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
            .expect("set directory permissions");
        let atime = FileTime::from_unix_time(1_700_010_000, 0);
        let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
        set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

        let destination_dir = tmp.path().join("dest-dir");
        let config = ClientConfig::builder()
            .transfer_args([source_dir.clone(), destination_dir.clone()])
            .build();

        run_client(config).expect("directory copy succeeds");

        let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
        assert!(dest_metadata.is_dir());
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
    }

    #[test]
    fn run_client_merges_directory_contents_when_trailing_separator_present() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        let file_path = nested.join("file.txt");
        fs::write(&file_path, b"contents").expect("write file");

        let dest_root = tmp.path().join("dest");
        let mut source_arg = source_root.clone().into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .build();

        run_client(config).expect("directory contents copy succeeds");

        assert!(dest_root.is_dir());
        assert!(dest_root.join("nested").is_dir());
        assert_eq!(
            fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
            b"contents"
        );
        assert!(!dest_root.join("source").exists());
    }
}

/// Transfer specification derived from parsed command-line arguments.
#[derive(Debug)]
struct TransferSpec {
    sources: Vec<SourceSpec>,
    destination: PathBuf,
}

impl TransferSpec {
    fn from_args(args: &[OsString]) -> Result<Self, ClientError> {
        if args.len() < 2 {
            return Err(missing_operands_error());
        }

        let sources: Vec<SourceSpec> = args[..args.len() - 1]
            .iter()
            .map(SourceSpec::from_argument)
            .collect();
        let destination = PathBuf::from(&args[args.len() - 1]);

        if sources
            .iter()
            .any(|source| source.path.as_os_str().is_empty())
        {
            return Err(invalid_argument_error(
                "source operands must be non-empty",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }

        if destination.as_os_str().is_empty() {
            return Err(invalid_argument_error(
                "destination operand must be non-empty",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }

        Ok(Self {
            sources,
            destination,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DestinationState {
    exists: bool,
    is_dir: bool,
}

#[derive(Clone, Debug)]
struct SourceSpec {
    path: PathBuf,
    copy_contents: bool,
}

impl SourceSpec {
    fn from_argument(argument: &OsString) -> Self {
        let copy_contents = has_trailing_separator(argument.as_os_str());
        Self {
            path: PathBuf::from(argument),
            copy_contents,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn copy_contents(&self) -> bool {
        self.copy_contents
    }
}

fn has_trailing_separator(path: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_bytes();
        return !bytes.is_empty() && bytes.ends_with(&[b'/']);
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        if let Some(ch) = path.encode_wide().rev().find(|&ch| ch != 0) {
            return ch == b'/' as u16 || ch == b'\\' as u16;
        }
        return false;
    }

    #[cfg(not(any(unix, windows)))]
    {
        let text = path.to_string_lossy();
        return text.ends_with('/') || text.ends_with('\\');
    }
}

fn query_destination_state(path: &Path) -> Result<DestinationState, ClientError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::default()),
        Err(error) => Err(io_error("inspect destination", path, error)),
    }
}

fn copy_sources(spec: &TransferSpec) -> Result<(), ClientError> {
    let multiple_sources = spec.sources.len() > 1;
    let mut destination_state = query_destination_state(&spec.destination)?;

    if multiple_sources {
        if destination_state.exists && !destination_state.is_dir {
            return Err(invalid_argument_error(
                "destination must be an existing directory when copying multiple sources",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }

        if !destination_state.exists {
            fs::create_dir_all(&spec.destination).map_err(|error| {
                io_error("create destination directory", &spec.destination, error)
            })?;
            destination_state.exists = true;
            destination_state.is_dir = true;
        }
    }

    for source in &spec.sources {
        let source_path = source.path();
        let metadata = fs::symlink_metadata(source_path)
            .map_err(|error| io_error("access source", source_path, error))?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            let target = if source.copy_contents() {
                spec.destination.clone()
            } else if destination_state.is_dir || multiple_sources {
                let name = source_path.file_name().ok_or_else(|| {
                    invalid_argument_error(
                        "cannot determine directory name",
                        PARTIAL_TRANSFER_EXIT_CODE,
                    )
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            copy_directory_recursive(source_path, &target, &metadata)?;
        } else if file_type.is_file() {
            let target = if destination_state.is_dir {
                let name = source_path.file_name().ok_or_else(|| {
                    invalid_argument_error("cannot determine file name", PARTIAL_TRANSFER_EXIT_CODE)
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            copy_file(source_path, &target, &metadata)?;
        } else if file_type.is_symlink() {
            let target = if destination_state.is_dir {
                let name = source_path.file_name().ok_or_else(|| {
                    invalid_argument_error("cannot determine link name", PARTIAL_TRANSFER_EXIT_CODE)
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            copy_symlink(source_path, &target, &metadata)?;
        } else {
            return Err(invalid_argument_error(
                "unsupported file type encountered",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }
    }

    Ok(())
}

fn copy_directory_recursive(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ClientError> {
    let mut destination_preexisted = false;

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            if !existing.file_type().is_dir() {
                return Err(invalid_argument_error(
                    "cannot replace non-directory destination with directory",
                    PARTIAL_TRANSFER_EXIT_CODE,
                ));
            }
            destination_preexisted = true;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(destination)
                .map_err(|error| io_error("create directory", destination, error))?;
        }
        Err(error) => {
            return Err(io_error(
                "inspect destination directory",
                destination,
                error,
            ));
        }
    }

    let entries = read_directory_entries_sorted(source)?;

    for entry in entries {
        let DirectoryEntry {
            file_name,
            path: entry_path,
            metadata: entry_metadata,
        } = entry;
        let entry_type = entry_metadata.file_type();
        let target_path = destination.join(Path::new(&file_name));

        if entry_type.is_dir() {
            copy_directory_recursive(&entry_path, &target_path, &entry_metadata)?;
        } else if entry_type.is_file() {
            copy_file(&entry_path, &target_path, &entry_metadata)?;
        } else if entry_type.is_symlink() {
            copy_symlink(&entry_path, &target_path, &entry_metadata)?;
        } else {
            return Err(invalid_argument_error(
                "unsupported file type encountered",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }
    }

    if !destination_preexisted {
        apply_directory_metadata(destination, metadata).map_err(map_metadata_error)?;
    }

    Ok(())
}

#[derive(Debug)]
struct DirectoryEntry {
    file_name: OsString,
    path: PathBuf,
    metadata: fs::Metadata,
}

fn read_directory_entries_sorted(path: &Path) -> Result<Vec<DirectoryEntry>, ClientError> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path).map_err(|error| io_error("read directory", path, error))?;

    for entry in read_dir {
        let entry = entry.map_err(|error| io_error("read directory entry", path, error))?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)
            .map_err(|error| io_error("inspect directory entry", &entry_path, error))?;
        entries.push(DirectoryEntry {
            file_name: entry.file_name(),
            path: entry_path,
            metadata,
        });
    }

    entries.sort_by(|a, b| compare_file_names(&a.file_name, &b.file_name));
    Ok(entries)
}

fn compare_file_names(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        return left.as_bytes().cmp(right.as_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let left_wide: Vec<u16> = left.encode_wide().collect();
        let right_wide: Vec<u16> = right.encode_wide().collect();
        return left_wide.cmp(&right_wide);
    }

    #[cfg(not(any(unix, windows)))]
    {
        return left.to_string_lossy().cmp(&right.to_string_lossy());
    }
}

fn copy_file(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ClientError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create parent directory", parent, error))?;
        }
    }

    fs::copy(source, destination).map_err(|error| io_error("copy file", source, error))?;
    apply_file_metadata(destination, metadata).map_err(map_metadata_error)?;
    Ok(())
}

fn copy_symlink(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ClientError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create parent directory", parent, error))?;
        }
    }

    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            let file_type = metadata.file_type();

            if file_type.is_symlink() {
                if file_type.is_dir() {
                    fs::remove_dir(destination).map_err(|error| {
                        io_error("remove existing destination", destination, error)
                    })?;
                } else {
                    fs::remove_file(destination).map_err(|error| {
                        io_error("remove existing destination", destination, error)
                    })?;
                }
            } else if file_type.is_dir() {
                return Err(invalid_argument_error(
                    "cannot replace existing directory with symbolic link",
                    PARTIAL_TRANSFER_EXIT_CODE,
                ));
            } else {
                fs::remove_file(destination)
                    .map_err(|error| io_error("remove existing destination", destination, error))?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_error("inspect existing destination", destination, error));
        }
    }

    let target =
        fs::read_link(source).map_err(|error| io_error("read symbolic link", source, error))?;

    create_symlink(&target, source, destination)
        .map_err(|error| io_error("create symbolic link", destination, error))?;

    apply_symlink_metadata(destination, metadata).map_err(map_metadata_error)?;

    Ok(())
}

fn map_metadata_error(error: MetadataError) -> ClientError {
    let (context, path, source) = error.into_parts();
    io_error(context, &path, source)
}

#[cfg(unix)]
fn create_symlink(target: &Path, _source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    symlink(target, destination)
}

#[cfg(windows)]
fn create_symlink(target: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    match source.metadata() {
        Ok(metadata) if metadata.file_type().is_dir() => symlink_dir(target, destination),
        Ok(_) => symlink_file(target, destination),
        Err(_) => symlink_file(target, destination),
    }
}

fn missing_operands_error() -> ClientError {
    let message = rsync_error!(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        "missing source operands: supply at least one source and a destination"
    )
    .with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn invalid_argument_error(text: &str, exit_code: i32) -> ClientError {
    let message = rsync_error!(exit_code, "{}", text).with_role(Role::Client);
    ClientError::new(exit_code, message)
}

fn io_error(action: &str, path: &Path, error: io::Error) -> ClientError {
    let text = format!(
        "failed to {action} '{path}': {error}",
        action = action,
        path = path.display(),
        error = error
    );
    let message = rsync_error!(PARTIAL_TRANSFER_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(PARTIAL_TRANSFER_EXIT_CODE, message)
}
