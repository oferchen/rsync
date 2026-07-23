//! `--debug=BACKUP` producer emissions for backup actions.
//!
//! Mirrors upstream rsync's `backup.c` `DEBUG_GTE(BACKUP, 1)` output
//! byte-for-byte so wire-comparable diagnostics align across
//! implementations.
//!
//! # Upstream Reference
//!
//! All upstream emissions are level 1 (gated by `DEBUG_GTE(BACKUP, 1)`)
//! and fire on the success branch of each backup placement strategy in
//! `make_backup` / `link_or_rename`:
//!
//! - `backup.c:201-202` - `"make_backup: HLINK %s successful.\n"`
//! - `backup.c:216-217` - `"make_backup: RENAME %s successful.\n"`
//! - `backup.c:282-283` - `"make_backup: DEVICE %s successful.\n"`
//! - `backup.c:299-300` - `"make_backup: SYMLINK %s successful.\n"`
//! - `backup.c:333-334` - `"make_backup: COPY %s successful.\n"`
//!
//! The flag table entry at `options.c:299` is
//! `DEBUG_WORD(BACKUP, W_REC, "Debug backup actions (levels 1-2)")`.
//! Upstream's help text mentions a level 2 but only level 1 sites exist
//! in `backup.c`; this module mirrors that reality. `%s` is the source
//! filename as it was passed to `make_backup`.

use logging::debug_log;

/// Emits the `make_backup: HLINK <fname> successful.` notice when an
/// existing destination is preserved via `link(2)` into the backup
/// location.
///
/// upstream: `backup.c:201-202` -
/// `"make_backup: HLINK %s successful.\n"`. Fires on the success branch
/// of `do_link(from, to)` inside `link_or_rename` when `prefer_rename`
/// is false and the kernel supports linking the source's file type.
#[inline]
pub fn trace_make_backup_hlink(fname: &str) {
    debug_log!(Backup, 1, "make_backup: HLINK {} successful.", fname);
}

/// Emits the `make_backup: RENAME <fname> successful.` notice when an
/// existing destination is moved into the backup location via
/// `rename(2)`.
///
/// upstream: `backup.c:216-217` -
/// `"make_backup: RENAME %s successful.\n"`. Fires on the success
/// branch of `do_rename(from, to)` inside `link_or_rename`.
#[inline]
pub fn trace_make_backup_rename(fname: &str) {
    debug_log!(Backup, 1, "make_backup: RENAME {} successful.", fname);
}

/// Emits the `make_backup: DEVICE <fname> successful.` notice when an
/// existing device or special file is recreated in the backup location
/// via `mknod(2)`.
///
/// upstream: `backup.c:282-283` -
/// `"make_backup: DEVICE %s successful.\n"`. Fires after `do_mknod`
/// succeeds for a device or special file under `--devices` /
/// `--specials`.
#[inline]
pub fn trace_make_backup_device(fname: &str) {
    debug_log!(Backup, 1, "make_backup: DEVICE {} successful.", fname);
}

/// Emits the `make_backup: SYMLINK <fname> successful.` notice when a
/// symbolic link is recreated in the backup location via `symlink(2)`.
///
/// upstream: `backup.c:299-300` -
/// `"make_backup: SYMLINK %s successful.\n"`. Fires after `do_symlink`
/// succeeds when the source is a symlink and `--links` is on.
#[inline]
pub fn trace_make_backup_symlink(fname: &str) {
    debug_log!(Backup, 1, "make_backup: SYMLINK {} successful.", fname);
}

/// Emits the `make_backup: COPY <fname> successful.` notice when a
/// regular file is copied into the backup location after a rename
/// crosses a filesystem boundary or another fast-path fails.
///
/// upstream: `backup.c:333-334` -
/// `"make_backup: COPY %s successful.\n"`. Fires after the fallback
/// `copy_file` succeeds for a regular file.
#[inline]
pub fn trace_make_backup_copy(fname: &str) {
    debug_log!(Backup, 1, "make_backup: COPY {} successful.", fname);
}

#[cfg(test)]
mod tests {
    //! Pinning tests for BACKUP emission shapes. Strings match upstream
    //! `backup.c` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.backup = level;
        init(cfg);
        let _ = drain_events();
    }

    fn backup_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Backup,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Pins every BACKUP debug line to upstream `backup.c` byte-for-byte.
    #[test]
    fn upstream_wire_shapes() {
        // upstream: backup.c:201-202, :216-217, :282-283, :299-300, :333-334
        init_at(1);
        trace_make_backup_hlink("src/file.txt");
        trace_make_backup_rename("src/file.txt");
        trace_make_backup_device("/dev/null0");
        trace_make_backup_symlink("link/target");
        trace_make_backup_copy("nested/big.bin");

        let m = backup_messages();
        for expected in [
            "make_backup: HLINK src/file.txt successful.",
            "make_backup: RENAME src/file.txt successful.",
            "make_backup: DEVICE /dev/null0 successful.",
            "make_backup: SYMLINK link/target successful.",
            "make_backup: COPY nested/big.bin successful.",
        ] {
            assert!(m.iter().any(|s| s == expected), "missing {expected}: {m:?}");
        }
    }

    /// Level 0 must suppress every emission.
    ///
    /// upstream: `DEBUG_GTE(BACKUP, 1)` gates each emission at level 1
    /// or higher; the default `-v0` ladder leaves the flag at 0.
    #[test]
    fn level_zero_suppresses() {
        init_at(0);
        trace_make_backup_hlink("a");
        trace_make_backup_rename("a");
        trace_make_backup_device("a");
        trace_make_backup_symlink("a");
        trace_make_backup_copy("a");
        assert!(
            backup_messages().is_empty(),
            "level 0 must suppress emissions"
        );
    }

    /// Level 1 enables every site - matches `-vvv` and `--debug=BACKUP`.
    #[test]
    fn level_one_enables_all_sites() {
        init_at(1);
        trace_make_backup_rename("path/a");
        let m = backup_messages();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0], "make_backup: RENAME path/a successful.");
    }

    /// Levels above 1 still fire (the help text advertises levels 1-2
    /// even though upstream's tree has no level-2 sites). This pins the
    /// upper bound behaviour so future level-2 additions extend, not
    /// regress, the matrix.
    #[test]
    fn level_two_still_emits_level_one_sites() {
        init_at(2);
        trace_make_backup_copy("x");
        let m = backup_messages();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0], "make_backup: COPY x successful.");
    }
}
