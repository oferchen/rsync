//! Operand parsing and classification.
//!
//! Determines whether CLI operands reference local or remote paths,
//! and resolves `--files-from` values into their source type.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Resolves a `--files-from` CLI value into a `FilesFromSource`.
///
/// The resolution mirrors upstream rsync's `options.c:2447-2490`:
/// - `"-"` means stdin
/// - `:path` (bare colon prefix) means a remote file opened by the server
///   using the host taken from the transfer operand
/// - `host:path` (hostspec form) means a remote file opened by the named
///   host (must match the transfer host, validated in `main.c:1420`)
/// - Otherwise, a local file read by the client
///
/// Only the last `--files-from` argument takes effect, matching upstream
/// behaviour where later options override earlier ones.
///
/// # Upstream Reference
///
/// - `options.c:2458` - `check_for_hostspec()` detects `host:path` prefix
/// - `options.c:2464-2465` - `files_from = p; filesfrom_host = h;`
/// - `options.c:2466-2469` - `:-` (remote stdin) is rejected
/// - `options.c:3112-3138` - `check_for_hostspec()` parses `host:path`
pub(crate) fn resolve_files_from_source(files_from: &[OsString]) -> core::client::FilesFromSource {
    use core::client::FilesFromSource;

    let last = match files_from.last() {
        Some(v) => v,
        None => return FilesFromSource::None,
    };

    let text = last.to_string_lossy();

    if text == "-" {
        return FilesFromSource::Stdin;
    }

    // Detect remote file: bare colon prefix `:path` (host taken from operand).
    if let Some(remote_path) = text.strip_prefix(':') {
        return FilesFromSource::RemoteFile(remote_path.to_owned());
    }

    // Detect remote file: `host:path` (upstream check_for_hostspec).
    // Reject Windows drive specs (`C:\...`), URLs (`rsync://`), and daemon
    // module specs (`host::module`). The latter is for daemon transfers,
    // which we do not currently support for files-from.
    if let Some((host, path)) = split_hostspec(&text) {
        // upstream: options.c:3112-3138 / options.c:2476-2483 -
        // check_for_hostspec strips the host prefix and forwards the path to
        // the remote server. When the host resolves to localhost AND the
        // stripped path is openable on this client, emit the hybrid variant.
        //
        // Classification cannot decide local-vs-wire here: it does not know
        // the transfer direction or whether the src operand is itself remote.
        // `HybridLocalRemote::resolve_for` makes that single-fd choice per
        // direction (PUSH opens the file locally; PULL stages its bytes and
        // forwards `--files-from=-`), so the hybrid variant is safe to emit
        // unconditionally for a localhost hostspec. Without it, PULL with
        // `--files-from=localhost:path` hangs at recv_filesfrom: the receiver
        // flushes None and the remote sender blocks waiting for the bytes.
        if host_is_localhost(host) {
            let local_path = PathBuf::from(path);
            if Path::new(&local_path).is_file() {
                return FilesFromSource::HybridLocalRemote {
                    local_path,
                    wire_arg: path.to_owned(),
                };
            }
        }
        return FilesFromSource::RemoteFile(path.to_owned());
    }

    FilesFromSource::LocalFile(PathBuf::from(last))
}

/// Returns `true` when the parsed hostspec names the local machine.
///
/// Matches upstream `main.c:1438 strcmp(filesfrom_host, shell_machine)`
/// semantics: a hostspec of `localhost` (case-insensitive) refers to this
/// client, allowing a hybrid local-open + wire-forward dispatch.
fn host_is_localhost(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
}

/// Splits a `host:path` argument into `(host, path)` if `text` is a remote
/// hostspec. Returns `None` for local paths, Windows drive letters, daemon
/// module specs (`::`), and URL forms.
///
/// Mirrors `options.c:3112-3138 check_for_hostspec()`:
/// - Reject URL forms (`rsync://`); those are daemon URLs, handled elsewhere
/// - Reject `host::module` (daemon module spec)
/// - Reject paths beginning with `/` (no host part)
/// - Reject Windows drive letters (`C:\foo`, `C:/foo`)
fn split_hostspec(text: &str) -> Option<(&str, &str)> {
    if text.starts_with('/') {
        return None;
    }
    if text.starts_with("rsync://") {
        return None;
    }

    let colon = text.find(':')?;
    let host = &text[..colon];
    let rest = &text[colon + 1..];

    // Reject daemon module spec `host::module`.
    if rest.starts_with(':') {
        return None;
    }

    // Reject Windows drive letter `C:\` / `C:/`.
    if host.len() == 1 && host.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }

    if host.is_empty() {
        return None;
    }

    // Reject `host:-` (upstream options.c:2466-2469 rejects remote stdin).
    if rest == "-" {
        return None;
    }

    Some((host, rest))
}

/// Determines whether the transfer involves any remote operands.
///
/// Returns `true` if any element in `remainder` (the CLI operands) or
/// `file_list` (entries from `--files-from`) appears to be a remote path.
#[cfg(test)]
pub(crate) fn transfer_requires_remote(
    remainder: &[OsString],
    file_list_operands: &[OsString],
) -> bool {
    remainder
        .iter()
        .chain(file_list_operands.iter())
        .any(|operand| operand_is_remote(operand.as_os_str()))
}

/// Returns `true` when an operand references a remote path.
///
/// Re-exported from [`engine::operand`], the single source of truth mirroring
/// upstream `options.c:check_for_hostspec()`. Recognises `rsync://`/`ssh://`
/// URLs, daemon module specs (`host::module`), and SSH hostspecs (`host:path`);
/// on Windows, native path prefixes (drive letters `C:\...`, UNC, `\\?\`,
/// `\\.\`) are local. One shared classifier keeps this front end, the core
/// remote-invocation planner, and the local-copy executor from ever disagreeing
/// on an operand (a #7153-class defect that shipped the wrong operand remote).
pub(crate) use engine::operand::operand_is_remote;
