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
//!   a simplified subset of upstream behaviour by copying files and directories
//!   on the local filesystem without delta compression or metadata
//!   preservation.
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

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
/// directories and regular files. Metadata preservation, delta compression, and
/// remote transports remain unimplemented.
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
    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;
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
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("target.txt");
        fs::write(&target, b"payload").expect("write target");

        let link = tmp.path().join("link");
        unix_fs::symlink(&target, &link).expect("create link");

        let destination = tmp.path().join("copied");
        let config = ClientConfig::builder()
            .transfer_args([link.clone(), destination.clone()])
            .build();

        run_client(config).expect("copy symlink succeeds");

        let copied_target = fs::read_link(&destination).expect("read copied link");
        assert_eq!(copied_target, target);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_overwrites_existing_symbolic_link() {
        let tmp = tempdir().expect("tempdir");
        let original_target = tmp.path().join("original.txt");
        let new_target = tmp.path().join("new.txt");
        fs::write(&original_target, b"original").expect("write original");
        fs::write(&new_target, b"new").expect("write new");

        let source_link = tmp.path().join("source-link");
        unix_fs::symlink(&new_target, &source_link).expect("create source link");

        let destination_link = tmp.path().join("dest-link");
        unix_fs::symlink(&original_target, &destination_link).expect("create dest link");

        let config = ClientConfig::builder()
            .transfer_args([source_link.clone(), destination_link.clone()])
            .build();

        run_client(config).expect("overwrite symlink succeeds");

        let copied_target = fs::read_link(&destination_link).expect("read overwritten link");
        assert_eq!(copied_target, new_target);
    }
}

/// Transfer specification derived from parsed command-line arguments.
#[derive(Debug)]
struct TransferSpec {
    sources: Vec<PathBuf>,
    destination: PathBuf,
}

impl TransferSpec {
    fn from_args(args: &[OsString]) -> Result<Self, ClientError> {
        if args.len() < 2 {
            return Err(missing_operands_error());
        }

        let sources: Vec<PathBuf> = args[..args.len() - 1]
            .iter()
            .map(|arg| PathBuf::from(arg))
            .collect();
        let destination = PathBuf::from(&args[args.len() - 1]);

        if sources.iter().any(|source| source.as_os_str().is_empty()) {
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
        let metadata = fs::symlink_metadata(source)
            .map_err(|error| io_error("access source", source, error))?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            let target = if destination_state.is_dir || multiple_sources {
                let name = source.file_name().ok_or_else(|| {
                    invalid_argument_error(
                        "cannot determine directory name",
                        PARTIAL_TRANSFER_EXIT_CODE,
                    )
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            ensure_directory_target_is_valid(source, &target)?;
            copy_directory_recursive(source, &target)?;
        } else if file_type.is_file() {
            let target = if destination_state.is_dir {
                let name = source.file_name().ok_or_else(|| {
                    invalid_argument_error("cannot determine file name", PARTIAL_TRANSFER_EXIT_CODE)
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            copy_file(source, &target)?;
        } else if file_type.is_symlink() {
            let target = if destination_state.is_dir {
                let name = source.file_name().ok_or_else(|| {
                    invalid_argument_error(
                        "cannot determine symbolic link name",
                        PARTIAL_TRANSFER_EXIT_CODE,
                    )
                })?;
                spec.destination.join(name)
            } else {
                spec.destination.clone()
            };

            copy_symlink(source, &target, file_type.is_dir())?;
        } else {
            return Err(invalid_argument_error(
                "unsupported file type encountered",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }
    }

    Ok(())
}

fn ensure_directory_target_is_valid(source: &Path, destination: &Path) -> Result<(), ClientError> {
    let source_abs = fs::canonicalize(source)
        .map_err(|error| io_error("canonicalize source directory", source, error))?;
    let destination_abs = absolute_path_allowing_missing(destination)?;

    if destination_abs.starts_with(&source_abs) {
        let text = format!(
            "refusing to copy directory '{src}' into '{dst}' because the destination is inside the source",
            src = source.display(),
            dst = destination.display(),
        );
        return Err(invalid_argument_error(&text, PARTIAL_TRANSFER_EXIT_CODE));
    }

    Ok(())
}

fn absolute_path_allowing_missing(path: &Path) -> Result<PathBuf, ClientError> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                let cwd = env::current_dir().map_err(|cwd_error| {
                    io_error("determine current directory", Path::new("."), cwd_error)
                })?;
                Ok(cwd.join(path))
            }
        }
        Err(error) => Err(io_error("canonicalize destination", path, error)),
    }
}

fn copy_directory_recursive(source: &Path, destination: &Path) -> Result<(), ClientError> {
    fs::create_dir_all(destination)
        .map_err(|error| io_error("create directory", destination, error))?;

    let entries =
        fs::read_dir(source).map_err(|error| io_error("read directory", source, error))?;

    for entry in entries {
        let entry = entry.map_err(|error| io_error("read directory entry", source, error))?;
        let entry_path = entry.path();
        let entry_type = entry
            .file_type()
            .map_err(|error| io_error("inspect directory entry", &entry_path, error))?;
        let target_path = destination.join(entry.file_name());

        if entry_type.is_dir() {
            copy_directory_recursive(&entry_path, &target_path)?;
        } else if entry_type.is_file() {
            copy_file(&entry_path, &target_path)?;
        } else if entry_type.is_symlink() {
            copy_symlink(&entry_path, &target_path, entry_type.is_dir())?;
        } else {
            return Err(invalid_argument_error(
                "unsupported file type encountered",
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
        }
    }

    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), ClientError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create parent directory", parent, error))?;
        }
    }

    fs::copy(source, destination).map_err(|error| io_error("copy file", source, error))?;
    Ok(())
}

fn copy_symlink(
    source: &Path,
    destination: &Path,
    source_is_directory: bool,
) -> Result<(), ClientError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create parent directory", parent, error))?;
        }
    }

    let target =
        fs::read_link(source).map_err(|error| io_error("read symbolic link", source, error))?;

    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            let file_type = metadata.file_type();

            if file_type.is_symlink() {
                match fs::remove_file(destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::IsADirectory => {
                        fs::remove_dir(destination).map_err(|error| {
                            io_error(
                                "remove existing directory symbolic link",
                                destination,
                                error,
                            )
                        })?;
                    }
                    Err(error) => {
                        return Err(io_error("remove existing file", destination, error));
                    }
                }
            } else if file_type.is_dir() {
                return Err(invalid_argument_error(
                    "cannot replace existing directory with symbolic link",
                    PARTIAL_TRANSFER_EXIT_CODE,
                ));
            } else {
                fs::remove_file(destination)
                    .map_err(|error| io_error("remove existing file", destination, error))?;
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io_error("inspect destination", destination, error));
        }
    }

    create_symlink(&target, destination, source_is_directory)
        .map_err(|error| io_error("create symbolic link", destination, error))?;

    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, destination: &Path, _source_is_directory: bool) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    symlink(target, destination)
}

#[cfg(windows)]
fn create_symlink(target: &Path, destination: &Path, source_is_directory: bool) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    if source_is_directory {
        symlink_dir(target, destination)
    } else {
        symlink_file(target, destination)
    }
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(
    _target: &Path,
    _destination: &Path,
    _source_is_directory: bool,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Other,
        "symbolic links are not supported on this platform",
    ))
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
