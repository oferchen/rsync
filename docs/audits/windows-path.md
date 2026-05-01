# Windows Path Separator Leakage Audit

Tracking issue: oc-rsync task #1905 ("verify no backslash leak"). Sibling
audit: [`docs/audits/windows-path-normalization.md`](windows-path-normalization.md)
which catalogues path-form parsing for the original normalization work
(task #1842).

Last updated: 2026-05-01

## Summary

This audit asks one narrow question: can a Windows-native backslash (`\`)
slip from a `Path`/`PathBuf` into a byte stream that is either (a) emitted
on the wire or (b) matched against a filter pattern, without first being
normalized to forward slash?

Upstream rsync 3.4.1 is POSIX-native. `util1.c:clean_fname()`
(`util1.c:943-1011`) only treats `b'/'` as a separator. `flist.c` writes
the same byte buffer (`thisname`) it receives from `make_file()` and
checks against filter rules with that buffer, all `/`-separated. The
Cygwin port (`#ifdef __CYGWIN__` in `clean_fname`, `util1.c:955-961`)
does no rewriting either - Cygwin's POSIX layer hands rsync `/`-paths
already. There is no precedent for `\` ever being legal on the wire.

The audit finds **two live leak sites** (both reachable today on the
`windows-msvc` / `windows-gnu` Rust targets) plus **one latent site**
that is not exploitable today only because the surrounding feature is
gated behind unimplemented platform support. F1 reproduces F1 from the
prior `windows-path-normalization.md` audit (task #1842). F2 is a new
finding: the filter chain matches against `\`-containing paths on
Windows generators, silently bypassing rules. F3 documents a dormant
analogue in `is_unsafe_symlink`. This audit consolidates these under
the wire/filter rubric required by issue #1905 and recommends a single
shared helper as the minimal fix.

## Methodology

Every site that converts a `Path` / `PathBuf` / `OsStr` to bytes or to a
`str` was inspected. Each site was classified by direction:

- **out-wire**: bytes leave the local process toward a peer (flist
  encode, symlink target encode, batch script).
- **in-wire**: bytes enter the local process from a peer (flist decode,
  symlink target decode).
- **filter-match**: bytes are compared against a filter pattern
  (`crates/filters` consumers).
- **internal**: bytes never escape the process; used for logging,
  display, or local syscalls.

`internal` sites are not relevant to issue #1905 and are noted only when
they sit on the same code path as an out-wire / filter-match site.

## Path Conversion Sites

The columns are: site (file:line), direction, what conversion happens,
whether `\` -> `/` normalization occurs.

### out-wire

| Site | What | Normalize `\` -> `/`? |
|---|---|---|
| `crates/protocol/src/flist/entry/accessors.rs:120-128` `name_bytes()` | `#[cfg(not(unix))]` returns `self.name().as_bytes()` where `name()` is `to_str().unwrap_or("")`. Native Windows `PathBuf` retains `\`. | NO |
| `crates/protocol/src/flist/write/mod.rs:376-384` `write_entry()` | calls `entry.name_bytes()`, optionally runs `apply_encoding_conversion()` (iconv, byte-transparent), writes via `write_name()` (`crates/protocol/src/flist/write/encoding.rs:79-98`). | NO (passes whatever `name_bytes()` returned) |
| `crates/protocol/src/flist/write/encoding.rs:115` `write_symlink_target()` | `target.as_os_str().as_encoded_bytes()`. On Windows this is WTF-8 of the native string, which keeps `\`. | NO |
| `crates/protocol/src/flist/write/metadata.rs:182-185` user/group name | name field (not a path). | n/a |
| `crates/transfer/src/generator/file_list/walk.rs:60` `relative = path.strip_prefix(base)` | `relative` is fed to `create_entry()` and stored verbatim in `FileEntry::name`. On Windows, `\` is preserved. | NO |
| `crates/transfer/src/generator/file_list/entry.rs:50,57,61,...` `FileEntry::new_*(relative_path, ...)` | constructor stores the `PathBuf` unchanged. | NO |
| `crates/batch/src/writer.rs` batch file output | batch files store the wire stream verbatim. Inherits whatever `name_bytes()` produced. | NO (transitively) |

### in-wire

| Site | What | Normalize `\` -> `/`? |
|---|---|---|
| `crates/protocol/src/flist/read/name.rs:35-99` `read_name()` | reads `same_len + suffix_len` raw bytes. No separator handling. | n/a (bytes preserved) |
| `crates/protocol/src/flist/read/name.rs:141-234` `clean_and_validate_name()` | Mirrors upstream `clean_fname(CFN_REFUSE_DOT_DOT_DIRS)` plus the leading-slash check at `flist.c:756-760`. Treats only `b'/'` as separator. | n/a (assumes `/` only, per upstream) |
| `crates/transfer/src/sanitize_path.rs:34-140` `sanitize_path()` (and `_keep_dot_dirs`) | byte-level `/`-only processing, mirrors upstream `util1.c:1035-1108`. | n/a (assumes `/` only) |
| `crates/protocol/src/flist/entry/constructors.rs:136-171` `FileEntry::from_raw_bytes()` | `#[cfg(not(unix))]` builds `PathBuf::from(String::from_utf8_lossy(&name).into_owned())`. | n/a; wire bytes assumed `/`-only by upstream contract |
| `crates/protocol/src/flist/read/extras.rs:53-66` symlink target decode | same pattern: `String::from_utf8_lossy` on Windows. | n/a |

### filter-match

| Site | What | Normalize `\` -> `/`? |
|---|---|---|
| `crates/transfer/src/generator/file_list/walk.rs:106` `filter_chain.allows(&relative, ...)` | `relative` is the `strip_prefix` slice from above; on Windows it carries `\`. Patterns are `/`-separated. | NO |
| `crates/transfer/src/receiver/directory/deletion.rs:135-138` `filter_chain.allows_deletion(&rel_for_filter, ...)` | `rel_for_filter = dir_relative.join(&name)` builds with the platform separator (`\` on Windows). | NO |
| `crates/filters/src/compiled/rule.rs:38-76` `CompiledRule::matches()` | `globset::GlobMatcher::is_match(path)` on a `Path`. globset 0.4 does **not** rewrite `\` to `/` before matching anchored patterns. | NO |
| `crates/filters/src/compiled/pattern.rs:13-30` `compile_patterns` | builds globs with `literal_separator(true)`, `backslash_escape(true)`. The pattern grammar treats `/` as the only path separator. | n/a (patterns) |

### internal-only (for completeness)

- `crates/protocol/src/flist/entry/accessors.rs:94-106`
  `strip_leading_slashes()` `#[cfg(not(unix))]` branch uses
  `to_string_lossy()`; it only trims leading `/`, never separators
  inside the path. Output is consumed by the wire encoder, so leakage
  follows F1.
- `crates/cli/src/frontend/arguments/parser/mod.rs:158,175,189` etc.
  use `to_string_lossy()` on user-supplied OsStr values, not on
  `PathBuf`s after directory traversal. Operands are forwarded as
  `OsString` (no normalization), but they are never sent over the wire
  as filenames - they become the local source/dest directories.
- `crates/cli/src/frontend/progress/render.rs`, `placeholder.rs`,
  `out_format/render` use `to_string_lossy()` for human-visible output
  only.
- `crates/core/src/client/remote/invocation/transfer_role.rs:25-56`
  `operand_is_remote()` reads `\` to decide local vs remote (correct -
  this is internal classification, never re-emitted on the wire).
- `crates/engine/src/local_copy/operands.rs:189-360` Windows-prefix
  detection (`\\?\`, `\\.\`, UNC). Only used to compute
  `relative_prefix_components`; the result is a count, never a string.

## Findings

### F1. Wire emission of native `\` from Windows sender (HIGH)

Reproduces F1 of `windows-path-normalization.md`. Two leak sites compose:

1. `crates/protocol/src/flist/entry/accessors.rs:120-128` returns the
   native string bytes from `name_bytes()`.
2. `crates/protocol/src/flist/write/mod.rs:376-384` writes those bytes
   to the wire via `write_name` without separator translation.
3. `crates/protocol/src/flist/write/encoding.rs:115` does the same for
   symlink targets via `as_encoded_bytes()`.

The native string is what `walk.rs:60` produced via `path.strip_prefix`,
which on `windows-msvc` retains the `\` separators of the input. On a
single-component relative entry the bug is invisible (no separator is
written). On any multi-component entry (`subdir\file.txt`), the wire
bytes contain `\`, which a Linux receiver decodes with
`OsStr::from_bytes(&name)` (`flist/entry/constructors.rs:144-150`) into
a single 16-byte filename, not into nested directories.

Upstream cannot reproduce this because Cygwin presents `/`-paths to
rsync. `flist.c:701-738` (`thisname` buffer) treats every byte after
prefix-decompression as either `/` or part of a component name.

**Severity.** HIGH (silent on-disk corruption on cross-platform push).
Same severity as the prior audit. Still unfixed.

### F2. Filter-rule matching against `\`-containing relative paths (HIGH)

`crates/transfer/src/generator/file_list/walk.rs:106` invokes
`self.filter_chain.allows(&relative, metadata.is_dir())`. `relative` is
the `strip_prefix` output (`walk.rs:60`), which retains `\` on Windows.

`crates/filters/src/compiled/rule.rs:38-76` calls
`GlobMatcher::is_match(path)` on each rule. The matchers were compiled
with `GlobBuilder::literal_separator(true)`
(`crates/filters/src/compiled/pattern.rs:23`), which makes `/` (and only
`/`) the separator inside the glob. globset 0.4 receives the path via
its `Candidate` impl, which on Windows does not rewrite `\` to `/`
before anchored matching - so:

- A pattern `/build/*` against `Path::new("build\\out.o")` does not match.
- A pattern `**/*.o` against `Path::new("src\\lib\\util.o")` matches the
  trailing `.o` only by accident (globstar consumes any byte).
- A descendant rule from `src/` does not see `src\\foo.bar`.

Net effect: filter rules silently fail to apply to nested paths on
Windows senders. `crates/transfer/src/receiver/directory/deletion.rs:135-138`
has the same defect (`dir_relative.join(&name)` uses the platform
separator).

This is not equivalent to F1: even a Windows -> Windows transfer where
both sides round-trip `\` correctly would have wrong filter behaviour,
because the filter pattern grammar is not platform-conditional.

Upstream `exclude.c:check_filter()` (`exclude.c:1031-1108`) operates on
the wire-form `/`-separated `fname` exclusively; the question never
arises for upstream.

**Severity.** HIGH (silent filter bypass on nested Windows paths). New
finding for this audit.

### F3. `is_unsafe_symlink` separator assumption (LOW, latent)

`crates/transfer/src/symlink_safety.rs:36-54` and the helpers
`compute_link_depth`, `is_target_within_depth`, `has_mid_path_dotdot`
all walk the byte buffer treating `/` as the only segment separator.
On Windows, a target obtained via `std::fs::read_link()` may contain
`\`. The depth check then never decrements because no segment ever
equals `..` (it sees `..\file` as a single segment).

This is dormant today: Windows symlink creation/preservation is a no-op
in the receiver path (see `docs/windows_platform_parity.md`). When
Windows symlink support lands, this site joins F1 / F2.

**Severity.** LOW (dead on Windows today; activates with future symlink
support).

### F4. `sanitize_path` and `clean_and_validate_name` are `/`-only (INFORMATIONAL)

`crates/transfer/src/sanitize_path.rs` and
`crates/protocol/src/flist/read/name.rs:141-234` are byte-level mirrors
of `util1.c:1035-1108` and `util1.c:943-1011 + flist.c:756-760`. They
intentionally treat only `b'/'` as a separator. This matches upstream
exactly. If F1 is ever fixed (no more outgoing `\`), and if all incoming
paths can be assumed `/`-separated by protocol contract, these
functions need no change.

If F1 is *not* fixed, a malicious or buggy Windows sender could ship
`..\..\..\etc` as a single component. The receiver's `clean_and_validate_name`
would not recognize the `..` and would write a literal filename. This
is not a directory-traversal escape (the bytes `..\\..\\etc` form one
component which gets prefixed with the destination directory), so it is
not exploitable. Documented for completeness.

**Severity.** INFORMATIONAL.

### F5. Daemon module path resolution (NO LEAK)

The daemon's `auth_path` and module `path` settings come from
`oc-rsyncd.conf` (`crates/daemon/src/daemon/sections/config_paths.rs`).
These are server-side filesystem paths, never sent on the wire. The
daemon runs predominantly on Linux; on Windows it is feature-stubbed.
No leakage path here.

## Evidence Chain

To establish that today's leakage is exactly the sites above and not a
hidden fourth one, the audit traced every conversion in the
"Path Conversion Sites" table to either:

1. an internal sink (logging, comparison, syscall - not on wire / not
   matched against filter), or
2. a wire-format byte stream (covered by F1), or
3. a filter-rule matching call (covered by F2), or
4. a symlink-safety check (covered by F3, currently dormant).

There is no fifth path. In particular:

- **iconv** (`crates/protocol/src/flist/write/encoding.rs:307-322`,
  `read/name.rs:106-118`) operates on bytes opaquely; it does not
  rewrite separators.
- **batch mode** (`crates/batch/src/writer.rs`,
  `crates/batch/src/script.rs`) records the wire bytes verbatim, so it
  inherits F1's outgoing leak but introduces none of its own.
- **CLI argument parsing** does not encode user paths into wire bytes;
  source/dest operands become local syscall arguments after
  classification by `operand_is_remote()`. The `to_string_lossy()`
  calls in `cli/src/frontend/arguments/parser/mod.rs:158-189` are for
  parsing flag values like `--port`, `--bwlimit`, not paths.
- **xattr names** are not paths.

## Recommended minimal fix (out of scope here)

A single helper, applied at the exact moment a path becomes wire bytes
or a filter argument, fixes F1 and F2 simultaneously. The
`crates/protocol/src/flist/wire_mode.rs` precedent (identity on Unix,
canonicalize on Windows) is the right shape:

```text
fn to_wire_path_bytes(p: &Path) -> Cow<'_, [u8]>
```

- On Unix: `Cow::Borrowed(p.as_os_str().as_bytes())`.
- On Windows: encode via `OsStrExt::encode_wide()` -> WTF-8 -> rewrite
  `\\` -> `/` -> `Cow::Owned(Vec<u8>)`.

Apply at four points:

1. `crates/protocol/src/flist/entry/accessors.rs:120-128`
   `name_bytes()` non-Unix branch.
2. `crates/protocol/src/flist/write/encoding.rs:115`
   `write_symlink_target()` - run the helper before
   `write_all(target_bytes)`.
3. `crates/transfer/src/generator/file_list/walk.rs` between line 60
   (`strip_prefix`) and line 106 (`filter_chain.allows`) - normalize
   `relative` to a `/`-separated `PathBuf` for the filter call. The
   FS-side use of `relative` for `create_entry` should also flow
   through the same helper so that the `FileEntry` stores a
   wire-canonical path - this collapses F1 and F2 into the same fix.
4. `crates/transfer/src/receiver/directory/deletion.rs:135` -
   normalize `rel_for_filter` before `allows_deletion`.

The receiver decode side (`from_raw_bytes`, `read_name`) already accepts
`/`-bytes and converts via `PathBuf::from`, which `Path::join` on
Windows handles transparently. No receiver change is required.

A regression test that builds a `FileEntry` whose `name` was created via
`PathBuf::push("subdir"); push("file.txt")` on Windows and asserts the
wire bytes contain only `/` is the acceptance criterion. Same wire-byte
golden for the symlink target path.

This audit does not implement the fix because:

- Issue #1842 closed the original normalization work, and #1905 is the
  follow-up *verification* task. The fix proper belongs to a new
  `feat:` PR with golden-byte updates, Linux-receiver interop tests
  driving Windows-encoded inputs, and a CI matrix entry that exercises
  `windows-msvc` -> `linux-musl` interop.
- The audit is doc-only by mandate of the issue. No code is touched.

## Follow-up tasks

1. **HIGH** Implement `to_wire_path_bytes()` once in `protocol::flist`
   (next to `wire_mode.rs`) and apply at the four sites listed above.
   Tracks F1 and F2.
2. **LOW** When Windows symlink support lands, route
   `is_unsafe_symlink`'s byte input through the same helper so `\`
   segments are recognized. Tracks F3.
3. **INFORMATIONAL** Once F1 is fixed, add a debug-assertion on the
   receive side (`clean_and_validate_name`) that the input contains no
   `b'\\'` bytes. This catches regressions cheaply because the
   asserted invariant is exactly the upstream contract.

## References

- Upstream `target/interop/upstream-src/rsync-3.4.1/util1.c:943-1011`
  (`clean_fname`).
- Upstream `target/interop/upstream-src/rsync-3.4.1/util1.c:1035-1108`
  (`sanitize_path`).
- Upstream `target/interop/upstream-src/rsync-3.4.1/flist.c:701-768`
  (`thisname` recv path).
- Upstream `target/interop/upstream-src/rsync-3.4.1/exclude.c:1012-1052`
  (`check_filter`).
- Upstream Cygwin slash-preservation guard:
  `target/interop/upstream-src/rsync-3.4.1/util1.c:955-961`.
- Existing oc-rsync helpers:
  `crates/protocol/src/flist/wire_mode.rs` (precedent for
  identity-on-Unix / normalize-on-Windows wire helpers),
  `crates/transfer/src/sanitize_path.rs`,
  `crates/protocol/src/flist/read/name.rs:141-234`.
- Sibling audit: `docs/audits/windows-path-normalization.md` (#1842).
- Platform parity: `docs/windows_platform_parity.md`.
- globset 0.4: `Cargo.lock` records version 0.4.18; matcher behavior
  for `Path::is_match` on Windows verified against the crate source.
