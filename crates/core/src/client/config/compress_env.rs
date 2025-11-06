#![deny(unsafe_code)]

use std::env;

use crate::{
    message::{Message, Role},
    rsync_error,
};

const TRUE_VALUES: &[&str] = &["1", "true", "yes", "on"];
const FALSE_VALUES: &[&str] = &["0", "false", "no", "off"];

/// Interprets an environment variable that forces compression to be disabled.
///
/// Returning [`Ok(None)`] indicates that the variable is unset. [`Ok(Some(true))`]
/// disables compression regardless of CLI flags, while [`Ok(Some(false))`]
/// leaves CLI/rsync defaults in place. Any other value results in a branded
/// diagnostic so callers can correct the configuration.
pub fn force_no_compress_from_env(variable: &str) -> Result<Option<bool>, Message> {
    let Some(value) = env::var_os(variable) else {
        return Ok(None);
    };

    let text = value.to_str().ok_or_else(|| {
        rsync_error!(
            1,
            format!("{variable} accepts only UTF-8 values in this build")
        )
        .with_role(Role::Client)
    })?;

    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(rsync_error!(
            1,
            format!("{variable} must be set to 0, 1, true, false, yes, no, on, or off")
        )
        .with_role(Role::Client));
    }

    if TRUE_VALUES
        .iter()
        .any(|candidate| trimmed.eq_ignore_ascii_case(candidate))
    {
        return Ok(Some(true));
    }

    if FALSE_VALUES
        .iter()
        .any(|candidate| trimmed.eq_ignore_ascii_case(candidate))
    {
        return Ok(Some(false));
    }

    Err(rsync_error!(
        1,
        format!(
            "invalid {variable} value '{text}': expected 0, 1, true, false, yes, no, on, or off"
        )
    )
    .with_role(Role::Client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::{OsStr, OsString};

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }

        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    #[allow(unsafe_code)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }

    const VARIABLE: &str = "OC_RSYNC_FORCE_NO_COMPRESS_TEST";

    #[test]
    fn unset_variable_returns_none() {
        let _guard = EnvGuard::remove(VARIABLE);
        assert_eq!(
            force_no_compress_from_env(VARIABLE).expect("unset variable"),
            None
        );
    }

    #[test]
    fn true_values_enable_forced_disable() {
        let _guard = EnvGuard::set(VARIABLE, OsStr::new("1"));
        assert_eq!(
            force_no_compress_from_env(VARIABLE).expect("parse true"),
            Some(true)
        );

        let _guard = EnvGuard::set(VARIABLE, OsStr::new("TRUE"));
        assert_eq!(
            force_no_compress_from_env(VARIABLE).expect("parse upper true"),
            Some(true)
        );
    }

    #[test]
    fn false_values_disable_override() {
        let _guard = EnvGuard::set(VARIABLE, OsStr::new("0"));
        assert_eq!(
            force_no_compress_from_env(VARIABLE).expect("parse false"),
            Some(false)
        );

        let _guard = EnvGuard::set(VARIABLE, OsStr::new("No"));
        assert_eq!(
            force_no_compress_from_env(VARIABLE).expect("parse case-insensitive"),
            Some(false)
        );
    }

    #[test]
    fn invalid_values_error() {
        let _guard = EnvGuard::set(VARIABLE, OsStr::new("maybe"));
        let error = force_no_compress_from_env(VARIABLE).expect_err("invalid value");
        let rendered = error.to_string();
        assert!(rendered.contains("invalid"));
        assert!(rendered.contains(VARIABLE));
    }

    #[test]
    fn non_utf8_values_error() {
        #[cfg(unix)]
        let value = {
            use std::os::unix::ffi::OsStringExt;

            OsString::from_vec(vec![0xFF])
        };

        #[cfg(windows)]
        let value = {
            use std::os::windows::ffi::OsStringExt;

            // Use an unpaired surrogate to produce a non-UTF-16 value.
            OsString::from_wide(&[0xD800])
        };

        let _guard = EnvGuard::set(VARIABLE, &value);
        let error = force_no_compress_from_env(VARIABLE).expect_err("non UTF-8");
        let rendered = error.to_string();
        assert!(rendered.contains("UTF-8"));
        assert!(rendered.contains(VARIABLE));
    }
}
