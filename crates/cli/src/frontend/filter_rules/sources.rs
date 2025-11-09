use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use core::client::{FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

pub(crate) fn append_filter_rules_from_files(
    destination: &mut Vec<FilterRuleSpec>,
    files: &[OsString],
    kind: FilterRuleKind,
) -> Result<(), Message> {
    if matches!(kind, FilterRuleKind::DirMerge) {
        let message = rsync_error!(
            1,
            "dir-merge directives cannot be loaded via --include-from/--exclude-from in this build"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    for path in files {
        let patterns = load_filter_file_patterns(Path::new(path.as_os_str()))?;
        destination.extend(patterns.into_iter().map(|pattern| match kind {
            FilterRuleKind::Include => FilterRuleSpec::include(pattern),
            FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern),
            FilterRuleKind::Clear => FilterRuleSpec::clear(),
            FilterRuleKind::ExcludeIfPresent => FilterRuleSpec::exclude_if_present(pattern),
            FilterRuleKind::Protect => FilterRuleSpec::protect(pattern),
            FilterRuleKind::Risk => FilterRuleSpec::risk(pattern),
            FilterRuleKind::DirMerge => unreachable!("dir-merge handled above"),
        }));
    }
    Ok(())
}

pub(crate) fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    if path == Path::new("-") {
        return read_filter_patterns_from_standard_input();
    }

    let path_display = path.display().to_string();
    let file = File::open(path).map_err(|error| {
        let text = format!("failed to read filter file '{path_display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })?;

    let mut reader = BufReader::new(file);
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter file '{path_display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_merge_file(path: &Path) -> Result<String, Message> {
    let display = path.display();
    fs::read_to_string(path).map_err(|error| {
        let text = format!("failed to read filter file '{display}': {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_merge_from_standard_input() -> Result<String, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        return String::from_utf8(data).map_err(|error| {
            let text = format!("failed to read filter patterns from standard input: {error}");
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let mut buffer = String::new();
    io::stdin().read_to_string(&mut buffer).map_err(|error| {
        let text = format!("failed to read filter patterns from standard input: {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })?;
    Ok(buffer)
}

pub(crate) fn read_filter_patterns_from_standard_input() -> Result<Vec<String>, Message> {
    #[cfg(test)]
    if let Some(data) = take_filter_stdin_input() {
        let mut cursor = io::Cursor::new(data);
        return read_filter_patterns(&mut cursor).map_err(|error| {
            let text = format!("failed to read filter patterns from standard input: {error}");
            rsync_error!(1, text).with_role(Role::Client)
        });
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    read_filter_patterns(&mut reader).map_err(|error| {
        let text = format!("failed to read filter patterns from standard input: {error}");
        rsync_error!(1, text).with_role(Role::Client)
    })
}

pub(super) fn read_filter_patterns<R: BufRead>(reader: &mut R) -> io::Result<Vec<String>> {
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();

    loop {
        buffer.clear();
        let bytes_read = reader.read_until(b'\n', &mut buffer)?;

        if bytes_read == 0 {
            break;
        }

        if buffer.last() == Some(&b'\n') {
            buffer.pop();
        }
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }

        let line = String::from_utf8_lossy(&buffer);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        patterns.push(line.into_owned());
    }

    Ok(patterns)
}

#[cfg(test)]
thread_local! {
    static FILTER_STDIN_INPUT: std::cell::RefCell<Option<Vec<u8>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(super) fn take_filter_stdin_input() -> Option<Vec<u8>> {
    FILTER_STDIN_INPUT.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
pub(crate) fn set_filter_stdin_input(data: Vec<u8>) {
    FILTER_STDIN_INPUT.with(|slot| *slot.borrow_mut() = Some(data));
}
