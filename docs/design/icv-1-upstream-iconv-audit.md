# ICV-1: Upstream rsync --iconv Behavior Audit

Audit of upstream rsync 3.4.1 `--iconv` charset conversion end-to-end.

## 1. Option Parsing (options.c)

### Compile-time gating

The entire `--iconv` feature is gated behind `ICONV_OPTION`, defined in
`configure.ac:987-998`. Three configurations exist:

- `--disable-iconv` - `ICONV_OPTION` undefined, feature compiled out
- `--enable-iconv` (or `--enable-iconv=yes`) - `ICONV_OPTION` set to `NULL` (available but off by default)
- `--enable-iconv=CHARSET` - `ICONV_OPTION` set to a string literal (e.g., `"UTF-8"`), enabled by default with that charset

The default build (`config.h:649`) sets `ICONV_OPTION NULL` - the option exists but is not active unless the user passes `--iconv`.

Reference: `configure.ac:987-998`, `rsync.h:515-531`, `options.c:219-224`.

### Argument format

`--iconv` accepts a string argument (`options.c:814`):

```
--iconv LOCAL[,REMOTE]
```

- Single value: used for both local and remote sides
- Comma-separated: first part is local charset, second is remote charset
- `.` or empty string: use the system's locale charset (`nl_langinfo(CODESET)` or `locale_charset()`)
- `-` (dash, on client only): disable iconv processing (`options.c:2053-2054`)

The `--no-iconv` flag (`OPT_NO_ICONV`) sets `iconv_opt = NULL` (`options.c:1666-1669`).

### Environment variable

If `RSYNC_ICONV` is set and non-empty, it is used as the default when `--iconv`
is not specified on the command line. This only applies to the client (not
daemon, not when `protect_args` is active). Reference: `options.c:1364-1367`.

### Daemon and server reset

When `--server` is parsed, `iconv_opt` is reset to `NULL` (`options.c:1395-1397`).
The server side only gets iconv re-enabled when the client sends `--iconv` via
`server_options()`. Same for `--daemon` (`options.c:1415-1417`).

### Refuse logic

Daemons can refuse `--iconv`. If `ICONV_OPTION` is compiled in but the module's
`charset` parameter is empty, `iconv` is added to the refuse list
(`options.c:994-996`). If `ICONV_OPTION` is not compiled at all, `iconv` is
always refused (`options.c:1010-1011`).

The `refused_no_iconv` variable tracks whether `--no-iconv` was refused
(`options.c:327, 1051-1052`), which prevents the client from disabling a
daemon-mandated charset.

### Post-parse validation

After all options are parsed (`options.c:2051-2062`):

- If `--iconv` is `-` on the client side, iconv is disabled (`iconv_opt = NULL`)
- Otherwise `need_unsorted_flist = 1` is set, because charset conversion can
  change the sort order of filenames

### Divergence risks for oc-rsync

- Must support the `LOCAL,REMOTE` comma-separated format
- Must support `.` for locale detection and `-` for explicit disable
- Must handle `RSYNC_ICONV` environment variable with the same precedence rules
- Must set `need_unsorted_flist` when iconv is active (file list must maintain
  both sorted and unsorted views)
- Must implement the refuse logic for daemon modules without a `charset` parameter

## 2. iconv Descriptor Initialization (rsync.c)

### Three iconv descriptors

Upstream maintains three `iconv_t` handles (`rsync.c:71-73`):

| Handle | Direction | Purpose |
|--------|-----------|---------|
| `ic_chck` | locale-to-locale | Validates displayable characters in log output |
| `ic_send` | local charset -> UTF-8 | Converts filenames before sending on the wire |
| `ic_recv` | UTF-8 -> local charset | Converts filenames received from the wire |

### setup_iconv() (rsync.c:87-147)

Called at multiple points:

1. Server startup (`main.c:1820`) - sets up `ic_chck` only (no `iconv_opt` on server until client sends it)
2. After spawning child/SSH (`main.c:638, 648, 653`) - sets up all three
3. Client daemon connection (`clientserver.c:142`) - sets up all three
4. Daemon module selection (`clientserver.c:712-717`) - temporary setup using module's `charset`, then resets `iconv_opt` to `NULL`

The comma-separated `LOCAL,REMOTE` format is parsed here:

- On the server side (`am_server`): use the part after the comma (the remote charset)
- On the client side: use the part before the comma (the local charset), truncating at the comma

If the charset is empty or `.`, the system's default locale charset is used via
`default_charset()` which tries `locale_charset()`, then `nl_langinfo(CODESET)`,
then falls back to `""`.

Both `iconv_open()` calls use `UTF8_CHARSET` (defined as `"UTF-8"`) as one
argument. This means the wire format is always UTF-8 - iconv converts between
the local charset and UTF-8.

Reference: `rsync.c:87-147`, `configure.ac:999`.

### Divergence risks for oc-rsync

- The wire protocol always uses UTF-8 as the interchange charset
- Must replicate the `am_server` logic that selects the post-comma charset
- Locale detection (`nl_langinfo`, `locale_charset`) needs a Rust equivalent -
  likely the `encoding_rs` or `locale_config` crate, or platform-specific calls
- The `ic_chck` handle is separate from `--iconv` - it is used even without
  `--iconv` for log output sanitization (replacing non-printable bytes with
  `\#NNN` octal escapes)

## 3. Charset Conversion in the Transfer Pipeline

### 3.1 File list sending (flist.c:1579-1624)

When `ic_send != (iconv_t)-1`, `send_file_entry()` converts:

1. **Directory name** (`file->dirname`) - converted via `iconvbufs(ic_send, ...)` with a 2-byte reserved margin for `/` separator
2. **Basename** (`file->basename`) - converted separately and appended after the dirname
3. **Symlink target** (if `sender_symlink_iconv` is set) - converted into a separate `symlink_buf[MAXPATHLEN]`

The converted filename is placed in `fbuf[MAXPATHLEN]` and sent over the wire in
UTF-8.

On conversion error, the function sets `io_error |= IOERR_GENERAL`, logs an error
via `FERROR_XFER`, and returns `NULL` (skipping the file).

Reference: `flist.c:1579-1624`.

### 3.2 File list receiving (flist.c:738-754)

When `ic_recv != (iconv_t)-1`, `recv_file_entry()` converts the received filename
from UTF-8 back to the local charset:

1. The raw wire name is read into `lastname`
2. `iconvbufs(ic_recv, ...)` converts it into `thisname`
3. On error: sets `io_error |= IOERR_GENERAL`, logs via `FERROR_UTF8`, and sets
   the output length to 0 (producing an empty filename)

Reference: `flist.c:738-754`.

### 3.3 Symlink targets on receive (flist.c:1127-1152)

When `sender_symlink_iconv` is set, the receiver:

1. Allocates a double-sized buffer for the symlink data
2. Reads the raw wire data into the second half of the buffer
3. Converts it in-place via `iconvbufs(ic_recv, ...)`
4. On error: sets `io_error |= IOERR_GENERAL`, logs via `FERROR_XFER`, and
   zeroes the symlink data

The double-size buffer is allocated because conversion to a multi-byte local
charset could expand the data. Reference: `flist.c:935-941, 1127-1152`.

### 3.4 Protected args (rsync.c:283-320)

When `protect_args` (`-s`) is active and iconv is enabled, all command-line
arguments sent to the remote side are converted through `ic_send` with flags
`ICB_EXPAND_OUT | ICB_INCLUDE_BAD | ICB_INCLUDE_INCOMPLETE | ICB_INIT`. This
means bad bytes are passed through verbatim rather than causing an error.

Reference: `rsync.c:283-320`.

### 3.5 Protected args receiving (io.c:1292-1302)

On the server side, `read_args()` adds `RL_CONVERT` to the read-line flags when
`protect_args` is active and `ic_recv` is valid. The `read_line()` function then
reads into a temporary `iconv_buf`, converts via `iconvbufs(ic_recv, ...)` with
`ICB_INCLUDE_BAD | ICB_INCLUDE_INCOMPLETE | ICB_INIT`, and writes the result to
the caller's buffer.

Reference: `io.c:1240-1302`.

### 3.6 MSG_DELETED messages (io.c:1558-1591)

Delete messages received over the multiplex channel are converted using `ic_send`
(local-to-UTF8) when `ic_recv` is valid. This is used when the receiver is
processing `MSG_DELETED` frames from the generator - the generator uses UTF-8
wire-format names, and the receiver converts them back via `ic_send` for display
logging.

Note: the guard condition checks `ic_recv != (iconv_t)-1` but the actual
conversion uses `ic_send`. This appears intentional - the MSG_DELETED names
arrive in UTF-8 from the wire and need to be converted to local charset for
display. However, `ic_send` converts local-to-UTF8, which seems inverted. The
likely explanation is that MSG_DELETED messages travel from the generator (which
may be remote) through the receiver (local), and the receiver needs to convert
the UTF-8 wire name to local charset for `log_delete()`. Using `ic_send` here
may be a naming confusion in the upstream code, or there may be a subtlety in
the generator/receiver process relationship that makes this correct.

Reference: `io.c:1558-1591`.

### 3.7 send_msg() with conversion (io.c:983-1030)

The `send_msg()` function accepts a `convert` parameter. When `convert > 0` and
`ic_send` is valid, it allocates double-size room in the message buffer and
converts through `ic_send` with `ICB_INCLUDE_BAD | ICB_INCLUDE_INCOMPLETE |
ICB_CIRCULAR_OUT | ICB_INIT`.

Reference: `io.c:983-1030`.

### Divergence risks for oc-rsync

- Filename conversion must happen at the send/receive boundary of the file list,
  not in the protocol framing layer
- Symlink target conversion requires a double-size buffer allocation strategy
- Protected args conversion includes bad/incomplete bytes rather than failing -
  this is a different error policy than file list conversion
- The MSG_DELETED iconv direction needs careful analysis to avoid wire format
  divergence

## 4. Invalid Byte Sequence Handling

### iconvbufs() error policy (rsync.c:149-280)

The `iconvbufs()` function is the central conversion routine. Its behavior with
invalid bytes depends on the caller's flags:

| Flag | Effect |
|------|--------|
| `ICB_INCLUDE_BAD` | Invalid byte sequences (EILSEQ) are copied verbatim to output |
| `ICB_INCLUDE_INCOMPLETE` | Incomplete multi-byte sequences at buffer end are copied verbatim |
| `ICB_EXPAND_OUT` | Output buffer grows dynamically if too small |
| `ICB_CIRCULAR_OUT` | Output buffer wraps around (for circular I/O buffers) |
| `ICB_INIT` | Reset iconv state before processing |

When `ICB_INCLUDE_BAD` is set and an EILSEQ error occurs, the invalid byte is
copied as-is to the output (`*obuf++ = *ibuf++` at `rsync.c:261`). This means
invalid characters are preserved byte-for-byte rather than being replaced with
`?` or stripped.

When `ICB_INCLUDE_BAD` is NOT set (as in file list send/receive), the function
returns -1 with `errno = EILSEQ`, and the caller decides what to do:

- **send_file_entry**: logs error, sets `io_error`, returns `NULL` (file skipped)
- **recv_file_entry** (filename): logs error, sets `io_error`, output length set to 0
- **recv_file_entry** (symlink): logs error, sets `io_error`, symlink data zeroed

### Display output sanitization (log.c:362-401)

The `rwrite()` function uses `ic_chck` (or `ic_recv` for UTF-8 messages) to
validate log output. When conversion encounters EILSEQ or EINVAL, the invalid
byte is displayed as `\#NNN` (octal escape, `log.c:386`). This is the `ic_chck`
path - separate from `--iconv` but related.

### Summary of error behaviors by call site

| Call site | Flags | Bad byte behavior |
|-----------|-------|-------------------|
| send_file_entry (filename) | ICB_INIT | Error, file skipped |
| recv_file_entry (filename) | ICB_INIT | Error, empty filename |
| recv_file_entry (symlink) | ICB_INIT | Error, symlink zeroed |
| send_protected_args | ICB_INCLUDE_BAD, ICB_INCLUDE_INCOMPLETE, ICB_EXPAND_OUT, ICB_INIT | Bad bytes passed through |
| read_line (RL_CONVERT) | ICB_INCLUDE_BAD, ICB_INCLUDE_INCOMPLETE, ICB_INIT | Bad bytes passed through |
| send_msg (convert) | ICB_INCLUDE_BAD, ICB_INCLUDE_INCOMPLETE, ICB_CIRCULAR_OUT, ICB_INIT | Bad bytes passed through |
| files-from I/O | ICB_INCLUDE_BAD, ICB_INCLUDE_INCOMPLETE, ICB_CIRCULAR_OUT | Bad bytes passed through |
| log display (ic_chck) | none | EILSEQ/EINVAL -> `\#NNN` octal escape |

### Divergence risks for oc-rsync

- Must implement both "fail on bad byte" and "pass through bad byte" policies
  depending on call site
- The `\#NNN` octal escape format for log output must match upstream exactly
- File list conversion failures must set `io_error` flags, not abort the transfer
- Protected args and files-from must be permissive (include bad bytes)

## 5. Filter Rules

### No iconv conversion on filter patterns

The `exclude.c` file contains zero references to iconv, ic_send, ic_recv, or
iconvbufs. Filter rule patterns are NOT converted through iconv.

This means filter patterns (`--filter`, `--exclude`, `--include`, `.rsync-filter`
files) are matched against filenames in whatever charset they were originally
specified in. On the sending side, patterns are matched before conversion. On
the receiving side, patterns are matched after conversion.

### Divergence risks for oc-rsync

- Filter patterns must NOT be charset-converted
- Pattern matching operates on the native charset representation at each side
- This creates a subtle asymmetry: a sender-side exclude pattern in Shift-JIS
  matches Shift-JIS filenames before they are converted to UTF-8 for the wire,
  while a receiver-side exclude pattern would need to match the already-converted
  local-charset filename
- For daemon mode with a `charset` setting, the daemon sees filenames in its own
  charset but filter rules from the config file are in whatever encoding the
  config file was written in

## 6. Server Args Forwarding (options.c:2715-2724)

### Format sent to remote

The client sends `--iconv` to the server via `server_options()`. The logic
(`options.c:2716-2723`):

1. If `iconv_opt` contains a comma, send only the part after the comma (the remote charset)
2. If no comma, send the entire `iconv_opt` value
3. Sent via `safe_arg("--iconv", set)` which handles shell-safe quoting

This means the server always receives a single charset name, never the
`LOCAL,REMOTE` format. The server uses this as its local charset for conversion
to/from UTF-8.

### Divergence risks for oc-rsync

- When acting as client: must split `LOCAL,REMOTE` and send only the remote part
- When acting as server: must accept a single charset name
- The `safe_arg()` quoting must be replicated for shell-safe argument passing

## 7. Compat Flags and Symlink Iconv Negotiation (compat.c)

### CF_SYMLINK_ICONV flag

Protocol version >= 30 negotiates symlink iconv support via the `compat_flags`
byte (`compat.c:117-119`):

- `CF_SYMLINK_ICONV (1<<2)` - advertised by server when `ICONV_OPTION` is compiled in
- The client reads this flag from the compat_flags response

### sender_symlink_iconv

Set in `compat.c:764-767`:

```c
sender_symlink_iconv = iconv_opt && (am_server
    ? strchr(client_info, 's') != NULL
    : !!(compat_flags & CF_SYMLINK_ICONV));
```

The `'s'` in the client_info capability string indicates the client supports
symlink iconv. The flag controls whether symlink targets are charset-converted
in addition to filenames.

### files-from conversion (compat.c:799-806)

When `protect_args` is active and `files_from` is set:

- Sender side: `filesfrom_convert = filesfrom_host && ic_send != (iconv_t)-1`
- Receiver side: `filesfrom_convert = !filesfrom_host && ic_recv != (iconv_t)-1`

This controls whether `--files-from` entries are charset-converted during I/O.

### Divergence risks for oc-rsync

- Must advertise `CF_SYMLINK_ICONV` in compat flags when iconv is compiled in
- Must check for `'s'` in client capability string on the server side
- Must implement `sender_symlink_iconv` and `filesfrom_convert` flags
- Symlink iconv is only relevant for protocol >= 30

## 8. Daemon Mode (clientserver.c)

### Module charset parameter

Daemon modules can set a `charset` parameter in `rsyncd.conf`
(`daemon-parm.txt:19`). When a client connects to a module:

1. `lp_charset(i)` retrieves the module's charset (`clientserver.c:713`)
2. If non-empty, `setup_iconv()` is called to set up `ic_send` and `ic_recv`
   using the module's charset
3. `iconv_opt` is immediately reset to `NULL` (`clientserver.c:716`)

This means the daemon temporarily opens iconv descriptors for the module listing
phase (converting module names/comments), then closes them.

### Post-option-parse cleanup (clientserver.c:1174-1185)

After the client's options are parsed on the server side, if `iconv_opt` is
`NULL` (client did not send `--iconv`), any previously opened iconv descriptors
are closed. This handles the case where the daemon set up iconv for module
listing but the client is not using `--iconv`.

### Refuse interaction

If a daemon module has no `charset` set, `--iconv` is added to the refuse list
(`options.c:994-996`). This means a client cannot use `--iconv` with a module
that has not configured a charset.

### One-sided iconv

If only the daemon side has `charset` configured but the client does not send
`--iconv`, the iconv descriptors are closed after option parsing
(`clientserver.c:1174-1185`). No conversion occurs. The `charset` parameter alone
does not force conversion - the client must also request it.

If the client sends `--iconv` but the daemon module has no `charset`, the option
is refused. Both sides must agree for conversion to happen.

### Divergence risks for oc-rsync

- Daemon config must support the `charset` module parameter
- Module listing may use charset conversion for module names/comments
- Refuse logic must tie `--iconv` availability to the `charset` parameter
- Iconv descriptors must be cleaned up when the client does not request conversion
- Both sides must cooperate - there is no unilateral conversion mode

## 9. Files-From with iconv (io.c:393-454)

### Sending side

When `filesfrom_convert` is set, the files-from data read from the local file
is converted through `ic_send` before being sent to the remote side. The
conversion processes each null-terminated string separately
(`io.c:417-452`):

1. Each null-terminated filename is extracted from the input buffer
2. Converted via `iconvbufs(ic_send, ..., ICB_INCLUDE_BAD | ICB_INCLUDE_INCOMPLETE | ICB_CIRCULAR_OUT)`
3. The null terminator is sent separately after conversion
4. Partial strings at buffer boundaries are handled by saving incomplete
   multi-byte chars and continuing on the next read

### Receiving side

On the receiving side, `recv_file_list()` sets `RL_CONVERT` in the read-line
flags when `filesfrom_convert` is set (`flist.c:2207-2209`). This causes
`read_line()` to convert each filename through `ic_recv` as it is read.

### Divergence risks for oc-rsync

- Files-from conversion must handle streaming data with buffer boundaries
- Incomplete multi-byte sequences at buffer boundaries must be preserved across
  reads
- The null-terminated string protocol must be preserved exactly

## 10. Unsorted File List Requirement

### Why iconv requires unsorted flist

When iconv is active and `protect_args` is not set to 2, `need_unsorted_flist`
is set to 1 (`options.c:2056`). This is because charset conversion can change
the lexicographic sort order of filenames. The sender sorts filenames in its
local charset, but the receiver sorts in its local charset (which may differ).
If the receiver re-sorted the converted names, index numbers would not match
between sender and receiver.

The solution is to maintain two views of the file list:

1. `flist->files` - unsorted, preserving the sender's index order
2. `flist->sorted` - sorted copy for local operations (generator, flist_find)

This is allocated in `flist.c:2149-2153` (sender side) and `flist.c:2696-2703`
(receiver side) when `need_unsorted_flist` is true.

### Divergence risks for oc-rsync

- Must maintain dual file list views (sorted + unsorted) when iconv is active
- Index numbers exchanged between sender and receiver must use the unsorted
  (original sender order) indices
- Local operations (finding files, generating transfers) use the sorted view

## 11. Summary of Conversion Points

| Location | Direction | When | Error Policy |
|----------|-----------|------|-------------|
| send_file_entry (dirname + basename) | local -> UTF-8 via `ic_send` | Always when iconv active | Fail, skip file |
| send_file_entry (symlink target) | local -> UTF-8 via `ic_send` | When `sender_symlink_iconv` | Fail, skip file |
| recv_file_entry (filename) | UTF-8 -> local via `ic_recv` | Always when iconv active | Fail, empty name |
| recv_file_entry (symlink target) | UTF-8 -> local via `ic_recv` | When `sender_symlink_iconv` | Fail, zero symlink |
| send_protected_args | local -> UTF-8 via `ic_send` | When protect_args + iconv | Permissive |
| read_args (server) | UTF-8 -> local via `ic_recv` | When protect_args + iconv | Permissive |
| files-from (send) | local -> UTF-8 via `ic_send` | When `filesfrom_convert` | Permissive |
| files-from (recv) | UTF-8 -> local via `ic_recv` | When `filesfrom_convert` | Permissive |
| MSG_DELETED | via `ic_send` | When `ic_recv` valid | Permissive |
| send_msg (convert) | local -> UTF-8 via `ic_send` | When convert > 0 | Permissive |
| log output display | via `ic_chck` or `ic_recv` | Always (even without --iconv) | `\#NNN` escapes |

## 12. oc-rsync Implementation Considerations

### Required Rust crate support

- `encoding_rs` - charset conversion (covers all IANA charsets, used by Firefox)
- Alternatively: FFI to system `iconv` via the `iconv` crate for exact compatibility

### Key design decisions

1. **iconv vs encoding_rs**: System `iconv` has broader charset coverage but
   requires FFI. `encoding_rs` is pure Rust but may not support all charsets
   that system iconv does (e.g., obscure legacy encodings). For wire
   compatibility, exact charset-name-to-encoding mapping must match upstream's
   `iconv_open()` behavior.

2. **Error handling**: Must implement both strict (fail on bad byte) and
   permissive (pass through bad byte) policies, matching the per-call-site
   behavior documented above.

3. **Buffer management**: The `xbuf` abstraction with circular buffer support,
   expansion, and in-place conversion needs a Rust equivalent. The double-size
   buffer trick for symlink targets must be preserved.

4. **ic_chck for log sanitization**: This is independent of `--iconv` and should
   be implemented separately. It uses locale-to-locale conversion as a validation
   pass, with non-convertible bytes displayed as `\#NNN` octal escapes.

5. **Unsorted file list**: When iconv is active, the existing `FileList` must
   support dual views (sorted for local use, unsorted for wire index exchange).

### Wire format invariants

- The wire always carries UTF-8 encoded filenames when iconv is active
- Filter patterns are never converted
- The `CF_SYMLINK_ICONV` compat flag must be negotiated for symlink target
  conversion
- Daemon `charset` parameter controls per-module iconv availability
