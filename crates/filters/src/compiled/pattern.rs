use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use globset::GlobBuilder;

use crate::FilterError;
use crate::wildmatch::wildmatch;

/// A compiled filter pattern matched with upstream rsync's `wildmatch()`.
///
/// Matching is delegated to [`wildmatch`] so `*`, `**`, `?`, `[...]`, and `\`
/// behave byte-for-byte like `lib/wildmatch.c:dowild()`. globset is still used
/// at compile time to reject malformed patterns, keeping [`FilterError`]
/// behaviour unchanged.
#[derive(Debug, Clone)]
pub(crate) struct CompiledPattern {
    bytes: Vec<u8>,
}

impl CompiledPattern {
    /// Returns the source glob string this pattern was compiled from. Patterns
    /// always originate from a `String`, so the bytes are valid UTF-8.
    #[cfg(test)]
    pub(super) fn glob(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// Tests whether `path` matches this pattern using upstream wildmatch
    /// semantics. The path is rendered as a `/`-joined relative byte string so
    /// matching is identical across platforms (rsync transfer paths are always
    /// `/`-separated and relative).
    pub(super) fn is_match(&self, path: &Path) -> bool {
        let body = path_match_bytes(path);
        // upstream: exclude.c:929-931 rule_matches() - when the pattern begins
        // with `**` (FILTRULE_WILD2_PREFIX) the candidate is matched with a
        // leading "/" prepended, so `**/bar` matches a top-level `bar` (via
        // `/bar`) as well as `a/b/bar`. Our relative candidates never start
        // with '/', so the prepend always applies.
        if self.bytes.starts_with(b"**") {
            let mut candidate = Vec::with_capacity(body.len() + 1);
            candidate.push(b'/');
            candidate.extend_from_slice(&body);
            wildmatch(&self.bytes, &candidate)
        } else {
            wildmatch(&self.bytes, &body)
        }
    }
}

/// Renders `path` as the byte string upstream rsync matches against: the
/// relative name verbatim, with platform separators normalised to `/`.
///
/// rsync feeds filter candidates as `/`-separated relative names, so a literal
/// rendering (preserving `.`/`..` and single components) is what `wildmatch()`
/// expects. Backslashes are folded to `/` on Windows so matching is identical
/// across platforms.
fn path_match_bytes(path: &Path) -> Vec<u8> {
    let rendered = path.to_string_lossy();
    if cfg!(windows) && rendered.contains('\\') {
        rendered.replace('\\', "/").into_bytes()
    } else {
        rendered.into_owned().into_bytes()
    }
}

/// Compiles a set of glob pattern strings into sorted, deduplicated matchers.
///
/// Patterns are sorted for deterministic evaluation order. Each pattern is
/// built with `literal_separator(true)` so that `*` does not match `/`,
/// matching upstream rsync's wildcard semantics.
///
/// Bare interior `**` sequences carry the upstream wildmatch semantic
/// "match anything including `/`". globset's `literal_separator(true)`
/// only treats `**` as recursive when it is bounded by `/`, so the input
/// pattern is expanded into TWO variants: the original (covers the
/// in-segment case where `**` behaves like a single-segment `*`) and a
/// slash-bounded rewrite (covers the cross-segment case). Both are added
/// to the matcher set so either form can satisfy upstream parity.
///
/// upstream: `lib/wildmatch.c:dowild()` - `**` always matches across `/`.
pub(crate) fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<CompiledPattern>, FilterError> {
    let mut expanded: HashSet<String> = HashSet::with_capacity(patterns.len() * 2);
    for pattern in patterns {
        let rewritten = match normalise_recursive_wildcards(&pattern) {
            Cow::Borrowed(_) => None,
            Cow::Owned(s) => Some(s),
        };
        if let Some(s) = rewritten {
            expanded.insert(s);
        }
        expanded.insert(pattern);
    }

    let mut unique: Vec<_> = expanded.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        // globset still validates the pattern so malformed globs surface as
        // FilterError exactly as before; matching is delegated to wildmatch.
        GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterError::new(original.to_owned(), error))?;
        matchers.push(CompiledPattern {
            bytes: pattern.into_bytes(),
        });
    }
    Ok(matchers)
}

/// Rewrites bare interior `**` sequences into slash-delimited `/**/` so
/// globset treats them as recursive wildcards.
///
/// upstream: `lib/wildmatch.c:dowild()` - when `**` is encountered, the
/// `special` flag is set, and the wildcard matches across `/` boundaries
/// regardless of surrounding characters. globset only treats `**` as
/// recursive when it is bounded by `/` (or string boundaries), so a pattern
/// like `foo**too` must be rewritten to `foo/**/too` to match
/// `bar/down/to/foo/too`.
///
/// Runs of three or more `*` characters are collapsed to `**` first, since
/// upstream's `dowild()` skips all consecutive `*` after seeing `**`
/// (`while (*++p == '*') {}`).
///
/// Boundary handling:
/// - `**` at the very start of the pattern keeps its prefix free
///   (`**foo` -> `**/foo`, `**/foo` unchanged).
/// - `**` at the very end keeps its suffix free
///   (`foo**` -> `foo/**`, `foo/**` unchanged).
/// - `**` already bounded by `/` on both sides is unchanged.
/// - `**` adjacent to non-`/` is padded with the missing slash.
///
/// `*` and `?` outside `**` runs are left intact. Backslash-escaped
/// characters (`\*`) are passed through verbatim - the escape is consumed
/// with its escapee so neither participates in `**` detection.
fn normalise_recursive_wildcards(pattern: &str) -> Cow<'_, str> {
    if !pattern.contains("**") {
        return Cow::Borrowed(pattern);
    }

    // `*`, `\`, and `/` are all single-byte ASCII so byte-indexed scanning
    // is safe within a UTF-8 string. Multi-byte UTF-8 sequences are copied
    // verbatim via str slicing between cut points to preserve encoding.
    let bytes = pattern.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 4);
    let mut cut = 0;
    let mut i = 0;
    let mut changed = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            // Skip the escape pair so neither byte participates in `**` detection.
            i += 2;
            continue;
        }
        if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // Found `**`. Consume the entire run of `*` (collapses `***+` to `**`).
            let run_start = i;
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] == b'*' {
                j += 1;
            }

            // Flush any pending verbatim slice before the `**` run.
            out.push_str(&pattern[cut..run_start]);

            if j - run_start > 2 {
                changed = true;
            }
            let at_start = run_start == 0;
            let at_end = j == bytes.len();
            let prev_is_slash = run_start > 0 && bytes[run_start - 1] == b'/';
            let next_is_slash = j < bytes.len() && bytes[j] == b'/';

            let need_leading_slash = !at_start && !prev_is_slash;
            let need_trailing_slash = !at_end && !next_is_slash;

            if need_leading_slash {
                out.push('/');
                changed = true;
            }
            out.push_str("**");
            if need_trailing_slash {
                out.push('/');
                changed = true;
            }
            i = j;
            cut = j;
            continue;
        }
        i += 1;
    }

    if !changed {
        return Cow::Borrowed(pattern);
    }

    out.push_str(&pattern[cut..]);
    Cow::Owned(out)
}

/// Normalizes a pattern by stripping leading `/` (anchored) and trailing `/` (directory-only).
///
/// Returns `Cow::Borrowed` when no stripping is needed (most common case),
/// avoiding a heap allocation.
///
/// A pattern is anchored if:
/// - It starts with `/`, OR
/// - It contains `/` anywhere in the pattern (besides trailing `/`)
///
/// A trailing `/***` suffix is treated as directory-only on the stem.
/// upstream: `exclude.c:936-937` - `FILTRULE_WILD3_SUFFIX` appends `/` to
/// directory names during matching, allowing `dir/***` to match both the
/// directory itself and all its contents. We normalize `dir/***` to `dir/`
/// (directory-only) so the standard descendant-matcher expansion produces
/// the correct `dir/**` content matchers.
///
/// This mirrors upstream rsync's pattern normalization in
/// `exclude.c:parse_filter_str()` where leading and trailing slashes are
/// stripped and used to set `FILTRULE_ABS_PATH` and `FILTRULE_DIRECTORY`
/// flags respectively.
pub(super) fn normalise_pattern(pattern: &str) -> (bool, bool, Cow<'_, str>) {
    let starts_with_slash = pattern.starts_with('/');

    // upstream: exclude.c:190-193 then 243-248 - add_rule() first peels a
    // single trailing `/` (FILTRULE_DIRECTORY), THEN detects a trailing `***`
    // (FILTRULE_WILD3_SUFFIX). Matching that order is required so the combined
    // `dir/***/` form collapses to a directory-only stem; checking `/***`
    // before the slash-peel misses it and leaves `*/***`, which cannot match a
    // slashless directory name (differential fuzzer divergence on `*/***/`).
    let mut directory_only = false;
    let mut stem: &str = pattern;
    if stem.len() > 1 && stem.ends_with('/') {
        stem = &stem[..stem.len() - 1];
        directory_only = true;
    } else if stem == "/" {
        directory_only = true;
    }
    if stem.len() > 4 && stem.ends_with("/***") {
        // `/***` (SLASH_WILD3_SUFFIX) means "match both the directory and
        // everything inside it". Strip it and treat the stem as directory-only;
        // the descendant-matcher expansion then produces the `dir/**` content
        // matchers.
        stem = &stem[..stem.len() - 4];
        directory_only = true;
    }
    let stripped = stem;

    // Strip the leading `/` if present.
    let core_pattern = if starts_with_slash {
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        stripped
    };

    // upstream: exclude.c:rule_matches() - FILTRULE_ABS_PATH is only set
    // for patterns that start with `/` (or when XFLG_ABS_IF_SLASH is in
    // effect, which is restricted to daemon module configs). A pattern
    // with internal slashes but no leading `/` is NOT anchored; instead
    // upstream tail-matches it against the last N+1 path components (line
    // 947-951). The glob equivalent is `**/pattern`, which our caller
    // adds for unanchored patterns.
    let anchored = starts_with_slash;

    if !starts_with_slash && !directory_only {
        // Nothing was stripped - borrow the original.
        (anchored, false, Cow::Borrowed(pattern))
    } else {
        (
            anchored,
            directory_only,
            Cow::Owned(core_pattern.to_string()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_pattern_plain() {
        let (anchored, dir_only, core) = normalise_pattern("foo");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored() {
        let (anchored, dir_only, core) = normalise_pattern("/foo");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_directory_only() {
        let (anchored, dir_only, core) = normalise_pattern("foo/");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_anchored_directory() {
        let (anchored, dir_only, core) = normalise_pattern("/foo/");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "foo");
    }

    #[test]
    fn normalise_pattern_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("*.txt");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_anchored_wildcard() {
        let (anchored, dir_only, core) = normalise_pattern("/*.txt");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "*.txt");
    }

    #[test]
    fn normalise_pattern_nested_path() {
        let (anchored, dir_only, core) = normalise_pattern("src/lib/");
        // upstream: internal slashes without a leading `/` are NOT anchored;
        // they use tail-matching (match last N+1 path components via `**/pattern`).
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "src/lib");
    }

    #[test]
    fn normalise_pattern_anchored_nested_path() {
        // Leading `/` anchors even with internal slashes.
        let (anchored, dir_only, core) = normalise_pattern("/src/lib/");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "src/lib");
    }

    #[test]
    fn normalise_pattern_empty_after_strip() {
        // Edge case: pattern is just "/"
        let (anchored, dir_only, core) = normalise_pattern("/");
        assert!(anchored);
        assert!(dir_only);
        // Core is empty but we don't strip further because it would be empty
        assert_eq!(core, "");
    }

    /// upstream: exclude.c:936-937 - FILTRULE_WILD3_SUFFIX appends `/` to
    /// directory names during matching. `dir/***` matches both the directory
    /// itself (when is_dir) and everything inside it.
    #[test]
    fn normalise_pattern_wild3_suffix() {
        let (anchored, dir_only, core) = normalise_pattern("new/lose/***");
        assert!(!anchored);
        assert!(dir_only);
        assert_eq!(core, "new/lose");
    }

    #[test]
    fn normalise_pattern_anchored_wild3_suffix() {
        let (anchored, dir_only, core) = normalise_pattern("/new/lose/***");
        assert!(anchored);
        assert!(dir_only);
        assert_eq!(core, "new/lose");
    }

    /// Bare `/***` (no directory stem) should be treated as directory-only
    /// on the empty path, matching upstream behavior.
    #[test]
    fn normalise_pattern_bare_wild3_suffix() {
        // Pattern "/***" has len 4, not > 4, so the `/***` branch does NOT
        // fire. This is by design: bare `***` is just a wildcard pattern.
        let (anchored, dir_only, core) = normalise_pattern("/***");
        assert!(anchored);
        assert!(!dir_only);
        assert_eq!(core, "***");
    }

    /// Pattern ending with `***` but without a preceding `/` is a regular
    /// wildcard, not the WILD3_SUFFIX semantic.
    #[test]
    fn normalise_pattern_trailing_triple_star_no_slash() {
        let (anchored, dir_only, core) = normalise_pattern("foo***");
        assert!(!anchored);
        assert!(!dir_only);
        assert_eq!(core, "foo***");
    }

    /// `**` between non-slash characters must be rewritten to `/**/` so
    /// globset treats it as a recursive wildcard.
    ///
    /// upstream: `lib/wildmatch.c:dowild()` - `**` always matches across `/`.
    #[test]
    fn normalise_recursive_wildcards_interior_rewrites() {
        assert_eq!(normalise_recursive_wildcards("foo**too"), "foo/**/too");
        assert_eq!(normalise_recursive_wildcards("a**b**c"), "a/**/b/**/c");
    }

    /// `**` already adjacent to `/` on at least one side gets the missing
    /// slash on the other side.
    #[test]
    fn normalise_recursive_wildcards_one_sided_slash() {
        assert_eq!(normalise_recursive_wildcards("foo/**bar"), "foo/**/bar");
        assert_eq!(normalise_recursive_wildcards("bar**/foo"), "bar/**/foo");
    }

    /// `**` already fully slash-bounded must not be touched.
    #[test]
    fn normalise_recursive_wildcards_already_bounded() {
        for p in &["**/bar", "bar/**", "foo/**/bar", "**", "**/foo/**"] {
            assert_eq!(normalise_recursive_wildcards(p), *p, "pattern {p:?}");
        }
    }

    /// `**` at string start/end is treated as bounded by the implicit
    /// edges, not as needing a slash inserted there.
    #[test]
    fn normalise_recursive_wildcards_edges() {
        assert_eq!(normalise_recursive_wildcards("**foo"), "**/foo");
        assert_eq!(normalise_recursive_wildcards("foo**"), "foo/**");
    }

    /// Three or more consecutive `*` characters collapse to `**` then get
    /// boundary-normalised. This mirrors upstream's
    /// `while (*++p == '*') {}` consumption.
    #[test]
    fn normalise_recursive_wildcards_collapse_runs() {
        assert_eq!(normalise_recursive_wildcards("foo***too"), "foo/**/too");
        assert_eq!(normalise_recursive_wildcards("foo****"), "foo/**");
    }

    /// Single `*` and `?` wildcards are left intact - they retain their
    /// "match anything except `/`" semantics in globset.
    #[test]
    fn normalise_recursive_wildcards_leaves_single_wildcards() {
        for p in &["*.txt", "foo?bar", "src/*.rs", "?", "*"] {
            assert_eq!(normalise_recursive_wildcards(p), *p, "pattern {p:?}");
        }
    }

    /// Patterns without `**` are returned borrowed without allocation.
    #[test]
    fn normalise_recursive_wildcards_no_double_star_is_borrowed() {
        let p = "foo/bar/baz";
        assert!(matches!(normalise_recursive_wildcards(p), Cow::Borrowed(_)));
    }

    /// Escaped `\*` sequences must not be treated as wildcards. The escape
    /// pair is preserved verbatim and skipped during `**` detection, so a
    /// pattern like `foo\**bar` decomposes into `foo` + literal `*` +
    /// single `*` + `bar`, which is NOT a `**` recursive wildcard.
    #[test]
    fn normalise_recursive_wildcards_respects_backslash_escape() {
        assert_eq!(normalise_recursive_wildcards("foo\\**bar"), "foo\\**bar");
        assert_eq!(normalise_recursive_wildcards("\\*\\*foo"), "\\*\\*foo");
    }
}
