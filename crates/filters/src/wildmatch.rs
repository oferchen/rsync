//! Faithful port of upstream rsync's shell-style wildcard matcher.
//!
//! This is a direct translation of `lib/wildmatch.c:dowild()` from rsync
//! 3.4.4. It exists because globset's `**` semantics diverge from upstream's
//! `dowild()` in edge cases (multi-star runs, `**` adjacency, abort codes),
//! which the overnight differential fuzzer surfaces. Matching upstream byte for
//! byte is the only way to stay wire-compatible.
//!
//! upstream: `lib/wildmatch.c` (Rich $alz 1986, Wayne Davison `/`-special-case
//! and `**` extensions). Only the single-string `wildmatch()` entry point is
//! ported: rsync's virtually-joined `a` array is an allocation-avoidance device
//! that is semantically equivalent to matching against the concatenation of its
//! segments, so callers that need the joined form pass a pre-joined byte slice.

/// `dowild` returned a match.
const TRUE: i32 = 1;
/// `dowild` returned no-match for this branch.
const FALSE: i32 = 0;
/// Abort the whole match: no later starting position can succeed.
const ABORT_ALL: i32 = -1;
/// Abort back to the nearest enclosing `**`: a `/` was hit under a single `*`.
const ABORT_TO_STARSTAR: i32 = -2;

/// The character that marks an inverted character class (`[!...]`).
const NEGATE_CLASS: u8 = b'!';
/// The alternate inverted-class marker (`[^...]`), normalised to `!`.
const NEGATE_CLASS2: u8 = b'^';

/// POSIX character-class predicates used inside `[[:class:]]`.
///
/// upstream: `lib/wildmatch.c:85-225` `CC_EQ` dispatch. The `ISASCII` guard in
/// upstream is a no-op under `STDC_HEADERS`, so these mirror the C `is*`
/// functions restricted to ASCII (bytes >= 0x80 never satisfy a class).
fn cc_matches(class: &[u8], ch: u8) -> Option<bool> {
    let is_ascii = ch < 0x80;
    let res = match class {
        b"alnum" => is_ascii && ch.is_ascii_alphanumeric(),
        b"alpha" => is_ascii && ch.is_ascii_alphabetic(),
        b"blank" => ch == b' ' || ch == b'\t',
        b"cntrl" => is_ascii && ch.is_ascii_control(),
        b"digit" => ch.is_ascii_digit(),
        b"graph" => is_ascii && ch.is_ascii_graphic(),
        b"lower" => is_ascii && ch.is_ascii_lowercase(),
        b"print" => is_ascii && (ch.is_ascii_graphic() || ch == b' '),
        b"punct" => is_ascii && ch.is_ascii_punctuation(),
        b"space" => is_ascii && (ch == b' ' || (b'\t'..=b'\r').contains(&ch)),
        b"upper" => is_ascii && ch.is_ascii_uppercase(),
        b"xdigit" => ch.is_ascii_hexdigit(),
        _ => return None,
    };
    Some(res)
}

/// Returns the byte at `i`, or NUL when past the end (mirrors C string reads of
/// `*p` past the terminator).
#[inline]
fn at(bytes: &[u8], i: usize) -> u8 {
    bytes.get(i).copied().unwrap_or(0)
}

/// Core recursive matcher. `p` is the remaining pattern, `text` the remaining
/// candidate. Returns `TRUE`/`FALSE`/`ABORT_ALL`/`ABORT_TO_STARSTAR` exactly as
/// upstream's `dowild()`.
///
/// upstream: `lib/wildmatch.c:64` `static int dowild(...)` (single-string case;
/// the `a` virtual-join array is always empty here).
fn dowild(p: &[u8], text: &[u8]) -> i32 {
    let mut pi = 0usize;
    let mut ti = 0usize;

    while at(p, pi) != 0 {
        let p_ch = p[pi];
        let mut t_ch = at(text, ti);

        // while ((t_ch = *text) == '\0') { if (*a == NULL) { if p_ch != '*'
        // return ABORT_ALL; break; } ... } - single string: a is always NULL.
        if t_ch == 0 && p_ch != b'*' {
            return ABORT_ALL;
        }

        match p_ch {
            b'\\' => {
                // Literal match with following character. p[1]=='\0' falls to
                // the default test below via p_ch becoming NUL.
                pi += 1;
                let esc = at(p, pi);
                if t_ch != esc {
                    return FALSE;
                }
                pi += 1;
                ti += 1;
            }
            b'?' => {
                // Match anything but '/'.
                if t_ch == b'/' {
                    return FALSE;
                }
                pi += 1;
                ti += 1;
            }
            b'*' => {
                pi += 1;
                let special = if at(p, pi) == b'*' {
                    while at(p, pi) == b'*' {
                        pi += 1;
                    }
                    true
                } else {
                    false
                };
                if at(p, pi) == 0 {
                    // Trailing "**" matches everything. Trailing "*" matches
                    // only if there are no more slash characters.
                    if !special && text[ti..].contains(&b'/') {
                        return FALSE;
                    }
                    return TRUE;
                }
                loop {
                    if t_ch == 0 {
                        break;
                    }
                    let matched = dowild(&p[pi..], &text[ti..]);
                    if matched != FALSE {
                        if !special || matched != ABORT_TO_STARSTAR {
                            return matched;
                        }
                    } else if !special && t_ch == b'/' {
                        return ABORT_TO_STARSTAR;
                    }
                    ti += 1;
                    t_ch = at(text, ti);
                }
                return ABORT_ALL;
            }
            b'[' => {
                pi += 1;
                let mut p_ch_class = at(p, pi);
                if p_ch_class == NEGATE_CLASS2 {
                    p_ch_class = NEGATE_CLASS;
                }
                let special = p_ch_class == NEGATE_CLASS;
                if special {
                    pi += 1;
                    p_ch_class = at(p, pi);
                }
                let mut prev_ch: u8 = 0;
                let mut matched = false;
                loop {
                    if p_ch_class == 0 {
                        return ABORT_ALL;
                    }
                    if p_ch_class == b'\\' {
                        pi += 1;
                        p_ch_class = at(p, pi);
                        if p_ch_class == 0 {
                            return ABORT_ALL;
                        }
                        if t_ch == p_ch_class {
                            matched = true;
                        }
                    } else if p_ch_class == b'-'
                        && prev_ch != 0
                        && at(p, pi + 1) != 0
                        && at(p, pi + 1) != b']'
                    {
                        pi += 1;
                        p_ch_class = at(p, pi);
                        if p_ch_class == b'\\' {
                            pi += 1;
                            p_ch_class = at(p, pi);
                            if p_ch_class == 0 {
                                return ABORT_ALL;
                            }
                        }
                        if t_ch <= p_ch_class && t_ch >= prev_ch {
                            matched = true;
                        }
                        p_ch_class = 0; // makes prev_ch get set to 0
                    } else if p_ch_class == b'[' && at(p, pi + 1) == b':' {
                        let s = pi + 2;
                        let mut e = s;
                        while at(p, e) != 0 && at(p, e) != b']' {
                            e += 1;
                        }
                        pi = e;
                        p_ch_class = at(p, pi);
                        if p_ch_class == 0 {
                            return ABORT_ALL;
                        }
                        // i = p - s - 1: length of the class name (between
                        // "[:" and ":]"). p[-1] must be ':'.
                        if e <= s || at(p, e - 1) != b':' {
                            // Didn't find ":]", treat like a normal set: rewind
                            // to the '[' and match it literally.
                            pi = s - 2;
                            p_ch_class = b'[';
                            if t_ch == p_ch_class {
                                matched = true;
                            }
                            // upstream `continue` re-enters the do-while with
                            // prev_ch = p_ch_class; fall through to the tail.
                        } else {
                            let name = &p[s..e - 1];
                            match cc_matches(name, t_ch) {
                                Some(true) => matched = true,
                                Some(false) => {}
                                None => return ABORT_ALL, // malformed [:class:]
                            }
                            p_ch_class = 0; // makes prev_ch get set to 0
                        }
                    } else if t_ch == p_ch_class {
                        matched = true;
                    }
                    // } while (prev_ch = p_ch, (p_ch = *++p) != ']');
                    prev_ch = p_ch_class;
                    pi += 1;
                    p_ch_class = at(p, pi);
                    if p_ch_class == b']' {
                        break;
                    }
                }
                if matched == special || t_ch == b'/' {
                    return FALSE;
                }
                pi += 1;
                ti += 1;
            }
            _ => {
                if t_ch != p_ch {
                    return FALSE;
                }
                pi += 1;
                ti += 1;
            }
        }
    }

    // do { if (*text) return FALSE; } while ((text = *a++) != NULL);
    if at(text, ti) != 0 {
        return FALSE;
    }
    TRUE
}

/// Matches `pattern` against `text` using upstream rsync's wildcard rules:
/// `?` matches any byte but `/`, `*` matches within a path segment, `**`
/// matches across `/`, `[...]` is a character class, and `\` escapes.
///
/// upstream: `lib/wildmatch.c:288` `int wildmatch(const char *pattern, const
/// char *text)`.
pub(crate) fn wildmatch(pattern: &[u8], text: &[u8]) -> bool {
    dowild(pattern, text) == TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upstream's canonical `wildtest.txt` corpus (rsync 3.4.4). Each tuple is
    /// `(expected_match, text, pattern)`, transcribed verbatim from the file's
    /// first and remaining columns (the second column, fnmatch-parity, is not
    /// relevant to wildmatch and is dropped). Comment and non-portable 8-bit
    /// rows are represented with explicit byte escapes.
    ///
    /// This is the authoritative spec for `dowild()`; a regression here means
    /// the port diverged from upstream.
    const VECTORS: &[(bool, &[u8], &[u8])] = &[
        // Basic wildmat features
        (true, b"foo", b"foo"),
        (false, b"foo", b"bar"),
        (true, b"", b""),
        (true, b"foo", b"???"),
        (false, b"foo", b"??"),
        (true, b"foo", b"*"),
        (true, b"foo", b"f*"),
        (false, b"foo", b"*f"),
        (true, b"foo", b"*foo*"),
        (true, b"foobar", b"*ob*a*r*"),
        (true, b"aaaaaaabababab", b"*ab"),
        (true, b"foo*", b"foo\\*"),
        (false, b"foobar", b"foo\\*bar"),
        (true, b"f\\oo", b"f\\\\oo"),
        (true, b"ball", b"*[al]?"),
        (false, b"ten", b"[ten]"),
        (true, b"ten", b"**[!te]"),
        (false, b"ten", b"**[!ten]"),
        (true, b"ten", b"t[a-g]n"),
        (false, b"ten", b"t[!a-g]n"),
        (true, b"ton", b"t[!a-g]n"),
        (true, b"ton", b"t[^a-g]n"),
        (true, b"a]b", b"a[]]b"),
        (true, b"a-b", b"a[]-]b"),
        (true, b"a]b", b"a[]-]b"),
        (false, b"aab", b"a[]-]b"),
        (true, b"aab", b"a[]a-]b"),
        (true, b"]", b"]"),
        // Extended slash-matching features
        (false, b"foo/baz/bar", b"foo*bar"),
        (true, b"foo/baz/bar", b"foo**bar"),
        (false, b"foo/bar", b"foo?bar"),
        (false, b"foo/bar", b"foo[/]bar"),
        (false, b"foo/bar", b"f[^eiu][^eiu][^eiu][^eiu][^eiu]r"),
        (true, b"foo-bar", b"f[^eiu][^eiu][^eiu][^eiu][^eiu]r"),
        (false, b"foo", b"**/foo"),
        (true, b"/foo", b"**/foo"),
        (true, b"bar/baz/foo", b"**/foo"),
        (false, b"bar/baz/foo", b"*/foo"),
        (false, b"foo/bar/baz", b"**/bar*"),
        (true, b"deep/foo/bar/baz", b"**/bar/*"),
        (false, b"deep/foo/bar/baz/", b"**/bar/*"),
        (true, b"deep/foo/bar/baz/", b"**/bar/**"),
        (false, b"deep/foo/bar", b"**/bar/*"),
        (true, b"deep/foo/bar/", b"**/bar/**"),
        (true, b"foo/bar/baz", b"**/bar**"),
        (true, b"foo/bar/baz/x", b"*/bar/**"),
        (false, b"deep/foo/bar/baz/x", b"*/bar/**"),
        (true, b"deep/foo/bar/baz/x", b"**/bar/*/*"),
        // Various additional tests
        (false, b"acrt", b"a[c-c]st"),
        (true, b"acrt", b"a[c-c]rt"),
        (false, b"]", b"[!]-]"),
        (true, b"a", b"[!]-]"),
        (false, b"", b"\\"),
        (false, b"\\", b"\\"),
        (false, b"/\\", b"*/\\"),
        (true, b"/\\", b"*/\\\\"),
        (true, b"foo", b"foo"),
        (true, b"@foo", b"@foo"),
        (false, b"foo", b"@foo"),
        (true, b"[ab]", b"\\[ab]"),
        (true, b"[ab]", b"[[]ab]"),
        (true, b"[ab]", b"[[:]ab]"),
        (false, b"[ab]", b"[[::]ab]"),
        (true, b"[ab]", b"[[:digit]ab]"),
        (true, b"[ab]", b"[\\[:]ab]"),
        (true, b"?a?b", b"\\??\\?b"),
        (true, b"abc", b"\\a\\b\\c"),
        (false, b"foo", b""),
        (true, b"foo/bar/baz/to", b"**/t[o]"),
        // Character class tests
        (true, b"a1B", b"[[:alpha:]][[:digit:]][[:upper:]]"),
        (false, b"a", b"[[:digit:][:upper:][:space:]]"),
        (true, b"A", b"[[:digit:][:upper:][:space:]]"),
        (true, b"1", b"[[:digit:][:upper:][:space:]]"),
        (false, b"1", b"[[:digit:][:upper:][:spaci:]]"),
        (true, b" ", b"[[:digit:][:upper:][:space:]]"),
        (false, b".", b"[[:digit:][:upper:][:space:]]"),
        (true, b".", b"[[:digit:][:punct:][:space:]]"),
        (true, b"5", b"[[:xdigit:]]"),
        (true, b"f", b"[[:xdigit:]]"),
        (true, b"D", b"[[:xdigit:]]"),
        (
            true,
            b"_",
            b"[[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:graph:][:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]",
        ),
        (
            true,
            b"\x06",
            b"[^[:alnum:][:alpha:][:blank:][:digit:][:graph:][:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]",
        ),
        (
            true,
            b".",
            b"[^[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:lower:][:space:][:upper:][:xdigit:]]",
        ),
        (true, b"5", b"[a-c[:digit:]x-z]"),
        (true, b"b", b"[a-c[:digit:]x-z]"),
        (true, b"y", b"[a-c[:digit:]x-z]"),
        (false, b"q", b"[a-c[:digit:]x-z]"),
        // Additional tests, including some malformed wildmats
        (true, b"]", b"[\\\\-^]"),
        (false, b"[", b"[\\\\-^]"),
        (true, b"-", b"[\\-_]"),
        (true, b"]", b"[\\]]"),
        (false, b"\\]", b"[\\]]"),
        (false, b"\\", b"[\\]]"),
        (false, b"ab", b"a[]b"),
        (false, b"a[]b", b"a[]b"),
        (false, b"ab[", b"ab["),
        (false, b"ab", b"[!"),
        (false, b"ab", b"[-"),
        (true, b"-", b"[-]"),
        (false, b"-", b"[a-"),
        (false, b"-", b"[!a-"),
        (true, b"-", b"[--A]"),
        (true, b"5", b"[--A]"),
        (true, b" ", b"[ --]"),
        (true, b"$", b"[ --]"),
        (true, b"-", b"[ --]"),
        (false, b"0", b"[ --]"),
        (true, b"-", b"[---]"),
        (true, b"-", b"[------]"),
        (false, b"j", b"[a-e-n]"),
        (true, b"-", b"[a-e-n]"),
        (true, b"a", b"[!------]"),
        (false, b"[", b"[]-a]"),
        (true, b"^", b"[]-a]"),
        (false, b"^", b"[!]-a]"),
        (true, b"[", b"[!]-a]"),
        (true, b"^", b"[a^bc]"),
        (true, b"-b]", b"[a-]b]"),
        (false, b"\\", b"[\\]"),
        (true, b"\\", b"[\\\\]"),
        (false, b"\\", b"[!\\\\]"),
        (true, b"G", b"[A-\\\\]"),
        (false, b"aaabbb", b"b*a"),
        (false, b"aabcaa", b"*ba*"),
        (true, b",", b"[,]"),
        (true, b",", b"[\\\\,]"),
        (true, b"\\", b"[\\\\,]"),
        (true, b"-", b"[,-.]"),
        (false, b"+", b"[,-.]"),
        (false, b"-.]", b"[,-.]"),
        (true, b"2", b"[\\1-\\3]"),
        (true, b"3", b"[\\1-\\3]"),
        (false, b"4", b"[\\1-\\3]"),
        (true, b"\\", b"[[-\\]]"),
        (true, b"[", b"[[-\\]]"),
        (true, b"]", b"[[-\\]]"),
        (false, b"-", b"[[-\\]]"),
        // Recursion and the abort code
        (
            true,
            b"-adobe-courier-bold-o-normal--12-120-75-75-m-70-iso8859-1",
            b"-*-*-*-*-*-*-12-*-*-*-m-*-*-*",
        ),
        (
            false,
            b"-adobe-courier-bold-o-normal--12-120-75-75-X-70-iso8859-1",
            b"-*-*-*-*-*-*-12-*-*-*-m-*-*-*",
        ),
        (
            false,
            b"-adobe-courier-bold-o-normal--12-120-75-75-/-70-iso8859-1",
            b"-*-*-*-*-*-*-12-*-*-*-m-*-*-*",
        ),
        (
            true,
            b"/adobe/courier/bold/o/normal//12/120/75/75/m/70/iso8859/1",
            b"/*/*/*/*/*/*/12/*/*/*/m/*/*/*",
        ),
        (
            false,
            b"/adobe/courier/bold/o/normal//12/120/75/75/X/70/iso8859/1",
            b"/*/*/*/*/*/*/12/*/*/*/m/*/*/*",
        ),
        (
            true,
            b"abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txt",
            b"**/*a*b*g*n*t",
        ),
        (
            false,
            b"abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txtz",
            b"**/*a*b*g*n*t",
        ),
    ];

    #[test]
    fn upstream_wildtest_corpus() {
        for &(expected, text, pattern) in VECTORS {
            let got = wildmatch(pattern, text);
            assert_eq!(
                got,
                expected,
                "wildmatch(pattern={:?}, text={:?}) = {got}, want {expected}",
                String::from_utf8_lossy(pattern),
                String::from_utf8_lossy(text),
            );
        }
    }

    #[test]
    fn star_does_not_cross_slash() {
        assert!(!wildmatch(b"foo*bar", b"foo/x/bar"));
        assert!(wildmatch(b"foo**bar", b"foo/x/bar"));
    }

    #[test]
    fn triple_star_collapses_to_double() {
        // Runs of 3+ stars behave like `**` (cross-segment).
        assert!(wildmatch(b"foo/***", b"foo/a/b"));
        assert!(wildmatch(b"*/***", b"0/"));
    }

    #[test]
    fn question_mark_excludes_slash() {
        assert!(wildmatch(b"a?c", b"abc"));
        assert!(!wildmatch(b"a?c", b"a/c"));
    }
}
