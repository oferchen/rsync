//! The `--list-only` line a server receiver renders for its client.
//!
//! On a push the generator runs on the server, so the server formats each
//! listing line and `rwrite()` frames it as `MSG_INFO` for the client to print
//! (upstream: log.c:355-366). The time columns are therefore rendered in the
//! server's local zone, as upstream's `timestring()` does there.

use crate::receiver::stats::ListOnlyEntry;

/// Width of the size column.
///
/// upstream: generator.c:1322 - `human_readable ? 14 : 11`. `server_options()`
/// never forwards `-h` or `--no-h` (options.c:2778-3190), so a server keeps
/// `human_readable = 1` (options.c:116) and renders sizes with commas.
const SIZE_WIDTH: usize = 14;

/// The options that shape a listing line, as the server holds them.
#[derive(Clone, Copy, Debug)]
pub(in crate::receiver) struct ListLineStyle {
    /// `-U`/`--atimes`: an access-time column (upstream `atimes_ndx`).
    pub(in crate::receiver) atimes: bool,
    /// `-N`/`--crtimes`: a creation-time column (upstream `crtimes_ndx`).
    pub(in crate::receiver) crtimes: bool,
    /// `-l`: a symlink is followed by ` -> target` (upstream `preserve_links`).
    pub(in crate::receiver) preserve_links: bool,
    /// `--delete-missing-args`: a missing operand is listed as `*missing`
    /// (upstream `missing_args == 2`).
    pub(in crate::receiver) delete_missing_args: bool,
}

impl ListLineStyle {
    /// Renders `entry` as one newline-terminated listing line.
    ///
    /// The name and link target are copied as raw bytes; the client escapes
    /// them when it prints the line (upstream: log.c:425 `filtered_fwrite()`).
    ///
    /// upstream: generator.c:1318-1356 `list_file_entry()`.
    pub(in crate::receiver) fn render(self, entry: &ListOnlyEntry) -> Vec<u8> {
        let mtime = timestring(entry.mtime);
        let time_width = 1 + mtime.len();
        let atime_width = if self.atimes { time_width } else { 0 };
        let crtime_width = if self.crtimes { time_width } else { 0 };
        let name = entry.path.as_os_str().as_encoded_bytes();

        let mut line = if self.delete_missing_args && entry.mode == 0 {
            let width = 10 + 1 + SIZE_WIDTH + time_width + atime_width + crtime_width;
            format!("{:<width$} ", "*missing").into_bytes()
        } else {
            let atime = if self.atimes && !is_dir(entry.mode) {
                timestring(entry.atime)
            } else {
                String::new()
            };
            let crtime = if self.crtimes {
                timestring(entry.crtime)
            } else {
                String::new()
            };
            format!(
                "{} {:>SIZE_WIDTH$} {mtime}{atime:>atime_width$}{crtime:>crtime_width$} ",
                logging::permstring(entry.mode),
                logging::comma_num(entry.size),
            )
            .into_bytes()
        };
        line.extend_from_slice(name);
        if self.preserve_links
            && entry.is_symlink
            && let Some(target) = &entry.symlink_target
        {
            line.extend_from_slice(b" -> ");
            line.extend_from_slice(target.as_os_str().as_encoded_bytes());
        }
        line.push(b'\n');
        line
    }
}

const fn is_dir(mode: u32) -> bool {
    mode & 0o170_000 == 0o040_000
}

/// upstream: util1.c:1722-1739 `timestring()` - `localtime_r()` in the zone in
/// effect at that instant.
fn timestring(unix_secs: i64) -> String {
    let offset = i64::from(platform::local_time::local_utc_offset_seconds(unix_secs));
    logging::timestring_at_offset(unix_secs, offset)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// 2020-01-02 03:04:05 UTC; the render takes the zone from the process,
    /// so expectations derive the time text the same way.
    const MTIME: i64 = 1_577_934_245;

    fn entry(path: &str, mode: u32, size: u64) -> ListOnlyEntry {
        ListOnlyEntry {
            path: PathBuf::from(path),
            mode,
            size,
            mtime: MTIME,
            mtime_nsec: 0,
            atime: MTIME + 60,
            atime_nsec: 0,
            crtime: MTIME - 60,
            crtime_nsec: 0,
            symlink_target: None,
            is_symlink: false,
        }
    }

    const PLAIN: ListLineStyle = ListLineStyle {
        atimes: false,
        crtimes: false,
        preserve_links: false,
        delete_missing_args: false,
    };

    fn render(style: ListLineStyle, entry: &ListOnlyEntry) -> String {
        String::from_utf8(style.render(entry)).expect("ascii line")
    }

    /// upstream: generator.c:1352 - `"%s %*s %s%*s%*s %s%s%s\n"` with a
    /// 14-wide comma-grouped size column.
    #[test]
    fn regular_file_line_matches_upstream_layout() {
        let t = timestring(MTIME);
        assert_eq!(
            render(PLAIN, &entry("sub/b.txt", 0o100_644, 12_345)),
            format!("-rw-r--r--         12,345 {t} sub/b.txt\n")
        );
    }

    /// upstream: generator.c:1340-1348 - the arrow needs `preserve_links`.
    #[test]
    fn symlink_target_follows_only_under_preserve_links() {
        let mut link = entry("link", 0o120_777, 5);
        link.is_symlink = true;
        link.symlink_target = Some(PathBuf::from("a.txt"));
        let t = timestring(MTIME);
        let base = format!("lrwxrwxrwx              5 {t} link");
        assert_eq!(render(PLAIN, &link), format!("{base}\n"));
        let links = ListLineStyle {
            preserve_links: true,
            ..PLAIN
        };
        assert_eq!(render(links, &link), format!("{base} -> a.txt\n"));
    }

    /// upstream: generator.c:1336 - a directory keeps the atime column's width
    /// but leaves it blank; the crtime column is shown for every type.
    #[test]
    fn time_columns_pad_and_blank_like_upstream() {
        let style = ListLineStyle {
            atimes: true,
            crtimes: true,
            ..PLAIN
        };
        let (m, a, c) = (
            timestring(MTIME),
            timestring(MTIME + 60),
            timestring(MTIME - 60),
        );
        assert_eq!(
            render(style, &entry("f", 0o100_600, 1)),
            format!("-rw-------              1 {m} {a} {c} f\n")
        );
        let blank = " ".repeat(1 + m.len());
        assert_eq!(
            render(style, &entry("d", 0o040_700, 4_096)),
            format!("drwx------          4,096 {m}{blank} {c} d\n")
        );
    }

    /// upstream: generator.c:1331-1334 - a deleted missing operand is padded
    /// across every column it lacks.
    #[test]
    fn missing_operand_is_listed_as_missing() {
        let style = ListLineStyle {
            delete_missing_args: true,
            ..PLAIN
        };
        let width = 10 + 1 + SIZE_WIDTH + 1 + timestring(MTIME).len();
        assert_eq!(
            render(style, &entry("gone", 0, 0)),
            format!("{:<width$} gone\n", "*missing")
        );
        assert!(render(PLAIN, &entry("gone", 0, 0)).starts_with("----------"));
    }

    /// The name is copied raw; escaping is the printing client's job.
    #[cfg(unix)]
    #[test]
    fn non_utf8_name_bytes_pass_through() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let mut raw = entry("x", 0o100_644, 0);
        raw.path = PathBuf::from(OsStr::from_bytes(b"caf\xe9"));
        assert!(PLAIN.render(&raw).ends_with(b" caf\xe9\n"));
    }
}
