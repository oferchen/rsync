# Windows Path Separator Encoding Audit

Confirms that every sender-side wire-emit site for filenames and symlink
targets normalizes Windows native `\` separators to POSIX `/` before the
bytes leave the host. Complements the receiver-side hardening in
PR #3496 (Windows drive-prefix rejection) and closes Finding F1 of the
prior audit at `docs/audits/windows-path-normalization.md`.

Tracks: issue #1905 (wire-encoded filename separator leak).

Last updated: 2026-05-01.

## Scope

The rsync wire format treats filename bytes as opaque sequences with one
implicit invariant: directory separators are `/`. Upstream rsync's only
Windows port runs under Cygwin, whose POSIX layer presents `/`-separated
paths to the application, so upstream never normalizes separators in the
emit path - it relies on the OS surface. oc-rsync targets native Win32
(`windows-msvc` and `windows-gnu`) where `std::path::PathBuf::push` and
`Path::join` produce `\`-separated bytes. Without explicit normalization
on the sender, those bytes would reach the wire and break interop with
any non-Cygwin peer.

This audit walks every code path that converts a `Path` (or `PathBuf`)
into bytes destined for the wire and confirms the conversion goes
through the central helper.

## Central helper

`crates/protocol/src/flist/wire_path.rs:33-60` defines
`path_bytes_to_wire(p: &Path) -> Cow<'_, [u8]>`:

- On Unix (`#[cfg(unix)]`), the function is a zero-copy borrow of the
  `OsStr` bytes. `\` is a legitimate filename byte on POSIX and is
  preserved verbatim.
- On non-Unix (`#[cfg(not(unix))]`, i.e. Windows), the function inspects
  the encoded bytes and only allocates when at least one `\` byte is
  present, in which case every `\` is replaced with `/`. Inputs that
  were already `/`-separated take the borrow path with no allocation.

The helper mirrors the identity-on-Unix / convert-on-Windows shape of
the sibling `wire_mode` module so the platform contract is documented
in one place.

## Sender-side wire-emit sites

Every site that writes a filename or symlink-target byte stream to the
wire goes through `path_bytes_to_wire`:

1. **Filename emission** -
   `crates/protocol/src/flist/entry/accessors.rs:131-135`
   `FileEntry::name_bytes()` returns `path_bytes_to_wire(&self.name)`.
   This is the single accessor used by the writer for the on-wire name.

2. **Filename writer call site** -
   `crates/protocol/src/flist/write/mod.rs:376`
   `let raw_name = entry.name_bytes();` is the only path that produces
   the bytes consumed by `write_name()`. There is no parallel "raw" path.

3. **Symlink target emission** -
   `crates/protocol/src/flist/write/encoding.rs:115-124`
   `write_symlink_target` calls `path_bytes_to_wire(target.as_path())`
   before `writer.write_all(&target_bytes)`. The `varint30(len)` prefix
   is computed from the normalized byte count.

4. **Batched flist writer** -
   `crates/protocol/src/flist/batched_writer/writer.rs:216`
   delegates to `FileListWriter::write_entry`, inheriting the same
   normalization for `--write-batch` mode.

5. **Generator file-list emission** -
   `crates/transfer/src/generator/protocol_io.rs:240` and
   `crates/transfer/src/generator/protocol_io.rs:310`
   both call `flist_writer.write_entry(writer, entry)`, so the
   sender's two emission loops (initial flist, INC_RECURSE segments)
   share the normalized path.

6. **Local-copy executor batch capture** -
   `crates/engine/src/local_copy/executor/directory/recursive/batch.rs:152`
   calls `flist_writer.write_entry(&mut buf, &entry)` to capture the
   batch flist into a buffer that is later spooled to the batch file.
   The same `path_bytes_to_wire` route is taken.

## Sites that do *not* require normalization

The following sites surface path bytes for purposes other than wire
emission and are correctly left unnormalized:

- `crates/protocol/src/flist/entry/accessors.rs:79-95` Unix branch of
  `strip_leading_slashes`, which trims `/` (and only `/`) from a path
  that lives in a `PathBuf`. The companion non-Unix branch trims both
  `/` and `\` so receive-side normalization stays consistent (line
  103). Trimming is independent from wire emission.
- `crates/protocol/src/flist/trace.rs:180` formats the entry name for
  human-readable trace logging, not the wire.
- `crates/protocol/src/xattr/wire/encode.rs:56` writes xattr *attribute
  names* (e.g. `user.foo`), not paths. Attribute names never contain
  separators.
- `crates/engine/src/local_copy/executor/directory/recursive/batch.rs:169`
  records sort-key metadata for `flist_sort_and_clean()` parity. The
  bytes never leave the process - they exist only to compute the
  traversal-to-sorted index map for the local batch writer.

## Test coverage

Per-platform regression tests live next to the helper in
`crates/protocol/src/flist/wire_path.rs:62-136`:

- `forward_slash_path_is_identity` (all platforms).
- `empty_path_yields_empty_bytes` (all platforms).
- `dot_path_is_identity` (all platforms).
- `unix_borrows_without_allocation` (`#[cfg(unix)]`) - verifies the
  zero-copy invariant.
- `unix_preserves_backslash_byte_in_filename` (`#[cfg(unix)]`) -
  guards against accidentally rewriting `\` on POSIX where it is a
  valid filename byte.
- `windows_backslash_is_translated_to_forward_slash` (`#[cfg(windows)]`)
  - the canonical fix-target: `PathBuf::push` produces `\` bytes that
  must be rewritten.
- `windows_deep_path_is_translated` (`#[cfg(windows)]`) - multiple
  `\` separators in a single literal.
- `windows_already_forward_slash_borrows` (`#[cfg(windows)]`) - the
  borrow path is exercised when no `\` byte is present.
- `windows_mixed_separators_are_normalized` (`#[cfg(windows)]`) -
  inputs that interleave `/` and `\` (e.g. paths constructed by
  `Path::join("a/b", "c\\d")`) emit pure `/`.

## Result

Audit confirmed clean: the fix that closes F1 from
`docs/audits/windows-path-normalization.md` is live, every wire-emit
site routes through `path_bytes_to_wire`, and the regression test
matrix covers the documented edge cases. No code change is required by
this audit.

## References

- Upstream `flist.c:534-570` `send_file_entry()` filename emission -
  bytes are written verbatim with no separator normalization.
- Upstream `flist.c:660-670` `send_file_entry()` symlink-target
  emission - same verbatim contract as filename.
- Upstream `util1.c:955-961` `__CYGWIN__` block - the only `\`
  handling in upstream lives at the Cygwin POSIX boundary, which
  oc-rsync does not run under.
- PR #3456 - prior fix that introduced `path_bytes_to_wire` and the
  per-platform test matrix.
- PR #3496 - receiver-side counterpart that rejects Windows drive
  prefixes (`Component::Prefix`) from untrusted senders during
  `sanitize_file_list`.
- `docs/audits/windows-path-normalization.md` - the originating audit
  whose Finding F1 this audit retires.
- `docs/audits/windows-path-edge-cases.md` - the broader Windows
  hazard catalog referenced by PR #3496.
