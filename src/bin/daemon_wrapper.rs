#![deny(unsafe_code)]

use std::ffi::OsString;

/// Wraps daemon wrapper invocations so they behave like `program --daemon ARGS...`.
///
/// The helper mirrors upstream symlink launchers where the daemon binary is an
/// alias for the client with an implicit `--daemon`. When no program name is
/// provided the supplied `fallback_program` is used so downstream banners remain
/// deterministic.
#[must_use]
pub fn wrap_daemon_arguments<I, S>(arguments: I, fallback_program: &str) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut iter = arguments.into_iter();
    let program = iter
        .next()
        .map(Into::into)
        .unwrap_or_else(|| OsString::from(fallback_program));

    let mut forwarded = Vec::with_capacity(2);
    forwarded.push(program);
    forwarded.push(OsString::from("--daemon"));
    forwarded.extend(iter.map(Into::into));
    forwarded
}

#[cfg(test)]
mod tests {
    use super::wrap_daemon_arguments;
    use std::ffi::OsString;

    #[test]
    fn wrap_daemon_arguments_inserts_flag_and_preserves_rest() {
        let wrapped = wrap_daemon_arguments(
            [
                OsString::from("oc-rsyncd"),
                OsString::from("--config"),
                OsString::from("/tmp/conf"),
            ],
            "oc-rsyncd",
        );

        assert_eq!(wrapped[0], OsString::from("oc-rsyncd"));
        assert_eq!(wrapped[1], OsString::from("--daemon"));
        assert_eq!(wrapped[2], OsString::from("--config"));
        assert_eq!(wrapped[3], OsString::from("/tmp/conf"));
    }

    #[test]
    fn wrap_daemon_arguments_uses_fallback_when_empty() {
        let wrapped = wrap_daemon_arguments::<[OsString; 0], _>([], "rsyncd");
        assert_eq!(
            wrapped,
            vec![OsString::from("rsyncd"), OsString::from("--daemon")]
        );
    }
}
