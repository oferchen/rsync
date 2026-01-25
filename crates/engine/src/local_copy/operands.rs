//! Operand parsing utilities for local copy planning.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use super::{LocalCopyArgumentError, LocalCopyError};

/// Source operand within a [`LocalCopyPlan`](super::LocalCopyPlan).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceSpec {
    path: PathBuf,
    copy_contents: bool,
    relative_prefix_components: Option<usize>,
}

impl SourceSpec {
    pub(crate) fn from_operand(operand: &OsString) -> Result<Self, LocalCopyError> {
        if operand.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptySourceOperand,
            ));
        }

        if operand_is_remote(operand.as_os_str()) {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::RemoteOperandUnsupported,
            ));
        }

        let copy_contents = has_trailing_separator(operand.as_os_str());
        Ok(Self {
            path: PathBuf::from(operand),
            copy_contents,
            relative_prefix_components: detect_relative_prefix_components(operand.as_os_str()),
        })
    }

    pub(crate) fn relative_root(&self) -> Option<PathBuf> {
        let skip = self.relative_prefix_components.unwrap_or(0);
        let mut index = 0;
        let mut relative = PathBuf::new();

        for component in self.path.components() {
            if index < skip {
                index += 1;
                continue;
            }

            index += 1;

            match component {
                Component::CurDir | Component::RootDir => {}
                Component::Prefix(prefix) => {
                    relative.push(Path::new(prefix.as_os_str()));
                }
                Component::ParentDir => relative.push(Path::new("..")),
                Component::Normal(part) => relative.push(Path::new(part)),
            }
        }

        if relative.as_os_str().is_empty() {
            None
        } else {
            Some(relative)
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn copy_contents(&self) -> bool {
        self.copy_contents
    }
}

/// Destination operand capturing directory semantics requested by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DestinationSpec {
    path: PathBuf,
    force_directory: bool,
}

impl DestinationSpec {
    pub(crate) fn from_operand(operand: &OsString) -> Self {
        let force_directory = has_trailing_separator(operand.as_os_str());
        Self {
            path: PathBuf::from(operand),
            force_directory,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn force_directory(&self) -> bool {
        self.force_directory
    }
}

fn detect_relative_prefix_components(operand: &OsStr) -> Option<usize> {
    let path = Path::new(operand);

    #[cfg(unix)]
    if let Some(count) = detect_marker_components_unix(operand) {
        return Some(count);
    }

    #[cfg(windows)]
    if let Some(count) = detect_marker_components_windows(operand) {
        return Some(count);
    }

    // Iterate without collecting - only need first 2 components
    let mut components = path.components();
    let mut skip = 0;

    let first = components.next()?;

    if matches!(first, Component::Prefix(_)) {
        if !path.has_root() {
            return None;
        }
        skip += 1;
        // Check second component for RootDir
        if let Some(Component::RootDir) = components.next() {
            skip += 1;
        }
    } else if matches!(first, Component::RootDir) {
        skip += 1;
    }

    if skip > 0 { Some(skip) } else { None }
}

#[cfg(unix)]
fn detect_marker_components_unix(operand: &OsStr) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = operand.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut index = 0;
    let len = bytes.len();
    let mut component_count = 0;

    if bytes[0] == b'/' {
        component_count += 1;
        while index < len && bytes[index] == b'/' {
            index += 1;
        }
    }

    if index >= len {
        return None;
    }

    let mut start = index;
    let mut count = component_count;

    while index <= len {
        if index == len || bytes[index] == b'/' {
            if start != index {
                let component = &bytes[start..index];
                if component == b"." {
                    return Some(count);
                }
                count += 1;
            }

            while index < len && bytes[index] == b'/' {
                index += 1;
            }
            start = index;
            if index == len {
                break;
            }
        } else {
            index += 1;
        }
    }

    None
}

#[cfg(windows)]
fn detect_marker_components_windows(operand: &OsStr) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt;

    fn is_separator(unit: u16) -> bool {
        unit == b'/' as u16 || unit == b'\\' as u16
    }

    fn is_single_dot(units: &[u16]) -> bool {
        units.len() == 1 && units[0] == b'.' as u16
    }

    let units: Vec<u16> = operand.encode_wide().collect();
    if units.is_empty() {
        return None;
    }

    let len = units.len();
    let mut index = 0;
    let mut count = 0;

    if len >= 2 && units[1] == b':' as u16 {
        count += 1;
        index = 2;
        if index < len && is_separator(units[index]) {
            count += 1;
            while index < len && is_separator(units[index]) {
                index += 1;
            }
        }
    }

    if index >= len {
        return None;
    }

    let mut start = index;

    while index <= len {
        if index == len || is_separator(units[index]) {
            if start != index {
                let component = &units[start..index];
                if is_single_dot(component) {
                    return Some(count);
                }
                count += 1;
            }

            while index < len && is_separator(units[index]) {
                index += 1;
            }
            start = index;
            if index == len {
                break;
            }
        } else {
            index += 1;
        }
    }

    None
}

pub(crate) fn has_trailing_separator(path: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_bytes();
        !bytes.is_empty() && bytes.ends_with(b"/")
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let mut last_nonzero = None;
        for ch in path.encode_wide() {
            if ch != 0 {
                last_nonzero = Some(ch);
            }
        }

        last_nonzero.is_some_and(|ch| ch == b'/' as u16 || ch == b'\\' as u16)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let text = path.to_string_lossy();
        text.ends_with('/') || text.ends_with('\\')
    }
}

pub(crate) fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        #[cfg(windows)]
        if operand_has_windows_prefix(path) {
            return false;
        }

        let after = &text[colon_index + 1..];
        if after.starts_with(':') {
            return true;
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        if colon_index == 1 && before.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return false;
        }

        return true;
    }

    false
}

#[cfg(windows)]
fn operand_has_windows_prefix(path: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    const COLON: u16 = b':' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const SLASH: u16 = b'/' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    fn is_ascii_alpha(unit: u16) -> bool {
        (unit >= b'a' as u16 && unit <= b'z' as u16) || (unit >= b'A' as u16 && unit <= b'Z' as u16)
    }

    fn is_separator(unit: u16) -> bool {
        unit == SLASH || unit == BACKSLASH
    }

    let units: Vec<u16> = path.encode_wide().collect();
    if units.is_empty() {
        return false;
    }

    if units.len() >= 4
        && is_separator(units[0])
        && is_separator(units[1])
        && (units[2] == QUESTION || units[2] == DOT)
        && is_separator(units[3])
    {
        return true;
    }

    if units.len() >= 2 && is_separator(units[0]) && is_separator(units[1]) {
        return true;
    }

    if units.len() >= 2 && is_ascii_alpha(units[0]) && units[1] == COLON {
        return true;
    }

    false
}

#[cfg(all(test, windows))]
pub(crate) mod windows_operand_detection {
    use super::operand_is_remote;
    use std::ffi::OsStr;

    #[test]
    fn drive_letter_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(r"d:relative\\path")));
    }

    #[test]
    fn extended_prefixes_are_local() {
        assert!(!operand_is_remote(OsStr::new(r"\\\\?\\C:\\tmp\\file.txt")));
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\?\\UNC\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new(r"\\\\.\\pipe\\rsync")));
    }

    #[test]
    fn unc_and_forward_slash_paths_are_local() {
        assert!(!operand_is_remote(OsStr::new(
            r"\\\\server\\share\\file.txt"
        )));
        assert!(!operand_is_remote(OsStr::new("//server/share/file.txt")));
    }

    #[test]
    fn remote_operands_remain_remote() {
        assert!(operand_is_remote(OsStr::new("host:path")));
        assert!(operand_is_remote(OsStr::new("user@host:path")));
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operand_is_remote_rsync_url() {
        assert!(operand_is_remote(OsStr::new("rsync://example.com/module")));
        assert!(operand_is_remote(OsStr::new("rsync://user@host/path")));
    }

    #[test]
    fn operand_is_remote_double_colon() {
        assert!(operand_is_remote(OsStr::new("host::module")));
        assert!(operand_is_remote(OsStr::new("user@host::module/path")));
    }

    #[test]
    fn operand_is_remote_ssh_style() {
        assert!(operand_is_remote(OsStr::new("host:/path")));
        assert!(operand_is_remote(OsStr::new("user@host:/path")));
    }

    #[test]
    fn operand_is_local_plain_path() {
        assert!(!operand_is_remote(OsStr::new("/tmp/file.txt")));
        assert!(!operand_is_remote(OsStr::new("relative/path")));
        assert!(!operand_is_remote(OsStr::new("./file.txt")));
    }

    #[test]
    fn operand_is_local_path_with_slash_before_colon() {
        assert!(!operand_is_remote(OsStr::new("/path/to:file")));
        assert!(!operand_is_remote(OsStr::new("./dir:name/file")));
    }

    #[test]
    fn has_trailing_separator_true() {
        assert!(has_trailing_separator(OsStr::new("/tmp/dir/")));
        assert!(has_trailing_separator(OsStr::new("path/")));
    }

    #[test]
    fn has_trailing_separator_false() {
        assert!(!has_trailing_separator(OsStr::new("/tmp/file")));
        assert!(!has_trailing_separator(OsStr::new("path")));
    }

    #[test]
    fn has_trailing_separator_empty() {
        assert!(!has_trailing_separator(OsStr::new("")));
    }

    #[test]
    fn source_spec_from_operand_valid() {
        let spec = SourceSpec::from_operand(&OsString::from("/tmp/file.txt")).unwrap();
        assert_eq!(spec.path(), Path::new("/tmp/file.txt"));
        assert!(!spec.copy_contents());
    }

    #[test]
    fn source_spec_from_operand_with_trailing_slash() {
        let spec = SourceSpec::from_operand(&OsString::from("/tmp/dir/")).unwrap();
        assert!(spec.copy_contents());
    }

    #[test]
    fn source_spec_from_operand_empty_fails() {
        let result = SourceSpec::from_operand(&OsString::from(""));
        assert!(result.is_err());
    }

    #[test]
    fn source_spec_from_operand_remote_fails() {
        let result = SourceSpec::from_operand(&OsString::from("host:/path"));
        assert!(result.is_err());
    }

    #[test]
    fn source_spec_relative_root_simple() {
        let spec = SourceSpec::from_operand(&OsString::from("file.txt")).unwrap();
        let root = spec.relative_root();
        assert_eq!(root, Some(PathBuf::from("file.txt")));
    }

    #[test]
    fn source_spec_relative_root_absolute() {
        let spec = SourceSpec::from_operand(&OsString::from("/tmp/file.txt")).unwrap();
        let root = spec.relative_root();
        assert_eq!(root, Some(PathBuf::from("tmp/file.txt")));
    }

    #[test]
    fn destination_spec_from_operand_basic() {
        let spec = DestinationSpec::from_operand(&OsString::from("/dest"));
        assert_eq!(spec.path(), Path::new("/dest"));
        assert!(!spec.force_directory());
    }

    #[test]
    fn destination_spec_from_operand_trailing_slash() {
        let spec = DestinationSpec::from_operand(&OsString::from("/dest/"));
        assert!(spec.force_directory());
    }

    #[test]
    fn destination_spec_eq() {
        let a = DestinationSpec::from_operand(&OsString::from("/dest"));
        let b = DestinationSpec::from_operand(&OsString::from("/dest"));
        assert_eq!(a, b);
    }

    #[test]
    fn source_spec_eq() {
        let a = SourceSpec::from_operand(&OsString::from("/src")).unwrap();
        let b = SourceSpec::from_operand(&OsString::from("/src")).unwrap();
        assert_eq!(a, b);
    }
}
