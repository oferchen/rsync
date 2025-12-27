use std::env;
use std::ffi::OsStr;

use crate::{
    message::{Message, Role},
    rsync_error,
};
use engine::SkipCompressList;

/// Parses a `--skip-compress` specification into a [`SkipCompressList`].
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

/// Parses the `RSYNC_SKIP_COMPRESS` environment variable into a
/// [`SkipCompressList`].
///
/// Returning `Ok(None)` indicates that the variable was unset, allowing
/// callers to retain their default skip-compress configuration. When the
/// variable is present but empty the function returns an empty list, matching
/// upstream rsync's semantics where an explicitly empty list disables the
/// optimisation altogether.
pub fn skip_compress_from_env(variable: &str) -> Result<Option<SkipCompressList>, Message> {
    let Some(value) = env::var_os(variable) else {
        return Ok(None);
    };

    let text = value.to_str().ok_or_else(|| {
        rsync_error!(
            1,
            format!("{variable} accepts only UTF-8 patterns in this build")
        )
        .with_role(Role::Client)
    })?;

    SkipCompressList::parse(text).map(Some).map_err(|error| {
        rsync_error!(1, format!("invalid {variable} specification: {error}"))
            .with_role(Role::Client)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    // Tests for parse_skip_compress_list
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

    // Tests for skip_compress_from_env
    #[test]
    fn skip_compress_from_env_unset_returns_none() {
        // Use a unique variable name that won't be set
        let result = skip_compress_from_env("RSYNC_SKIP_COMPRESS_TEST_UNSET_VAR_12345");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
