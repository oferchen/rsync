#![cfg(test)]

use super::*;
use crate::client::fallback::write_daemon_password;
use crate::fallback::CLIENT_FALLBACK_ENV;
use crate::version::RUST_VERSION;
use rsync_compress::zlib::CompressionLevel;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

#[cfg(unix)]
const CAPTURE_PASSWORD_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
cat > "$OUTPUT"
"#;

#[cfg(unix)]
const CAPTURE_ARGS_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
: > "$OUTPUT"
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  ;;
*)
  printf '%s\n' "$arg" >> "$OUTPUT"
  ;;
  esac
done
"#;

#[cfg(unix)]
fn capture_password_script() -> String {
    CAPTURE_PASSWORD_SCRIPT.to_string()
}

#[cfg(unix)]
fn capture_args_script() -> String {
    CAPTURE_ARGS_SCRIPT.to_string()
}

include!("part_01.rs");
include!("part_02.rs");
include!("part_03.rs");
include!("part_04.rs");
include!("part_05.rs");
include!("part_06.rs");
include!("part_07.rs");
include!("part_08.rs");
include!("part_09.rs");
