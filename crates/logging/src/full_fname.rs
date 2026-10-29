//! Upstream `full_fname()` rendering of a path outside a daemon module.

use std::path::{Component, Path};

/// Renders `path` as upstream `full_fname()` does outside a daemon module:
/// double quoted, with a relative name prefixed by the working directory.
///
/// upstream: util1.c:1528 full_fname()
#[must_use]
pub fn full_fname(path: &Path) -> String {
    let name = slash_path(path);
    if path.has_root() {
        return format!("\"{name}\"");
    }
    match std::env::current_dir() {
        Ok(cwd) => format!("\"{}/{name}\"", cwd.display()),
        Err(_) => format!("\"{name}\""),
    }
}

/// Joins `path` with `/`, dropping `.` components as upstream's cleaned
/// names never carry them.
#[must_use]
pub fn slash_path(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_is_quoted_unchanged() {
        assert_eq!(full_fname(Path::new("/tmp/./x/bf")), "\"/tmp/x/bf\"");
    }

    #[test]
    fn relative_path_is_anchored_to_the_working_directory() {
        // upstream: util1.c:1546-1553 prefixes curr_dir and a slash.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            full_fname(Path::new("./nodir/bf.sh")),
            format!("\"{}/nodir/bf.sh\"", cwd.display())
        );
    }
}
