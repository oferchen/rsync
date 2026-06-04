# Partial file retention on interrupt

This guide describes how `oc-rsync` handles interrupted transfers and how
the `--partial`, `--partial-dir`, and `--delay-updates` options interact
to enable resumable transfers. All behavior mirrors upstream rsync 3.4.x.

## Default behavior (no flags)

When a transfer is interrupted - by a signal (SIGINT, SIGTERM), a
connection drop, or an I/O error - `oc-rsync` deletes the temporary file
for the in-progress transfer. The destination tree is left unchanged:
either the previous version of the file remains or no file exists if this
was an initial copy.

```sh
# Transfer interrupted - temp file is cleaned up, dest unchanged
oc-rsync -a source/ dest/
```

This is the safest default. No partially written files appear at the
destination, and other processes reading from the destination tree never
see incomplete data.

## --partial

With `--partial`, the incomplete temporary file is renamed to the final
destination path when the transfer is interrupted. The partial file
replaces any existing file at that path.

On Unix, the retained file's modification time is set to the epoch
(1970-01-01 00:00:00 UTC, i.e. mtime = 0). This is deliberate - it
ensures that a subsequent `--update` run will not skip the partial file,
because the epoch mtime is always older than any real source file.

```sh
# First run - interrupted mid-transfer
oc-rsync -a --partial source/ dest/

# Second run - resumes from the partial file
oc-rsync -a --partial source/ dest/
```

On the second run, `oc-rsync` detects that the destination file differs
from the source (by size and mtime) and uses the partial file as a delta
basis. Only the missing data is transferred.

### Windows note

Windows NTFS cannot represent a modification time of 1970-01-01 00:00:00
UTC. On Windows, partial files keep whatever mtime they had at the time
of interruption. This means `--update` may skip the partial file if its
mtime is newer than the source. Workarounds:

- Omit `--update` on the retry run.
- Use `--partial-dir` instead so partial files do not occupy the final
  destination path.

## --partial-dir DIR

With `--partial-dir`, interrupted transfers are moved into the specified
directory rather than to the final destination. This implies `--partial`.

```sh
# First run - interrupted mid-transfer
oc-rsync -a --partial-dir=.rsync-partial source/ dest/

# Second run - resumes from partial files in .rsync-partial/
oc-rsync -a --partial-dir=.rsync-partial source/ dest/
```

The destination tree remains unchanged after an interrupt - no incomplete
files appear at their final paths. On a subsequent run, the receiver
checks the partial directory for a matching basis file and uses it for
delta transfer.

This is the recommended approach when other processes read from the
destination tree and must not encounter incomplete files.

## --delay-updates

With `--delay-updates`, all transferred files are staged in a temporary
directory and renamed to their final destinations only after the entire
transfer completes successfully. This provides atomic updates - either
all files are updated or none are.

When no explicit `--partial-dir` is specified, `--delay-updates`
implicitly sets `--partial-dir` to `.~tmp~` and enables `--partial`.

```sh
# Atomic update - all files land at once on success
oc-rsync -a --delay-updates source/ dest/
```

If the transfer is interrupted before the final rename sweep:

- All completed files remain in the staging directory (`.~tmp~` or the
  configured `--partial-dir`).
- The destination tree is untouched.
- A subsequent run picks up the staged files and resumes.

## Interaction with --update

The `--update` flag skips files where the destination is newer than the
source. The epoch mtime (mtime = 0) on partial files prevents `--update`
from skipping them on Unix:

| Scenario | --update behavior |
|----------|-------------------|
| No partial file at dest | Normal transfer |
| Partial file, mtime = 0 (Unix) | Transfers - epoch is older than source |
| Partial file, mtime preserved (Windows) | May skip if dest mtime >= source mtime |
| Partial file in --partial-dir | Transfers - dest path has no partial file |

## Resume mechanics

When `oc-rsync` finds an existing file at the destination (or in the
partial directory), it uses that file as a basis for delta transfer:

1. The generator compares the destination file's size and mtime against
   the source. A mismatch triggers a transfer.
2. The generator computes rolling checksums over the basis file (the
   existing partial or destination file) and sends them to the sender.
3. The sender matches source data against the rolling checksums and
   sends only the non-matching blocks.
4. The receiver reconstructs the complete file by copying matched blocks
   from the basis and inserting new data from the sender.

This means the resume is not a simple byte-offset append - it uses the
same delta-transfer algorithm as any update, which handles insertions and
deletions anywhere in the file.

## Option combinations

| Flags | On interrupt | Dest tree changed? | Resume source |
|-------|-------------|-------------------|---------------|
| (none) | Temp file deleted | No | Full re-transfer |
| `--partial` | Partial file at dest path | Yes | Dest file as delta basis |
| `--partial-dir=DIR` | Partial file in DIR | No | DIR file as delta basis |
| `--delay-updates` | Files staged in .~tmp~ | No | Staged files as delta basis |
| `--delay-updates --partial-dir=DIR` | Files staged in DIR | No | DIR files as delta basis |

## Examples

Resumable transfer with partial files at the destination:

```sh
oc-rsync -avz --partial source/ dest/
```

Resumable transfer with partial files in a separate directory:

```sh
oc-rsync -avz --partial-dir=.rsync-partial source/ dest/
```

Atomic update with automatic resume on interrupt:

```sh
oc-rsync -avz --delay-updates source/ dest/
```

Atomic update with a custom staging directory:

```sh
oc-rsync -avz --delay-updates --partial-dir=/tmp/staging source/ dest/
```
