//! A pull with `-s --iconv` and a local `--files-from` list accepts the names
//! the list requested.
//!
//! upstream: compat.c:820-827 converts a forwarded list only under `-s`
//! (`filesfrom_convert`); io.c:86 and io.c:512 call `add_implied_include()` on
//! each name before `iconvbufs()` converts it, so the implied rules stay in the
//! local charset. The receiver converts received names back to the local
//! charset before checking them (flist.c:1368), so the two match.
//!
//! oc recorded the converted wire bytes instead, so a name the conversion
//! changes was refused as unrequested (exit 4) where rsync 3.5.2 copies it.
//!
//! Every cell asserts the exit code and tree measured with a 3.5.2 client. When
//! the 3.5.2 oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), it runs the same pull and must match.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// The ISO-8859-1 spelling the client's list uses.
const LATIN1_NAME: &[u8] = b"caf\xe9";
/// The UTF-8 spelling the server's files carry.
const UTF8_NAME: &[u8] = b"caf\xc3\xa9";

/// The client's charset is ISO-8859-1 and the server's is UTF-8, so the list
/// names change when converted for the wire.
const ICONV: &str = "--iconv=ISO-8859-1,UTF-8";

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src");
        fs::create_dir(&src).expect("mkdir src");
        fs::write(src.join(os(UTF8_NAME)), "x\n").expect("write named file");
        fs::write(src.join("plain"), "p\n").expect("write plain");
        fs::write(
            dir.path().join("list"),
            [LATIN1_NAME, b"\nplain\n"].concat(),
        )
        .expect("write list");
        // The client runs `<rsh> <host> <command...>`; drop the host.
        let rsh = dir.path().join("lsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        Self { dir }
    }

    /// Pulls with `client` (also used as the server) into a fresh destination.
    fn pull(&self, client: &Path, extra: &[&str]) -> (Output, Vec<Vec<u8>>) {
        let dest = self.dir.path().join("dst");
        if dest.exists() {
            fs::remove_dir_all(&dest).expect("clear destination");
        }
        fs::create_dir(&dest).expect("mkdir destination");
        let output = Command::new(client)
            .arg("-r")
            .args(extra)
            .arg(format!(
                "--files-from={}",
                self.dir.path().join("list").display()
            ))
            .arg("-e")
            .arg(self.dir.path().join("lsh"))
            .arg(format!("--rsync-path={}", client.display()))
            .arg(format!(
                "localhost:{}/",
                self.dir.path().join("src").display()
            ))
            .arg(format!("{}/", dest.display()))
            .output()
            .expect("run client");
        let mut names: Vec<Vec<u8>> = fs::read_dir(&dest)
            .expect("read destination")
            .map(|e| e.expect("dir entry").file_name().into_vec())
            .collect();
        names.sort();
        (output, names)
    }
}

fn os(bytes: &[u8]) -> OsString {
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

/// One cell: oc must give `rc` and `expected`, and so must 3.5.2.
fn check(cell: &str, extra: &[&str], rc: i32, expected: &[&[u8]]) {
    let fixture = Fixture::new();
    let (oc, oc_tree) = fixture.pull(&oc_rsync_bin(), extra);
    assert_eq!(oc.status.code(), Some(rc), "{cell}: {oc:?}");
    let expected: Vec<Vec<u8>> = expected.iter().map(|n| n.to_vec()).collect();
    assert_eq!(oc_tree, expected, "{cell}: {oc:?}");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let (reference, reference_tree) = fixture.pull(upstream.binary(), extra);
        assert_eq!(
            (reference.status.code(), reference_tree),
            (oc.status.code(), oc_tree),
            "{cell}: oc differs from rsync 3.5.2: {reference:?}"
        );
    }
}

/// Under `-s` the list is converted for the sender, and the name it requested
/// is accepted when it arrives - landing in the client's charset.
#[test]
fn a_converted_files_from_name_is_accepted_as_requested() {
    check("-s --iconv", &["-s", ICONV], 0, &[LATIN1_NAME, b"plain"]);
}

/// Without `-s` the list is forwarded unconverted (compat.c:820-827), so the
/// sender finds no file of that spelling: exit 23, only `plain` arrives.
#[test]
fn without_protect_args_the_list_is_forwarded_unconverted() {
    check("--iconv alone", &[ICONV], 23, &[b"plain"]);
}
