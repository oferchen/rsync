# Audit: `--iconv` and filter-rule path matching

Tracking: oc-rsync task #1914.

This audit answers a single question: when `--iconv=LOCAL,REMOTE` is in
effect, does oc-rsync evaluate filter rules (`--include`, `--exclude`,
`--filter`, per-directory merge files) against the same byte stream
(local-charset names) as upstream rsync 3.4.1?

References:

- `target/interop/upstream-src/rsync-3.4.1/exclude.c`
- `target/interop/upstream-src/rsync-3.4.1/flist.c`
- `target/interop/upstream-src/rsync-3.4.1/rsync.h`
- `docs/audits/iconv-pipeline.md` (companion: where iconv conversions sit
  on the wire)

## Upstream policy

A search of `target/interop/upstream-src/rsync-3.4.1/exclude.c` returns
zero references to `iconv`, `iconvbufs`, `ic_send`, or `ic_recv`. The
filter-evaluation path never invokes iconv. Patterns and names alike are
treated as opaque byte sequences in whatever charset the local process
considers native.

The hand-off between iconv and the filter system happens entirely at the
flist boundary in upstream rsync. The relevant call sites are:

- **Sender side.** `flist.c:1332` calls `is_excluded(thisname, ...)` from
  `make_file()` *before* `send_file_entry()` runs `iconvbufs(ic_send,
  ...)` at `flist.c:1579-1603`. `thisname` is the path returned by
  `readlink_stat()` on the local filesystem - it is in the local
  charset, never the remote charset.
- **Receiver side.** `flist.c:738-754` runs `iconvbufs(ic_recv, ...)`
  inside `recv_file_entry()` and stores the converted (local-charset)
  bytes back into `thisname` *before* the entry is appended to the
  receiver's flist. Later, `flist.c:3107` calls
  `is_excluded(f_name(file, fbuf), 1, ALL_FILTERS)` while pruning the
  received list for `--delete-excluded`. Because the file list now holds
  local-charset names, the filter sees local-charset input.
- **Local-copy side.** When upstream walks the destination tree to apply
  `--delete`, it reads `dirent.d_name` (already local-charset) and
  passes it directly to `check_filter()` (`generator.c:delete_in_dir()`).
  No iconv is involved.

In all three call sites the filter input is a local-charset byte
sequence. Filter pattern strings come from the command line, `.rsync-filter`
merge files, or `--filter`/`--exclude-from` files, all of which are read
verbatim in the local charset. Comparison is therefore byte-equivalent
on both sides of the conversion - exactly what users expect when they
write a UTF-8 pattern on a UTF-8 host and the wire happens to carry
ISO-8859-1.

## oc-rsync mapping

| Upstream call site | Direction | oc-rsync call site | Charset at filter input | Status |
|---|---|---|---|---|
| `flist.c:1332` `is_excluded(thisname, ...)` in `make_file()` (sender walk) | local | `crates/transfer/src/generator/file_list/walk.rs:106` `self.filter_chain.allows(&relative, ...)` | local (raw filesystem path; iconv not yet applied) | Correct. |
| `flist.c:3107` `is_excluded(f_name(file, fbuf), 1, ALL_FILTERS)` (receiver `--delete-excluded` prune) | local | `crates/transfer/src/receiver/directory/deletion.rs:138` `filter_chain.allows_deletion(&rel_for_filter, ...)` | local (entry name from `read_dir`; receiver flist names are post-`ic_recv`) | Correct. |
| `generator.c:delete_in_dir()` `check_filter(...)` against `dirent.d_name` | local | `crates/transfer/src/receiver/directory/deletion.rs:138` (same site as above) | local (`entry.file_name()` from `read_dir`) | Correct. |
| Local-copy walker `is_excluded()` | local | `crates/engine/src/walk/filtered_walker.rs:109` and `crates/engine/src/local_copy/context_impl/options.rs:449` | local (no iconv on `cp`-style local copies) | Correct. |

The wire-side conversions live in `crates/protocol/src/flist/{read,write}/`
and run *after* the filter chain has already accepted or rejected the
entry on the sender side, and *before* the entry is observed by the
filter chain on the receiver side. See `docs/audits/iconv-pipeline.md`
for the full conversion-site inventory.

## Pattern strings

Upstream stores filter patterns as raw `char *` in `filter_rule.pattern`
(`exclude.c:add_rule()`). It never iconv-converts those bytes. oc-rsync
follows the same model: `crates/cli/src/frontend/arguments/parser/mod.rs`
reads `--exclude`, `--include`, and `--filter` as `OsString`/`String`,
forwards the bytes through `crates/transfer/src/generator/filters.rs::parse_received_filters`,
and stores them as `FilterRule::pattern` (a `String`, byte-equivalent
on Unix to the raw command-line bytes). Per-directory merge files are
read with `fs::read_to_string()` in `crates/filters/src/chain.rs::enter_directory`,
which preserves the raw on-disk bytes. No iconv hop is performed on
either side, mirroring upstream.

This means a UTF-8 user running `--exclude='café*'` on a UTF-8 host with
`--iconv=UTF-8,ISO-8859-1` (where the wire carries ISO-8859-1)
correctly matches a local file whose name is `café.txt` in UTF-8: the
filter check happens before the file name is iconv'd to the wire on the
sender, and after the file name has been iconv'd from the wire on the
receiver. Both sides compare local-charset patterns against
local-charset names.

## Finding

No code change is required. The filter-evaluation entry points already
operate on local-charset names by construction:

1. The sender walks the local filesystem and feeds raw `Path` bytes into
   `FilterChain::allows()` before any iconv conversion.
2. The receiver applies `ic_recv` inside the file-list reader, so by the
   time the file list reaches the filter chain (during
   `--delete-excluded` pruning, deletion sweeps, or the local-copy
   executor), the names are already local-charset.
3. Pattern strings are stored verbatim, exactly as upstream stores them.

Wiring a `FilenameConverter` into the filter chain would be incorrect:
it would double-convert names on the receiver and convert patterns away
from the user's local charset on both sides, breaking pattern matches
for any non-ASCII pattern.

The behaviour is therefore wire-equivalent to upstream rsync 3.4.1 with
`--iconv` enabled. This audit is the deliverable for task #1914; no
further code action is needed.

## Test coverage

The existing iconv pipeline tests cover the wire conversion direction
and the round-trip identity, and the existing filter tests cover pattern
evaluation against local-charset paths. No additional tests are
required because the system under test is the unmodified composition of
those two already-tested pieces - there is no new code path to
exercise.
