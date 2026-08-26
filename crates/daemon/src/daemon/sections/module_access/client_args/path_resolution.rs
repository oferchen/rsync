// Resolution of the client's positional path args into on-disk receiver
// destinations and sender source paths, with glob expansion and module-root
// containment of alt-basis (`--link-dest` / `--copy-dest` / `--compare-dest`)
// directories.
/// Extracts the positional path arguments sent by the client after the `.`
/// separator and strips the leading module-name component from each so the
/// receiver can resolve them relative to the on-disk module path.
///
/// Mirrors upstream `read_args()` (io.c:1295) and `glob_expand_module()`
/// (util1.c:804): everything before a standalone `.` in the wire arg list is
/// options/flags; everything after is the client's positional paths. Each
/// positional begins with the module name (e.g. `upload/realdir/` when the
/// module is `upload`), which is the prefix `glob_expand_module()` strips
/// before the path is handed to the server-side option parser.
///
/// Returns the stripped relative paths in original order. A path that does
/// not start with the module name is returned as-is so the caller can still
/// see it (this matches upstream's loose prefix match - it only strips when
/// the prefix is present).
fn extract_module_relative_paths(client_args: &[String], module_name: &str) -> Vec<String> {
    let mut dot_seen = false;
    let mut out = Vec::new();
    for arg in client_args {
        if !dot_seen {
            if arg == "." {
                dot_seen = true;
            }
            continue;
        }
        // upstream: util1.c:813-814 - `if (strncmp(arg, base, base_len) == 0)
        // arg += base_len;` - strips the bare module name. The remainder may
        // be empty (then represents the module root), start with `/`
        // (subpath), or be the rest of a longer arg sharing the prefix.
        let stripped = if let Some(rest) = arg.strip_prefix(module_name) {
            // Only strip when the next char is `/` or end-of-string so we do
            // not chop the prefix of a sibling module that merely shares a
            // string prefix (e.g. `uploads/` vs module `upload`).
            if rest.is_empty() || rest.starts_with('/') {
                rest.trim_start_matches('/').to_owned()
            } else {
                arg.clone()
            }
        } else {
            arg.clone()
        };
        out.push(stripped);
    }
    out
}

/// Collapses a module-relative client tail the way upstream's `sanitize_path()`
/// does for a daemon connection, returning a `/`-joined relative path.
///
/// upstream: `util1.c:1138 sanitize_path(dest, p, rootdir, depth, flags)`, whose
/// documented contract is to "ALWAYS collapse `..` elements (except for those at
/// the start of the string up to `depth` deep)". The daemon reaches it via
/// `options.c:2405` `sanitize_path(NULL, argv[i], "", 0, SP_KEEP_DOT_DIRS)`,
/// gated on `sanitize_paths`, which `clientserver.c:1068` sets for every daemon
/// connection. **The depth there is 0**, so `util1.c:1183`'s
/// `if (depth <= 0 || sanp != start)` arm always wins: a `..` either backs up
/// over one already-emitted component or, with nothing to back up over, is
/// discarded. There is no arm that refuses.
///
/// That is why collapsing is safe rather than permissive: the output is closed
/// under the module root by construction. `a/../b` becomes `b`, and `../etc`
/// becomes `etc` - both still resolve beneath the module directory. Upstream
/// serves both; refusing them rejects requests that 3.4.4 and 3.5.0 both accept.
///
/// Joins with a literal `/` on every host, matching upstream's `pathjoin()`
/// rather than `PathBuf::join`, which would emit `\` on Windows for a path that
/// is about to be compared against module-relative wire names.
///
/// Deliberately NOT `lexically_normalize`, which walks the same components but
/// PRESERVES an unpoppable leading `..` so its caller's `starts_with` containment
/// check can reject an escaping alt-basis path. Four differences - input type,
/// output type, separator policy, leading-`..` policy - so they are two
/// functions with two contracts, not one to be deduplicated.
fn collapse_module_relative(tail: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    // Splits on `/` ONLY. `tail` is a peer-supplied wire path, and the wire is
    // `/`-separated on every host (upstream `pathjoin()`); upstream's own
    // `sanitize_path` tests `== '/'` at every separator check and has no `\`
    // arm at all, so a `\` is copied through as an ordinary byte of the
    // component. On Unix `\` is a legal filename byte: treating it as a
    // separator silently relocates a legitimately-named file - `a\b` becomes
    // `a/b` - and strips a trailing one. Contrast the `module_path` checks
    // below, which DO accept `\` because they inspect an operator-configured
    // LOCAL path that may legitimately be Windows-style.
    for segment in tail.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                // upstream util1.c:1183-1191 - back up one component, or drop
                // the `..` outright when already at the start (depth 0).
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// Resolves the receiver's on-disk destination directory from the client's
/// positional path args.
///
/// Mirrors the post-`change_dir(module_chdir)` behaviour upstream relies on:
/// after upstream's `glob_expand_module()` strips the module name, the
/// receiver's `get_local_name()` (main.c:697) interprets the remaining path
/// as relative to the module root on disk. Because oc-rsync does not chdir
/// per connection, we resolve that join explicitly.
///
/// Returns the module path itself when no positional was supplied or when
/// the stripped tail is empty (push directly into the module root).
///
/// `..` segments in the tail are COLLAPSED, never refused, mirroring
/// upstream's `sanitize_path()` contract - see [`collapse_module_relative`]
/// for the anchor table and why the collapse is closed under the module root.
fn resolve_receiver_dest(
    module_path: &std::path::Path,
    client_args: &[String],
    module_name: &str,
) -> std::path::PathBuf {
    let positionals = extract_module_relative_paths(client_args, module_name);
    // upstream: main.c:1422-1423 - `local_name = get_local_name(flist, argv[0])`
    // uses the FIRST remaining positional (after the `.` placeholder has been
    // consumed by `do_server_recv` at lines 1174-1177). For a receiver that
    // translates to the last wire positional - the destination.
    let Some(last) = positionals.last() else {
        return module_path.to_path_buf();
    };
    let tail = last.trim();
    if tail.is_empty() || tail == "." {
        return module_path.to_path_buf();
    }
    // A leading separator is stripped first for the same reason upstream's
    // `sanitize_path` consumes one before walking (`util1.c:1147` `p++`): an
    // absolute client path is interpreted against the module root, not the
    // host root. Collapsing then folds away every `.` and `..`, so the joined
    // destination is under `module_path` by construction on any host.
    let collapsed = collapse_module_relative(tail.trim_start_matches('/'));
    if collapsed.is_empty() {
        return module_path.to_path_buf();
    }
    // Preserve a trailing `/`. upstream main.c:741 `get_local_name()` computes
    // `trailing_slash = cp && !cp[1]` from the dest arg and takes the
    // make-a-directory branch on `file_total > 1 || trailing_slash`: it mkdirs
    // the dest, chdirs into it and returns a NULL local_name, so a single
    // source file lands INSIDE. Without the slash the dest names the file
    // itself. `collapse_module_relative` splits on `/` and drops the empty
    // trailing segment, so the signal dies here unless it is re-appended -
    // and the receiver then silently writes a FILE where the peer asked for a
    // directory. oc's local path already implements the upstream rule
    // correctly (measured); only the daemon was losing the input to it.
    //
    // Built by pushing onto the OsString rather than `PathBuf::join`, which
    // normalises a trailing separator away, for the same reason
    // `resolve_sender_sources` below does it by hand.
    if tail.ends_with('/') {
        let mut buf = module_path.as_os_str().to_owned();
        if !buf
            .as_encoded_bytes()
            .last()
            .is_some_and(|b| *b == b'/' || *b == b'\\')
        {
            buf.push("/");
        }
        buf.push(&collapsed);
        buf.push("/");
        return std::path::PathBuf::from(buf);
    }
    module_path.join(collapsed)
}

/// Resolves the sender's on-disk source paths from the client's positional
/// path args for a pull request (Generator role).
///
/// Mirrors upstream's `glob_expand_module()` + `chdir(module_chdir)` ordering:
/// once the module name has been stripped, upstream's daemon-mode sender sees
/// argv positionals as paths relative to the module root, and the sender's
/// per-arg `dir/fn` split (flist.c:2338-2349) chops the last `/` so the wire
/// emits `fn` as the file-list name. We don't chdir, so each positional is
/// resolved by joining the stripped tail with `module_path`. The trailing
/// slash (if any) is preserved so the sender's existing dotdir branch can
/// trigger when the client wrote `module/sub/` instead of `module/sub`.
///
/// Returns `[module_path]` when no positional was supplied or when every
/// stripped tail is empty, matching the pre-existing "pull from module root"
/// behaviour exactly.
///
/// Sub-paths containing `..`, and host-absolute sub-paths, are COLLAPSED
/// against the module root rather than refused - see
/// [`collapse_module_relative`]. A crafted `rsync://host/mod/../etc/...` URL
/// therefore resolves to `<module>/etc/...` and cannot enumerate outside the
/// module root, which is the same containment upstream gets from
/// `sanitize_path` at depth 0.
///
/// # Upstream Reference
///
/// - `util1.c:804 glob_expand_module()` - strips the module name from each arg
/// - `clientserver.c:992 change_dir(module_chdir, CD_NORMAL)` - relativises args
/// - `flist.c:2338-2349 send_file_list()` - `dir/fn` split per positional
fn resolve_sender_sources(
    module_path: &std::path::Path,
    client_args: &[String],
    module_name: &str,
) -> Vec<std::path::PathBuf> {
    let positionals = extract_module_relative_paths(client_args, module_name);
    if positionals.is_empty() {
        return vec![module_root_dotdir(module_path)];
    }
    let mut sources = Vec::with_capacity(positionals.len());
    let mut all_empty = true;
    for raw in &positionals {
        let tail = raw.trim();
        if tail.is_empty() || tail == "." {
            sources.push(module_root_dotdir(module_path));
            continue;
        }
        all_empty = false;
        // Collapse `.` and `..` exactly as upstream's `sanitize_path` does at
        // depth 0 - see [`collapse_module_relative`]. The result cannot escape
        // the module root, so there is nothing left to refuse.
        let collapsed = collapse_module_relative(tail.trim_start_matches('/'));
        let trimmed = collapsed.as_str();
        if trimmed.is_empty() {
            sources.push(module_root_dotdir(module_path));
            continue;
        }
        // Preserve the trailing slash so the sender can detect a dotdir-style
        // source (upstream flist.c:2312-2322 appends `.` and sets DOTDIR_NAME
        // for any `fbuf[len-1] == '/'`). Upstream rsync joins module-relative
        // paths with a literal `/` regardless of host OS (util1.c pathjoin()),
        // so build the result the same way instead of going through
        // PathBuf::join, which on Windows inserts `\` and on macOS leaves
        // a trailing `/` that doubles when we re-append.
        // upstream flist.c tests `fbuf[len-1] == '/'` only - a trailing `\` is
        // part of the NAME on Unix, not a dotdir marker.
        let trailing = tail.ends_with('/');
        let mut buf = module_path.as_os_str().to_owned();
        let needs_leading_sep = !buf
            .as_encoded_bytes()
            .last()
            .is_some_and(|b| *b == b'/' || *b == b'\\');
        if needs_leading_sep {
            buf.push("/");
        }
        buf.push(trimmed);
        if trailing
            && !buf
                .as_encoded_bytes()
                .last()
                .is_some_and(|b| *b == b'/' || *b == b'\\')
        {
            buf.push("/");
        }
        sources.push(std::path::PathBuf::from(buf));
    }
    if all_empty {
        return vec![module_root_dotdir(module_path)];
    }
    // upstream: util1.c:804 `glob_expand_module()` runs each module-relative
    // positional through `glob_expand()` (util1.c:755) which in turn calls
    // POSIX `glob(3)` to expand shell metacharacters (`*`, `?`, `[...]`)
    // against the on-disk module tree. Without this expansion, a request
    // like `rsync rsync://host/mod/f*` walks a literal path `<module>/f*`
    // that does not exist, the sender returns 0 entries, and the server
    // sits in `recv_filter_list -> read_int(0)` waiting for the receiver's
    // phase-transition NDX while the receiver is still waiting for the
    // file list - a wire-level deadlock that surfaces as the upstream
    // `daemon` testsuite timing out on subtest 4 (test-from/f*) and
    // subtest 5 (test-from/f* with -U).
    //
    // Upstream behaviour, mirrored here:
    //   * Only positionals containing a glob metacharacter are expanded.
    //     Plain paths fall straight through.
    //   * A pattern that matches nothing is preserved verbatim, matching
    //     `glob_expand()`'s `glob.argc == save_argc` branch at util1.c:786
    //     (the literal arg surfaces downstream as a normal `link_stat`
    //     failure instead of a silent drop).
    //   * Expansion is rooted at the module path so the resulting absolute
    //     paths land inside the module's tree, the same containment
    //     guarantee that the chroot / Landlock allowlist enforces. The
    //     `..` collapse above the loop runs before this, so a pattern
    //     reaching the globber is already confined to the module root.
    expand_sender_source_globs(module_path, sources)
}

/// Returns the module root path with a trailing `/` appended (idempotent).
///
/// The trailing slash signals "transfer contents" through
/// `non_relative_walk_base` in the engine - it keeps `base == path` so the
/// walk emits a `.` entry for the root and child names without the module
/// basename prefix. A sub-path positional (e.g. `<mod>/foo`) is left
/// without a trailing slash so the engine's last-`/` split assigns the
/// parent as the base, giving wire-side entries `foo` and `foo/one`
/// instead of the post-strip-prefix `.` and `one` that would otherwise
/// trip the receiver's "rejecting unrequested file-list name" check.
///
/// upstream: `flist.c:1886-1896` - `fbuf[len-1] == '/'` enters the
/// `DOTDIR_NAME` branch, which is how the daemon distinguishes
/// "transfer module contents" from "transfer a named sub-path".
fn module_root_dotdir(module_path: &std::path::Path) -> std::path::PathBuf {
    let mut buf = module_path.as_os_str().to_owned();
    if !buf
        .as_encoded_bytes()
        .last()
        .is_some_and(|b| *b == b'/' || *b == b'\\')
    {
        buf.push("/");
    }
    std::path::PathBuf::from(buf)
}

/// Returns `true` if `name` contains a shell glob metacharacter recognised
/// by `glob(3)`.
///
/// upstream: util1.c:743 - `wildcard_chars[] = "*?["` is the metaset.
fn path_has_glob_metachar(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes()
        .iter()
        .any(|&b| matches!(b, b'*' | b'?' | b'['))
}

/// Expands each source path under `module_path` that contains a glob
/// metacharacter via a single-component walk. Mirrors upstream's
/// `glob_expand()` (util1.c:755) with the simpler subset rsync's daemon
/// path actually receives: each positional is a relative path joined to
/// the module root, so we expand component-by-component. A pattern that
/// matches nothing is left in place so the sender surfaces the normal
/// link_stat error instead of silently dropping the arg.
fn expand_sender_source_globs(
    module_path: &std::path::Path,
    sources: Vec<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut out = Vec::with_capacity(sources.len());
    for path in sources {
        match path.strip_prefix(module_path) {
            Ok(rel)
                if rel.components().any(
                    |c| matches!(c, std::path::Component::Normal(s) if path_has_glob_metachar(s)),
                ) =>
            {
                let matches = expand_relative_glob(module_path, rel);
                if matches.is_empty() {
                    out.push(path);
                } else {
                    out.extend(matches);
                }
            }
            _ => out.push(path),
        }
    }
    out
}

/// Expands a relative path that may contain glob metacharacters in any
/// component, rooted at `base`. Returns the matching absolute paths.
fn expand_relative_glob(base: &std::path::Path, rel: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut current = vec![base.to_path_buf()];
    for component in rel.components() {
        let segment = match component {
            std::path::Component::Normal(s) => s,
            // RootDir / Prefix should not appear in a stripped relative path,
            // and `..` was already rejected by the caller. CurDir is a no-op.
            std::path::Component::CurDir => continue,
            _ => return Vec::new(),
        };
        let mut next = Vec::new();
        if path_has_glob_metachar(segment) {
            let pattern = match segment.to_str() {
                Some(s) => s,
                None => return Vec::new(),
            };
            for dir in &current {
                let entries = match std::fs::read_dir(dir) {
                    Ok(it) => it,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if let Some(name_str) = name.to_str() {
                        // Skip dotfiles unless the pattern starts with `.`,
                        // matching POSIX glob default behaviour.
                        if name_str.starts_with('.') && !pattern.starts_with('.') {
                            continue;
                        }
                        if glob_match_segment(pattern, name_str) {
                            next.push(dir.join(&name));
                        }
                    }
                }
            }
            next.sort();
        } else {
            for dir in &current {
                next.push(dir.join(segment));
            }
        }
        if next.is_empty() {
            return Vec::new();
        }
        current = next;
    }
    current
}

/// Single-segment glob matcher: `*` matches any run, `?` matches one byte,
/// `[abc]` / `[!abc]` matches a character class. Mirrors `glob(3)` for the
/// subset of patterns rsync emits.
fn glob_match_segment(pattern: &str, name: &str) -> bool {
    let pat = pattern.as_bytes();
    let s = name.as_bytes();
    fn go(p: &[u8], s: &[u8]) -> bool {
        let mut pi = 0;
        let mut si = 0;
        let mut star: Option<(usize, usize)> = None;
        while si < s.len() {
            if pi < p.len() {
                match p[pi] {
                    b'?' => {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                    b'*' => {
                        star = Some((pi + 1, si));
                        pi += 1;
                        continue;
                    }
                    b'[' => {
                        // Find matching `]`.
                        let mut end = pi + 1;
                        let negate = end < p.len() && p[end] == b'!';
                        if negate {
                            end += 1;
                        }
                        let class_start = end;
                        while end < p.len() && p[end] != b']' {
                            end += 1;
                        }
                        if end >= p.len() {
                            // Malformed class - treat `[` as literal.
                            if p[pi] == s[si] {
                                pi += 1;
                                si += 1;
                                continue;
                            }
                        } else {
                            let class = &p[class_start..end];
                            let matched = class.contains(&s[si]);
                            if matched != negate {
                                pi = end + 1;
                                si += 1;
                                continue;
                            }
                        }
                    }
                    c => {
                        if c == s[si] {
                            pi += 1;
                            si += 1;
                            continue;
                        }
                    }
                }
            }
            if let Some((ps, ss)) = star {
                pi = ps;
                si = ss + 1;
                star = Some((ps, ss + 1));
            } else {
                return false;
            }
        }
        while pi < p.len() && p[pi] == b'*' {
            pi += 1;
        }
        pi == p.len()
    }
    go(pat, s)
}

/// Collapses `.` and `..` in a module-relative tail, the `Path`-typed
/// counterpart of [`collapse_module_relative`].
///
/// Same rule, same anchor: upstream's `sanitize_path()` runs at **depth 0** for
/// a daemon connection, so `util1.c:1183`'s `if (depth <= 0 || sanp != start)`
/// arm always wins - a `..` either backs up over one already-emitted component
/// or, with nothing to back up over, is discarded. There is no arm that
/// refuses. The output is therefore closed under the module root by
/// construction, which is what lets the caller join it onto the root without a
/// separate containment check.
///
/// Two functions rather than one because the inputs differ in type and
/// separator policy: [`collapse_module_relative`] walks a `/`-separated wire
/// string, this one walks OS `Path` components so a non-UTF-8 basis keeps its
/// bytes. Both implement the identical upstream rule.
///
/// Pure path arithmetic: no syscalls, no canonicalisation, so the result is
/// well-defined even when the resolved directory does not exist yet (a
/// `--link-dest` basis is allowed to be missing without aborting the transfer).
fn collapse_under_root(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push(name),
            // A tail is module-relative by construction; a root or drive
            // prefix carries no meaning under the module and is dropped the
            // way upstream drops the leading `/` at `util1.c:1151` (`p++`).
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
        }
    }
    out
}

/// Clamps a client-supplied alt-basis path (`--link-dest` / `--copy-dest` /
/// `--compare-dest`) to the served module, mirroring upstream's
/// `sanitize_path()` rewrite.
///
/// upstream: `main.c:1233-1236` - a daemon receiver passes every `basis_dir[]`
/// through `sanitize_path(NULL, dir, NULL, curr_dir_depth, SP_DEFAULT)` before
/// `check_alt_basis_dirs()` runs. That call **rewrites**; it never rejects. A
/// `--link-dest=../sibling` sent against the module root becomes `sibling`,
/// which then fails to exist and draws the `arg does not exist` warning at
/// `main.c:901` - so the operator learns their basis was out of tree instead of
/// silently getting a full copy. `curr_dir_depth` is the destination's depth
/// below the module root, so a client pushing into `mod/sub/` may legitimately
/// climb one level to `mod/sibling`; the budget is exactly "up to the module
/// root, no further".
///
/// Two arms, both upstream's:
/// - absolute: `util1.c:1145-1152` re-roots at `module_dir` and forces
///   `depth = 0`, so `/etc` addresses `<module>/etc`.
/// - relative: folded onto `curr_dir` (the receiver's destination), which oc
///   supplies explicitly as `resolve_base` because the daemon does not chdir
///   per connection.
///
/// The `..` collapse in [`collapse_under_root`] is what makes the result closed
/// under the module root, so no separate containment check is needed - and no
/// syscall either, which matters because a basis directory is allowed to be
/// missing on disk.
fn clamp_basis_to_module(
    ref_path: &std::path::Path,
    resolve_base: &std::path::Path,
    module_root_canonical: &std::path::Path,
) -> std::path::PathBuf {
    // upstream: `util1.c:1145` tests `*p == '/'` on the peer-supplied byte
    // string, not a platform notion of absoluteness. `Path::is_absolute()` is
    // FALSE on Windows for `/etc/foo` (no drive prefix), which would route a
    // peer-sent absolute value down the relative arm and skip the re-root
    // entirely. `has_root()` is true for a leading separator on both platforms
    // and additionally true for a drive-absolute Windows path.
    let tail = if ref_path.has_root() {
        ref_path.to_path_buf()
    } else {
        destination_below_root(resolve_base, module_root_canonical).join(ref_path)
    };
    module_root_canonical.join(collapse_under_root(&tail))
}

/// Collapses a RELATIVE operator path the way `sanitize_path()` does when the
/// value carries no leading `/`, keeping the result relative.
///
/// upstream: `util1.c:1145-1151` prefixes the rootdir **only** inside
/// `if (*p == '/')`. A relative value therefore keeps no prefix at all: it is
/// merely `..`-collapsed and handed back relative, for the consumer to anchor
/// wherever it anchors.
///
/// The `depth` budget is upstream's, and it is a budget for *leading* `..`
/// only (`util1.c:1184-1197`): a `..` is kept when nothing has been emitted
/// yet and budget remains, in which case upstream advances its virtual start
/// past the `../` so the following component is again "at the start" - which
/// is why consecutive leading `..` each consume one unit, and why a `..` that
/// pops the output back to empty re-enters the leading state. Any other `..`
/// backs up over one emitted component, or is discarded when there is nothing
/// to back up over. There is no arm that refuses.
///
/// Pure path arithmetic: no syscalls, so it is well defined for a directory
/// that does not exist yet.
fn collapse_relative_within_depth(path: &std::path::Path, depth: usize) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};

    let mut budget = depth;
    let mut leading_parents = 0usize;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(name) => tail.push(name.to_os_string()),
            // upstream: util1.c:1173-1182 drops extra slashes and `.` elements.
            // A root or prefix cannot reach here - the caller routes absolute
            // values to `clamp_basis_to_module` instead.
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                if tail.is_empty() && budget > 0 {
                    budget -= 1;
                    leading_parents += 1;
                } else {
                    tail.pop();
                }
            }
        }
    }

    let mut out = PathBuf::new();
    for _ in 0..leading_parents {
        out.push("..");
    }
    out.extend(tail);
    if out.as_os_str().is_empty() {
        // upstream: util1.c:1203-1206 - "If the resulting name would be empty,
        // change it into a `.`".
        return PathBuf::from(".");
    }
    out
}

/// Sanitises a client-supplied `--partial-dir` for a daemon receiver.
///
/// upstream: `main.c:1238-1239` runs `partial_dir` through the very same
/// `sanitize_path(NULL, partial_dir, NULL, curr_dir_depth, SP_DEFAULT)` call it
/// runs over `basis_dir[]`, so the *rewrite* is identical for both. What
/// differs is the CONSUMER, and that is why this cannot simply reuse
/// [`clamp_basis_to_module`]:
///
/// - an alt-basis dir and `--backup-dir` are anchored ONCE, at the destination,
///   so pre-joining the destination onto a relative value (what
///   `clamp_basis_to_module` does) reaches the same place upstream reaches when
///   its consumer resolves the still-relative value against `curr_dir`;
/// - `--partial-dir` is anchored PER FILE at `dirname(fname)`
///   (`util1.c` `partial_dir_fname()`, mirrored by `temp_guard.rs`
///   `partial_dir_fname`). Pre-joining the destination would pin every entry's
///   staging directory at the transfer root, so a nested entry would stage at
///   `<module_root>/pdir` where upstream stages at `<dest>/sub/pdir`.
///
/// So the two arms are kept apart: an ABSOLUTE value re-roots at the module
/// root exactly as a basis dir does, and a RELATIVE value stays relative with
/// only its `..` collapsed, for `partial_dir_fname` to anchor per file.
///
/// Keeping a relative value relative also preserves the meaning of
/// `engine::remove_partial_dir`'s absolute-path guard, which reads the value's
/// own shape to decide whether the directory is operator-named.
fn sanitize_partial_dir(
    ref_path: &std::path::Path,
    resolve_base: &std::path::Path,
    module_root_canonical: &std::path::Path,
) -> std::path::PathBuf {
    if ref_path.has_root() {
        // upstream: util1.c:1145-1151 - the test is `*p == '/'` on the
        // peer-supplied bytes, so `has_root()` not `is_absolute()`: the latter
        // is FALSE on Windows for a leading-slash path and would skip the
        // re-root. See `clamp_basis_to_module` for the same rule. - the rootdir replaces the leading slash
        // and `depth` is forced to 0, which is exactly the absolute arm of
        // `clamp_basis_to_module`.
        return clamp_basis_to_module(ref_path, resolve_base, module_root_canonical);
    }
    // upstream: util1.c:47 + :1391 - `curr_dir_depth` is the destination's
    // depth below the module root, "only set for a sanitizing daemon", counted
    // by `count_dir_elements()`. oc has no chdir, so the destination is passed
    // explicitly and its component count is the same number.
    let depth = destination_below_root(resolve_base, module_root_canonical)
        .components()
        .count();
    collapse_relative_within_depth(ref_path, depth)
}

/// Whether an already-clamped basis directory resolves out of the module tree
/// through a symlink.
///
/// The clamp in [`clamp_basis_to_module`] is lexical, so a name that is
/// module-relative on paper - `cd` - can still land outside once `mod/cd` turns
/// out to be a symlink to `/outside`. That is the `alt-dest-symlink-race`
/// attack shape: the basis becomes a read-disclosure primitive through the
/// delta-rolling checksums.
///
/// upstream: `receiver.c` `secure_relative_open()` refuses this at
/// basis-lookup time rather than at argument-parse time. oc drops the basis
/// here so the request never reaches the receiver; the receiver then
/// re-transfers, which is the same observable outcome as upstream refusing the
/// open.
///
/// A basis is allowed to be MISSING - that is upstream's `arg does not exist`
/// warning, not an escape - so an unresolvable path is kept, not dropped.
fn basis_resolves_outside_module(
    clamped: &std::path::Path,
    module_root_canonical: &std::path::Path,
) -> bool {
    clamped
        .canonicalize()
        .is_ok_and(|resolved| !resolved.starts_with(module_root_canonical))
}

/// The receiver's destination expressed relative to the module root - upstream's
/// `curr_dir_depth`, as a path rather than a count.
///
/// A destination that does not sit under the root (which `resolve_receiver_dest`
/// does not produce) degrades to the root itself, which clamps harder rather
/// than escaping.
fn destination_below_root(
    resolve_base: &std::path::Path,
    module_root_canonical: &std::path::Path,
) -> std::path::PathBuf {
    if let Ok(relative) = resolve_base.strip_prefix(module_root_canonical) {
        return relative.to_path_buf();
    }
    // `resolve_base` can carry an uncanonicalised spelling of the same tree
    // when the module path is reached through a symlink, so compare canonical
    // forms before giving up.
    resolve_base
        .canonicalize()
        .ok()
        .and_then(|canonical| {
            canonical
                .strip_prefix(module_root_canonical)
                .map(std::path::Path::to_path_buf)
                .ok()
        })
        .unwrap_or_default()
}
