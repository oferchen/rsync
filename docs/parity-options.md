# rsync 3.4.1 option parity matrix

This inventory compares upstream **rsync 3.4.1** (`options.c`) with the
current `oc-rsync` CLI surface. The matrix focuses exclusively on CLI option
availability; behavioural parity is tracked separately in
[`docs/parity-status.md`](parity-status.md).

The data in this report were generated with:

```sh
python3 tools/parity/generate_option_matrix.py \
    /tmp/rsync-3.4.1-options.c \
    /tmp/oc_help.txt \
    > docs/parity-options.yml
```

`/tmp/oc_help.txt` is captured from `target/debug/oc-rsync --help` in this
repository, ensuring the comparison reflects the binary that ships from this
workspace.

## Status overview

| status | count |
|---|---:|
| implemented | 161 |
| missing | 90 |

`missing` entries denote upstream flags that are absent from the current
`oc-rsync` help surface. Many of these are `--no-*` aliases that suppress a
short option or toggle individual archive bits; they still represent gaps that
must be closed before we can claim full compatibility.

### Coverage by category

| category | implemented | missing |
|---|---:|---:|
| connection | 12 | 3 |
| daemon | 3 | 2 |
| deletion | 13 | 0 |
| filters | 9 | 0 |
| general | 51 | 71 |
| logging | 7 | 3 |
| metadata | 23 | 4 |
| transfer | 29 | 5 |
| traversal | 14 | 2 |

The `general` bucket (71 missing options) is dominated by compatibility
aliases such as `--no-r`, `--no-g`, and `--no-times`, together with
functionality that is currently absent (`--links`, `--fake-super`, daemon-only
options such as `--config`, etc.).

### Missing upstream options

- **connection** (3):
  --address, --no-contimeout, --no-timeout
- **daemon** (2):
  --config, --motd
- **filters** (0): (none)
- **general** (71):
  --8-bit-output, --cc, --copy-as, --detach, --dparam, --early-input, --executability,
  --fake-super, --force, --fuzzy, --i-d, --ignore-errors, --ignore-non-existing,
  --links, --max-alloc, --munge-links, --no-8, --no-8-bit-output, --no-A, --no-D,
  --no-H, --no-J, --no-N, --no-O, --no-R, --no-U, --no-W, --no-X, --no-backup,
  --no-c, --no-d, --no-detach, --no-force, --no-fuzzy, --no-g, --no-h, --no-i,
  --no-i-d, --no-ignore-errors, --no-l, --no-links, --no-m, --no-munge-links,
  --no-o, --no-old-args, --no-p, --no-r, --no-s, --no-t, --no-v, --no-write-devices,
  --no-x, --no-y, --no-z, --old-args, --old-d, --only-write-batch, --qsort, --quiet,
  --read-batch, --sender, --server, --stderr, --stop-after, --stop-at, --time-limit,
  --trust-sender, --write-batch, --write-devices, --zc, --zl
- **logging** (2):
  --no-itemize-changes, --no-verbose
- **metadata** (4):
  --atimes, --crtimes, --no-atimes, --no-crtimes
- **transfer** (5):
  --compress-choice, --log-format, --new-compress, --no-checksum, --old-compress
- **traversal** (2):
  --no-mkpath, --old-dirs

### oc-rsync–only switches

`oc-rsync` currently exposes four flags that upstream `rsync 3.4.1` does not
advertise:

- `--connect-program`
- `--no-copy-links`
- `--no-copy-unsafe-links`
- `--no-keep-dirlinks`

These should either gain upstream-compatible aliases or be documented as
intentional extensions in the parity status notes.

## Machine-readable source

The authoritative dataset for this report lives in
[`docs/parity-options.yml`](parity-options.yml). Tooling should consume the YAML
file directly so we can regenerate the Markdown summary without risking manual
skews.
