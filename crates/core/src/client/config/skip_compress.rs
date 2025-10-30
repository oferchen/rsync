use std::env;
use std::ffi::OsStr;

use crate::{
    message::{Message, Role},
    rsync_error,
};
use rsync_engine::SkipCompressList;

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
/// Returning [`Ok(None)`] indicates that the variable was unset, allowing
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
