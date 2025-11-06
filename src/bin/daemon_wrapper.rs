#![deny(unsafe_code)]

use std::ffi::{OsStr, OsString};

fn capacity_hint_for_osstring(hint: usize, overhead: usize) -> usize {
    const MAX_HINT: usize = 1024;
    hint.saturating_add(overhead).min(MAX_HINT)
}

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
    let (lower_bound, upper_bound) = iter.size_hint();
    let program = iter
        .next()
        .map(Into::into)
        .unwrap_or_else(|| OsString::from(fallback_program));

    let rest_capacity = upper_bound.unwrap_or(lower_bound);
    let mut forwarded = Vec::with_capacity(capacity_hint_for_osstring(rest_capacity, 2));
    forwarded.push(program);

    let mut rest = Vec::with_capacity(capacity_hint_for_osstring(rest_capacity, 0));
    let mut saw_daemon_flag = false;
    let mut reached_double_dash = false;

    for argument in iter {
        let argument = argument.into();
        if !reached_double_dash {
            let value = argument.as_os_str();
            if value == OsStr::new("--") {
                reached_double_dash = true;
            } else if value == OsStr::new("--daemon") {
                saw_daemon_flag = true;
            }
        }
        rest.push(argument);
    }

    if !saw_daemon_flag {
        forwarded.push(OsString::from("--daemon"));
    }

    forwarded.extend(rest);
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

    #[test]
    fn wrap_daemon_arguments_does_not_duplicate_daemon_flag() {
        let wrapped = wrap_daemon_arguments(
            [
                OsString::from("oc-rsyncd"),
                OsString::from("--daemon"),
                OsString::from("--config"),
                OsString::from("/tmp/conf"),
            ],
            "oc-rsyncd",
        );

        assert_eq!(
            wrapped,
            vec![
                OsString::from("oc-rsyncd"),
                OsString::from("--daemon"),
                OsString::from("--config"),
                OsString::from("/tmp/conf"),
            ]
        );
    }

    #[test]
    fn wrap_daemon_arguments_inserts_daemon_flag_after_double_dash() {
        let wrapped = wrap_daemon_arguments(
            [
                OsString::from("oc-rsyncd"),
                OsString::from("--"),
                OsString::from("--daemon"),
            ],
            "oc-rsyncd",
        );

        assert_eq!(
            wrapped,
            vec![
                OsString::from("oc-rsyncd"),
                OsString::from("--daemon"),
                OsString::from("--"),
                OsString::from("--daemon"),
            ]
        );
    }

    #[test]
    fn wrap_daemon_arguments_handles_huge_upper_bound_hints() {
        struct HugeHintIterator {
            yielded: bool,
        }

        impl Iterator for HugeHintIterator {
            type Item = OsString;

            fn next(&mut self) -> Option<Self::Item> {
                if self.yielded {
                    None
                } else {
                    self.yielded = true;
                    Some(OsString::from("oc-rsyncd"))
                }
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                let lower = if self.yielded { 0 } else { 1 };
                (lower, Some(usize::MAX))
            }
        }

        let wrapped = wrap_daemon_arguments(HugeHintIterator { yielded: false }, "fallback");

        assert_eq!(
            wrapped,
            vec![OsString::from("oc-rsyncd"), OsString::from("--daemon")]
        );
    }
}
