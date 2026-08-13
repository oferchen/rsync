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

/// Folds an upper-case ASCII byte to lower case when `fold` is set.
///
/// upstream: `lib/wildmatch.c dowild()` - `if (force_lower_case && ISUPPER(c))`.
/// `ISASCII` is a no-op under `STDC_HEADERS`, so upstream's `isupper` runs in
/// the C locale, where only ASCII `A`-`Z` qualify.
#[inline]
const fn maybe_fold(ch: u8, fold: bool) -> u8 {
    if fold && ch.is_ascii_uppercase() {
        ch.to_ascii_lowercase()
    } else {
        ch
    }
}

/// Core recursive matcher. `p` is the remaining pattern, `text` the remaining
/// candidate. `fold` selects the case-insensitive mode, folding both the text
/// and the pattern. Returns `TRUE`/`FALSE`/`ABORT_ALL`/`ABORT_TO_STARSTAR`
/// exactly as upstream's `dowild()`.
///
/// upstream: `lib/wildmatch.c:64` `static int dowild(...)` (single-string case;
/// the `a` virtual-join array is always empty here). `fold` stands in for
/// upstream's `force_lower_case` file-static, which `iwildmatch()` raises
/// around its `dowild()` call (`lib/wildmatch.c:298`).
fn dowild(p: &[u8], text: &[u8], fold: bool) -> i32 {
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

        t_ch = maybe_fold(t_ch, fold);

        match p_ch {
            b'\\' => {
                // Literal match with following character. p[1]=='\0' falls to
                // the default test below via p_ch becoming NUL.
                pi += 1;
                let esc = maybe_fold(at(p, pi), fold);
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
                    let matched = dowild(&p[pi..], &text[ti..], fold);
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
                        p_ch_class = maybe_fold(p_ch_class, fold);
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
                        p_ch_class = maybe_fold(p_ch_class, fold);
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
                    } else {
                        p_ch_class = maybe_fold(p_ch_class, fold);
                        if t_ch == p_ch_class {
                            matched = true;
                        }
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
                if t_ch != maybe_fold(p_ch, fold) {
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
pub fn wildmatch(pattern: &[u8], text: &[u8]) -> bool {
    dowild(pattern, text, false) == TRUE
}

/// Case-insensitive [`wildmatch`]: ASCII `A`-`Z` fold to lower case on **both**
/// sides, including inside `[...]` bracket expressions.
///
/// Folding the pattern as well as the text is what makes the match symmetric.
/// Before rsync 3.5.0 only the text was folded, so an upper-case token such as
/// a `hosts deny` entry `*.BADDOMAIN.COM` never matched a lower-case peer name
/// and the access check failed open. The same asymmetry hit bracket members, so
/// `[A-Z]bc` did not match `abc`.
///
/// The fold applies to literal pattern bytes, escaped bracket members
/// (`[\A]`), and both ends of a bracket range (`[A-Z]`; the low end is folded
/// as the previous member before it becomes the range start). POSIX classes are
/// deliberately *not* special-cased: they test the already-folded text byte, so
/// `[[:upper:]]` never matches under this entry point, exactly as upstream.
///
/// upstream: `lib/wildmatch.c:298` `int iwildmatch(const char *pattern, const
/// char *text)`. rsync 3.5.0 added the four pattern-side folds inside
/// `dowild()`: the literal/escaped default arm, the escaped bracket member, the
/// bracket range end, and the plain bracket member.
pub fn iwildmatch(pattern: &[u8], text: &[u8]) -> bool {
    dowild(pattern, text, true) == TRUE
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

    /// Case-folding matrix generated from upstream rsync 3.5.0's own
    /// `lib/wildmatch.c`. Each row is
    /// `(pattern, text, iwildmatch_expected, wildmatch_expected)`.
    ///
    /// The first 518 rows sweep every bracket construct upstream's `dowild()` handles -
    /// ranges (both directions), `[!...]`/`[^...]` negation, escaped members,
    /// a literal `]` first in the set, POSIX classes, sets mixing ranges with
    /// classes, and malformed sets - against a spread of upper-case,
    /// lower-case, caseless and delimiter texts. The remaining rows are
    /// upstream's own `t_iwildmatch.c` cases plus the bracket-bearing rows of
    /// `wildtest.txt`, each permuted over as-is/upper/lower on both sides.
    ///
    /// The `wildmatch` column is generated from the same binary and is
    /// identical under 3.4.4 and 3.5.0 (measured: 0 of 685 rows differ), so a
    /// fold that leaked into the case-sensitive entry point fails here.
    ///
    /// upstream: `lib/wildmatch.c` (rsync 3.5.0), `t_iwildmatch.c`,
    /// `wildtest.txt`.
    const FOLD_VECTORS: &[(&[u8], &[u8], bool, bool)] = &[
        (b"[a-z]x", b"ax", true, true),
        (b"[a-z]x", b"Ax", true, false),
        (b"[a-z]x", b"mx", true, true),
        (b"[a-z]x", b"Mx", true, false),
        (b"[a-z]x", b"zx", true, true),
        (b"[a-z]x", b"Zx", true, false),
        (b"[a-z]x", b"5x", false, false),
        (b"[a-z]x", b"-x", false, false),
        (b"[a-z]x", b"]x", false, false),
        (b"[a-z]x", b"[x", false, false),
        (b"[a-z]x", b"\\x", false, false),
        (b"[a-z]x", b"_x", false, false),
        (b"[a-z]x", b" x", false, false),
        (b"[a-z]x", b"/x", false, false),
        (b"[A-Z]x", b"ax", true, false),
        (b"[A-Z]x", b"Ax", true, true),
        (b"[A-Z]x", b"mx", true, false),
        (b"[A-Z]x", b"Mx", true, true),
        (b"[A-Z]x", b"zx", true, false),
        (b"[A-Z]x", b"Zx", true, true),
        (b"[A-Z]x", b"5x", false, false),
        (b"[A-Z]x", b"-x", false, false),
        (b"[A-Z]x", b"]x", false, false),
        (b"[A-Z]x", b"[x", false, false),
        (b"[A-Z]x", b"\\x", false, false),
        (b"[A-Z]x", b"_x", false, false),
        (b"[A-Z]x", b" x", false, false),
        (b"[A-Z]x", b"/x", false, false),
        (b"[a-Z]x", b"ax", true, true),
        (b"[a-Z]x", b"Ax", true, false),
        (b"[a-Z]x", b"mx", true, false),
        (b"[a-Z]x", b"Mx", true, false),
        (b"[a-Z]x", b"zx", true, false),
        (b"[a-Z]x", b"Zx", true, false),
        (b"[a-Z]x", b"5x", false, false),
        (b"[a-Z]x", b"-x", false, false),
        (b"[a-Z]x", b"]x", false, false),
        (b"[a-Z]x", b"[x", false, false),
        (b"[a-Z]x", b"\\x", false, false),
        (b"[a-Z]x", b"_x", false, false),
        (b"[a-Z]x", b" x", false, false),
        (b"[a-Z]x", b"/x", false, false),
        (b"[A-z]x", b"ax", true, true),
        (b"[A-z]x", b"Ax", true, true),
        (b"[A-z]x", b"mx", true, true),
        (b"[A-z]x", b"Mx", true, true),
        (b"[A-z]x", b"zx", true, true),
        (b"[A-z]x", b"Zx", true, true),
        (b"[A-z]x", b"5x", false, false),
        (b"[A-z]x", b"-x", false, false),
        (b"[A-z]x", b"]x", false, true),
        (b"[A-z]x", b"[x", false, true),
        (b"[A-z]x", b"\\x", false, true),
        (b"[A-z]x", b"_x", false, true),
        (b"[A-z]x", b" x", false, false),
        (b"[A-z]x", b"/x", false, false),
        (b"[!a-z]x", b"ax", false, false),
        (b"[!a-z]x", b"Ax", false, true),
        (b"[!a-z]x", b"mx", false, false),
        (b"[!a-z]x", b"Mx", false, true),
        (b"[!a-z]x", b"zx", false, false),
        (b"[!a-z]x", b"Zx", false, true),
        (b"[!a-z]x", b"5x", true, true),
        (b"[!a-z]x", b"-x", true, true),
        (b"[!a-z]x", b"]x", true, true),
        (b"[!a-z]x", b"[x", true, true),
        (b"[!a-z]x", b"\\x", true, true),
        (b"[!a-z]x", b"_x", true, true),
        (b"[!a-z]x", b" x", true, true),
        (b"[!a-z]x", b"/x", false, false),
        (b"[!A-Z]x", b"ax", false, true),
        (b"[!A-Z]x", b"Ax", false, false),
        (b"[!A-Z]x", b"mx", false, true),
        (b"[!A-Z]x", b"Mx", false, false),
        (b"[!A-Z]x", b"zx", false, true),
        (b"[!A-Z]x", b"Zx", false, false),
        (b"[!A-Z]x", b"5x", true, true),
        (b"[!A-Z]x", b"-x", true, true),
        (b"[!A-Z]x", b"]x", true, true),
        (b"[!A-Z]x", b"[x", true, true),
        (b"[!A-Z]x", b"\\x", true, true),
        (b"[!A-Z]x", b"_x", true, true),
        (b"[!A-Z]x", b" x", true, true),
        (b"[!A-Z]x", b"/x", false, false),
        (b"[^a-z]x", b"ax", false, false),
        (b"[^a-z]x", b"Ax", false, true),
        (b"[^a-z]x", b"mx", false, false),
        (b"[^a-z]x", b"Mx", false, true),
        (b"[^a-z]x", b"zx", false, false),
        (b"[^a-z]x", b"Zx", false, true),
        (b"[^a-z]x", b"5x", true, true),
        (b"[^a-z]x", b"-x", true, true),
        (b"[^a-z]x", b"]x", true, true),
        (b"[^a-z]x", b"[x", true, true),
        (b"[^a-z]x", b"\\x", true, true),
        (b"[^a-z]x", b"_x", true, true),
        (b"[^a-z]x", b" x", true, true),
        (b"[^a-z]x", b"/x", false, false),
        (b"[^A-Z]x", b"ax", false, true),
        (b"[^A-Z]x", b"Ax", false, false),
        (b"[^A-Z]x", b"mx", false, true),
        (b"[^A-Z]x", b"Mx", false, false),
        (b"[^A-Z]x", b"zx", false, true),
        (b"[^A-Z]x", b"Zx", false, false),
        (b"[^A-Z]x", b"5x", true, true),
        (b"[^A-Z]x", b"-x", true, true),
        (b"[^A-Z]x", b"]x", true, true),
        (b"[^A-Z]x", b"[x", true, true),
        (b"[^A-Z]x", b"\\x", true, true),
        (b"[^A-Z]x", b"_x", true, true),
        (b"[^A-Z]x", b" x", true, true),
        (b"[^A-Z]x", b"/x", false, false),
        (b"[abc]x", b"ax", true, true),
        (b"[abc]x", b"Ax", true, false),
        (b"[abc]x", b"mx", false, false),
        (b"[abc]x", b"Mx", false, false),
        (b"[abc]x", b"zx", false, false),
        (b"[abc]x", b"Zx", false, false),
        (b"[abc]x", b"5x", false, false),
        (b"[abc]x", b"-x", false, false),
        (b"[abc]x", b"]x", false, false),
        (b"[abc]x", b"[x", false, false),
        (b"[abc]x", b"\\x", false, false),
        (b"[abc]x", b"_x", false, false),
        (b"[abc]x", b" x", false, false),
        (b"[abc]x", b"/x", false, false),
        (b"[ABC]x", b"ax", true, false),
        (b"[ABC]x", b"Ax", true, true),
        (b"[ABC]x", b"mx", false, false),
        (b"[ABC]x", b"Mx", false, false),
        (b"[ABC]x", b"zx", false, false),
        (b"[ABC]x", b"Zx", false, false),
        (b"[ABC]x", b"5x", false, false),
        (b"[ABC]x", b"-x", false, false),
        (b"[ABC]x", b"]x", false, false),
        (b"[ABC]x", b"[x", false, false),
        (b"[ABC]x", b"\\x", false, false),
        (b"[ABC]x", b"_x", false, false),
        (b"[ABC]x", b" x", false, false),
        (b"[ABC]x", b"/x", false, false),
        (b"[aBc]x", b"ax", true, true),
        (b"[aBc]x", b"Ax", true, false),
        (b"[aBc]x", b"mx", false, false),
        (b"[aBc]x", b"Mx", false, false),
        (b"[aBc]x", b"zx", false, false),
        (b"[aBc]x", b"Zx", false, false),
        (b"[aBc]x", b"5x", false, false),
        (b"[aBc]x", b"-x", false, false),
        (b"[aBc]x", b"]x", false, false),
        (b"[aBc]x", b"[x", false, false),
        (b"[aBc]x", b"\\x", false, false),
        (b"[aBc]x", b"_x", false, false),
        (b"[aBc]x", b" x", false, false),
        (b"[aBc]x", b"/x", false, false),
        (b"[\\A]x", b"ax", true, false),
        (b"[\\A]x", b"Ax", true, true),
        (b"[\\A]x", b"mx", false, false),
        (b"[\\A]x", b"Mx", false, false),
        (b"[\\A]x", b"zx", false, false),
        (b"[\\A]x", b"Zx", false, false),
        (b"[\\A]x", b"5x", false, false),
        (b"[\\A]x", b"-x", false, false),
        (b"[\\A]x", b"]x", false, false),
        (b"[\\A]x", b"[x", false, false),
        (b"[\\A]x", b"\\x", false, false),
        (b"[\\A]x", b"_x", false, false),
        (b"[\\A]x", b" x", false, false),
        (b"[\\A]x", b"/x", false, false),
        (b"[\\a]x", b"ax", true, true),
        (b"[\\a]x", b"Ax", true, false),
        (b"[\\a]x", b"mx", false, false),
        (b"[\\a]x", b"Mx", false, false),
        (b"[\\a]x", b"zx", false, false),
        (b"[\\a]x", b"Zx", false, false),
        (b"[\\a]x", b"5x", false, false),
        (b"[\\a]x", b"-x", false, false),
        (b"[\\a]x", b"]x", false, false),
        (b"[\\a]x", b"[x", false, false),
        (b"[\\a]x", b"\\x", false, false),
        (b"[\\a]x", b"_x", false, false),
        (b"[\\a]x", b" x", false, false),
        (b"[\\a]x", b"/x", false, false),
        (b"[!\\A]x", b"ax", false, true),
        (b"[!\\A]x", b"Ax", false, false),
        (b"[!\\A]x", b"mx", true, true),
        (b"[!\\A]x", b"Mx", true, true),
        (b"[!\\A]x", b"zx", true, true),
        (b"[!\\A]x", b"Zx", true, true),
        (b"[!\\A]x", b"5x", true, true),
        (b"[!\\A]x", b"-x", true, true),
        (b"[!\\A]x", b"]x", true, true),
        (b"[!\\A]x", b"[x", true, true),
        (b"[!\\A]x", b"\\x", true, true),
        (b"[!\\A]x", b"_x", true, true),
        (b"[!\\A]x", b" x", true, true),
        (b"[!\\A]x", b"/x", false, false),
        (b"[]A-]x", b"ax", true, false),
        (b"[]A-]x", b"Ax", true, true),
        (b"[]A-]x", b"mx", false, false),
        (b"[]A-]x", b"Mx", false, false),
        (b"[]A-]x", b"zx", false, false),
        (b"[]A-]x", b"Zx", false, false),
        (b"[]A-]x", b"5x", false, false),
        (b"[]A-]x", b"-x", true, true),
        (b"[]A-]x", b"]x", true, true),
        (b"[]A-]x", b"[x", false, false),
        (b"[]A-]x", b"\\x", false, false),
        (b"[]A-]x", b"_x", false, false),
        (b"[]A-]x", b" x", false, false),
        (b"[]A-]x", b"/x", false, false),
        (b"[]a-]x", b"ax", true, true),
        (b"[]a-]x", b"Ax", true, false),
        (b"[]a-]x", b"mx", false, false),
        (b"[]a-]x", b"Mx", false, false),
        (b"[]a-]x", b"zx", false, false),
        (b"[]a-]x", b"Zx", false, false),
        (b"[]a-]x", b"5x", false, false),
        (b"[]a-]x", b"-x", true, true),
        (b"[]a-]x", b"]x", true, true),
        (b"[]a-]x", b"[x", false, false),
        (b"[]a-]x", b"\\x", false, false),
        (b"[]a-]x", b"_x", false, false),
        (b"[]a-]x", b" x", false, false),
        (b"[]a-]x", b"/x", false, false),
        (b"[]A-Z]x", b"ax", true, false),
        (b"[]A-Z]x", b"Ax", true, true),
        (b"[]A-Z]x", b"mx", true, false),
        (b"[]A-Z]x", b"Mx", true, true),
        (b"[]A-Z]x", b"zx", true, false),
        (b"[]A-Z]x", b"Zx", true, true),
        (b"[]A-Z]x", b"5x", false, false),
        (b"[]A-Z]x", b"-x", false, false),
        (b"[]A-Z]x", b"]x", true, true),
        (b"[]A-Z]x", b"[x", false, false),
        (b"[]A-Z]x", b"\\x", false, false),
        (b"[]A-Z]x", b"_x", false, false),
        (b"[]A-Z]x", b" x", false, false),
        (b"[]A-Z]x", b"/x", false, false),
        (b"[[:alpha:]]x", b"ax", true, true),
        (b"[[:alpha:]]x", b"Ax", true, true),
        (b"[[:alpha:]]x", b"mx", true, true),
        (b"[[:alpha:]]x", b"Mx", true, true),
        (b"[[:alpha:]]x", b"zx", true, true),
        (b"[[:alpha:]]x", b"Zx", true, true),
        (b"[[:alpha:]]x", b"5x", false, false),
        (b"[[:alpha:]]x", b"-x", false, false),
        (b"[[:alpha:]]x", b"]x", false, false),
        (b"[[:alpha:]]x", b"[x", false, false),
        (b"[[:alpha:]]x", b"\\x", false, false),
        (b"[[:alpha:]]x", b"_x", false, false),
        (b"[[:alpha:]]x", b" x", false, false),
        (b"[[:alpha:]]x", b"/x", false, false),
        (b"[[:upper:]]x", b"ax", false, false),
        (b"[[:upper:]]x", b"Ax", false, true),
        (b"[[:upper:]]x", b"mx", false, false),
        (b"[[:upper:]]x", b"Mx", false, true),
        (b"[[:upper:]]x", b"zx", false, false),
        (b"[[:upper:]]x", b"Zx", false, true),
        (b"[[:upper:]]x", b"5x", false, false),
        (b"[[:upper:]]x", b"-x", false, false),
        (b"[[:upper:]]x", b"]x", false, false),
        (b"[[:upper:]]x", b"[x", false, false),
        (b"[[:upper:]]x", b"\\x", false, false),
        (b"[[:upper:]]x", b"_x", false, false),
        (b"[[:upper:]]x", b" x", false, false),
        (b"[[:upper:]]x", b"/x", false, false),
        (b"[[:lower:]]x", b"ax", true, true),
        (b"[[:lower:]]x", b"Ax", true, false),
        (b"[[:lower:]]x", b"mx", true, true),
        (b"[[:lower:]]x", b"Mx", true, false),
        (b"[[:lower:]]x", b"zx", true, true),
        (b"[[:lower:]]x", b"Zx", true, false),
        (b"[[:lower:]]x", b"5x", false, false),
        (b"[[:lower:]]x", b"-x", false, false),
        (b"[[:lower:]]x", b"]x", false, false),
        (b"[[:lower:]]x", b"[x", false, false),
        (b"[[:lower:]]x", b"\\x", false, false),
        (b"[[:lower:]]x", b"_x", false, false),
        (b"[[:lower:]]x", b" x", false, false),
        (b"[[:lower:]]x", b"/x", false, false),
        (b"[[:digit:]]x", b"ax", false, false),
        (b"[[:digit:]]x", b"Ax", false, false),
        (b"[[:digit:]]x", b"mx", false, false),
        (b"[[:digit:]]x", b"Mx", false, false),
        (b"[[:digit:]]x", b"zx", false, false),
        (b"[[:digit:]]x", b"Zx", false, false),
        (b"[[:digit:]]x", b"5x", true, true),
        (b"[[:digit:]]x", b"-x", false, false),
        (b"[[:digit:]]x", b"]x", false, false),
        (b"[[:digit:]]x", b"[x", false, false),
        (b"[[:digit:]]x", b"\\x", false, false),
        (b"[[:digit:]]x", b"_x", false, false),
        (b"[[:digit:]]x", b" x", false, false),
        (b"[[:digit:]]x", b"/x", false, false),
        (b"[[:xdigit:]]x", b"ax", true, true),
        (b"[[:xdigit:]]x", b"Ax", true, true),
        (b"[[:xdigit:]]x", b"mx", false, false),
        (b"[[:xdigit:]]x", b"Mx", false, false),
        (b"[[:xdigit:]]x", b"zx", false, false),
        (b"[[:xdigit:]]x", b"Zx", false, false),
        (b"[[:xdigit:]]x", b"5x", true, true),
        (b"[[:xdigit:]]x", b"-x", false, false),
        (b"[[:xdigit:]]x", b"]x", false, false),
        (b"[[:xdigit:]]x", b"[x", false, false),
        (b"[[:xdigit:]]x", b"\\x", false, false),
        (b"[[:xdigit:]]x", b"_x", false, false),
        (b"[[:xdigit:]]x", b" x", false, false),
        (b"[[:xdigit:]]x", b"/x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"ax", true, true),
        (b"[a-c[:digit:]X-Z]x", b"Ax", true, false),
        (b"[a-c[:digit:]X-Z]x", b"mx", false, false),
        (b"[a-c[:digit:]X-Z]x", b"Mx", false, false),
        (b"[a-c[:digit:]X-Z]x", b"zx", true, false),
        (b"[a-c[:digit:]X-Z]x", b"Zx", true, true),
        (b"[a-c[:digit:]X-Z]x", b"5x", true, true),
        (b"[a-c[:digit:]X-Z]x", b"-x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"]x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"[x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"\\x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"_x", false, false),
        (b"[a-c[:digit:]X-Z]x", b" x", false, false),
        (b"[a-c[:digit:]X-Z]x", b"/x", false, false),
        (b"[A-C[:digit:]x-z]x", b"ax", true, false),
        (b"[A-C[:digit:]x-z]x", b"Ax", true, true),
        (b"[A-C[:digit:]x-z]x", b"mx", false, false),
        (b"[A-C[:digit:]x-z]x", b"Mx", false, false),
        (b"[A-C[:digit:]x-z]x", b"zx", true, true),
        (b"[A-C[:digit:]x-z]x", b"Zx", true, false),
        (b"[A-C[:digit:]x-z]x", b"5x", true, true),
        (b"[A-C[:digit:]x-z]x", b"-x", false, false),
        (b"[A-C[:digit:]x-z]x", b"]x", false, false),
        (b"[A-C[:digit:]x-z]x", b"[x", false, false),
        (b"[A-C[:digit:]x-z]x", b"\\x", false, false),
        (b"[A-C[:digit:]x-z]x", b"_x", false, false),
        (b"[A-C[:digit:]x-z]x", b" x", false, false),
        (b"[A-C[:digit:]x-z]x", b"/x", false, false),
        (b"[--A]x", b"ax", true, false),
        (b"[--A]x", b"Ax", true, true),
        (b"[--A]x", b"mx", false, false),
        (b"[--A]x", b"Mx", false, false),
        (b"[--A]x", b"zx", false, false),
        (b"[--A]x", b"Zx", false, false),
        (b"[--A]x", b"5x", true, true),
        (b"[--A]x", b"-x", true, true),
        (b"[--A]x", b"]x", true, false),
        (b"[--A]x", b"[x", true, false),
        (b"[--A]x", b"\\x", true, false),
        (b"[--A]x", b"_x", true, false),
        (b"[--A]x", b" x", false, false),
        (b"[--A]x", b"/x", false, false),
        (b"[--a]x", b"ax", true, true),
        (b"[--a]x", b"Ax", true, true),
        (b"[--a]x", b"mx", false, false),
        (b"[--a]x", b"Mx", false, true),
        (b"[--a]x", b"zx", false, false),
        (b"[--a]x", b"Zx", false, true),
        (b"[--a]x", b"5x", true, true),
        (b"[--a]x", b"-x", true, true),
        (b"[--a]x", b"]x", true, true),
        (b"[--a]x", b"[x", true, true),
        (b"[--a]x", b"\\x", true, true),
        (b"[--a]x", b"_x", true, true),
        (b"[--a]x", b" x", false, false),
        (b"[--a]x", b"/x", false, false),
        (b"[A-\\\\]x", b"ax", true, false),
        (b"[A-\\\\]x", b"Ax", true, true),
        (b"[A-\\\\]x", b"mx", false, false),
        (b"[A-\\\\]x", b"Mx", false, true),
        (b"[A-\\\\]x", b"zx", false, false),
        (b"[A-\\\\]x", b"Zx", false, true),
        (b"[A-\\\\]x", b"5x", false, false),
        (b"[A-\\\\]x", b"-x", false, false),
        (b"[A-\\\\]x", b"]x", false, false),
        (b"[A-\\\\]x", b"[x", false, true),
        (b"[A-\\\\]x", b"\\x", false, true),
        (b"[A-\\\\]x", b"_x", false, false),
        (b"[A-\\\\]x", b" x", false, false),
        (b"[A-\\\\]x", b"/x", false, false),
        (b"[\\1-\\3]x", b"ax", false, false),
        (b"[\\1-\\3]x", b"Ax", false, false),
        (b"[\\1-\\3]x", b"mx", false, false),
        (b"[\\1-\\3]x", b"Mx", false, false),
        (b"[\\1-\\3]x", b"zx", false, false),
        (b"[\\1-\\3]x", b"Zx", false, false),
        (b"[\\1-\\3]x", b"5x", false, false),
        (b"[\\1-\\3]x", b"-x", false, false),
        (b"[\\1-\\3]x", b"]x", false, false),
        (b"[\\1-\\3]x", b"[x", false, false),
        (b"[\\1-\\3]x", b"\\x", false, false),
        (b"[\\1-\\3]x", b"_x", false, false),
        (b"[\\1-\\3]x", b" x", false, false),
        (b"[\\1-\\3]x", b"/x", false, false),
        (b"[A-EZ]x", b"ax", true, false),
        (b"[A-EZ]x", b"Ax", true, true),
        (b"[A-EZ]x", b"mx", false, false),
        (b"[A-EZ]x", b"Mx", false, false),
        (b"[A-EZ]x", b"zx", true, false),
        (b"[A-EZ]x", b"Zx", true, true),
        (b"[A-EZ]x", b"5x", false, false),
        (b"[A-EZ]x", b"-x", false, false),
        (b"[A-EZ]x", b"]x", false, false),
        (b"[A-EZ]x", b"[x", false, false),
        (b"[A-EZ]x", b"\\x", false, false),
        (b"[A-EZ]x", b"_x", false, false),
        (b"[A-EZ]x", b" x", false, false),
        (b"[A-EZ]x", b"/x", false, false),
        (b"[a-ez]x", b"ax", true, true),
        (b"[a-ez]x", b"Ax", true, false),
        (b"[a-ez]x", b"mx", false, false),
        (b"[a-ez]x", b"Mx", false, false),
        (b"[a-ez]x", b"zx", true, true),
        (b"[a-ez]x", b"Zx", true, false),
        (b"[a-ez]x", b"5x", false, false),
        (b"[a-ez]x", b"-x", false, false),
        (b"[a-ez]x", b"]x", false, false),
        (b"[a-ez]x", b"[x", false, false),
        (b"[a-ez]x", b"\\x", false, false),
        (b"[a-ez]x", b"_x", false, false),
        (b"[a-ez]x", b" x", false, false),
        (b"[a-ez]x", b"/x", false, false),
        (b"[!]-A]x", b"ax", false, true),
        (b"[!]-A]x", b"Ax", false, true),
        (b"[!]-A]x", b"mx", true, true),
        (b"[!]-A]x", b"Mx", true, true),
        (b"[!]-A]x", b"zx", true, true),
        (b"[!]-A]x", b"Zx", true, true),
        (b"[!]-A]x", b"5x", true, true),
        (b"[!]-A]x", b"-x", true, true),
        (b"[!]-A]x", b"]x", false, false),
        (b"[!]-A]x", b"[x", true, true),
        (b"[!]-A]x", b"\\x", true, true),
        (b"[!]-A]x", b"_x", false, true),
        (b"[!]-A]x", b" x", true, true),
        (b"[!]-A]x", b"/x", false, false),
        (b"[!]-a]x", b"ax", false, false),
        (b"[!]-a]x", b"Ax", false, true),
        (b"[!]-a]x", b"mx", true, true),
        (b"[!]-a]x", b"Mx", true, true),
        (b"[!]-a]x", b"zx", true, true),
        (b"[!]-a]x", b"Zx", true, true),
        (b"[!]-a]x", b"5x", true, true),
        (b"[!]-a]x", b"-x", true, true),
        (b"[!]-a]x", b"]x", false, false),
        (b"[!]-a]x", b"[x", true, true),
        (b"[!]-a]x", b"\\x", true, true),
        (b"[!]-a]x", b"_x", false, false),
        (b"[!]-a]x", b" x", true, true),
        (b"[!]-a]x", b"/x", false, false),
        (b"[Aa]x", b"ax", true, true),
        (b"[Aa]x", b"Ax", true, true),
        (b"[Aa]x", b"mx", false, false),
        (b"[Aa]x", b"Mx", false, false),
        (b"[Aa]x", b"zx", false, false),
        (b"[Aa]x", b"Zx", false, false),
        (b"[Aa]x", b"5x", false, false),
        (b"[Aa]x", b"-x", false, false),
        (b"[Aa]x", b"]x", false, false),
        (b"[Aa]x", b"[x", false, false),
        (b"[Aa]x", b"\\x", false, false),
        (b"[Aa]x", b"_x", false, false),
        (b"[Aa]x", b" x", false, false),
        (b"[Aa]x", b"/x", false, false),
        (b"[A]x", b"ax", true, false),
        (b"[A]x", b"Ax", true, true),
        (b"[A]x", b"mx", false, false),
        (b"[A]x", b"Mx", false, false),
        (b"[A]x", b"zx", false, false),
        (b"[A]x", b"Zx", false, false),
        (b"[A]x", b"5x", false, false),
        (b"[A]x", b"-x", false, false),
        (b"[A]x", b"]x", false, false),
        (b"[A]x", b"[x", false, false),
        (b"[A]x", b"\\x", false, false),
        (b"[A]x", b"_x", false, false),
        (b"[A]x", b" x", false, false),
        (b"[A]x", b"/x", false, false),
        (b"[a]x", b"ax", true, true),
        (b"[a]x", b"Ax", true, false),
        (b"[a]x", b"mx", false, false),
        (b"[a]x", b"Mx", false, false),
        (b"[a]x", b"zx", false, false),
        (b"[a]x", b"Zx", false, false),
        (b"[a]x", b"5x", false, false),
        (b"[a]x", b"-x", false, false),
        (b"[a]x", b"]x", false, false),
        (b"[a]x", b"[x", false, false),
        (b"[a]x", b"\\x", false, false),
        (b"[a]x", b"_x", false, false),
        (b"[a]x", b" x", false, false),
        (b"[a]x", b"/x", false, false),
        (b"[A-x", b"ax", false, false),
        (b"[A-x", b"Ax", false, false),
        (b"[A-x", b"mx", false, false),
        (b"[A-x", b"Mx", false, false),
        (b"[A-x", b"zx", false, false),
        (b"[A-x", b"Zx", false, false),
        (b"[A-x", b"5x", false, false),
        (b"[A-x", b"-x", false, false),
        (b"[A-x", b"]x", false, false),
        (b"[A-x", b"[x", false, false),
        (b"[A-x", b"\\x", false, false),
        (b"[A-x", b"_x", false, false),
        (b"[A-x", b" x", false, false),
        (b"[A-x", b"/x", false, false),
        (b"[!A-x", b"ax", false, false),
        (b"[!A-x", b"Ax", false, false),
        (b"[!A-x", b"mx", false, false),
        (b"[!A-x", b"Mx", false, false),
        (b"[!A-x", b"zx", false, false),
        (b"[!A-x", b"Zx", false, false),
        (b"[!A-x", b"5x", false, false),
        (b"[!A-x", b"-x", false, false),
        (b"[!A-x", b"]x", false, false),
        (b"[!A-x", b"[x", false, false),
        (b"[!A-x", b"\\x", false, false),
        (b"[!A-x", b"_x", false, false),
        (b"[!A-x", b" x", false, false),
        (b"[!A-x", b"/x", false, false),
        (b"[A]b]x", b"ax", false, false),
        (b"[A]b]x", b"Ax", false, false),
        (b"[A]b]x", b"mx", false, false),
        (b"[A]b]x", b"Mx", false, false),
        (b"[A]b]x", b"zx", false, false),
        (b"[A]b]x", b"Zx", false, false),
        (b"[A]b]x", b"5x", false, false),
        (b"[A]b]x", b"-x", false, false),
        (b"[A]b]x", b"]x", false, false),
        (b"[A]b]x", b"[x", false, false),
        (b"[A]b]x", b"\\x", false, false),
        (b"[A]b]x", b"_x", false, false),
        (b"[A]b]x", b" x", false, false),
        (b"[A]b]x", b"/x", false, false),
        (b"abc", b"ABC", true, false),
        (b"abc", b"abc", true, true),
        (b"ABC", b"ABC", true, true),
        (b"ABC", b"abc", true, false),
        (b"ABC", b"abd", false, false),
        (b"ABC", b"ABD", false, false),
        (b"abc", b"abd", false, false),
        (b"abc", b"ABD", false, false),
        (b"*.EXAMPLE.COM", b"foo.example.com", true, false),
        (b"*.EXAMPLE.COM", b"FOO.EXAMPLE.COM", true, true),
        (b"*.example.com", b"foo.example.com", true, true),
        (b"*.example.com", b"FOO.EXAMPLE.COM", true, false),
        (b"*.BADDOMAIN.COM", b"x.baddomain.com", true, false),
        (b"*.BADDOMAIN.COM", b"X.BADDOMAIN.COM", true, true),
        (b"*.baddomain.com", b"x.baddomain.com", true, true),
        (b"*.baddomain.com", b"X.BADDOMAIN.COM", true, false),
        (b"[A-Z]bc", b"abc", true, false),
        (b"[A-Z]bc", b"ABC", true, false),
        (b"[A-Z]BC", b"abc", true, false),
        (b"[A-Z]BC", b"ABC", true, true),
        (b"[a-z]bc", b"abc", true, true),
        (b"[a-z]bc", b"ABC", true, false),
        (b"[ABC]xy", b"bxy", true, false),
        (b"[ABC]xy", b"BXY", true, false),
        (b"[ABC]XY", b"bxy", true, false),
        (b"[ABC]XY", b"BXY", true, true),
        (b"[abc]xy", b"bxy", true, true),
        (b"[abc]xy", b"BXY", true, false),
        (b"x[A-Z]z", b"xyz", true, false),
        (b"x[A-Z]z", b"XYZ", true, false),
        (b"X[A-Z]Z", b"xyz", true, false),
        (b"X[A-Z]Z", b"XYZ", true, true),
        (b"x[a-z]z", b"xyz", true, true),
        (b"x[a-z]z", b"XYZ", true, false),
        (b"[a-z]BC", b"abc", true, false),
        (b"[a-z]BC", b"ABC", true, false),
        (b"[\\A]bc", b"abc", true, false),
        (b"[\\A]bc", b"ABC", true, false),
        (b"[\\A]BC", b"abc", true, false),
        (b"[\\A]BC", b"ABC", true, true),
        (b"[\\a]bc", b"abc", true, true),
        (b"[\\a]bc", b"ABC", true, false),
        (b"[A-Z]bc", b"5bc", false, false),
        (b"[A-Z]bc", b"5BC", false, false),
        (b"[A-Z]BC", b"5bc", false, false),
        (b"[A-Z]BC", b"5BC", false, false),
        (b"[a-z]bc", b"5bc", false, false),
        (b"[a-z]bc", b"5BC", false, false),
        (b"*[al]?", b"ball", true, true),
        (b"*[al]?", b"BALL", true, false),
        (b"*[AL]?", b"ball", true, false),
        (b"*[AL]?", b"BALL", true, true),
        (b"[ten]", b"ten", false, false),
        (b"[ten]", b"TEN", false, false),
        (b"[TEN]", b"ten", false, false),
        (b"[TEN]", b"TEN", false, false),
        (b"**[!te]", b"ten", true, true),
        (b"**[!te]", b"TEN", true, true),
        (b"**[!TE]", b"ten", true, true),
        (b"**[!TE]", b"TEN", true, true),
        (b"t[a-g]n", b"ten", true, true),
        (b"t[a-g]n", b"TEN", true, false),
        (b"T[A-G]N", b"ten", true, false),
        (b"T[A-G]N", b"TEN", true, true),
        (b"t[A-G]n", b"ten", true, false),
        (b"t[A-G]n", b"TEN", true, false),
        (b"t[!a-g]n", b"ton", true, true),
        (b"t[!a-g]n", b"TON", true, false),
        (b"T[!A-G]N", b"ton", true, false),
        (b"T[!A-G]N", b"TON", true, true),
        (b"t[!A-G]n", b"ton", true, true),
        (b"t[!A-G]n", b"TON", true, false),
        (b"t[^A-G]n", b"ton", true, true),
        (b"t[^A-G]n", b"TON", true, false),
        (b"T[^A-G]N", b"ton", true, false),
        (b"T[^A-G]N", b"TON", true, true),
        (b"t[^a-g]n", b"ton", true, true),
        (b"t[^a-g]n", b"TON", true, false),
        (b"a[]]b", b"a]b", true, true),
        (b"a[]]b", b"A]B", true, false),
        (b"A[]]B", b"a]b", true, false),
        (b"A[]]B", b"A]B", true, true),
        (b"a[]-]b", b"a-b", true, true),
        (b"a[]-]b", b"A-B", true, false),
        (b"A[]-]B", b"a-b", true, false),
        (b"A[]-]B", b"A-B", true, true),
        (b"a[]A-]b", b"aab", true, false),
        (b"a[]A-]b", b"AAB", true, false),
        (b"A[]A-]B", b"aab", true, false),
        (b"A[]A-]B", b"AAB", true, true),
        (b"a[]a-]b", b"aab", true, true),
        (b"a[]a-]b", b"AAB", true, false),
        (b"foo[/]bar", b"foo/bar", false, false),
        (b"foo[/]bar", b"FOO/BAR", false, false),
        (b"FOO[/]BAR", b"foo/bar", false, false),
        (b"FOO[/]BAR", b"FOO/BAR", false, false),
        (b"f[^EIU][^EIU][^EIU][^EIU][^EIU]r", b"foo-bar", true, true),
        (b"f[^EIU][^EIU][^EIU][^EIU][^EIU]r", b"FOO-BAR", true, false),
        (b"F[^EIU][^EIU][^EIU][^EIU][^EIU]R", b"foo-bar", true, false),
        (b"F[^EIU][^EIU][^EIU][^EIU][^EIU]R", b"FOO-BAR", true, true),
        (b"f[^eiu][^eiu][^eiu][^eiu][^eiu]r", b"foo-bar", true, true),
        (b"f[^eiu][^eiu][^eiu][^eiu][^eiu]r", b"FOO-BAR", true, false),
        (b"a[C-C]rt", b"acrt", true, false),
        (b"a[C-C]rt", b"ACRT", true, false),
        (b"A[C-C]RT", b"acrt", true, false),
        (b"A[C-C]RT", b"ACRT", true, true),
        (b"a[c-c]rt", b"acrt", true, true),
        (b"a[c-c]rt", b"ACRT", true, false),
        (b"[!]-]", b"a", true, true),
        (b"[!]-]", b"A", true, true),
        (b"[[]ab]", b"[ab]", true, true),
        (b"[[]ab]", b"[AB]", true, false),
        (b"[[]AB]", b"[ab]", true, false),
        (b"[[]AB]", b"[AB]", true, true),
        (b"[[:]ab]", b"[ab]", true, true),
        (b"[[:]ab]", b"[AB]", true, false),
        (b"[[:]AB]", b"[ab]", true, false),
        (b"[[:]AB]", b"[AB]", true, true),
        (b"[[:DIGIT]ab]", b"[ab]", true, true),
        (b"[[:DIGIT]ab]", b"[AB]", true, false),
        (b"[[:DIGIT]AB]", b"[ab]", true, false),
        (b"[[:DIGIT]AB]", b"[AB]", true, true),
        (b"[[:digit]ab]", b"[ab]", true, true),
        (b"[[:digit]ab]", b"[AB]", true, false),
        (b"[\\[:]ab]", b"[ab]", true, true),
        (b"[\\[:]ab]", b"[AB]", true, false),
        (b"[\\[:]AB]", b"[ab]", true, false),
        (b"[\\[:]AB]", b"[AB]", true, true),
        (b"[a-c[:digit:]x-z]", b"Y", true, false),
        (b"[a-c[:digit:]x-z]", b"y", true, true),
        (b"[A-C[:DIGIT:]X-Z]", b"Y", false, false),
        (b"[A-C[:DIGIT:]X-Z]", b"y", false, false),
        (b"**/T[O]", b"foo/bar/baz/to", true, false),
        (b"**/T[O]", b"FOO/BAR/BAZ/TO", true, true),
        (b"**/t[o]", b"foo/bar/baz/to", true, true),
        (b"**/t[o]", b"FOO/BAR/BAZ/TO", true, false),
        (b"[,-.]", b"-", true, true),
        (b"[[-\\]]", b"\\", true, true),
        (b"[\\\\,]", b"\\", true, true),
        (b"[A-\\\\]", b"G", false, true),
        (b"[A-\\\\]", b"g", false, false),
        (b"[a-\\\\]", b"G", false, false),
        (b"[a-\\\\]", b"g", false, false),
        (b"HOST[0-9].EXAMPLE.COM", b"host7.example.com", true, false),
        (b"HOST[0-9].EXAMPLE.COM", b"HOST7.EXAMPLE.COM", true, true),
        (b"host[0-9].example.com", b"host7.example.com", true, true),
        (b"host[0-9].example.com", b"HOST7.EXAMPLE.COM", true, false),
        (b"*.[Ee]xample.[Cc]om", b"WWW.EXAMPLE.COM", true, false),
        (b"*.[Ee]xample.[Cc]om", b"www.example.com", true, true),
        (b"*.[EE]XAMPLE.[CC]OM", b"WWW.EXAMPLE.COM", true, true),
        (b"*.[EE]XAMPLE.[CC]OM", b"www.example.com", true, false),
        (b"*.[ee]xample.[cc]om", b"WWW.EXAMPLE.COM", true, false),
        (b"*.[ee]xample.[cc]om", b"www.example.com", true, true),
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

    /// WHY: before rsync 3.5.0 the case-insensitive matcher folded only the
    /// text, so an upper-case pattern - including an upper-case bracket member
    /// or range end - failed to match a lower-case candidate and a `hosts deny`
    /// rule failed open. Both columns come from one upstream binary, so the
    /// same run also proves the case-sensitive entry point is untouched.
    #[test]
    fn iwildmatch_matches_upstream_350_oracle() {
        let mut fold_sensitive = 0usize;
        for &(pattern, text, want_folded, want_exact) in FOLD_VECTORS {
            let got_folded = iwildmatch(pattern, text);
            assert_eq!(
                got_folded,
                want_folded,
                "iwildmatch(pattern={:?}, text={:?}) = {got_folded}, want {want_folded}",
                String::from_utf8_lossy(pattern),
                String::from_utf8_lossy(text),
            );

            let got_exact = wildmatch(pattern, text);
            assert_eq!(
                got_exact,
                want_exact,
                "wildmatch(pattern={:?}, text={:?}) = {got_exact}, want {want_exact}",
                String::from_utf8_lossy(pattern),
                String::from_utf8_lossy(text),
            );

            if want_folded != want_exact {
                fold_sensitive += 1;
            }
        }

        // A matrix where folding never changed the answer would pass whatever
        // the implementation did. Upstream separates the two entry points on
        // 139 of these rows.
        assert_eq!(
            fold_sensitive, 139,
            "matrix no longer discriminates folded from exact matching"
        );
    }

    /// WHY: the 3.5.0 fix is not "one more comparison" but the invariant that
    /// `iwildmatch` is blind to ASCII case on both sides. Asserting it across
    /// every bracket form catches a future arm that folds the text and forgets
    /// the pattern, which is exactly how the original defect survived.
    ///
    /// POSIX class names are excluded: they are matched by literal name, so
    /// `[[:ALPHA:]]` is a malformed class rather than a case variant of
    /// `[[:alpha:]]`, and upstream aborts the match instead of folding it.
    #[test]
    fn iwildmatch_is_case_blind_over_every_bracket_form() {
        let mut checked = 0usize;
        for &(pattern, text, _, _) in FOLD_VECTORS {
            if pattern.windows(2).any(|pair| pair == b"[:") {
                continue;
            }

            let want = iwildmatch(pattern, text);
            for p in [
                pattern.to_ascii_uppercase(),
                pattern.to_ascii_lowercase(),
                pattern.to_vec(),
            ] {
                for t in [
                    text.to_ascii_uppercase(),
                    text.to_ascii_lowercase(),
                    text.to_vec(),
                ] {
                    let got = iwildmatch(&p, &t);
                    assert_eq!(
                        got,
                        want,
                        "iwildmatch is case-sensitive: ({:?}, {:?}) = {want} but ({:?}, {:?}) = {got}",
                        String::from_utf8_lossy(pattern),
                        String::from_utf8_lossy(text),
                        String::from_utf8_lossy(&p),
                        String::from_utf8_lossy(&t),
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 4000,
            "case-blindness sweep covered only {checked}"
        );
    }

    /// WHY: `wildmatch()` is the filter-rule matcher, and every wire-visible
    /// filter decision runs through it. Folding must stay confined to the
    /// `iwildmatch` entry point, so the upstream corpus is re-checked for case
    /// sensitivity: a case-varied pattern or text must still change the answer.
    #[test]
    fn wildmatch_stays_case_sensitive() {
        let mut discriminating = 0usize;
        for &(expected, text, pattern) in VECTORS {
            let upper_pattern = pattern.to_ascii_uppercase();
            let upper_text = text.to_ascii_uppercase();
            if upper_pattern == pattern && upper_text == text {
                continue;
            }

            if wildmatch(&upper_pattern, text) != expected
                || wildmatch(pattern, &upper_text) != expected
            {
                discriminating += 1;
            }
        }
        assert!(
            discriminating > 0,
            "no corpus row is case-discriminating; the guard is vacuous"
        );
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
