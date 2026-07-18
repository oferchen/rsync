# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.4.4 (protocol 32). Release
tags are mirrored on GitHub at <https://github.com/oferchen/rsync/releases>.

## [0.6.4] - 2026-07-18

This release rolls up every change merged since v0.6.3 (roughly 1,200 pull
requests). The entries below are grouped by area and highlight the notable,
user-facing work; consult the linked PRs for full detail.

### Added

**Daemon**
- Support `@netgroup` tokens and forward-resolve hostnames in `hosts allow`/`hosts deny` (#6640, #6595)
- Add per-module syslog tag and facility parameters (#6637)
- Wire per-connection verbosity into daemon log filtering (#6081)
- Replicate the listener across `N` acceptor threads via `SO_REUSEPORT` (#6166)
- Add a macOS `kqueue` accept engine with blocking fallback (#6217)
- Harden worker startup: drop unnecessary capabilities, set `PR_SET_NO_NEW_PRIVS`, log active LSMs, and add an opt-in seccomp BPF syscall filter

**Transfer & SSH**
- Honor `--timeout` with SSH stall detection (#6636)
- Compile the embedded SSH (`russh`) transport in by default (#6271)
- Implement `--copy-dirlinks` and correct `--keep-dirlinks` receiver behavior (#6499)
- Apply alt-dest handling to symlinks, devices, and specials (#6652)
- Copy to backup under `--inplace` to preserve the destination inode, with a basis-offset guard in the delta generator (#6630, #6629)
- Preallocate destination files on the receiver (#6620)
- Support `--only-write-batch` on the server receiver (#6535)
- Match upstream `--partial` temp naming and finalize partial files on interrupt (#6411)
- Deduplicate `INC_RECURSE` sub-lists and the received file list, with path-belongs validation (#6632, #6631)
- Perform implied recursive `--list-only` listing for a daemon source (#6160)
- Materialize symlinks on the Windows network receiver (#6489)
- Add `-vvv` receiver/flist trace messages and a `-vv` delta-transmission status line matching upstream (#6353, #6110)
- Add an experimental, default-off async receiver/sender pipeline (`tokio-transfer`) (#6460)

**Delta & matching**
- Emit `FNAMECMP_FUZZY` and basis xname for `--fuzzy` transfers (#6692)
- Add an opt-in parallel delta scan for large basis files (#6486)
- Wire `DeltaApplicator` into the receiver apply path with compressed-token and sparse-size support (#6212, #6211)
- Enable adaptive work-queue depth by default in the delta pipeline, with an opt-in AIMD grow/shrink controller (#6391, #6348)

**Compression**
- Thread signed compression levels so negative `zstd` levels reach the encoder (#6654)
- Advertise the `lz4` codec after wire-format validation (#6503)
- Honor `RSYNC_CHECKSUM_LIST`/`RSYNC_COMPRESS_LIST` and refuse env-excluded `--checksum`/`--compress-choice` (#6590, #6599)

**Metadata (ACL/xattr/ownership)**
- Preserve creation time on Windows via `SetFileTime` (#6157)
- Classify NTFS reparse points (symlink/junction/mount-point/cloud) and parse their target names into `FileEntry` generation
- Fall back to junctions for unprivileged Windows directory symlinks (#6484)
- Audit-trail unmappable SIDs on Windows DACL apply (#6466)
- Honor `--max-alloc` for xattr datum length (#6680)
- Add Windows long-path support via `to_extended_path` across ACL/xattr and reparse FFI boundaries

**Filters & deletion**
- Wire `MSG_DELETED` for server-side `--delete` output (#6622)
- Emit upstream non-empty-dir and IO-error skip notices during deletion (#6362)

**I/O (fast_io / io_uring)**
- Reflink basis ranges via `FICLONE`/`FICLONERANGE` in the local-copy executor and delta-apply COPY tokens, gated on same-fs (#5836, #5824, #6237, #6101)
- Take a `clonefile` fast path for plain `-a` on macOS, stripping cloned xattrs (#6099)
- Add runtime CoW filesystem detection on Linux (#5832)
- Create NTFS sparse files via `FSCTL_SET_SPARSE` (#6236)
- Dispatch Windows file reads through IOCP with std fallback (#6475)
- Add an `RWF_DONTCACHE` uncached bulk file writer and wire it into the receiver (#6148, #6149)
- Add TCP Fast Open on client connect and daemon accept, plus `TCP_NOTSENT_LOWAT`, `TCP_QUICKACK`, congestion/cork/`SO_REUSEPORT`, and `SO_BUSY_POLL` socket tuning (#6151, #6142, #6124, #6141, #6216)
- Mirror `--bwlimit` as an `SO_MAX_PACING_RATE` kernel hint (#6128)
- Wire io_uring `SEND_ZC` into the daemon sender behind opt-in `--zero-copy` (#6349)
- Add an opt-in Windows Registered I/O socket path (#5821)
- Make the buffer-pool memory cap and block size runtime-configurable via env (#6548, #6509)
- Anchor sandbox operations on the full parent path via `RESOLVE_BENEATH` to close a TOCTOU window (#6343)

**CLI & options**
- Add `--reflink=auto|always|never` mirroring upstream (#5823)
- Add `--checksum-threads` to activate parallel signature hashing (#6227)
- Add `--io-uring=sqpoll-off` (#131) and `--lsm-status` with an EACCES audit hint (#128)
- Port upstream filename octal escaping for output (#6355)
- Render `--list-only` atime/crtime columns with `-U`/`--crtimes` (#6162)
- Produce meaningful local-copy `--stats` (file-list size in sent bytes, I/O acceleration report) (#6096)
- Honor negative `--modify-window` for nsec-exact mtime comparison (#6517)

**Protocol**
- Forward out-format `%o` and `--remove-sent-files` in server args (#6677)

**Other**
- Emit per-directory itemize rows on receiver creation (#6007)

### Fixed

**Daemon**
- Survive transient `accept(2)` errors (`ECONNABORTED`/`EMFILE`) under load instead of treating them as fatal (#6763)
- Enable `SO_KEEPALIVE` on accepted client sockets and align listener socket options with upstream (`SO_REUSEADDR` only by default, honor `--port 0`) (#6720, #6562)
- Hold `max-connections` slots with `fcntl` record locks and honor per-module reverse-lookup and lock-file directives (#6712, #6610)
- Resolve uid/gid names in `rsyncd.conf` (not just numeric) and emit `@ERROR: invalid uid` on resolution failure (#6675, #6691)
- Force numeric ids for an unset directive under `chroot`, and keep the uid/gid name-list on the wire for daemon-forced `numeric-ids` (#6699, #6468)
- Match upstream `rsyncd.conf` parsing: whitespace-insensitive param names, bool/int values, backslash continuation, `&include` scoping, and `path = /` modules (#6596, #6579, #6584, #5517)
- Apply previously-unhandled module directives (auth access-level, ignore-errors, numeric-ids, incoming/outgoing `chmod`) at transfer time (#6464, #6603)
- Enforce `refuse options` against client requests and expand the compress alias to all `-z` variants (#6270, #6529)
- Match upstream `secrets-file` handling: group-readable permission mask, `@group`/wildmatch auth-user matching, and reject strict-modes secrets with `@ERROR` (#6251, #6392, #6409)
- Refuse device options by default and reject out-of-module `--link-dest`/`--copy-dest` paths, confining alt-basis paths to the module root (#6463, #5778, #5540)
- Do not send `@RSYNCD: EXIT` after `@ERROR`; echo the `@ERROR` line verbatim before the structured error and emit MOTD with the correct trailing newline (#6381, #6372)
- Drain to peer EOF after half-close to avoid an abortive RST mid-download, and retry `EINTR`/`EWOULDBLOCK` in the teardown goodbye drain (#6556, #6564, #5718)
- Run `post-xfer exec` on refused transfers and scope `RSYNC_ARG*`/`RSYNC_REQUEST`/`RSYNC_RAW_STATUS` to match upstream (#6482, #6481, #6476)
- List all daemon modules regardless of host access, and forward-confirm reverse DNS for `hosts allow`/`deny` (#6616, #6339)
- Surface Landlock best-effort downgrade instead of hiding it, and widen the Landlock allowlist to validated `ref_dirs`/`temp_dir`/`partial_dir` (#6686, #6600)
- Emit deletions and `NDX_DEL_STATS` on daemon-receive uploads even without `--stats` (#6588, #6543)
- Default listener behavior corrected to upstream IPv4-only with per-family bind fallback (#5908, #5885, #5875)

**Transfer & receiver**
- Do not delete an `--inplace` destination on mid-transfer abort, preventing data loss (#6340)
- Verify the whole-file checksum before committing on the receiver (#6626)
- Defer `--remove-source-files` unlink until `MSG_SUCCESS`, and guard the sender-side unlink (#6668, #6580)
- Defer `--delete-delay`/`--delete-after` unlink until after transfer so per-directory filters protect at delete time (#6618, #6519)
- Back up existing specials/symlinks before the receiver replaces them, and create fifo/device specials on protocol receive (#6614, #6469)
- Dirfd-anchor receiver commit/backup rename and anchor Windows temp-create + rename against reparse-point TOCTOU (#6336, #6688)
- Drain the delta stream when a pipelined receiver temp-create fails (exit 23, no desync) (#6249, #6253)
- Resolve sent uid/gid names to local ids for file ownership, and honor `--numeric-ids` in name matching (#6500, #6296)
- Match upstream sparse hole granularity and `sparse_end` for network sparse writes (#6501, #6442)
- Gate receiver dest-path creation on `--mkpath` and auto-create the destination root only for multi-file transfers (#6257)
- `--update` transfers when the destination type differs from the source, and honors `--modify-window` in the quick-check (#6255, #6252)
- Full-content resend on the `--append` redo pass; verify the append-prefix checksum for protocol < 30 and skip a source shrunk below flist length (#6662, #6497, #6589)
- Validate received flist names against implied includes and re-filter them on the receiver (#6627, #6624)
- Fail-closed `INC_RECURSE` sub-list `dir_ndx` validation, and honor negotiated `CF_INC_RECURSE` on the receiver (#6619, #6495)
- Report the real "Total transferred file size" and reconstruct the `created_*` `--stats` breakdown on remote transfers (#6723, #6687, #6681)
- Surface delay-updates rename failure instead of silently skipping, and link hardlink followers after the delayed-updates rename (#6728, #6645)
- Handle receiver data-discard without panicking, and turn a buffered `map_file` out-of-range read into an `Err` (#6272, #6586)
- Grant transient `u+rwx` to read-only dirs during transfer and retry inplace open without `O_CREAT` on `EACCES` (#6494, #6104)
- Report signal aborts as `RERR_SIGNAL`, not a per-file partial (#6413)
- Protect mount points in the `--one-file-system` delete pass and scope the receiver root delete to content dirs (#6571, #6527)
- Use a partial-dir file as the delta basis on resume (`FNAMECMP_PARTIAL_DIR`) (#6506)
- Implement `--copy-devices` in the protocol sender and gate device server-args on sender direction (#6473, #6467)

**SSH transport**
- Thread `--timeout` into the SSH stall watchdog end-to-end and connect-timeout via `--contimeout` (expiry exit 35) (#6649, #6704)
- Auto-enable `blocking_io` for `rsh`/`remsh` remote shells and forward `--ipv4`/`--ipv6` to the ssh child as `-4`/`-6` (#6724, #6715)
- Surface async-runtime death on the sync bridge instead of hanging (#6278)
- Keep server stdio blocking and retry `WouldBlock` in the write loop; half-close/drain stdout in both server roles to break shutdown deadlocks (#5733, #5781, #5792)

**Delta & matching**
- Enforce inplace basis offset-monotonicity in the matcher and seek past in-place matched blocks at the same offset (#6625, #6603)
- Avoid read-after-write basis corruption with `--inplace` + delta and mirror upstream inplace matched-block copy for re-ordered content (#5889, #5862)
- Produce wire-identical parallel delta via spatial-split overlap merge and reset the consumed bitset before a chunked parallel scan (#6546, #6187)
- Match the trailing partial block in local-copy delta (#6333)
- Port upstream `fuzzy_distance` and compare fuzzy candidate names by raw bytes for `--fuzzy` basis selection (#6439, #6646)
- Select the best-match reference basis by `match_level`; copy a `link-dest` match-level-2 basis instead of hard-linking (#6441, #6401)
- Compute per-file flist checksums on the sender under `--checksum`, and use `xxh128` for local `--checksum` to match negotiation (#6520, #6415)

**Filters & exclude/delete**
- Per-dir `!` clears inherited ancestor merge rules, and scope `!` clears to the local merge context (#6701, #5905)
- Inherit ancestor per-dir-merge rules into subdirs for delete-protection timing, and gate dir-exclude descendants by ancestor first-match (#6513, #6559)
- Evaluate CLI filter rules in true command-line order, and protect excluded-dir children on the delete pass (#6405, #6034)
- First-match-wins for protect/risk rules, and honor receiver-side, exclude, and perishable rules in `--delete` (#6274, #6414)
- Isolate destination-deletion merge load from source filters and drop excluded entries from the keep-set when deletable (#6066, #6064)
- Port upstream `wildmatch dowild`, eliminating `**` divergences and normalizing bare interior `**` (#6079, #5751)
- Abort on a perishable rule sent to a proto<30 peer, and gate the `:C` CVS modifier per protocol (#6726, #6718)
- CVS handling: keep `-C` CVS rules local on the receiver, emit only `C` on the wire, and apply `no-inherit` per upstream (#6718, #6428, #5869)
- Case-sensitive long-form filter directive keywords and correct rule-separator/whitespace handling (#6588, #6576, #6448)
- Match upstream unknown-rule error text and exit code, and reject the `e` modifier on non-merge rules (#6352, #6292)
- Order the local delete plan by upstream traversal order, not a byte sort (#6446)
- Auto-exclude `--partial-dir` from transfer and deletion (#6505)
- Carry dir-merge `:s`/`:r` side onto the wire for delete-pass parity and inherit parent side flags in nested merges (#6075, #6065)

**Metadata (perms/ACL/xattr/ownership/times)**
- Restore setuid/setgid/sticky bits after applying ACLs and after `chown` (#6721, #6581)
- Preserve the transmitted ACL mask on the receiver (no narrowing to a named entry) and remap ACL user/group ids via id-list for cross-host `-A` (#6493, #6346)
- Resolve ACL named-entry id/name instead of dropping to root, and inherit the default ACL when computing destination file mode (#6127, #5841)
- Non-root `-X` sender transmits `security.*` xattrs; filter received xattrs on apply via `xattr_name_allowed` and drop non-user xattrs on a non-root receiver (#6722, #6682, #6591)
- Number wire xattrs ascending to match the receiver, and unseed the xattr-abbreviation checksum to match upstream (#6698, #6375)
- `--usermap`/`--groupmap` must match the sender-transmitted name; mirror upstream numeric-range parsing and warn-and-continue on unknown targets (#6696, #6574, #6344)
- Apply dir perms/setgid without `-p`, omit atime unless requested, and reject `--chmod` copy-syntax clauses like upstream (#6694, #6561)
- Match upstream `chmod.c` parse/apply semantics exactly, including permission-copy specs (#6373, #6265)
- Skip a `crtime` set when unchanged, tolerate the HFS+ root, and tolerate `ENXIO`/`EROFS`/`EOPNOTSUPP` when setting times on special files (#6725, #6113)
- Symmetric `--modify-window` mtime comparison matching upstream `same_time` (#6247)
- Read macOS resource forks past the 64 MiB `getxattr` ceiling (#6433)
- Correct `--fake-super` `%stat` xattr encode/decode and treat `ENODATA`/`ENOATTR` as success when removing fake-super metadata (#6268, #6487)
- Gate non-root `chown` by privilege to match upstream (#6067)
- Chmod through a dirfd to block a parent-symlink escape (#5732)

**Compression**
- `-z --skip-compress` keeps codec framing, and use the upstream `DEFAULT_DONT_COMPRESS` skip-compress suffix list (#6697, #6285)
- Stream compressed-token inflate to match upstream, fixing explicit-choice vstring desync and an all-literal pipeline deadlock (#6657, #6471)
- Clamp `--compress-level` to the codec range, allowing negative zstd levels down to `ZSTD_minCLevel` (#6403, #6648)
- Apply the negotiated compress level to the token encoder and honor daemon `dont-compress '*'` whole-stream store (#6578, #6602)

**Protocol & wire**
- Map bounded wire-read overruns and protocol violations to `RERR_PROTOCOL` (exit 2), with correct RERR codes for xattr/acl/nsec/multiplex overruns (#6633, #6594, #6635)
- Reject out-of-range ACL access bits, out-of-range hardlink reference index, and flist entries with invalid mode-type bits (#6661, #6644, #6575)
- Tombstone flist duplicates to keep `NDX` aligned, and gate `XMIT_*_NAME_FOLLOWS` on `inc_recurse` (#6670, #6498)
- Match upstream ACL/xattr wire caps and cap del-stat; gate xattr name-abbreviation encoding on protocol version (#6669, #6496)
- Preserve invalid bytes verbatim in iconv include-bad and honor negotiated symlink-target iconv gating with strict-failure semantics (#6674, #6641)
- Reject a 256-byte negotiation vstring and decode the sender xname as a vstring (not a varint) (#6299, #6298)
- Legacy rdev-major reset for proto 28-30 specials and legacy longint end-of-run stats for protocol < 30 (#6388, #6438)
- Proto-29 sender checksum seed and hardlink flist encoding, plus MD4 whole-file seed gated on protocol < 30 (#6434, #6700)
- Honor `--checksum-choice` during binary negotiation and clamp `s2length` to the negotiated digest width (#6421, #6660)
- Checked arithmetic on wire-derived indices, and guard `read_varint`/flist-name decode against integer/length overflow (#6085, #5874, #5764)
- Send `--delete` instead of `--delete-during` for bare delete mode (#6358)
- Add defense-in-depth wire bounds for flist names, ID lists, and timestamps, and bound the compressed-token decoder counters against overflow (#5511, #5509)

**CLI & options**
- Options after operands (popt order parity) and lone `-h` help / `--old`/`--secluded` conflict / `-a` flag ordering (#6474, #6729)
- Match upstream `--stats`/`--progress` output formatting, `progress2` TTY framing with 1s throttle, and thread human-readable mode into count fields (#6693, #6544, #6727)
- Honor upstream's 4 human-readable levels, `-hh` base-1024, and thousands-grouping of counts/rates/speedup (#6371, #6107, #6135, #6119)
- Transfer rate uses the wall-clock span, not summed per-file durations (#6123)
- Match upstream `parse_size_arg` for `--max-size`/`--min-size` and reject scientific notation in `--bwlimit` (#6389, #6404)
- `--max-delete` must not enable deletion, with option-validation parity (#6695)
- Forward the correct server-args over SSH: `-C`, `--skip-compress`, `-XX`/`-UU`, `--append`/`--append-verify`, `--debug=`, negation flags, and more `server_options()` long flags (#6667, #6492, #6587, #6524, #6528)
- Honor `--protocol=NUM` over ssh/remote-shell and align choice/protocol/empty-filter exit codes with upstream (#6514, #6526)
- Pass 8-bit filename bytes raw under `--8-bit-output`, and render listing/out-format dates in local time via `localtime_r` (#6384, #6130)
- Distinguish `%b`/`%c` transfer-byte direction and match upstream `%L` width, `%G` default, and skipping-directory output (#6293, #6363)
- `--progress` implies `--info=name` and silences per-file progress for up-to-date entries; normalize progress path separators to `/` on Windows (#6118, #6095, #6138)
- List symlinks and specials in `--list-only`, exit 0 for a successful local `--list-only`, and list a directory entry without a trailing slash (#6369, #6193, #6198)
- Reject invalid `--chmod` specs to match `parse_chmod`, accept copy-from-category and empty perm sets (#6256, #6408)
- Default recursive to false when neither `-r`, `-a`, nor `--files-from` is set, and only send `-W` when whole-file is explicitly requested (#5739)
- `--files-from` fixes: flatten under `--no-relative`, gate implied-dirs emission to protocol >= 30, and resolve a `localhost:` prefix as hybrid local-open + wire-forward (#6530, #6512, #5982)

**Batch**
- Honor `--checksum-seed` and enforce `MAX_BATCH_NAME_LEN`, and gate batch-file stats encoding on protocol version (#6717, #6453)
- Enforce iconv batch-flag match and honor `--from0` in the `.sh` wrapper; include pass-through options in `--write-batch .sh` (#6577, #6387)
- Never open a non-regular replay entry as a delta basis and preserve regular-file mode through symlink replay (#6031, #5881)
- Skip destination writes in `--only-write-batch` local-copy mode (#6598, #6027)

**Core / misc**
- Propagate raw child/remote exit codes like upstream and match upstream error role trailers (`[sender]`/`[generator]`) and text (#6378, #6341)
- Propagate the daemon rejection exit code instead of a fixed 23 (#6477)
- Match upstream `errno` suffix `(N)` in I/O error messages and align exit-code description strings with `rerr_names` (#6146, #6059)
- Validate `--temp-dir` exists before transferring, and exit 23 for a missing source while continuing the rest (#6523, #6132)
- Fold `--files-from` into the config `dirs()` resolver and honor `--delay-updates` on a daemon-pull receiver (#6710, #6647)
- Match upstream `server_options` arg forwarding on the SSH path and forward `--delete`/`--ignore-missing-args` to the daemon server (#6656, #6269)
- Stream the whole-file checksum via a read window rather than mmap (#6709)
- Unregister the temp path from the cleanup registry on guard drop to fix a leak (#6342)
- Validate sum-header wire fields to reject malformed input (DoS) and audit CVE-2026-43617 hostname ACL bypass with a regression test (#6338, #5508)
- Bound recursive copy depth to prevent stack overflow (#6048)
- Windows fast-copy: drain in-flight IOCP ops on mid-batch error (data-loss/UAF), honor `-X` and xattr filters, and correct the `COPY_FILE_NO_BUFFERING` flag value (#6331, #6325, #6121)

### Performance

- **Engine/delta**: small-transfer fast path for the delete pass (`DML-4`) (#6550) and incremental destination filter stack for the delete pass (#6213)
- **Engine/delta**: gate spill zstd on compressibility to cut round-trip CPU (#6537)
- **Engine/delta**: parallelize local-copy delta basis-signature generation (#6182) with bounded-memory parallel signature generation (#6176)
- **Engine/delta**: eliminate per-file copy-buffer churn in local copy (#6312)
- **Engine/delta**: dedupe redundant destination `statx` in `--checksum` local copy (#6424); cache parent device id to drop redundant per-file `statx` (#6416)
- **Transfer & SSH**: track sparse offset in a variable, one `lseek` per hole (#6665)
- **Transfer & SSH**: default `mmap` for large-basis signature reads, byte-transparent (#6347)
- **Transfer & SSH**: intern per-source base instead of per-file full path (#6427)
- **Transfer & SSH**: cork around mux flush burst to coalesce delta-stream segments (`NBUF-2`) (#6235)
- **Transfer & SSH**: opt-in parallel basis signature generation (#6177)
- **fast_io / io_uring**: `RWF_DONTCACHE` basis-window reads (`UNCACHE-5`) (#6164) with version-gated writer selection (#6154)
- **fast_io / io_uring**: shared same-device helper, gate whole-file `FICLONE` on `st_dev` (#6163); skip `FICLONE` on cross-filesystem local copies (#6152)
- **fast_io / io_uring**: gate partial-range `FICLONERANGE` on same filesystem (#6153)
- **fast_io / io_uring**: apply `FILE_FLAG_SEQUENTIAL_SCAN` to basis reads on Windows (#6156)
- **Matching**: drop discarded per-block copies in the gated delta scan (#6658)
- **Matching**: chunked parallel sender-scan delta generator (#6183)
- **Daemon**: honor max connections in the async accept-loop worker cap (#6540)
- **Daemon**: `kqueue` socket-readiness for the macOS daemon accept path, default-off (#6329)
- **Protocol**: stream zstd token literals per `CHUNK_SIZE` (#6592)
- **Memory/RSS**: return freed pages promptly via jemalloc to bound RSS at scale (#6313)
- **Memory/RSS**: memoize uid/gid name lookups during flist build (#6422)
- **Other**: `kqueue` `EVFILT_TIMER` for sub-ms bandwidth sleeps on macOS (#5818)

### Changed

- Removed the dormant async-pipeline and ack-batcher from the transfer path (#6676)
- Removed non-upstream in-binary TLS: the client-tls scaffold and deps (#6301) and the `daemon-tls` native TLS feature (#6139)
- Unified `rsyncd.conf` parsing on a single path (#6672); consolidated daemon config parsing into submodules (#6207, #6203)
- Decomposed the receiver and transfer setup into submodules (#6210, #6206), with lazy on-demand flist-segment fetch in the receiver, no behavior change (#6479)
- Split the client remote `ssh_transfer` (#6209) and `disk_commit` (#6208) into submodules
- Split CLI frontend argument and filter-rule parsing into submodules (#6195, #6202)
- Split engine `local_copy` buffer pool (#6199) and `concurrent_delta` parallel-apply (#6196) into submodules; collapsed the transitional `SlotBarrier` adapter (#6430)
- Split `fast_io` `at_syscalls` per syscall (#6189) and retired the dead `send_zc` `from_shared_ring` constructor (`IUC-4`) (#6200)
- Introduced sans-io compressed-token decoding, byte-identical and async-driver ready (#6226), and split the wire `zstd_codec` into submodules (#6204)
- Added the `AcceptEngine` trait to abstract accept-loop polling (#6165) and encapsulated generator sort behind a `DualFileList` API (#5782)
- Removed the flat-flist dead-weight dual path (#6137)
- Consolidated default sources: skip-compress suffixes (#6383) and CVS-ignore patterns (#6374)

### Documentation

- Systematic rustdoc and comment-cleanup campaign across every workspace crate: `filters`, `compress`, `checksums`, `metadata`, `protocol`, `daemon`, `engine`, `transfer`, `fast_io`, `rsync_io`, `xtask`, plus a batch of smaller crates (#6731, #6732, #6737, #6741, #6760)
- Per-submodule rustdoc tidy for hot paths: `local_copy` executor, `concurrent_delta`, `delete/`, generator, receiver, `disk_commit`, `io_uring` (#6776, #6743, #6738, #6745, #6744)
- Dropped decorative dividers, banners, debug-narration and restatement comments from tests and root modules while preserving upstream-reference notes (#6759, #6756, #6753, #6386)
- Corrected stale doc claims against actual code: buffer-pool clamp and memory cap, temp-file naming, `walk` default crate (`jwalk`), `statx`/`io_uring` behaviour, `InvalidFnameCmpType` (#6765, #6764, #6750, #6757, #6752)
- Fixed daemon stale connection-limit and Windows gid docs, and CLI out-format token / filter / progress rustdoc (#6771, #6768, #6767)
- Repaired unresolved rustdoc intra-doc links breaking the Pages build across the workspace (#6671, #6558, #6420, #6418, #5872, #5616, #5621)
- Refreshed the tracked upstream reference to rsync 3.4.4 in prose, comparisons and benchmarks (#6779, #6770)
- Aligned status docs with shipped code: incremental-recursion default-on, ACL interop receiver gap resolved, Windows symlink support in README (#6402, #6547, #6508)
- Upstream-fidelity and security audits: `.unwrap()` panic surface in hot paths, bare-slice indexing on attacker inputs, per-dir `:C` merge-modifier parse gap, non-trailing-slash sub-path behaviour (#5580, #5708, #142)
- UTS root-cause synthesis and audit trail: exclude-lsh deep audit, files-from hang, reverse-daemon-delta varint overflow, goodbye-flush regression, cross-cutting UTS-X triage (#5977, #5976, #5975, #5974, #5546)
- Windows Tier-2 support-matrix disclosure and stub inventory reconciled against shipped-vs-design audits (#5595, #5584, #5999)
- I/O-acceleration design and platform-parity docs: reflink dispatch, `TCP_NOTSENT_LOWAT`, TCP Fast Open, RIO, kqueue, io_uring buffer-ring sizing, cross-platform acceleration matrix (#5997, #5996, #5993, #5964, #5965, #6170)
- Design and roadmap records for delete/exclude parallelism (DECIDE/EXECUTE seam), ReorderBuffer ring sizing, async receiver scoping, flat-flist flip decision (#6089, #6051, #5987, #6231, #5827)
- Removed daemon-tls and flat-flist design/audit docs and scrubbed native TLS references after feature removal (#6143, #6140)
- Packaging and operator guidance: AppArmor profile and SELinux policy templates for `oc-rsyncd`, landlock build guidance, `rust-landlock` as preferred sandboxing primitive (#5602, #6215, #5549)
- Environment and infra notes: GHA IPv6 dual-stack listener quirk, SQPOLL in rootless containers, `Cargo.lock` maintenance discipline (#5956, #5896, #5753)

### Testing

- Ported upstream testsuite edge cases to nextest in successive rounds, covering hardlinks INC_RECURSE, atimes/crtimes round-trip, delete-missing sentinels, chdir-symlink-race, compress-zlib-insert overflow (#6335, #6330, #6324, #5950, #5895)
- Added an upstream CLI-argument fidelity suite and pinned exclude-lsh six-leg sub-transfer and files-from dotdir-walk matrices (#6516, #5979, #5883)
- Interop-validated filter `protect`/`risk`/`hide`/`show` modifiers and `:C` bare-modifier wire bytes against upstream (#6273, #5980)
- Fuzz and differential coverage: seeded under-provisioned corpora, ancestor-directory exclusion in filter harnesses, `buffered_map` fuzz target plus UTS-18 regression corpus (#6429, #6023, #89)
- Property and bound tests: `bithash` false-positive bound and block-skip iteration count, zlib/zstd/lz4 decoder panic-freedom, zlib size monotonicity replacing a flaky speed check (#6488, #96, #6364)
- Determinism and stress hardening: reorder disk-reassembly under adversarial arrival, parallel-apply write/verify overlap, FICLONE concurrent same-fs clones, deterministic AIMD and adaptive-pool clocks (#6443, #6478, #6100, #6719, #6555)
- Deflaked and serialized global-state tests: buffer-pool singleton, `disk_commit` cleanup registry, reorder backpressure, Windows daemon-spawn negotiation (#6368, #6444, #6628, #6307)
- Platform-gated tests for cross-platform CI: Unix-only timestamp and POSIX-absolute reference cells, Windows-incompatible platform tests, SQPOLL integration gated to Linux (#5766, #5762, #5777, #5695)
- Windows metadata coverage: symlink/junction and ADS round-trips, reparse-point RAII fixtures, NTFS-path assertions (#94, #95, #85)
- Daemon and goodbye-phase regression coverage: daemon-gzip `-zz` goodbye flush, `path=/` with `use chroot=no`, goodbye timeout/disconnect, `NDX_DONE` contract (#6002, #5532, #5707, #93)
- Added shared test-support harnesses: `DirDiff` tree comparison, `OcRsyncCliRunner` + `LshRunnerStub`, self-skip prerequisite helpers (#6279, #6288, #6282)
- Security-focused coverage: hostname ACL resolution before chroot (GHSA-rjfm-3w2m-jf4f), `security.selinux` xattr round-trip, DirSandbox error contract (#5533, #98, #97)

### CI

- Made the upstream rsync testsuite a required check with reusable real+skip legs, and added root and non-root testsuite legs for the 3.4.4 gate (#6233, #6245)
- Added a standalone Upstream Testsuite workflow with README badge and surfaced per-test FAIL via annotations, summaries and XFAIL log detail (#6683, #6006, #5850, #6161)
- Added a one-shot validation workflow that re-runs a failing testsuite test against the upstream rsync binary (#5851)
- Pinned upstream rsync 3.4.4 across interop and remaining workflow matrices; promoted proto-29 RP28 legs to blocking (#5539, #5545, #6449)
- Introduced a fast PR gate against `--locked` removal from cargo invocations and shell helpers, auditing all workflow YAMLs (#5816, #5754, #5750)
- Added a suite of Windows nightly test cells: IOCP high-IOPS, daemon-crate, OpenSSH/`rsync_io`, NTFS ACL, reparse/symlink, ADS xattr, long-path `\\?\`, case-insensitive collision (#6323, #43, #45, #47, #49)
- Published workspace rustdoc to GitHub Pages and dropped `--cfg docsrs` from Pages RUSTDOCFLAGS to compile on stable (#5560, #5562)
- Benchmark workflows: published zsync matching benchmarks and pointed `benchmark.py` at upstream rsync 3.4.4 (#6542, #6043)
- Cargo.lock automation: auto-sync on Cargo.toml PRs, weekly `cargo-update` cron, regen-at-job-start, fork-PR diff comments (#5706, #5701, #5699, #6011)
- Reliability fixes for flaky infra: free disk space on Linux jobs, retry musl rustup fetch timeouts, retry interop smoke on daemon max-connections, tolerate cold-cache offline `cargo update` (#6201, #6300, #6309, #5729)
- Pinned the nightly toolchain around a `rustc_ast` ICE and tracked a non-required nightly 3.5.0dev testsuite cell (#6585, #6240)
- Grouped dependency bumps via the actions group and DRY'd release-binary builds into a composite action (#6320, #6186, #5797, #6035)

### Maintenance

- Removed validated orphan and never-compiled source files, plus orphaned `set_tcp_congestion` helper and dead non-unix `sendfile`/`recv_fd` re-exports (#6749, #6144, #99)
- Dependency bumps: `russh` 0.62.1, `tikv-jemallocator` 0.7.0, `zlib-rs` 0.6.6, `cargo_metadata` 0.23.1, plus grouped minor-and-patch batches (#6321, #6322, #6419, #5512, #6551)
- Security advisory bump for `crossbeam-epoch` 0.9.20 (RUSTSEC-2026-0204) (#6328)
- Lockfile housekeeping: regenerate for upstream drift, pin `cargo-platform` 0.3.2 for rustc 1.88 compat, sync compress proptest dev-dep (#100, #101, #90)
- Formatting and hygiene: `cargo fmt --all` on master, import ordering in `filters/set.rs`, `inspect_err().ok()` for charset parse, master fmt+clippy hygiene (#5700, #5744, #6532, #5728)
- Version and template housekeeping: pin upstream rsync 3.4.4 (closes #4965), release-notes template reference, Homebrew formulas for v0.6.3 (#5538, #5776, #5506)

## [0.6.3] - 2026-06-05

### Security

- SEC-1 status promoted to MOSTLY FIXED reflecting `.f/.g/.h/.i/.j/.k/.l/.m/.n` ship state (#4691)
- Partial-mitigation status for CVE-2026-29518 / CVE-2026-43619 via SEC-1 `*at` chain (SEC-1.o-partial) (#4672)
- `renameat` sandbox helper for atomic in-sandbox renames (SEC-1.j) (#4693)
- `fchmodat`/`fchownat`/`utimensat` sandbox helpers for metadata application (SEC-1.i) (#4690)
- `mkdirat`/`symlinkat`/`linkat` sandbox helpers for create-path operations (SEC-1.h) (#4683)
- Replace `remove_file`/`remove_dir` with `unlinkat` in `fast_io` + `transfer` (SEC-1.g) (#4671)
- Replace `lstat`/`symlink_metadata` with `fstatat(AT_SYMLINK_NOFOLLOW)` (SEC-1.f) (#4668)

### Features

- `pre-xfer exec` / `post-xfer exec` daemon directives with `RSYNC_ARG#` env vars and stdout capture (#5503)
- `--password-command` option for daemon authentication (#5500)
- Forward `--stop-at` deadline to remote server in SSH transfers (#5499)
- Forward `--remote-option` (`-M`) args to remote rsync process (#5498)
- Wire `--compress-threads` through transfer pipeline to zstd encoder (#5496)
- Embed filter rules in batch replay scripts (#5495)
- Wire `--info` subcategory dispatch to thread-local verbosity config (#5494)
- Parse missing upstream `rsyncd.conf` directives and warn on unknown keys (#5489)
- `--delay-updates` final rename sweep in remote receiver (#5398)
- `--partial` / `--partial-dir` file retention on interrupt (#5388)
- `--info=progress2` sliding-window rate, format, and parsing (#5382)
- Wire progress tracker into daemon transfer pipeline (#5383)
- `--ignore-missing-args` and `--delete-missing-args` flags (#5384)
- Handle invalid byte sequences in `FilenameConverter` (#5385)
- Handle progress2 interaction with `--outbuf` and terminal detection
- Stamp `mtime=0` on retained partial files for plain `--partial` (#5430)
- Negate modifier (`!`) for filter rules (#5426)
- Daemon-over-remote-shell mode for SSH with `::` operands (#5364)
- `--server --daemon` remote-shell daemon mode over stdio (#5353)
- `flush_workers`/`drain_inflight` barrier API on `ParallelDeltaApplier` (FFB-2) (#4665)
- Warn when `rsync --compress` meets SSH `-C` (double-compression detection, SSC-1) (#4667)
- Warn on SSH stderr socketpair-to-pipe fallback (SSF-2) (#4663)
- Adaptive per-file basis-read dispatch in `fast_io` (SMR-3c) (#4441)
- mmap-to-io_uring size threshold dispatch in `fast_io` (SMR-3b) (#4435)
- Wire `SpillGranularity::PerItem` in spill write path (STN-5) (#4428)
- `--spill-dir` and `--spill-threshold-bytes` CLI flags (STN-11) (#4423)
- io_uring file reader behind `iouring-data-reads` feature (IUD-6) (#4410)
- Mark `ssh-socketpair-stderr` as opt-in feature with default-path test (SSE-5) (#4389)
- Env-var overrides for `SpillPolicy` (STN-8/9/10) (#4404)
- Graceful BGID exhaustion fallback with typed error (BGE-6) (#4391)
- Wire `--acls` to Windows DACL (#4388)
- `IORING_OP_SEND_ZC` behind `iouring-send-zc` feature (IUD-7) (#4422)
- `SpillCompression::Zstd` behind `spill-compression` feature (STN-7) (#4416)
- Page-aligned `BufferPool` for IOCP no-buffering (#4374)
- `SpillPolicy.reclaim`: `KeepInMemory` vs `RespillAfterRead` (STN-4) (#4400)
- Typed error variants for `Arc::try_unwrap` failure paths (#4357)
- Opt-in io_uring data-write dispatch for large files (IUD-5) (#4397)
- mmap-free-basis experimental feature in `fast_io` (SMR-3a) (#4438)
- RSS-aware spill trigger (STN-6) (#4421)
- Async stderr drain task for SSH socketpair (#4363)

### Bug Fixes

- Align daemon `@ERROR` responses with upstream rsync wording (#5504)
- Forward `--trust-sender` and `--checksum-seed` to remote server (#5501)
- Wire `--contimeout` to embedded SSH (russh) connection path (#5497)
- Increase default daemon listen backlog from 5 to 128 (#5487)
- Suppress descendant matchers for anchored wildcard filter patterns (#5441)
- Build delta signature before backup rename to prevent false vanished error (#5440)
- Skip parent directory preparation in dry-run mode (#5439)
- Re-apply directory mtimes after transfer to prevent clobbering by child writes (#5442)
- Emit directory records before children in itemize output (#5432)
- Apply umask masking for chmod clauses without explicit who-specifier (#5428)
- Implement `dest_mode()` computation for non-preserve-perms transfers (#5427)
- Deduplicate repeated source operands to prevent duplicate transfers (#5425)
- Handle embedded `/./` markers in `--files-from` entries (#5433)
- Follow symlinks when emitting implied parent directories (#5436)
- Preserve directory mtime after deferred deletions (#5431)
- Force dry-run mode for `--only-write-batch` local transfers (#5424)
- Allow `--rsync-path` on local copies to match upstream behavior
- Gracefully skip daemon scenarios when upstream rsync cannot bind
- Remove erroneous CAP assertion from daemon config test (#5367)
- Align daemon module listing protocol with upstream behavior (#5366)
- Remove stale SEC-1.j TODO comments from completed task (#5365)
- Use socketpair instead of pipes for RSYNC_CONNECT_PROG child stdin (#5363)
- Detect inetd/connect-program stdin socket in standalone daemon (#5359)
- Build tls/getgroups helpers for upstream testsuite and remove last known failures (#5358)
- Run daemon protocol over stdio for remote-shell and connect-program modes (#5357)
- Add `build_capability_string_suffix` and remove ssh-basic from known failures (#5356)
- Embed capability string in compact flag string for server mode (#5352)
- Prevent deadlock in sync bridge multi-chunk wire parity test (#5351)
- Add `.nojekyll` to prevent Liquid template errors in GitHub Pages (#5349)
- Upstream testsuite hardlinks test compatibility (#5346)
- Resolve relative `OC_RSYNC_BIN` path in upstream testsuite runner (#5345)
- Remove chmod-temp-dir from upstream testsuite known failures (#5344)
- Export `setfacl_nodef` in upstream testsuite harness for ACL tests (#5343)
- Apply metadata before rename to match upstream `finish_transfer` semantics (#5338)
- Parse secluded-args and capability string from compact server flag string (#5336)
- Inherit `P_LOCAL` directives from global `rsyncd.conf` section into module context (#5334)
- Update clap error message assertion for clap 4.6 wording (#5331)
- Preserve atime independently of mtime in local copy metadata path (#5328)
- Unlink destination before cross-device copy in temp-dir fallback (#5327)
- Widen `open_daemon_stream` visibility for cross-module re-export (#5323)
- Use explicit builder in `to_builder_allows_modification` test (#5322)
- Align debug flag level tests with upstream clamping behavior (#5321)
- Wire `--old-args` through client config to unblock upstream 00-hello test (#5320)
- Clamp `--debug` flag levels to `MAX_OUT_LEVEL` instead of rejecting (#5319)
- Preserve original wire NDX for INC_RECURSE gap echo-back (#5318)
- Support `RSYNC_CONNECT_PROG` and double-colon syntax in daemon transport (#5317)
- Implement `-VV` JSON output and remove atimes from known failures (#5316)
- Gate `kqueue_stub` `c_int` import on non-unix only (#4429)
- Import `FileReader` trait for `IoUringFileReader::open` (#4452)
- Clippy compliance in `nvme_data_path` bench (#4454)

### Changed

- Enable parallel receive-delta by default via Path B heuristic (PIP-3 + PIP-5) (#4666)

### Refactoring

- Comment cleanup for daemon crate (#5362)
- Rename `apply_chunk_parallel` to `apply_one_chunk` for clarity (RJN-2) (#4660)
- Extract `spill/tempfile.rs` (SPL-3) (#4434)
- Channel-based drain shutdown for delete emitter (ATU-4) (#4401)
- MPE `traversal.rs` audit followup (#4380)
- Replace `lock().expect()` in `delete/emitter` (#4379)
- Replace `lock().expect()` in `delete/plan_map.rs` (#4375)
- Extract `spill/error.rs` (SPL-2) (#4345)
- Replace bare `io::ErrorKind::Other` with typed errors (#4377)

### Tests

- IP/CIDR host ACL allow/deny validation tests (#5502)
- `--partial` interrupt parity interop tests (#5480)
- Wire-byte parity for batched generator flush (#5463)
- Validate progress2 output format matches upstream rsync (#5392)
- `--delay-updates` sweep tests for remote transfer path (#5397)
- Interop test for no-partial mid-transfer temp file removal
- `--partial-dir` mid-transfer interrupt interop tests (#5395)
- Verify `mtime=0` partial files are not skipped by `--update` (#5389)
- Interop tests for `--partial` mid-transfer kill retention
- `--iconv=utf8,latin1` filename round-trip integration test
- `CleanupManager` integration tests for disk commit thread
- FFV-5/6/7 tests for `--files-from` vanished file handling
- `--iconv` with non-ASCII filter rules interop tests
- `--delay-updates` interrupt leaves files in partial-dir
- Comprehensive symlink-swap attack regression for SEC-1 sandbox (SEC-1.m) (#4675)
- Legitimate symlink transfers must not regress under SEC-1 sandbox (SEC-1.n) (#4678)
- Socketpair-to-pipe fallback warning fires exactly once (SSF-4) (#4684)
- Re-enable stale ignored tests and remove obsolete entries (#4431)
- Windows source to Linux destination ACL round-trip (WAS-7) (#4420)
- Env-var driven E2E spill integration test (STN-14) (#4408)
- Byte-identical regression for io_uring data path (IUD-8) (#4395)
- Isolated unit tests per `SpillPolicy` knob (STN-13) (#4393)
- Fuzz targets for `rsyncd.conf`, auth response, incremental flist (FCV-3) (#4444)
- Thread panic recovery for delete pipeline (MPE-10) (#4376)
- 100K session BGID leak stress (#4373)
- Extend filter parser fuzz edge cases (#4371)
- `NegotiationPrologueSniffer` pre-auth fuzz target (FCV-3 P0) (#4367)
- Legacy greeting + version negotiation fuzz target (#4414)
- Daemon `@RSYNCD` greeting parser fuzz target (FCV-3 P0) (#4409)
- Extend varint decode fuzz target with round-trip (FCV-5) (#4405)

### Documentation

- User guide for partial file interrupt behavior (#5437)
- Document `--partial` interrupt semantics (#5399)
- Add interop compatibility status document (#5361)
- Publish interop compatibility status document (#5360)
- **SSH transport**: documented the opt-in `rsync_io/ssh-socketpair-stderr`
  Cargo feature - what it does (socketpair-backed SSH stderr instead of an
  anonymous pipe), why it exists (avoid deadlock when chatty remote children
  fill the 64 KiB pipe buffer), when to enable it, and platform constraints.
  Added `docs/ssh-transport.md` and cross-linked from the Cargo features
  table in `README.md` (#2377).
- Refresh spill layout and migration status (SPL-12) (#4394).
- Cross-platform CI hazard preflight audit (#4427).
- BR-6 beta-readiness sign-off check-in (#4692)
- Close WPG-1 as deferred to post-beta Windows hardware capture (#4688)
- Close PIP-4: interop suite exercises parallel-receive-delta path via PIP-5 default flip (#4689) [SUPERSEDED: PIP-7 (#4730) proved the dispatch scaffolding was a side-effect-only no-op; PIP-8 tore out the dead receiver-side wiring, and the proper integration is tracked by PIP-9]
- Close FFB-3/FFB-4/PIP-2 as satisfied by FFB-1 design + PIP-3+5 wire-up (#4677)
- Close RJN-4 as N/A after RJN-3 was rename-only (#4686)
- Defer RJN-3 (fanout) and RJN-4 (bench) as N/A after RJN-2 rename (#4676)
- Close ABW-3 as N/A pending per-file `Mutex` refactor (#4685)
- Defer ABW-2/3/4 pending BR-3j.f bench evidence (ABW-1 audit closure) (#4673)
- `apply_batch_parallel` verify-vs-write overlap audit (ABW-1) (#4670)
- Pre-frame IUS-4 SEND_ZC opt-in vs default-on decision (#4687)
- IORING_OP_SEND_ZC kernel compatibility matrix (IUS-2) (#4664)
- `--zero-copy` SEND_ZC build-time dependency note (IUS-1) (#4661)
- `flush_workers` barrier API design for `ParallelDeltaApplier` (FFB-1) (#4659)
- Token loop vs `ParallelDeltaApplier` migration surface audit (PIP-1) (#4657)
- `apply_chunk_parallel` call sites and per-chunk dispatch benefit audit (RJN-1) (#4656)
- SSH stderr socketpair-to-pipe fallback site audit (SSF-1) (#4658)
- Document `ssh-socketpair-stderr` feature and fallback warnings (SSF-3) (#4669)
- README warning for SSH+rsync double-compression (SSC-2) (#4655)
- Evaluate `ssh_config` parsers for SSC-3 double-compression detection (#4674)
- Formalize SEC-1.h `mknodat` deferral and document re-open triggers (#4694)
- Plan re-fold of SEC-1 `*at` helper modules post SEC-1.j ship (#4695)
- Runnable Windows IOCP vs MSYS2 profiling methodology (WPG-1) (#4442)
- SPL-8 still blocked until SPL-3/4 merge (#4439)
- Workspace dependency consolidation opportunities (#4425)
- Workspace rustdoc coverage audit (#4424)
- CI workflow hazards and quick wins (#4419)
- Catalogue ignored tests with re-enable recommendations (#4418)
- mmap-vs-SQPOLL decision framework (SMR-2) (#4417)
- SPL-10 enforce-limits audit (#4413)
- Record recent series completions in agents notes (#4411)
- FCV-3 protocol-parsing fuzz coverage gaps (#4407)
- Windows ACL behavior for `--acls` (WAS-8) (#4406)
- mmap-vs-SQPOLL status table and SHIPPED marker (SMR-5) (#4402)
- WAS-6 Windows hardlink ACL inheritance (#4399)
- Module-level rustdoc on spill submodules (SPL-11) (#4392)
- Add `///` on `pub mod` declarations, round 1 (#4437)
- SMR-4 regression strategy for SQPOLL-on-large-deltas test (#4433)
- Add `///` on remaining `pub mod` declarations, round 2 (#4449)
- Rolling SIMD checksum-sync regression hypothesis (CSP-1) (#4450)
- PRC-3a DACL-POSIX overlap analysis (#4453)

### CI/Build

- Add iconv feature to CI test matrix (#5386)
- Install `libxxhash-dev` and guard grep pipeline in upstream testsuite (#5350)
- Add upstream rsync testsuite workflow with UPASS detection (#5342)
- Standardize cache keys and add missing `CARGO_TERM_COLOR` (#5341)
- Align ci-skip interop job names with `ci.yml` check names (#5340)
- Fix ci-skip path filters to avoid overlap with `ci.yml` (#5339)
- Add `--no-tests=warn` to async-wire-parity workflow (#5337)
- Add nextest `--profile ci`, `--locked`, and missing timeouts (#5335)
- Pin all GitHub Actions to SHA hashes (#5333)
- Standardize cache keys on `Cargo.lock` (#5332)
- Fix daemon bench workflows using wrong package name (#5330)
- Fix xargs flag conflict and proc/status race in daemon concurrency CI (#5329)
- Remove job-level `if` conditions that broke push-triggered CI runs (#5326)
- Reduce runner contention by limiting non-required jobs to schedule (#5324)
- Matrix benchmark-release and harden `parallel_determinism` (#4443)
- Apply top quick wins from workflow audit (#4432)
- Weekly fuzz coverage report workflow (FCV-9) (#4403)
- mmap vs read_fixed+SQPOLL basis-read characterization bench (SMR-1) (#4387)
- Production io_uring path vs stdlib baseline bench (IUD-9) (#4398)

### Other Changes

- Triage environment-dependent upstream testsuite known failures (#5355)
- Triage environment-dependent upstream testsuite known failures as root (#5354)
- Format crtime test builder chain inline (#5325)
- Add SAFETY comments to the remaining 21 unsafe blocks (#4440)
- Consolidate cross-crate deps into `[workspace.dependencies]` (#4436)
- Gate Unix-only test modules and deny broken rustdoc links (#4430)

### Performance

- Add million-file RSS benchmark scaffold (#5478)
- Add DashMap concurrent-access benchmark scaffold (#5479)
- Add checksum wall-clock benchmark scaffold (#5476)
- Add daemon connection scaling benchmark scaffold (#5475)
- Add `copy_basis_range` benchmark scaffold (#5474)
- Add concurrent session scaling benchmark scaffold (#5473)
- Add bandwidth-constrained checksum benchmark scaffold (#5472)
- Add SEND_ZC zero-copy benchmark scaffold (#5477)
- Tune russh client config for faster SSH handshake (#5490)
- Optimize generator no-change scan path (#5466)
- Optimize no-change scan path for 100K-file scale (#5468)
- Eliminate redundant stat calls in metadata no-change path (#5492)
- Add `metadata_unchanged` fast-path for no-change generator scan (#5462)
- Unify multiplex flush discipline across transfer roles (#5464)
- Compact `FileEntry` from 88 to 80 bytes per entry (#5481)
- Reduce per-file overhead in SSH push no-change scan path (#5471)
- Eliminate redundant file reads in SSH push sender path (#5470)
- Eliminate redundant stat syscalls in SSH pull path (#5469)
- Implement remaining checksum overhead optimizations (#5465)
- Reclaim completed INC_RECURSE flist segments to reduce RSS (#5467)
- Increase checksum read buffer from 64KB to 256KB (#5460)
- Add BufReader wrapping for SSH pull read path (#5461)
- Remove intermediate BufReader from whole-file transfer (#5459)
- Tune mimalloc arena reservation and purge delay for lower RSS (#5488)
- Reuse readdir buffer across recursive directory traversal (#5484)
- Replace `Path::join` with `PathBuf::push/pop` in traversal (#5483)
- Eliminate heap allocations in `format_decimal_bytes` (#5486)
- Use move semantics for `ClientEvent` conversion (#5485)
- Pre-size `Vec<LocalCopyRecord>` to eliminate growth copies (#5482)
- Scaffold PIP-6 end-to-end parallel-vs-sequential bench harness (#4679)
- Scaffold BR-3j.f DashMap cores-vs-throughput re-bench harness (#4682)
- Scaffold IUS-3 SEND_ZC vs plain SEND bench harness (#4680)
- Keep rolling `s1`/`s2` in SIMD registers across stripe (CSP-2 F1) (#4451)
- **Delta matching**: incorporated four zsync-inspired internal optimizations
  to the receiver's block-match path. All four are pure refactors of the
  in-memory match index - wire bytes, capability flags, sum-head fields, and
  golden-byte fixtures are unchanged, and transfers against upstream rsync
  3.0.9 / 3.1.3 / 3.4.1 remain byte-identical.
  - **bithash prefilter** ([#3737](https://github.com/oferchen/rsync/pull/3737),
    commit `3d0391d8`): a 32-bit one-sided bit array gates the strong-checksum
    lookup so non-matching rolling-hash windows are rejected before any
    hashtable probe. Mirrors zsync's `librcksum/rsum.c` bithash gate and
    eliminates roughly seven of every eight post-tag-table misses on the hot
    path.
  - **sequential-match extension** ([#3751](https://github.com/oferchen/rsync/pull/3751),
    commit `6122b507`): after a confirmed block match the receiver attempts to
    extend the run by checking consecutive basis blocks directly, avoiding
    re-entry into the rolling-hash loop while a contiguous span of basis
    blocks keeps matching.
  - **matched-block pruning** ([#3748](https://github.com/oferchen/rsync/pull/3748),
    commit `aa7eb8a4`): once a basis block is consumed by a match it is
    removed from the lookup table so later windows skip duplicate probes.
    Mirrors zsync's `librcksum` post-match prune; duplicate basis blocks are
    handled by the existing strong-checksum gate.
  - **compact-key layout** ([#3994](https://github.com/oferchen/rsync/pull/3994),
    commit `58860a82`): replaces the pointer-chasing
    `FxHashMap<(u16, u16), Vec<usize>>` with a flat open-addressing table
    keyed by packed `(rsum_low, bucket_idx)` entries, giving sequential probes
    cache-friendly access and removing per-bucket heap allocations.

[Unreleased]: https://github.com/oferchen/rsync/compare/v0.6.4...HEAD
[0.6.4]: https://github.com/oferchen/rsync/compare/v0.6.3...v0.6.4
[0.6.3]: https://github.com/oferchen/rsync/compare/v0.6.2...v0.6.3
