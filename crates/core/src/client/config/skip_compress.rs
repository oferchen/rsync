use std::ffi::OsStr;

use crate::{
    message::{Message, Role},
    rsync_error,
};
use engine::SkipCompressList;

/// Parses a `--skip-compress` specification into a [`SkipCompressList`].
///
/// This function converts a user-provided pattern list (e.g., `"*.jpg/*.zip"`)
/// into a structured skip-compress list that the engine can use to avoid
/// compressing files that are already compressed.
///
/// # Arguments
///
/// * `value` - The pattern specification, typically from `--skip-compress`.
///   Should be UTF-8 encoded patterns separated by forward slashes.
///
/// # Returns
///
/// Returns `Ok(SkipCompressList)` if the patterns are valid, or an error
/// message if the specification is invalid or non-UTF-8.
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
/// use core::client::parse_skip_compress_list;
///
/// let list = parse_skip_compress_list(OsStr::new("*.jpg/*.zip"))
///     .expect("valid pattern");
/// ```
pub fn parse_skip_compress_list(value: &OsStr) -> Result<SkipCompressList, Message> {
    let text = value.to_str().ok_or_else(|| {
        rsync_error!(
            1,
            "--skip-compress accepts only UTF-8 patterns in this build"
        )
        .with_role(Role::Client)
    })?;

    SkipCompressList::parse(text).map_err(|error| {
        rsync_error!(1, format!("invalid --skip-compress specification: {error}"))
            .with_role(Role::Client)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn parse_skip_compress_list_valid_pattern() {
        let value = OsString::from("*.jpg/*.png/*.gif");
        let result = parse_skip_compress_list(&value);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_skip_compress_list_empty_string() {
        let value = OsString::from("");
        let result = parse_skip_compress_list(&value);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_skip_compress_list_single_pattern() {
        let value = OsString::from("*.zip");
        let result = parse_skip_compress_list(&value);
        assert!(result.is_ok());
    }
}
