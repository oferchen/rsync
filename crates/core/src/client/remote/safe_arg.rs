//! Port of upstream `safe_arg()` (`options.c:2703-2769`), the backslash
//! escaping a client applies to each argument it passes to the server.
//!
//! A remote shell joins the `--server` argv into one command line and splits it
//! again, and an upstream daemon reverses the escaping with `unbackslash_arg()`
//! (`io.c:1467`). Escaping the same bytes upstream escapes is what lets a value
//! carrying spaces, quotes, wildcards or a newline reach the server intact.
//!
//! The escape runs over raw bytes on Unix, so a non-UTF-8 path survives
//! verbatim. Every escaped byte is ASCII, so membership tests never split a
//! multibyte sequence. Other targets escape the lossy Unicode view, which is
//! exact for the argv-sourced values they carry.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

/// upstream: options.c:2705 `SHELL_CHARS`. `\n` and `\r` are load-bearing: an
/// unescaped newline ends the remote command and starts a second one.
const SHELL_CHARS: &[u8] = b"!#$&;|<>(){}\"'` \t\n\r\\";

/// upstream: options.c:2706 `WILD_CHARS`. Brace expansion is deliberately absent.
const WILD_CHARS: &[u8] = b"*?[]";

/// The two `safe_arg()` modes, chosen by whether `opt` is `NULL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArgKind {
    /// `safe_arg(NULL, arg)`: wildcards stay live for the remote glob, and a
    /// backslash before a wildcard is kept as the user's wildcard escape.
    Filename,
    /// `safe_arg(opt, arg)`: wildcards are escaped too and every backslash is
    /// doubled, so the server sees the value literally.
    OptionValue,
}

impl ArgKind {
    fn escapes(self, byte: u8) -> bool {
        // upstream: options.c:2708 - `is_filename_arg ? SHELL_CHARS : WILD_CHARS SHELL_CHARS`.
        SHELL_CHARS.contains(&byte) || (self == Self::OptionValue && is_wild(byte))
    }
}

fn is_wild(byte: u8) -> bool {
    WILD_CHARS.contains(&byte)
}

/// Backslash-escapes the metacharacters of `arg`, borrowing it when none occur.
///
/// A backslash is doubled unless, in a filename, it precedes a wildcard. A
/// trailing backslash is always doubled.
///
/// upstream: options.c:2724-2735 (the counter) and :2757-2765 (the writer).
pub(crate) fn escape_metachars(arg: &[u8], kind: ArgKind) -> Cow<'_, [u8]> {
    let escape_at = |i: usize| match arg[i] {
        b'\\' => !(kind == ArgKind::Filename && arg.get(i + 1).is_some_and(|&b| is_wild(b))),
        b => kind.escapes(b),
    };
    if !(0..arg.len()).any(&escape_at) {
        return Cow::Borrowed(arg);
    }
    let mut out = Vec::with_capacity(arg.len() + 8);
    for (i, &b) in arg.iter().enumerate() {
        if escape_at(i) {
            out.push(b'\\');
        }
        out.push(b);
    }
    Cow::Owned(out)
}

/// `safe_arg(NULL, arg)` for a remote path operand.
///
/// A leading `-` gains a `./` prefix so the server cannot read the path as an
/// option, and with `escape_leading_tilde` a leading `~` gains a backslash so
/// the remote shell does not expand it. The caller decides whether escaping
/// applies at all (the `protect_args`/`--old-args` gate) and when the tilde
/// rule holds (`options.c:2717-2722`).
///
/// upstream: options.c:2710 escape_leading_dash, :2749-2755.
pub(crate) fn filename_arg(arg: &OsStr, escape_leading_tilde: bool) -> OsString {
    map_bytes(arg, |bytes| {
        let prefix: &[u8] = match bytes.first() {
            Some(b'-') => b"./",
            Some(b'~') if escape_leading_tilde => b"\\",
            _ => b"",
        };
        let body = escape_metachars(bytes, ArgKind::Filename);
        [prefix, body.as_ref()].concat()
    })
}

/// `safe_arg(opt, value)` for an option value, escaped when `escape` holds.
///
/// A non-empty `opt` yields the joined `opt=value` token; an empty one yields
/// the value alone, for the split `--opt VALUE` idiom upstream uses so the
/// remote shell can expand a `~/path` value. The caller evaluates upstream's
/// escape gate.
///
/// upstream: options.c:2711-2747.
pub(crate) fn option_arg(opt: &str, value: &OsStr, escape: bool) -> OsString {
    map_bytes(value, |bytes| {
        let value = if escape {
            escape_metachars(bytes, ArgKind::OptionValue)
        } else {
            Cow::Borrowed(bytes)
        };
        if opt.is_empty() {
            value.into_owned()
        } else {
            [opt.as_bytes(), b"=", value.as_ref()].concat()
        }
    })
}

fn map_bytes(arg: &OsStr, f: impl FnOnce(&[u8]) -> Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        OsString::from_vec(f(arg.as_bytes()))
    }
    #[cfg(not(unix))]
    {
        let out = f(arg.to_string_lossy().as_bytes());
        OsString::from(String::from_utf8_lossy(&out).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(v: &str) -> String {
        option_arg("", OsStr::new(v), true)
            .into_string()
            .expect("ascii")
    }

    fn filename(v: &str) -> String {
        filename_arg(OsStr::new(v), false)
            .into_string()
            .expect("ascii")
    }

    /// Option values escape wildcards and double every backslash, so the
    /// server receives the literal value; filenames leave wildcards live for
    /// the remote glob. Vectors are what 3.5.1 sends (recorded with an rsh).
    #[test]
    fn option_values_and_filenames_escape_like_upstream() {
        assert_eq!(value("~/bak dir*"), "~/bak\\ dir\\*");
        assert_eq!(value("c\\d[x]"), "c\\\\d\\[x\\]");
        assert_eq!(value(".p d\"q"), ".p\\ d\\\"q");
        assert_eq!(value("/t'm p"), "/t\\'m\\ p");
        assert_eq!(filename("src dir"), "src\\ dir");
        assert_eq!(filename("a*b\\*c"), "a*b\\*c");
        assert_eq!(filename("-x"), "./-x");
    }

    /// A trailing backslash is doubled in both modes: upstream's `f[1] &&`
    /// guard stops the terminator from counting as a wildcard.
    #[test]
    fn trailing_backslash_is_doubled() {
        assert_eq!(value("a\\"), "a\\\\");
        assert_eq!(filename("a\\"), "a\\\\");
    }

    /// `~` is not a metacharacter: only the split option form and the
    /// filename tilde rule decide whether the remote shell expands it.
    #[test]
    fn tilde_is_escaped_only_by_the_leading_tilde_rule() {
        assert_eq!(value("~x"), "~x");
        assert_eq!(filename("~x"), "~x");
        assert_eq!(
            filename_arg(OsStr::new("~x"), true)
                .into_string()
                .expect("ascii"),
            "\\~x"
        );
    }

    #[test]
    fn option_arg_joins_a_named_option_and_skips_escaping_when_off() {
        assert_eq!(
            option_arg("--suffix", OsStr::new(".b k*"), true),
            OsString::from("--suffix=.b\\ k\\*")
        );
        assert_eq!(
            option_arg("--suffix", OsStr::new(".b k*"), false),
            OsString::from("--suffix=.b k*")
        );
    }

    #[test]
    fn clean_input_is_borrowed() {
        assert!(matches!(
            escape_metachars(b"plain/path", ArgKind::OptionValue),
            Cow::Borrowed(_)
        ));
    }

    /// Bytes >= 0x80 pass through untouched, so a non-UTF-8 value survives.
    #[cfg(unix)]
    #[test]
    fn non_utf8_bytes_pass_through() {
        use std::os::unix::ffi::OsStrExt;
        let escaped = option_arg("", OsStr::from_bytes(b"a\xff b"), true);
        assert_eq!(escaped.as_bytes(), b"a\xff\\ b");
    }
}
