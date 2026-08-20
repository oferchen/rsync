//! A non-itemizing `--out-format` does not announce attribute-only changes.
//!
//! upstream: log.c:875-890 `maybe_log_item()` gates `see_item` on `itemizing`,
//! so a format without `%i` reports only entries that actually moved data. An
//! attribute-only change - a chmod against an otherwise up-to-date file - is
//! announced solely when the format itemizes.
//!
//! The table below was measured against the real rsync 3.5.0 binary before the
//! fix; oc diverged in exactly one cell of twelve (attr-only under a
//! non-itemizing format, which `%n` and `%%i %n` both are):
//!
//! | --out-format | new  | content | attr-only | no change |
//! |--------------|------|---------|-----------|-----------|
//! | `%n`         | name | name    | SILENT    | SILENT    |
//! | `%%i %n`     | name | name    | SILENT    | SILENT    |
//! | `%i %n`      | row  | row     | row       | SILENT    |
//!
//! This is a table test rather than a single regression because the obvious
//! narrow fix - suppressing whenever the format does not itemize - would also
//! silence the `new` and `content` rows, which upstream prints. Those rows are
//! therefore the non-vacuity companions: they must keep printing, or the pin
//! would pass for a build that emitted nothing at all.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Runs one sync and returns stdout with the trailing newline trimmed.
fn sync(out_format: &str, src: &Path, dst: &Path) -> String {
    let output = Command::new(oc_binary())
        .arg("-a")
        .arg(format!("--out-format={out_format}"))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("run oc-rsync");
    assert!(
        output.status.success(),
        "transfer failed ({:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned()
}

/// Drives one format through the four transfer states in order and returns
/// `(new, content, attribute_only, unchanged)` stdout.
fn four_states(out_format: &str) -> (String, String, String, String) {
    let root = tempfile::tempdir().expect("temp dir");
    let src = root.path().join("src");
    let dst = root.path().join("dst");
    fs::create_dir_all(&src).expect("src");
    fs::create_dir_all(&dst).expect("dst");
    let file = src.join("a");

    fs::write(&file, b"one\n").expect("seed");
    let new = sync(out_format, &src, &dst);

    // A longer payload so the quick check sees a size difference and cannot
    // skip the transfer on a same-second mtime.
    fs::write(&file, b"two-longer\n").expect("rewrite");
    let content = sync(out_format, &src, &dst);

    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    let attribute_only = sync(out_format, &src, &dst);

    let unchanged = sync(out_format, &src, &dst);
    (new, content, attribute_only, unchanged)
}

#[test]
fn non_itemizing_out_format_is_silent_for_an_attribute_only_change() {
    for format in ["%n", "%%i %n"] {
        let (new, content, attribute_only, unchanged) = four_states(format);

        assert!(
            attribute_only.is_empty(),
            "{format:?} announced an attribute-only change: {attribute_only:?}"
        );
        assert!(
            unchanged.is_empty(),
            "{format:?} announced an unchanged entry: {unchanged:?}"
        );

        // Non-vacuity: without these the assertions above would also hold for a
        // build that printed nothing under any circumstance.
        assert!(
            new.contains('a'),
            "{format:?} dropped the new-file row: {new:?}"
        );
        assert!(
            content.contains('a'),
            "{format:?} dropped the content-change row: {content:?}"
        );
    }
}

#[test]
fn an_itemizing_out_format_still_announces_an_attribute_only_change() {
    let (new, content, attribute_only, unchanged) = four_states("%i %n");

    // The discriminator: the same chmod that stays silent above must surface
    // here, with the `p` column set (upstream renders `.f...p..... a`).
    assert!(
        attribute_only.contains('p') && attribute_only.contains('a'),
        "%i dropped the attribute-only row: {attribute_only:?}"
    );
    assert!(new.contains('+'), "%i dropped the new-file row: {new:?}");
    assert!(
        content.contains('s'),
        "%i dropped the content-change row: {content:?}"
    );
    assert!(
        unchanged.is_empty(),
        "%i announced an unchanged entry: {unchanged:?}"
    );
}
