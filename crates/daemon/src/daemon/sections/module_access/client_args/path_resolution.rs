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
/// Returns `None` when the destination tail contains a `..` traversal
/// segment, symmetric to `resolve_sender_sources`. Downstream file-list
/// sanitisation and the `DirSandbox` already confine the write, so this is a
/// no-op for every currently-valid destination path (a `..`-free tail is
/// unaffected); the guard rejects the escape up front as defense-in-depth.
///
/// upstream: util1.c:1035 `sanitize_path` collapses `..` against the module
/// root depth on the receiver side, behind the chroot wall. oc-rsync mirrors
/// the check explicitly because the daemon does not always chroot.
fn resolve_receiver_dest(
    module_path: &std::path::Path,
    client_args: &[String],
    module_name: &str,
) -> Option<std::path::PathBuf> {
    let positionals = extract_module_relative_paths(client_args, module_name);
    // upstream: main.c:1212-1213 - `local_name = get_local_name(flist, argv[0])`
    // uses the FIRST remaining positional (after the `.` placeholder has been
    // consumed by `do_server_recv` at line 1166). For a receiver that
    // translates to the last wire positional - the destination.
    let Some(last) = positionals.last() else {
        return Some(module_path.to_path_buf());
    };
    let tail = last.trim();
    if tail.is_empty() || tail == "." {
        return Some(module_path.to_path_buf());
    }
    // Reject `..` traversal segments so the joined destination cannot escape
    // the module root, symmetric to the sender-source guard below.
    let trimmed_for_scan = tail.trim_start_matches(['/', '\\']);
    for component in std::path::Path::new(trimmed_for_scan).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return None;
        }
    }
    // Defensive: a path arriving here that is absolute on the host OS
    // (Unix `is_absolute()` or a leading `/` that Windows treats as
    // drive-relative) cannot be allowed to escape the module root. Strip
    // the leading separators and join the remainder so the destination is
    // always under `module_path`. Tests cover both forms cross-platform.
    let rel = std::path::Path::new(tail);
    if rel.is_absolute() || tail.starts_with('/') || tail.starts_with('\\') {
        let stripped = tail.trim_start_matches(['/', '\\']);
        return Some(module_path.join(stripped));
    }
    Some(module_path.join(rel))
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
/// Sub-paths that contain `..` segments or that resolve to a host-absolute
/// path are rejected (returns `None`) so a malicious client cannot enumerate
/// files outside the module root via a crafted `rsync://host/mod/../etc/...`
/// URL. This is defense-in-depth on top of the chroot / Landlock sandbox.
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
) -> Option<Vec<std::path::PathBuf>> {
    let positionals = extract_module_relative_paths(client_args, module_name);
    if positionals.is_empty() {
        return Some(vec![module_root_dotdir(module_path)]);
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
        // Reject `..` traversal segments so the joined path cannot escape the
        // module root. Upstream rsync's sender-side `sanitize_path()` and the
        // chroot wall it lives behind cover this; oc-rsync mirrors the check
        // explicitly because the daemon does not always chroot.
        let trimmed = tail.trim_start_matches(['/', '\\']);
        for component in std::path::Path::new(trimmed).components() {
            if matches!(component, std::path::Component::ParentDir) {
                return None;
            }
        }
        // Preserve the trailing slash so the sender can detect a dotdir-style
        // source (upstream flist.c:2312-2322 appends `.` and sets DOTDIR_NAME
        // for any `fbuf[len-1] == '/'`). Upstream rsync joins module-relative
        // paths with a literal `/` regardless of host OS (util1.c pathjoin()),
        // so build the result the same way instead of going through
        // PathBuf::join, which on Windows inserts `\` and on macOS leaves
        // a trailing `/` that doubles when we re-append.
        let trailing = tail.ends_with('/') || tail.ends_with('\\');
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
        return Some(vec![module_root_dotdir(module_path)]);
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
    //     `..` rejection above the loop runs before this, so a pattern
    //     containing `..` is already rejected.
    Some(expand_sender_source_globs(module_path, sources))
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
/// upstream: `flist.c:2312-2322` - `fbuf[len-1] == '/'` enters the
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
            Ok(rel) if rel.components().any(|c| {
                matches!(c, std::path::Component::Normal(s) if path_has_glob_metachar(s))
            }) =>
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
            // and ParentDir is rejected upstream. CurDir is a no-op.
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

/// Collapses `.` and `..` segments lexically without touching the filesystem.
///
/// Used to fold a relative basis path like `mod/00/../01` into `mod/01` before
/// the module-root containment check runs. Pure path arithmetic: no syscalls,
/// no canonicalisation, so the result is well-defined even when the resolved
/// directory does not exist yet (a `--link-dest` basis is allowed to be
/// missing without aborting the transfer).
///
/// `..` at the head of an otherwise relative path is preserved verbatim so a
/// climb that escapes the join base survives into the caller's `starts_with`
/// check, which then rejects the basis as out-of-module.
fn lexically_normalize(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let popped = out.pop();
                if !popped {
                    // Nothing to pop; preserve the `..` so the caller's
                    // `starts_with(module_root)` check rejects the escape.
                    out.push("..");
                }
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Resolves a client-supplied alt-basis path (`--link-dest` / `--copy-dest` /
/// `--compare-dest`) against the receiver's resolve base and confines the
/// result inside the module root.
///
/// Returns `Some(resolved)` when the lexically-normalised path stays inside
/// `module_root_canonical`, and `None` when the path escapes (in which case
/// the caller silently drops the basis so the receiver re-transfers instead
/// of hard-linking outside the module tree).
///
/// Relative paths join under `resolve_base`; absolute paths are normalised
/// in place. Both branches then run the same canonicalise-with-lexical-
/// fallback containment check. The fallback is essential because a basis
/// directory is allowed to be missing on disk - upstream `main.c:841
/// check_alt_basis_dirs` only warns, never aborts - and we must still apply
/// the containment policy in that case.
///
/// upstream: util1.c:1035 `sanitize_path` collapses `..` against the
/// module root depth; main.c:1199-1206 `check_alt_basis_dirs` warns on
/// out-of-tree basis. We mirror the silent-drop side of that contract
/// instead of upstream's path-rewrite because our daemon does not chdir
/// per connection.
fn confine_basis_under_module(
    ref_path: &std::path::Path,
    resolve_base: &std::path::Path,
    module_root_canonical: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let joined = if ref_path.is_relative() {
        resolve_base.join(ref_path)
    } else {
        ref_path.to_path_buf()
    };
    let resolved = lexically_normalize(&joined);
    let resolved_canonical = resolved
        .canonicalize()
        .unwrap_or_else(|_| resolved.clone());
    if !resolved_canonical.starts_with(module_root_canonical) {
        return None;
    }
    Some(resolved)
}
