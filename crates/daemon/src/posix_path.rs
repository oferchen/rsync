//! POSIX-style daemon paths on Windows, resolved the way Cygwin resolves them.
//!
//! Upstream rsync runs on Windows only under Cygwin, so every path the daemon
//! reads - `/etc/rsyncd.conf`, `lock file = /var/run/rsyncd.lock`, `pid file`,
//! `log file`, `secrets file`, a module's `path` - goes through Cygwin's mount
//! table. This module reproduces that mapping natively, without `/etc/fstab`:
//!
//! - `/x/y` maps below the installation root, the parent of the directory that
//!   holds the running executable (newlib-cygwin mm/cygheap.cc:161-250
//!   `init_installation_root`, mount.cc:546-556 `create_root_entry`).
//! - `/cygdrive/<letter>/rest` maps to `<LETTER>:\rest`
//!   (winsup/cygwin/include/cygwin/version.h:543,
//!   mount.cc:733-751, mount.cc:896-917 `cygdrive_win32_path`); any other
//!   `/cygdrive` path names nothing (`ENOENT`).
//! - `//server/share/rest` maps to the UNC path `\\server\share\rest`; two
//!   leading slashes are kept by normalize_posix_path (path.cc:295-299).
//! - Native `C:\...`, `C:/...` and anything containing a backslash pass through
//!   unchanged (path.cc:274-275, :303-304). Relative paths also pass through and
//!   resolve against the working directory, as under Cygwin.
//! - `..` first checks that the directory it leaves exists and is a directory,
//!   following its symlinks, and clamps at `/` (path.cc:330-377).
//!
//! The mapping works on UTF-16 code units, as Cygwin works on wide paths, so a
//! path that is not valid Unicode maps like any other.
//!
//! Daemon path parameters are stored as written, as loadparm.c stores them,
//! and resolved only when they are opened, as Cygwin's `path_conv` runs inside
//! each syscall. A value that names nothing therefore fails at the open that
//! uses it, with the open's own error flow.
//!
//! Scope (user decision 2026-11-10): the mapping covers the paths the daemon
//! itself reads - its default and configured config, the path parameters of
//! that config and of `--dparam`, module paths, and the daemon-mode
//! command-line path options (`--config`, `--log-file`, `--lock-file`,
//! `--pid-file`, `--secrets-file`, `--motd-file`). Client operands and client
//! path options (`--backup-dir`, `--partial-dir`, `--files-from`, ...) are
//! used as typed.
//!
//! The mapping core is compiled on Windows and under test, so every rule is
//! testable on any host; only [`resolve_daemon_path`] consults the running
//! executable and the filesystem.

use std::io;
use std::path::{Path, PathBuf};

/// A path as UTF-16 code units.
#[cfg(any(windows, test))]
type Wide = Vec<u16>;

#[cfg(any(windows, test))]
const SLASH: u16 = b'/' as u16;
#[cfg(any(windows, test))]
const BACKSLASH: u16 = b'\\' as u16;
#[cfg(any(windows, test))]
const COLON: u16 = b':' as u16;

/// Cygwin's default cygdrive prefix component
/// (winsup/cygwin/include/cygwin/version.h:543
/// `CYGWIN_INFO_CYGDRIVE_DEFAULT_PREFIX`).
#[cfg(any(windows, test))]
const CYGDRIVE: &[u8] = b"cygdrive";

/// Why a POSIX value names nothing; Cygwin reports these as the errno of the
/// open that uses the path.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unresolvable {
    /// `ENOENT`: a missing directory before `..`, or a `/cygdrive` path that
    /// names no drive.
    NotFound,
    /// `ENOTDIR`: a non-directory before `..`.
    NotADirectory,
}

/// The filesystem lookup behind the `..` check (path.cc:340-372).
#[cfg(any(windows, test))]
pub(crate) trait DirectoryProbe {
    /// Resolves the directory at the Win32 path `native`, following symlinks,
    /// to its final Win32 path.
    fn resolve_directory(&mut self, native: &[u16]) -> Result<Wide, Unresolvable>;
}

/// A POSIX location: the components below `/`, or below `//` for UNC.
#[cfg(any(windows, test))]
struct Location {
    unc: bool,
    components: Vec<Wide>,
}

/// Returns the Cygwin installation root for the executable at `exe`, a Win32
/// path (`C:\dir\bin\oc-rsync.exe` or `\\server\share\bin\oc-rsync.exe`).
///
/// mm/cygheap.cc:220-250 strips the file name and then one more component, so
/// `C:\cygwin64\bin\cygwin1.dll` yields `C:\cygwin64`. When the executable sits
/// in the root of a drive or share, the second strip would leave an invalid
/// path, so the root is that drive or share (cygheap.cc:244-250, `w > p`).
#[cfg(any(windows, test))]
pub(crate) fn installation_root(exe: &[u16]) -> Wide {
    let exe = strip_verbatim_prefix(exe);
    let volume = volume_root_len(&exe);
    let last_separator = |path: &[u16]| path.iter().rposition(|&unit| unit == BACKSLASH);
    let dir = match last_separator(&exe) {
        Some(end) if end >= volume => &exe[..end],
        _ => &exe[..volume],
    };
    match last_separator(dir) {
        Some(end) if end >= volume => exe[..end].to_vec(),
        _ => exe[..volume].to_vec(),
    }
}

/// Maps a daemon path value to a Win32 path under `root`, following Cygwin.
#[cfg(any(windows, test))]
pub(crate) fn map_posix_path(
    root: &[u16],
    value: &[u16],
    probe: &mut impl DirectoryProbe,
) -> Result<Wide, Unresolvable> {
    if value.first() != Some(&SLASH) || value.contains(&BACKSLASH) {
        return Ok(value.to_vec());
    }

    let unc = value.get(1) == Some(&SLASH) && value.get(2) != Some(&SLASH);
    let mut location = Location {
        unc,
        components: Vec::new(),
    };
    for component in value.split(|&unit| unit == SLASH) {
        if component.is_empty() || is_dots(component, 1) {
            continue;
        }
        if !is_dots(component, 2) {
            location.components.push(component.to_vec());
            continue;
        }
        // path.cc:337-376: at `/` the `..` is dropped unchecked; elsewhere the
        // directory being left must exist and be a directory, and the pop
        // happens on its symlink-resolved form.
        if location.components.is_empty() {
            continue;
        }
        let resolved = probe.resolve_directory(&to_native(root, &location)?)?;
        location = to_posix(root, &resolved);
        location.components.pop();
    }
    to_native(root, &location)
}

/// Converts a POSIX location to its Win32 path.
#[cfg(any(windows, test))]
fn to_native(root: &[u16], location: &Location) -> Result<Wide, Unresolvable> {
    if location.unc {
        let mut native = vec![BACKSLASH, BACKSLASH];
        native.extend(join(&location.components));
        return Ok(native);
    }

    if let Some(first) = location.components.first()
        && ascii_eq(first, CYGDRIVE)
    {
        return cygdrive_path(&location.components[1..]);
    }

    let mut native = root.to_vec();
    while native.len() > volume_root_len(&native) && native.last() == Some(&BACKSLASH) {
        native.pop();
    }
    if location.components.is_empty() {
        if native.last() != Some(&BACKSLASH) {
            native.push(BACKSLASH);
        }
        return Ok(native);
    }
    if native.last() != Some(&BACKSLASH) {
        native.push(BACKSLASH);
    }
    native.extend(join(&location.components));
    Ok(native)
}

/// Converts a resolved Win32 path back to a POSIX location: below the root,
/// on a drive (`/cygdrive/<letter>`), or on a share (`//server/share`).
#[cfg(any(windows, test))]
fn to_posix(root: &[u16], native: &[u16]) -> Location {
    let native = strip_verbatim_prefix(native);
    let split = |path: &[u16]| -> Vec<Wide> {
        path.split(|&unit| unit == BACKSLASH)
            .filter(|component| !component.is_empty())
            .map(<[u16]>::to_vec)
            .collect()
    };

    let root_components = split(&root[volume_root_len(root).min(root.len())..]);
    let root_volume = &root[..volume_root_len(root)];
    let native_volume = &native[..volume_root_len(&native)];
    let native_components = split(&native[native_volume.len()..]);
    let same_volume = ascii_eq_wide(trim_separator(root_volume), trim_separator(native_volume));
    if same_volume
        && native_components.len() >= root_components.len()
        && root_components
            .iter()
            .zip(&native_components)
            .all(|(a, b)| ascii_eq_wide(a, b))
    {
        return Location {
            unc: false,
            components: native_components[root_components.len()..].to_vec(),
        };
    }

    if native.starts_with(&[BACKSLASH, BACKSLASH]) {
        return Location {
            unc: true,
            components: split(&native[2..]),
        };
    }

    let mut components = vec![CYGDRIVE.iter().map(|&byte| u16::from(byte)).collect()];
    if let Some(&letter) = native.first() {
        components.push(vec![ascii_lower(letter)]);
    }
    components.extend(native_components);
    Location {
        unc: false,
        components,
    }
}

/// Maps the components after `/cygdrive` to a drive path.
///
/// mount.cc:896-917: the first component must be a single letter; it is
/// upper-cased and the rest is appended with backslashes, giving `C:\` for a
/// bare drive. Anything else, including `/cygdrive` itself, names nothing
/// (mount.cc:749-750).
#[cfg(any(windows, test))]
fn cygdrive_path(components: &[Wide]) -> Result<Wide, Unresolvable> {
    let (drive, rest) = components.split_first().ok_or(Unresolvable::NotFound)?;
    let letter = match drive.as_slice() {
        [letter] if is_ascii_alpha(*letter) => ascii_upper(*letter),
        _ => return Err(Unresolvable::NotFound),
    };
    let mut native = vec![letter, COLON, BACKSLASH];
    native.extend(join(rest));
    Ok(native)
}

/// Resolves a stored daemon path for the open about to use it.
///
/// On Windows a POSIX-style value is mapped through [`map_posix_path`] below
/// the running executable's installation root, and a value that names nothing
/// fails here with the error the open reports: `ERROR_PATH_NOT_FOUND` for
/// Cygwin's `ENOENT`, `ERROR_DIRECTORY` for its `ENOTDIR`. Elsewhere the value
/// is the path, which is what loadparm.c's verbatim storage means on a POSIX
/// host.
pub(crate) fn resolve_daemon_path(value: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        /// Win32 `ERROR_PATH_NOT_FOUND`.
        const ERROR_PATH_NOT_FOUND: i32 = 3;
        /// Win32 `ERROR_DIRECTORY`.
        const ERROR_DIRECTORY: i32 = 267;

        let wide: Wide = value.as_os_str().encode_wide().collect();
        match map_posix_path(executable_root(), &wide, &mut FileSystem) {
            Ok(mapped) => Ok(PathBuf::from(OsString::from_wide(&mapped))),
            Err(Unresolvable::NotFound) => Err(io::Error::from_raw_os_error(ERROR_PATH_NOT_FOUND)),
            Err(Unresolvable::NotADirectory) => Err(io::Error::from_raw_os_error(ERROR_DIRECTORY)),
        }
    }
    #[cfg(not(windows))]
    {
        Ok(value.to_path_buf())
    }
}

/// Resolves a module `path` for the session that serves it.
///
/// upstream splits a `/./` inside-chroot marker off the configured string
/// before any of it is opened (clientserver.c:897-918), so only the part
/// before the marker goes through the mapping and the marker survives for the
/// chroot split; the inner part stays relative to it.
pub(crate) fn resolve_module_path(value: &Path) -> io::Result<PathBuf> {
    resolve_module_path_with(value, resolve_daemon_path)
}

/// [`resolve_module_path`] over the given `resolve` step.
fn resolve_module_path_with(
    value: &Path,
    resolve: impl Fn(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    let Some((outer, inner)) = value.to_str().and_then(|text| text.split_once("/./")) else {
        return resolve(value);
    };
    let outer = resolve(Path::new(if outer.is_empty() { "/" } else { outer }))?;
    let mut joined = outer.into_os_string();
    joined.push("/./");
    joined.push(inner);
    Ok(PathBuf::from(joined))
}

/// The installation root of the running executable, computed once.
///
/// mm/cygheap.cc:166-180 takes the module path from `GetModuleFileNameW` and
/// refines it with `GetFinalPathNameByHandleW`, keeping the module path when
/// that fails; `current_exe` and `canonicalize` are those two calls. Cygwin
/// cannot start without its module path (cygheap.cc:166-169 `api_fatal`), and
/// neither can this daemon.
#[cfg(windows)]
fn executable_root() -> &'static [u16] {
    use std::os::windows::ffi::OsStrExt;
    use std::sync::OnceLock;

    static ROOT: OnceLock<Wide> = OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe()
            .expect("the running executable's path names the daemon's installation root");
        let exe = exe.canonicalize().unwrap_or(exe);
        installation_root(&exe.as_os_str().encode_wide().collect::<Wide>())
    })
}

/// The live filesystem behind the `..` check.
#[cfg(windows)]
struct FileSystem;

#[cfg(windows)]
impl DirectoryProbe for FileSystem {
    fn resolve_directory(&mut self, native: &[u16]) -> Result<Wide, Unresolvable> {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let path = PathBuf::from(OsString::from_wide(native));
        let metadata = std::fs::metadata(&path).map_err(|_| Unresolvable::NotFound)?;
        if !metadata.is_dir() {
            return Err(Unresolvable::NotADirectory);
        }
        let resolved = path.canonicalize().map_err(|_| Unresolvable::NotFound)?;
        Ok(resolved.as_os_str().encode_wide().collect())
    }
}

#[cfg(any(windows, test))]
fn join(components: &[Wide]) -> Wide {
    let mut joined = Wide::new();
    for (index, component) in components.iter().enumerate() {
        if index > 0 {
            joined.push(BACKSLASH);
        }
        joined.extend_from_slice(component);
    }
    joined
}

/// True when `component` is exactly `count` dots.
#[cfg(any(windows, test))]
fn is_dots(component: &[u16], count: usize) -> bool {
    component.len() == count && component.iter().all(|&unit| unit == u16::from(b'.'))
}

/// Removes a `\\?\` or `\\?\UNC\` prefix, as cygheap.cc:227-236 converts the
/// final path back to its Win32 form.
#[cfg(any(windows, test))]
fn strip_verbatim_prefix(path: &[u16]) -> Wide {
    let verbatim: Wide = r"\\?\".encode_utf16().collect();
    let Some(rest) = path.strip_prefix(verbatim.as_slice()) else {
        return path.to_vec();
    };
    let unc: Wide = r"UNC\".encode_utf16().collect();
    match rest.strip_prefix(unc.as_slice()) {
        Some(share) => [BACKSLASH, BACKSLASH]
            .iter()
            .chain(share)
            .copied()
            .collect(),
        None => rest.to_vec(),
    }
}

/// Length of the volume root: `C:\` or `\\server\share\`.
#[cfg(any(windows, test))]
fn volume_root_len(path: &[u16]) -> usize {
    if path.starts_with(&[BACKSLASH, BACKSLASH]) {
        let mut separators = 0;
        for (index, &unit) in path.iter().enumerate().skip(2) {
            if unit == BACKSLASH {
                separators += 1;
                if separators == 2 {
                    return index + 1;
                }
            }
        }
        path.len()
    } else if path.len() >= 2 && path[1] == COLON && is_ascii_alpha(path[0]) {
        path.len().min(3)
    } else {
        0
    }
}

#[cfg(any(windows, test))]
fn trim_separator(path: &[u16]) -> &[u16] {
    path.strip_suffix(&[BACKSLASH]).unwrap_or(path)
}

#[cfg(any(windows, test))]
fn is_ascii_alpha(unit: u16) -> bool {
    u8::try_from(unit).is_ok_and(|byte| byte.is_ascii_alphabetic())
}

#[cfg(any(windows, test))]
fn ascii_upper(unit: u16) -> u16 {
    u8::try_from(unit).map_or(unit, |byte| u16::from(byte.to_ascii_uppercase()))
}

#[cfg(any(windows, test))]
fn ascii_lower(unit: u16) -> u16 {
    u8::try_from(unit).map_or(unit, |byte| u16::from(byte.to_ascii_lowercase()))
}

#[cfg(any(windows, test))]
fn ascii_eq(component: &[u16], expected: &[u8]) -> bool {
    component.len() == expected.len()
        && component
            .iter()
            .zip(expected)
            .all(|(&unit, &byte)| unit == u16::from(byte))
}

/// Case-insensitive comparison of Win32 path text, as NTFS names compare.
#[cfg(any(windows, test))]
fn ascii_eq_wide(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(&x, &y)| ascii_lower(x) == ascii_lower(y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const ROOT: &str = r"C:\cygwin64";

    fn wide(text: &str) -> Wide {
        text.encode_utf16().collect()
    }

    fn text(units: &[u16]) -> String {
        String::from_utf16(units).expect("test paths are valid UTF-16")
    }

    /// Directories by Win32 path, each resolving to its final path.
    #[derive(Default)]
    struct FakeTree {
        dirs: HashMap<String, String>,
        files: Vec<String>,
        probed: Vec<String>,
    }

    impl FakeTree {
        fn dir(mut self, path: &str, resolves_to: &str) -> Self {
            self.dirs.insert(path.to_owned(), resolves_to.to_owned());
            self
        }

        fn file(mut self, path: &str) -> Self {
            self.files.push(path.to_owned());
            self
        }
    }

    impl DirectoryProbe for FakeTree {
        fn resolve_directory(&mut self, native: &[u16]) -> Result<Wide, Unresolvable> {
            let native = text(native);
            self.probed.push(native.clone());
            if self.files.contains(&native) {
                return Err(Unresolvable::NotADirectory);
            }
            self.dirs
                .get(&native)
                .map(|resolved| wide(resolved))
                .ok_or(Unresolvable::NotFound)
        }
    }

    fn map(value: &str) -> Result<String, Unresolvable> {
        map_with(ROOT, value, &mut FakeTree::default())
    }

    fn map_with(root: &str, value: &str, tree: &mut FakeTree) -> Result<String, Unresolvable> {
        map_posix_path(&wide(root), &wide(value), tree).map(|mapped| text(&mapped))
    }

    #[test]
    fn installation_root_is_the_parent_of_the_executable_directory() {
        for (exe, root) in [
            (r"C:\cygwin64\bin\oc-rsync.exe", r"C:\cygwin64"),
            (r"\\?\C:\cygwin64\bin\oc-rsync.exe", r"C:\cygwin64"),
            (r"D:\a\b\c\oc-rsync.exe", r"D:\a\b"),
        ] {
            assert_eq!(text(&installation_root(&wide(exe))), root, "{exe}");
        }
    }

    #[test]
    fn an_executable_in_a_drive_root_or_one_level_below_roots_at_the_drive() {
        // cygheap.cc:244-250: the second strip is skipped when it would leave
        // the volume, so the root is the drive itself.
        assert_eq!(text(&installation_root(&wide(r"C:\oc-rsync.exe"))), r"C:\");
        assert_eq!(
            text(&installation_root(&wide(r"C:\bin\oc-rsync.exe"))),
            r"C:\"
        );
    }

    #[test]
    fn installation_root_on_a_share() {
        for (exe, root) in [
            (
                r"\\?\UNC\server\share\tools\bin\oc-rsync.exe",
                r"\\server\share\tools",
            ),
            (r"\\server\share\oc-rsync.exe", r"\\server\share\"),
            (r"\\server\share\bin\oc-rsync.exe", r"\\server\share\"),
        ] {
            assert_eq!(text(&installation_root(&wide(exe))), root, "{exe}");
        }
    }

    #[test]
    fn installation_root_keeps_units_that_are_not_unicode() {
        let mut exe = wide(r"C:\tools");
        exe.push(0xD800);
        exe.extend(wide(r"\bin\oc-rsync.exe"));
        let mut expected = wide(r"C:\tools");
        expected.push(0xD800);
        assert_eq!(installation_root(&exe), expected);
    }

    #[test]
    fn absolute_posix_paths_map_below_the_root() {
        assert_eq!(
            map("/var/run/rsyncd.lock").as_deref(),
            Ok(r"C:\cygwin64\var\run\rsyncd.lock")
        );
        assert_eq!(
            map("/etc/rsyncd.conf").as_deref(),
            Ok(r"C:\cygwin64\etc\rsyncd.conf")
        );
        assert_eq!(map("/").as_deref(), Ok(r"C:\cygwin64\"));
        assert_eq!(
            map_with(r"C:\", "/srv/data", &mut FakeTree::default()).as_deref(),
            Ok(r"C:\srv\data")
        );
    }

    #[test]
    fn a_value_that_is_not_unicode_maps_unit_for_unit() {
        let mut value = wide("/var/log/");
        value.push(0xDC00);
        let mut expected = wide(r"C:\cygwin64\var\log\");
        expected.push(0xDC00);
        let mapped = map_posix_path(&wide(ROOT), &value, &mut FakeTree::default());
        assert_eq!(mapped, Ok(expected));
    }

    #[test]
    fn slash_runs_and_dot_components_are_dropped() {
        assert_eq!(
            map("///srv//./data/").as_deref(),
            Ok(r"C:\cygwin64\srv\data")
        );
    }

    #[test]
    fn dot_dot_checks_the_directory_it_leaves() {
        let mut tree = FakeTree::default().dir(r"C:\cygwin64\srv\data", r"C:\cygwin64\srv\data");
        assert_eq!(
            map_with(ROOT, "/srv/data/../etc", &mut tree).as_deref(),
            Ok(r"C:\cygwin64\srv\etc")
        );
        assert_eq!(tree.probed, [r"C:\cygwin64\srv\data"]);
    }

    #[test]
    fn dot_dot_after_a_missing_directory_names_nothing() {
        // path.cc:357-358: `if (!head.exists ()) return ENOENT;`
        assert_eq!(map("/srv/missing/../etc"), Err(Unresolvable::NotFound));
    }

    #[test]
    fn dot_dot_after_a_file_is_not_a_directory() {
        // path.cc:359-360: `if (!head.isdir ()) return ENOTDIR;`
        let mut tree = FakeTree::default().file(r"C:\cygwin64\etc\rsyncd.conf");
        assert_eq!(
            map_with(ROOT, "/etc/rsyncd.conf/../x", &mut tree),
            Err(Unresolvable::NotADirectory)
        );
    }

    #[test]
    fn dot_dot_pops_the_symlink_target_not_the_link() {
        // path.cc:362-372: the pop happens on the resolved path.
        let mut tree = FakeTree::default().dir(r"C:\cygwin64\link", r"\\?\D:\real\dir");
        assert_eq!(
            map_with(ROOT, "/link/../x", &mut tree).as_deref(),
            Ok(r"D:\real\x")
        );

        let mut tree = FakeTree::default().dir(r"C:\cygwin64\link", r"C:\cygwin64\srv\dir");
        assert_eq!(
            map_with(ROOT, "/link/../x", &mut tree).as_deref(),
            Ok(r"C:\cygwin64\srv\x")
        );
    }

    #[test]
    fn dot_dot_at_the_root_is_dropped_unchecked() {
        // path.cc:342 checks only below `/`, and :376 clamps the pop there.
        let mut tree = FakeTree::default();
        assert_eq!(
            map_with(ROOT, "/../../etc", &mut tree).as_deref(),
            Ok(r"C:\cygwin64\etc")
        );
        assert!(tree.probed.is_empty());
    }

    #[test]
    fn cygdrive_paths_map_to_drive_letters() {
        assert_eq!(map("/cygdrive/d/backups/x").as_deref(), Ok(r"D:\backups\x"));
        assert_eq!(map("/cygdrive/c").as_deref(), Ok(r"C:\"));
        assert_eq!(map("/cygdrive/e/").as_deref(), Ok(r"E:\"));
    }

    #[test]
    fn a_cygdrive_path_without_a_drive_letter_names_nothing() {
        for value in ["/cygdrive", "/cygdrive/cd/x", "/cygdrive/1/x"] {
            assert_eq!(map(value), Err(Unresolvable::NotFound), "{value}");
        }
        // Only the exact prefix component counts (path.cc:189-204).
        assert_eq!(
            map("/cygdrivex/c").as_deref(),
            Ok(r"C:\cygwin64\cygdrivex\c")
        );
    }

    #[test]
    fn double_slash_paths_are_unc() {
        assert_eq!(
            map("//server/share/dir").as_deref(),
            Ok(r"\\server\share\dir")
        );
    }

    #[test]
    fn native_and_relative_paths_pass_through() {
        for value in [
            r"C:\data\rsyncd.lock",
            "C:/data/x",
            r"\\server\share\x",
            r"\temp\x",
            // A backslash anywhere marks a Win32 path (path.cc:303-304).
            r"/srv\data",
            "relative/x",
            "x.log",
        ] {
            assert_eq!(map(value).as_deref(), Ok(value), "{value}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn daemon_paths_are_used_as_given_off_windows() {
        let value = Path::new("/var/run/rsyncd.lock");
        assert_eq!(resolve_daemon_path(value).expect("resolves"), value);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_value_is_used_as_given_off_windows() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let value = Path::new(OsStr::from_bytes(b"/var/log/\xff.log"));
        assert_eq!(resolve_daemon_path(value).expect("resolves"), value);
    }

    /// Brackets the path the resolve step saw, so a test shows what was resolved.
    fn marked(value: &Path) -> io::Result<PathBuf> {
        Ok(PathBuf::from(format!("<{}>", value.display())))
    }

    #[test]
    fn a_module_path_resolves_only_the_part_before_its_chroot_marker() {
        let resolved = resolve_module_path_with(Path::new("/srv/outer/./in/ner"), marked);
        assert_eq!(
            resolved.expect("resolves").as_os_str(),
            "</srv/outer>/./in/ner"
        );
    }

    #[test]
    fn a_leading_chroot_marker_resolves_the_root() {
        let resolved = resolve_module_path_with(Path::new("/./inner"), marked);
        assert_eq!(resolved.expect("resolves").as_os_str(), "</>/./inner");
    }

    #[test]
    fn a_module_path_without_a_marker_resolves_whole() {
        let resolved = resolve_module_path_with(Path::new("/srv/share"), marked);
        assert_eq!(resolved.expect("resolves").as_os_str(), "</srv/share>");
    }

    #[test]
    fn an_unresolvable_module_root_fails_the_module_path() {
        let resolved = resolve_module_path_with(Path::new("/gone/./inner"), |_| {
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(
            resolved.expect_err("unresolvable").kind(),
            io::ErrorKind::NotFound
        );
    }

    /// A value that names nothing fails at the open with Win32's not-found
    /// error, the native counterpart of Cygwin's `ENOENT`.
    #[cfg(windows)]
    #[test]
    fn an_unresolvable_value_fails_as_not_found() {
        let error = resolve_daemon_path(Path::new("/cygdrive/cd/x")).expect_err("names nothing");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.raw_os_error(), Some(3));
    }
}
