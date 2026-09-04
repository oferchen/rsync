# Changelog

All notable changes to oc-rsync are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

oc-rsync is wire-compatible with upstream rsync 3.5.0 and the 3.4.x series
(protocol 32). Release tags are mirrored on GitHub at
<https://github.com/oferchen/rsync/releases>.

## [Unreleased]

This cycle is dominated by the move to upstream rsync 3.5.0 as the reference
implementation: retargeting the source-of-truth citations, adopting the new
3.5.0 option and directive surface, closing the path-confinement and daemon CVE
families, and putting the 3.5.0 test suite in front of every pull request.

### Security

**Daemon and client input hardening**
- The `proxy protocol hosts` daemon directive now gates who may supply a PROXY
  header. A direct peer that is not on the trusted list is refused, and an
  empty or unset list rejects every peer rather than accepting any - upstream's
  `allow_proxy_protocol_peer()` fails closed the same way, and warns at startup
  on the `proxy protocol = true`-with-no-list combination (#7648)
- Refuse an over-long proxy CONNECT request, authorization header and response
  header before writing them. Upstream has four length refusals where oc-rsync
  had one, and an over-long *header* was reporting the *status-line* wording;
  the 1023-byte bound is now derived from `PROXY_BUF_SIZE` rather than typed
  twice (#7650)
- Confine a peer-supplied alternate-basis xname to its basedir, with the
  sanitiser and its wire-driven consumer landing together so the guard cannot
  ship inert (#7651)
- The macOS `clonefile` fast path bypassed the confined source open; it now
  inherits it, so the fast path cannot resolve a component the slow path would
  refuse (#7653)
- Bound the client argument vector the daemon accepts, and stop logging it
  verbatim (#7633)
- Confine the `--delay-updates` staging path to the module. The staging path
  was resolved to its canonical spelling while the module root was carried
  unresolved, so a module under a symlinked ancestor escaped the guard - on
  Linux only Landlock stopped it, leaving any pre-Landlock kernel,
  `OC_RSYNC_NO_LANDLOCK`, or non-Linux platform exposed (#7659)

**Path confinement (CVE-2026-53795 family)**
- Confine the destination write and the source read against symlink races, and
  route the confined source open onto one shared per-component resolver instead
  of two platform arms that had drifted in opposite directions (#7393, #7349)
- Never follow a symlinked alt-dest basis entry, and read a source symlink
  target through the confined parent rather than the raw path (#7419, #7418)
- Resolve an absolute operator rename endpoint by ownership, closing an absolute
  `--temp-dir` rename injection (#7398)
- Resolve `--log-file` through the ownership walk (#7404)
- Decide each `--relative` implied parent level on its own, so a planted
  intermediate symlink cannot capture the rest of the path (#7415)
- Open a daemon module root plainly and confine only the peer-supplied tail;
  applying `RESOLVE_NO_SYMLINKS` to the fused whole made legitimate module roots
  unreachable without adding confinement (#7304)
- Apply the daemon filter to the destination argument, and honour
  `--confine-root` on the server instead of taking it for the destination
  (#7417, #7388)
- Add the path-confinement activation predicate as four explicit decisions
  (opt-out / hardened / root / path kind) rather than one boolean (#7322)
- Route every operator-named auxiliary file open onto the ownership walk, which
  refuses a symlink component owned by neither root nor the running user. This
  is upstream's `open_no_attacker_symlinks()` applied at the call sites upstream
  applies it: the daemon config, motd and secrets files (#7422), the daemon
  `lock file` at mode 0600 (#7439), `--password-file` (#7424), `--log-file`
  (#7441), `--files-from` and `--early-input` (#7442), `--exclude-from` /
  `--include-from` and the CLI merge files (#7426), per-directory merge files
  (#7425), and the batch read/write files at upstream's modes (#7443)
- Stat an alt-dest basis entry through the ownership walk rather than opening
  it, on the receiver (#7421) and on the local-copy path (#7463). The leaf is
  `lstat`ed and never opened, matching `generator.c`; opening it instead is what
  regressed the `--link-dest` / `--compare-dest` cells on the first attempt
- Honour `--insecure-links` inside the operator-path walk, so the opt-out
  reaches the resolver instead of being consulted only by its callers (#7459)
- Confine the `--partial-dir` reuse probe and discard the temp when the
  directory create is refused; the probe alone was inert because the abort
  path renamed regardless (#7483)
- Resolve an absolute `--backup-dir` against the module root, and route the
  `--inplace` backup copy and the `--temp-dir` staging temp through the
  ownership walk (#7493, #7494, #7496)
- Refuse a staging directory the module filter excludes, and confine the
  filter-file open to the module root (#7495, #7512)
- Honour the admin symlink opt-out at the sender confinement root, so an
  operator who opted out is not refused anyway (#7517)
- Stop confining a *client's* own destination on a daemon pull - the
  confinement applies to the peer-supplied side, not the operator's (#7503)
- Confine the `--inplace` backup copy, and the `--backup-dir` link and rename,
  to the module root (#7535, #7538)
- Anchor the source unlink to a confined parent dirfd, so `--remove-source-files`
  cannot be redirected by a planted path component (#7564)
- Stop pre-empting the confined `--relative` implied-parent creation. Every
  implied parent was created up front through a plain path, so a symlinked
  component pointed a daemon's implied parent outside the module root and the
  client still exited 0. Upstream's `make_path()` is a fallback, not a pre-pass,
  and builds each component with `do_mkdir_at()` - the ownership walk plus
  `mkdirat` - so the creation now runs after the directory pass and through the
  confined wrapper, which also settles an itemize divergence where a real run
  reported `.d..t......` for parents its own `--dry-run` called `cd+++++++++`.
  The conflicting-symlink removal on the same path moves onto the confined
  `unlink` wrapper, and a refused removal is reported instead of being recovered
  with an `exists()` probe that follows the symlink whose removal was just
  denied. The seccomp allowlist is `*at`-only by construction, so the `EPERM` it
  raised named an unported call site rather than a filter that was too narrow
  (#7565)
- Bound a peer-named merge file by `--confine-root`. A dir-merge rule travels
  over the protocol as a filter rule rather than in the argv an `rrsync` wrapper
  validates, so the peer names the file; the per-directory merge open took the
  ancillary entry point and followed a trusted symlink, and neither the client
  nor the non-daemon server published `--confine-root` into the walk's session,
  so even the already-confined opens measured against an empty root (#7598)
- Issue the ownership walk's own syscalls through libc rather than rustix's
  raw-syscall backend. Upstream's walk is plain C against libc, so an
  interposition layer an operator has in place - `fakeroot`, an audit or sandbox
  preload - saw upstream's resolution and did not see oc's (#7599)
- Let the ownership walk traverse under a Landlock sandbox, open its anchor with
  `openat` so the seccomp worker filter admits it, and fall back to the confined
  wrappers rather than to plain syscalls when a confined open is unavailable
  (#7541, #7545, #7548)

**Daemon**
- Refuse shell metacharacters when expanding a hook variable, closing command
  injection through the `pre-xfer exec` / `post-xfer exec` environment
  (CVE-2026-53790, #7465). The live sink is oc's own single-character expander,
  not the upstream-shaped one, so porting upstream's guard alone would have been
  inert
- Validate and quote the `RSYNC_CONNECT_PROG` `%H` substitution instead of
  interpolating the host verbatim (#7430)
- Enforce `max connections` on the stdio, inetd and remote-shell entry points,
  not only the TCP accept loop, and honour the per-module `lock file` (#7440)
- Gate the secrets-file mode check on `strict modes` rather than validating it
  unconditionally at parse time (#7468)
- `refuse options = delete` must also refuse a pull's `--remove-source-files`;
  the inference is `am_sender`-gated, so a blanket refusal would have rejected
  every push (#7448)
- A refuse rule names a capability, not one spelling of it (#7427)
- Apply the module `exclude` to the client's destination argument (#7450) and
  refuse an alternate-basis directory the module excludes (#7455); clamp a
  client's alt-dest basis into the module instead of dropping it silently
  (#7467)
- Close two replay-script injections in the generated batch `.sh` (#7445)
- Resolve the real peer address on both stdio paths. The inetd and remote-shell
  entry points fabricated `127.0.0.1`, which made every `hosts allow` /
  `hosts deny` rule evaluate against a synthetic localhost (#7303)
- Match upstream's peer-host naming and ACL evaluation; `hosts deny` no longer
  fails open when a configured hostname does not resolve (CVE-2026-70452,
  #7314)
- Honour the leading-comma separator form in `auth users` and `gid`
  (CVE-2026-70463, #7345)
- Add the `auth digest` minimum-digest floor (#7350)
- Refuse control bytes in a name-converter request, closing a newline-injection
  desync of the converter stream (#7346)
- Name the user and the rule in auth-failure log lines (#7395)
- Treat a backslash in a requested path as a filename byte, not a path separator
  (#7409)
- Honour the per-module `insecure links` directive (#7484)
- Pin the Landlock root after `chroot` rather than before it; the pre-chroot
  path names a directory the confined process can no longer reach (#7567)
- Pin the module root as a descriptor before the privilege drop, through the
  ownership walk rather than a plain open, and resolve the sender's lookups
  against that pin. Re-walking the absolute module path after `setuid` made a
  module under an unsearchable parent unservable, and it failed the Landlock
  rule-path open too - which the caller only warns about, so the module was
  served with no kernel sandbox at all, on exactly the layout the sandbox
  exists for (#7605)
- Make the connection FSM's close edge total, and stop one malformed client
  ending the listener for everybody. Four of ten call sites discarded the
  transition result, all of them targeting `Closing`; separating teardown from
  the handshake progression surfaced the one edge a peer can aim at, where a
  repeated `@RSYNCD:` banner took `Greeting -> ModuleSelect` twice and the
  resulting error killed the accept loop. Upstream reads the version line once
  and takes the next line as the request whatever it contains, so a second
  banner is a module name; and no per-connection outcome of any class reaches
  the accept loop any more, matching a `sigchld_handler` that discards the
  session status outright (#7579, #7590)
- Admit `mknod` / `mknodat` in the seccomp worker filter, which was dropping the
  device and FIFO creation the sandbox is meant to permit (#7547)
- One operator opt-out now governs both kernel sandbox layers symmetrically, so
  Landlock and seccomp cannot disagree about a single operator decision (#7546)

**Peer-supplied input bounds**
- Bound peer-supplied xattr bytes and the CONNECT host (#7297)
- Bound the peer-supplied filter-rule record length (#7377)
- Bound the equal-weak-checksum chain in the delta matcher (CVE-2026-70453,
  #7293)
- Mask peer-supplied `io_error` to the defined `IOERR_*` bits (#7291)
- Reject stray negative NDX instead of skipping it (#7365)
- Reject `--max-alloc=0` instead of disabling the guard (#7285)
- Sanitize received symlink targets when munging is off (#7333)
- Escape control characters in log-file output, in the sink rather than the CLI
  renderer so remote-controlled writes cannot reach the terminal raw (#7296,
  #7357)
- Redact peer-chosen rule text in filter-rule diagnostics (#7384)
- Bound merge-file nesting by depth (`MAX_MERGE_DEPTH`), not by cycle detection;
  a cycle set terminates a loop but not an unbounded acyclic chain (#7432)
- Clamp every `--files-from` line through upstream's `sanitize_path`, which
  `flist.c` applies unconditionally to a real files-from stream (#7460)
- Don't let the sender widen the receiver's `--delete` scope through an implied
  parent directory (#7446)
- Reject a non-directory encoding of the synthetic `.` transfer-root entry at
  decode time, both spellings. The root is exempt from the requested-name
  filter checks, so a sender that encoded it with regular-file mode made every
  make-way site see a directory where a non-directory had to be written and,
  under `--force`, clear the destination root recursively. Refused as a
  protocol violation where upstream refuses it, in `recv_file_entry`
  (`flist.c:1127-1134`), not in the obstacle arm - gating the arm would leave
  the forged entry live for mkdir, rename and backup (#7625)
- Reject a modern NDX that overflows a signed file index rather than wrapping
  it (#7630)

### Added

- `--confine-root=DIR`, `--insecure-links` / `--no-insecure-links`, and
  `--drop-D` / `--no-drop-D`, matching the new 3.5.0 option surface. Like
  upstream, `--confine-root` and `--drop-D` are deliberately not forwarded to
  the remote side (#7396, #7299)
- `--chmod` now supports chmod(1)-style permission copies (`u=g`, `g=o`) (#7295)
- The `auth digest` daemon directive (#7350)
- The soft file-descriptor limit is raised at startup, and a deep path that
  exhausts descriptors warns once instead of failing silently (#7324, #7323)
- A missing or non-directory `--compare-dest` / `--copy-dest` / `--link-dest`
  argument now warns instead of passing silently, on the local path (#7453) and
  on the network paths (#7454)
- `--fake-super` is accepted in a server argv, and repeated via `-M` on a
  local transfer without being rejected (#7520, #7497, #7524)
- `--info=stats3` emits the heap-statistics block (#7550)
- The INC_RECURSE lookahead window is sized rather than fixed (#7539)
- A safe `fork`/`waitpid` wrapper for daemon sessions replaces the raw calls
  (#7523)
- The local-copy receiver stages a `--partial-dir` resume in place, writing the
  reconstruction into the existing partial entry and renaming that entry onto
  the destination. This is upstream's `one_inplace`, whose in-place target is
  the partial file and never the live destination - the comment arguing the
  opposite was describing a hazard of oc's own implementation (#7599)
- `--version` reports rsync 3.5.0 as the wire-compatible pin, and names four
  features the build already carried and never surfaced: `quic`, `sd-notify`,
  `vmsplice` and `send_zc`. `daemon-seccomp` is deliberately still unreported,
  because it is declared on the workspace root and reaches no crate that
  renders `--version` (#7616)

### Changed

- Tracked upstream reference moved to rsync 3.5.0 (released 13 Aug 2026) in
  prose, comparison docs and the release benchmark. 3.5.0 carries the same wire
  protocol as 3.4.4 (`PROTOCOL_VERSION` 32, `SUBPROTOCOL_VERSION` 0, unchanged
  `errcode.h`), so wire compatibility is unaffected; the release is behavioural,
  covering 33 CVEs in path handling and the daemon.
- Upstream source citations across the workspace retargeted at the 3.5.0 line
  numbers, with the citation gate extended to run on docs-only changes and to
  fail when a cited file does not exist (#7305, #7321, #7331, #7315, #7318,
  #7308, #7286), and to range-check a citation written with an explicit
  `rsync-3.5.0/` path prefix instead of skipping it as a foreign upstream and
  leaving no trace in the report (#7607)
- rsync 3.5.0 now runs the full interop scenario matrix as a gating peer,
  alongside 3.0.9, 3.1.3 and 3.4.4 (#7290, #7337)
- The required upstream-testsuite gate runs the rsync 3.5.0 Python corpus
  instead of the 3.4.4 shell corpus, as four Linux legs: privilege
  {non-root, root} x daemon transport {stdio pipe, loopback TCP}, plus a
  non-root/pipe leg on macOS. Each leg carries an expected-
  outcome manifest generated from a real run, so only a change in outcome - a
  regression, or an unexpected pass - fails the gate (#7387, #7339, #7405,
  #7408, #7392, #7391)
- Release benchmarks now compare against upstream rsync 3.5.0 and report **peak
  RSS alongside elapsed time for every mode**. The measurement was already being
  collected for each run and discarded for all but the memory mode, so a speed
  win paid for in memory was invisible in the published numbers.
- Every upstream benchmark cell is now run against **both** rsync 3.4.4 and
  3.5.0, because the two baselines disagree materially and a single-baseline
  table reads upstream's own regressions as an oc-rsync win. Four harness
  defects were fixed in the same pass: `oc_rsync_version` published the wire
  compatibility version rather than the release, every "initial sync" cell was
  timing a no-change sync because the destination was reset once instead of per
  run (43x off), the RSS columns were only ever populated for the memory mode,
  and a run that failed in a millisecond was recorded as a very fast run. The
  SSH cells now pin both ends to the same release, and an `IORING_OP_SEND_ZC`
  probe reports kernel capability and binary capability as two separate facts
  (#7595)
- io_uring stops costing more than it saves, and the zero-copy send policy
  stops being a synonym for off. `Auto` now means what its name says and can
  reach `IORING_OP_SEND_ZC` in builds carrying the `iouring-send-zc` feature -
  `Enabled` is unreachable from a client, since the flag is deliberately never
  forwarded to a peer, so the transport was dead code in practice. The SEND_ZC
  writer no longer blocks on the notification CQE, which was 75% of the time
  spent inside a send and had made a loopback daemon pull 5.8x slower. And a
  write batch below the ring threshold goes out positionally, which took a
  10,000-file daemon pull from 1106 ms to 450 ms - nothing is ever in flight
  across batches, so a one-chunk batch paid for a ring round trip that bought
  it no concurrency. The zero-copy feature remains in no default set, because
  on loopback it is still behind a plain socket write (#7593, #7600, #7610)
- `daemon-seccomp` is reachable from the `oc-rsync` binary. It was declared
  only on the `daemon` crate and forwarded by nothing, so the worker filter was
  compiled in only by a workspace-wide `--all-features` build and was in no
  released artefact, while `README.md` listed it as a Tier-1 Linux capability.
  The build-time default is unchanged - still opt-in - and a feature-matrix row
  now pins the forwarding edge rather than the crate's own declaration (#7589)
- Interop failures name the peer version that broke, via `--only-version`
  (#7341)
- The local-copy context state module is split by concern (#7358)
- The receiver is the response-flist sink, and the backup ladder takes only the
  fields it reads (#7536, #7553)
- Three source files that nothing compiled were removed (#7543)

### Fixed

**3.5.0 testsuite divergences**
- `keep_backup failed` named the source file rather than the backup
  destination it failed to create - at both emitting sites (#7658)
- A directory's creation time is preserved under `--crtimes` (#7656)
- Apply the set-group-ID mode `chmod(2)` would apply: macOS `fchmodat` refuses
  the bit that `chmod` silently masks, so the two calls disagreed (#7655)
- Anchor the sender's directory scan on the transfer root. The daemon's flist
  walk read a process-global root that nothing installs; the anchor is now a
  parameter, which fixed two macOS cells with one change (#7654)
- A trailing `/.` must not pivot the `--relative` root - two upstream
  decisions had been conflated into one (#7652)
- Keep the DOTDIR marker on a module operand ending in `/.` (#7635)
- Reject an unknown `--info` / `--debug` item the way upstream does, instead
  of accepting it silently (#7637)
- Keep a cleared directory's `dir_flist` slot and refuse it, and refuse a
  transfer-phase NDX naming a cleared file entry (#7641, #7642)
- Report an oversized xattr datum and a zero block length with upstream's own
  wording (#7647, #7646)
- Clear the master red: a rustdoc link, a racing-converter cause, and a
  pre-mangled log operand (#7639)

**Filters**
- Reject invalid filter-rule modifiers instead of stopping at the first one
  (#7361)
- Match dir-merge modifiers and filter-rule keywords case-sensitively, per
  upstream `parse_rule_tok` (#7375, #7380, #7379, #7378)
- Accept the `x` modifier on dir-merge and on hide/show/protect/risk rules
  without inheriting XATTR, and unstack it from the namespace screen (#7372,
  #7373, #7374)
- A clear filter rule goes on the wire as one byte (#7376)
- Collapse a merge-file name's `..` before opening it (#7288)
- Fold the pattern inside brackets for case-insensitive matching (#7292)
- Mirror upstream's clear-list token boundary exactly (#7298)
- Refuse a sided rule inside a sided merge file, reporting the merge file by the
  given name rather than its resolved absolute path (#7383)
- Terminate a self-referential dir-merge instead of hanging (#7362)
- A receiver must not refuse a perishable rule below protocol 30 (#7381)
- Track `--delete-excluded` distinctly in the server arg parse (#7382)

**Transfer and receiver**
- Honour `-B` / `--block-size` on every wire transport; three decoders parsed the
  value and dropped it (#7301)
- Surface `MSG_NO_SEND` so a declined file cannot hang a pull (#7371)
- Frame server-side warnings to the peer instead of dropping them (#7366)
- A daemon `MSG_IO_TIMEOUT` of 0 must not disable the client's `--timeout`
  (#7347)
- Gate the end-of-flist `io_error` on both wire encodings, and honour
  `--ignore-errors` in the marker (#7414, #7309, #7300)
- Carry `--ignore-errors` onto the local receiver and local copy (#7302)
- Fold peer `io_error` before the late delete sweep, order `io_error` to
  `RERR_*` the way `cleanup.c` does, and drop the redundant re-check from the
  delayed-deletion flush (#7363, #7326, #7359)
- Resolve a relative `--temp-dir` against the destination (#7397)
- Local `--write-batch` must encode the flist the reader decodes (#7355)
- Write the pending token run before flushing literals, in the zlib (#7434) and
  lz4 (#7436) encoders. A round-trip cannot observe this - the decoder
  reassembles either order - so the regression is pinned on wire framing
- `--delay-updates` stages through the partial dir and is renamed onto the
  destination only after the walk, matching `receiver.c`: the implicit `.~tmp~`
  on the receiver (#7475) and the operator's `--partial-dir` on the local-copy
  path, created at upstream's private 0700 (#7476)
- TCP Fast Open must not defeat the multi-address connect fallback; deferring
  the SYN left a dead first address looking alive (#7447)
- Recover a read-only in-place destination instead of failing the file
  (#7485)
- Keep an absolute `--partial-dir` through the delayed-updates sweep, and
  clear a non-directory occupying the `--partial-dir` name (#7499, #7500)
- Report a failed metadata apply instead of only counting it (#7506)
- Gate device-node creation on upstream's `am_root` predicate (#7513)
- Follow a symlinked directory named with the DOTDIR marker, and keep that
  marker when the `/./` remainder is empty (#7510, #7516)
- Refuse a non-regular `--read-batch` path (#7515)
- Skip the delayed rename when the backup fails, instead of committing over a
  destination whose backup never landed (#7531)
- Retain into the `--partial-dir` through the shared owner, and route the
  delayed sweep onto the shared backup ladder (#7556, #7557)
- Let `--no-whole-file` read the source in userspace instead of taking a kernel
  copy path that cannot produce a delta (#7537)
- Warn on file-descriptor exhaustion from the confined walk rather than failing
  without naming the cause (#7534)
- Name the commit operation that failed instead of always reporting `mkstemp`
  (#7552)
- Report a refused `--remove-source-files` unlink at upstream's `FERROR_XFER`,
  and act on each confirmation where upstream would already have reacted. The
  network sender printed the failure locally and handed back only
  `IOERR_GENERAL`, and the single batched drain sat after the whole goodbye
  exchange, in a window where a pulling client had stopped reading - so both an
  ssh pull and a daemon pull exited 0 with a destructive option having silently
  removed nothing. The drain is now two-phase: before the sender answers the
  goodbye, which is the last moment a diagnostic still reaches the peer, and
  after it, for the sources confirmed while the handshake ran. Moving the drain
  earlier instead would have silently stopped removing that second set
  (#7576, #7581)
- A commit-path backup that cannot be placed aborts with `RERR_FILEIO` (11)
  rather than being counted as a per-file skip, so the receiver stops instead
  of walking the rest of a batch whose backup area is already known to be
  unusable. The two drains held byte-identical copies of the continue-or-abort
  decision; they now share one, split on the failing operation rather than on
  the errno (#7582)
- Key `--write-devices` on the destination's own type, not the file-list entry.
  The source of a `--write-devices` transfer is a regular file by construction,
  so the predicate was permanently false and the in-place commit ran `set_len()`
  against the device: exit 12 where upstream exits 0 (#7583)
- Never park on opening a non-regular destination as the delta basis. A FIFO
  with no writer wedged the receiver indefinitely, on the default path with no
  flag required, which for a writable daemon module is an unbounded external
  wedge. Upstream reaches the same invariant by ordering rather than a type
  check, so the open carries `O_NONBLOCK`, decides on the descriptor rather
  than on a pre-open stat that a planted node could outrace, and clears the
  flag before handing a regular file back (#7594)
- Remove whatever stands at the destination before creating a node there, and
  report the removal the way upstream does. A directory obstacle was backed up
  under `--backup`, which upstream never does, and otherwise met a `File`-only
  unlink whose result was discarded - so a FIFO over a directory reported
  success while losing the node, and a regular file over a directory stopped
  the whole run at `EISDIR`. All six call sites now share one
  `make_way_for_replacement`, which owns both the `rmdir` and the unlink arm
  (#7602)
- Apply the pre-image's ownership, timestamps and mode to a backup made by the
  copy tier. The hardlink and rename tiers move or share the inode so the
  attributes travel with it; the copy tier builds a new one and carried none,
  which is why upstream calls `set_file_attrs` on that branch alone
  (`backup.c:420`, reached only from `backup.c:400`) (#7622)
- Honour `--force` so a populated directory standing where a non-directory must
  be written is cleared, matching upstream's
  `delete_mode || force_delete ? DEL_RECURSE : 0`. `--force` was recognised by
  the stdio server-arg parser and discarded, the daemon's long-form parser had
  no arm for it, and the obstacle arm consulted neither term - so oc refused at
  exit 23 in all four flag combinations where 3.5.0 replaces in three of them
  (#7625)
- Skip a directory operand instead of transferring it when directory transfer
  is off, per `flist.c:2723-2726` (#7627)

**Local copy and engine**
- Size a local copy from the opened file, not the flist record, and clamp the
  read to the declared length so a growing source cannot silently truncate
  (#7403)
- `--backup` must not override the quick check; it was applying `--checksum`
  semantics and reading both trees on every run (#7364)
- Honour `--safe-links` before the backup fast path (#7356)
- `--link-dest` must create the node when a hard link is refused (#7327)
- Clearing a directory obstruction is not `--force`'s job (#7416)
- `--preallocate` with `--sparse` must punish neither: the reserved extent was
  being punched straight back out (#7429)
- A non-directory alt-dest argument must not fail the transfer, on the regular
  path (#7451) and on a symlink transfer (#7458)
- `--compare-dest` must clear a destination its basis makes redundant; upstream
  leaves the entry absent where oc left it stale (#7464)
- Match upstream's hardlink notices on both alt-dest paths (#7435)
- Follow an existing destination symlink under `--no-implied-dirs` (#7461)
- Diagnose a local-copy source that ended before its declared length instead of
  committing the short result (#7472)
- Itemize the `-R` implied dot-dir root against the basis (#7505)
- Keep a directory's own metadata when `readdir` fails part-way (#7521)
- Make `--no-zero-copy` reach the content path it documents (#7482)
- Compare an alt-dest basis mtime the way `same_time` does, so a basis is
  not wrongly demoted (#7479)
- Degrade the anonymous commit to a named temp where anonymous is
  unavailable (#7478)
- Answer `am_root` from the process identity rather than a raw `geteuid`
  syscall. rustix's Linux backend emits the syscall instruction directly, which
  `fakeroot`'s `LD_PRELOAD` cannot interpose and the `--copy-as` privilege drop
  cannot re-sample, so the `--super`, device-creation and fake-super gates
  flipped in exactly the runs meant to validate them. Two other consumers in
  the same crate already asked the question the upstream way (#7574)
- Carry hard links through `--write-batch` and `--read-batch`. The writer set
  no hardlink identity on the entries it encoded, and the replay decoded the
  fields an upstream-written batch does carry and then acted on none of them,
  so every cell of the writer/replayer cross-matrix except upstream-to-upstream
  produced unlinked copies and the payload was written once per cluster member
  (#7596)

**Daemon**
- Collapse `..` in client paths instead of refusing the request (#7343)
- Keep the trailing slash that marks the destination a directory (#7411)
- Probe `nobody` then `nogroup` for the default privilege-drop group (#7342)
- The Landlock root must be the destination's directory (#7316)
- Honour a forwarded `--max-size` / `--min-size` on a push (#7449)
- Linger before closing a refused connection, so the client reads the refusal
  instead of an RST (#7457)
- Frame a post-OK client-argument rejection as a multiplex error rather than a
  bare close (#7466)
- Route `--early-input` to the early-exec hook, not the pre-transfer one (#7471)
- Mirror upstream's `daemon_usage` text in `--daemon --help` (#7444)
- Honour a peer-supplied `--partial-dir` on the receiving side, and keep a
  relative one relative as upstream does (#7498, #7501)
- Resolve string module defaults at end of parse, so a global set *after* a
  module section still applies to it (#7519)
- Honour the server options the daemon parser dropped, and accept the forwarded
  `--only-write-batch` (#7532, #7559)
- Exit 4 on a refused option, not 1 (#7563)
- Keep a relative `--backup-dir` relative. The daemon anchored the client's
  value eagerly at argument-parse time against the destination operand, so a
  push whose destination named a file died with `ENOTDIR` on a `mkdir` under
  that file. Upstream applies its rootdir prefix only to an absolute value and
  lets the receiver join a relative one against the directory
  `get_local_name()` left it in - the same split `--partial-dir` already needed
  (#7577)
- Honour a client's forwarded `--timeout`. With no `timeout` directive in the
  module the daemon armed nothing, so the client's own lever was inert and a
  wedged peer held the connection indefinitely. Upstream's rule is a minimum
  over the non-zero values, which is also what stops a peer *lengthening* a
  short operator-set timeout; the SSH server half parsed the same value and
  never read it (#7584)
- Resolve the module identity before the `chroot`, not after. A root daemon
  defaults every module without explicit numeric ids to `nobody:nobody`, and
  those NSS lookups cannot succeed inside the jail, so every chrooted module
  answered `@ERROR: invalid uid nobody` and served nothing. All four impure
  arms move together - hoisting only the uid would have left
  `@ERROR: invalid gid nobody` behind on a module with a numeric `uid` and a
  defaulted `gid`. Only the three privilege-drop syscalls still run after the
  chroot (#7585)
- Keep a dead `name converter` apart from an unknown name. The query returned
  `Option<String>`, collapsing five outcomes into one `None`: no answer, an
  empty answer, a name carrying a control byte, an over-long request, and a
  converter whose stream had broken. Upstream can return a bare `BOOL` only
  because two of those exit instead of returning (`clientserver.c:1324-1334`),
  so merging them here failed open on exactly the mechanism an operator
  installs to take ownership decisions away from the peer (#7629)

**CLI and output**
- Honour the `--` end-of-options marker in server-mode argv (#7402)
- Mirror upstream `parse_output_words` for `--info` / `--debug` tokens (#7412)
- Bound the out-format width scan like upstream 3.5.0, and treat `%%` as a
  literal percent in `log_format_has` (#7400, #7344)
- Report an attribute-only change by name, not "is uptodate" - two independent
  sites, `--info=name2` and `-vv`, each carried its own copy (#7407)
- Don't log attribute-only changes under a non-itemizing `--out-format` (#7401)
- Emit the non-regular skip notice once, not twice at `-v` (#7353)
- `--iconv` with an unopenable charset must exit 4 rather than transfer (#7311)
- Reject a `-M` value that does not start with a dash (#7413)
- Re-port `SHELL_CHARS` to 3.5.0 and stop escaping daemon path operands (#7399)
- `--chmod=a+s` must set both setuid and setgid (#7287)
- Print `skipping directory` at default verbosity, in upstream's order (#7433)
- Exit `RERR_SOCKETIO` when `RSYNC_CONNECT_PROG` refuses the host (#7437)
- Let a connect program finish instead of killing it, which was destroying the
  daemon's post-transfer exec (#7470)
- Take the `rsync://` daemon host verbatim; three separate rewrites were being
  applied to it (#7431)
- Honour `--port` on a daemon transfer, not only on a listing (#7456)
- Accept `--log-file` as a server long flag instead of letting it leak into
  the destination operand (#7507)
- Honour `--drop-D` in the server argument decoder, and consume
  `--backup-dir` / `--temp-dir` instead of discarding them (#7508, #7511)
- List a remote source under `--list-only` (#7509)
- Carry `--checksum-seed` as a signed int, matching upstream's type, in the CLI
  (#7561) and in the protocol-state module, whose `u32` made half the seed
  domain unrepresentable and which every existing test missed by using a seed
  with the high bit clear. The local-copy options carried a second, never-read
  copy of the seed with a getter documenting behaviour it did not have; it is
  deleted (#7606)
- Print the non-incremental `building file list ... done` banner. Upstream
  picks exactly one of two banners before it lists a single entry, and oc only
  ever produced the incremental arm, so `-dv` and `--list-only` opened with no
  banner at all. It is two writes on two log codes, separated by the whole
  file-list build, and it is emitted above the `--list-only` early return where
  upstream places it - the previous emission site sat below that return, so a
  fix there would have looked right and done nothing (#7580)
- Send the server's exit code to the peer via `MSG_ERROR_EXIT`, and honour a
  peer's on the SSH client as the daemon client already did. `run_server_mode`
  collapsed every failure to a flat 1 and dropped the connection, so a client
  graded the run by the truncated stream and reported 12 where upstream reports
  3. Upstream's `am_receiver` half of the send gate is a forked-sibling relay
  condition rather than a wire one, so the port gates on the protocol version
  alone (#7609)

**Metadata**
- An installed name converter must replace the host database (#7360)
- Follow the operator's own symlinked destination root when applying metadata
  (#7462)
- Condense the fake-super access ACL to upstream's stored form (#7502)
- Answer upstream's `am_root` for the receiver xattr screen (#7562)

### Testing and CI

- The rsync 3.5.0 upstream testsuite now runs on **macOS** as a fifth leg
  (non-root, stdio pipe, full 345-cell corpus), with its own committed
  expect-manifest. It is the only leg that can observe a platform-conditional
  divergence: three of its cells *skip* on Linux, so they had never executed in
  this repository's CI at all (#7638)
- Build the old-rsync oracle binaries the 3.5.0 testsuite asserts against,
  rather than silently falling back to a weaker substitute (#7636)
- Bound the interop smoke harness's readiness probe so its deadline is real
  (#7640)
- Pin the `want_i` adjacent-match length re-check, the `preserve_hard_links`
  gate on both wire encodings, and the reserved slot below the daemon-argument
  ceiling (#7643, #7644, #7645)

- The 3.5.0 expect-manifest gate is proven non-vacuous on outcomes, with a
  whole-suite coverage guard so a silently deleted row cannot pass (#7387)
- Guard-page over-read harness for the SIMD rolling checksum, carrying its own
  negative control (#7294)
- Confined-walk resolver suite; fixes three macOS failures and the CI gap that
  hid them - the macOS cell omitted `fast_io`, the one crate whose entire
  purpose is platform-specific I/O (#7325)
- `test-support` rejects a stale `oc-rsync` binary instead of reporting it as a
  regression, asserting freshness from Cargo's own depfile (#7385)
- Umask-dependent mode expectations are derived rather than pinned to constants
  (#7354, #7348)
- Daemon test readiness is gated on IPv4 reachability, not any listener (#7370)
- The required interop context no longer reports a vacuous green: a duplicated
  copy of the upstream test suite that could only ever hide a failure was
  deleted, and the two non-blocking 2.6.9 parity cells now report their
  pre-`continue-on-error` outcome to the job summary (#7352)
- CI triggers on `merge_group` so a merge queue can validate (#7312)
- Regression tests for the benchmark report renderer, covering the peak-RSS
  columns, the missing-measurement fallback, and the requirement that a bytes
  metric is not described in duration language ("higher", not "slower").
- Every required check now has exactly one publishing workflow. The skip shim
  and the real workflow could both claim a context on a pull request that mixed
  code and docs, and the shim's green would win - a vacuous pass on a required
  gate (#7438)
- The batched-writer threshold tests disarm the flush clock instead of racing
  it, which is what made the musl beta cell flake (#7469)
- Default the upstream testsuite runner to 3.5.0 (#7486)
- Publish the testsuite binary to a per-run path rather than a shared one,
  so two concurrent runs cannot test each other's build (#7514)
- Key the parallel receive-delta fuzz oracle on every registered file, and
  let the caller name the upstream rsync the benchmark compares against
  (#7480, #7481)
- The format check now covers every tracked `.rs` file. `cargo fmt` is blind to
  files reached through `include!()`, and blind again to files reached through
  nothing at all, so a formatted workspace was not a formatted tree (#7529,
  #7542)
- Unsafe blocks are attributed to the right crate and the right policy category
  (#7544)
- Pull-request labels are reconciled against the title instead of accumulating
  (#7549)
- Behaviour that was previously assumed is now witnessed: the INC_RECURSE
  segment count and segment arrival (#7528), the fd-exhaustion hint and which
  caller it serves (#7530, #7566), delete routing refusing on all six methods
  (#7551), the `--delay-updates` staging sequence under the worker filter
  (#7554), io_uring drop-contract registration failures (#7558), and protocol
  negotiation environment overrides (#7575)
- The local-recursion tests are named for what they actually run (#7526)
- Three daemon integration suites ran a detaching daemon, so `become_daemon()`
  forked and the test binary - the parent of that fork - exited 0 before any
  assertion ran: 3 filter-directive tests, 25 server tests and the
  max-connections cap test all reported green without executing, in hundredths
  of a second. Turning them on surfaced the digest name list the fixtures
  omitted - mandatory above protocol 31, so every probe was refused at the
  greeting - a listing shape expecting a `CAP` and an `OK` line upstream never
  sends, auth cases that negotiated md5 whatever digest they claimed to
  exercise, guards whose conditions could not hold, and a module-refused push
  whose outcome arrives as exit 23 rather than as an error and so was never
  asserted at all. `check_daemon_no_detach.sh` now also counts root-level
  tests, against a shrink-only ceiling (#7578, #7588, #7592)
- The backup ladder's destination anchor is pinned at every consumer - the
  hard-link tier, the rename tier, the `--backup-dir` root create, the metadata
  options, and the three call sites that build the environment - by swapping
  the destination root's path for an out-of-tree symlink after the sandbox has
  pinned its dirfd. Every pre-existing test passed `BackupEnv::default()`, so
  none of them could tell a threaded environment from a dropped one (#7608)
- INC_RECURSE's slicing order is witnessed: the whole list is materialised
  before it is partitioned, so peak sender-side residency is taken before the
  first segment ships and no downstream reclaim can lower it. The two existing
  reclaim tests hand-assign their segments and say nothing about what
  production builds (#7611)
- `session_error_does_not_stop_the_accept_loop` provoked its session error with
  a repeated `@RSYNCD:` banner, which #7579 deliberately turned into a valid
  module name; it now uses a non-UTF-8 request line, and its assertion reports
  the greeting, both peers' results and the daemon log instead of discarding
  them. The fixture's own non-vacuity guard is what caught the change - two
  individually-correct commits that had never been run together (#7615, #7620)
- A failed io_uring buffer registration carries the kernel's `ErrorKind`
  instead of flattening it to `io::Error::other`, so the drop-contract probe
  can tell an `ENOMEM` locked-memory refusal - which says nothing about
  recovery - from the `EBUSY` that means a registration is still live. The
  errno had survived only inside the message string, so no caller could branch
  on it and the probe could not be given its siblings' tolerance (#7619)
- The backup-directory cells probe the directory they claim to check instead of
  asserting a condition that held whether or not the backup landed (#7621)
- The citation drift gate scans the 42% of citations its filter could not see,
  reported non-blocking until the backlog it exposes is worked down (#7623)
- The APT package cache verifies the packages are installed rather than
  trusting a cache hit, so a restored-but-empty cache fails its own job instead
  of reddening unrelated pull requests three jobs later (#7632)

### Documentation

- The upstream-testsuite figures in `README.md` and `SECURITY.md` were stale in
  every row. Both files are re-derived from the committed manifests - the pipe
  legs are at one divergence, not three; the distinct-failure count across all
  manifests is 23, not 29; all four Linux legs are required contexts, not two;
  and the macOS leg was missing from both tables entirely. `SECURITY.md` also
  described `proxy protocol hosts` as unimplemented after it had shipped. The
  two macOS-leg rationale comments in the workflows carried the same pre-fix
  figures and are recounted from the same source
- Replace the INC_RECURSE gate's stale rationale with the measured one. The
  comment named a mechanism that cannot apply - every call site of the function
  it blamed is inside `cfg(test)` - while its conclusion was nonetheless
  correct: the deadlock boundary is exactly upstream's `MIN_FILECNT_LOOKAHEAD`
  of 1000, bisected by entry count (#7649)
- Document the public items rustdoc could not see (#7634)

- Corrected stale upstream-version and CI-gate claims across the contributor
  docs: five required checks where the ruleset returns eight, a claim of one
  approving review where the count is zero, three sites naming rsync 3.4.1 as
  the source of truth, and a hardcoded interop version list that had drifted
  from the one the harness actually reads (#7351)
- The upstream 3.5.0 rrsync rule set extracted into a spec; it forces `--drop-D`,
  not `--no-D` (#7283)
- Path-confinement resolver API design note, and a spec for the receiver
  peer-tail confinement fail-open (#7310, #7330)
- Corrected several upstream claims that were stated backwards or cited the
  wrong construct: `RESOLVE_BENEATH` where the code omits it, the munge /
  safe-links ordering, the daemon `sanitize_path` `..` behaviour, and the 3.5.0
  new-option count (#7307, #7334, #7335, #7284)
- Fixed the intra-doc links breaking the rustdoc and Pages builds (#7338,
  #7406)
- Upstream's `iobuf` / `perform_io` contract extracted into a design note, with
  every anchor verified by reading rather than assumed (#7428)
- Retargeted the `check_alt_basis_dirs` citations at 3.5.0 (#7452)
- Close rustdoc gaps, repair references that no longer resolve, and add a
  comment-policy audit (#7488, #7489, #7522)
- Refresh the changelog and re-measure the 3.5.0 outcome figures (#7477)
- The 3.5.0 outcome figures and divergence counts are re-derived from the
  committed manifests rather than transcribed from a run log (#7525, #7533)
- Upstream's `struct file_list` mapped field-by-field onto the oc types (#7527)
- Citations retargeted at the 3.5.0 pin for the `successful_send` cluster and
  for the backup ladder (#7540, #7560)
- The four rustdoc links breaking the Pages build resolved (#7555)
- Line coverage is stated as not gated by CI (#7573)
- Upstream citations retargeted at the 3.5.0 pin across the remaining crates -
  `matching`, `checksums`, `metadata`, `compress` and `batch`, then `engine`,
  `daemon`, `core` and `protocol`, then `transfer` - each by locating the named
  construct at the pin and re-deriving both ends of every range, never by
  arithmetic. The sweep turned up citations that were wrong before they were
  stale: a `del.c` that has existed at no release, an `rsum.c` path pointing
  into the rsync tarball for a file that belongs to zsync, two function ranges
  that over-spanned at their own 3.4.1 baseline, and three `options.c` ranges
  naming the wrong construct there too. Neither existing gate can see this
  class - one only range-checks, the other only inspects citations carrying a
  quoted C string. 3.5.0 also rewrote the shell testsuite as Python, so the
  cited `testsuite/*.test` locators move to their `*_test.py` counterparts
  (#7601, #7603, #7604)
- The daemon's two refusal exit-code citations retargeted at 3.5.0. The old
  `clientserver.c:1183` was a closing brace that calls nothing, and `main.c:935`
  an `exit_cleanup(RERR_PROTOCOL)` on an unrelated check. Neither gate could
  have found them: one shared its quoted anchor with a sibling citation on the
  same line, and the other two carry no anchor at all (#7586)
- `README.md` and `SECURITY.md` still described a 3.4.4-gated world. The
  testsuite outcome figures are re-derived from the committed manifests, with
  the command that produces each row named so the next refresh is a re-run
  rather than a re-transcription; the copied interop version lists now point at
  the script that owns them; and the claim that the 3.4.4 corpus passes with an
  empty known-failures roster is withdrawn, since no workflow runs that corpus
  (#7591)
- Item prose stranded between a rustdoc block and the item it describes is
  promoted to `///`. rustdoc stops at the first non-doc line, so 154 upstream
  citations were invisible in the generated docs while reading, in source, as
  though they were part of the block. Runs above an item that has no rustdoc
  are deliberately left alone - promoting those would newly document the item
  (#7613)
- `missing_docs` is denied on `matching`, `test-support` and `windows-gnu-eh`,
  the three crates that carried only the broken-link lint. All three were
  already fully documented - measured by compiling each with the lint, then
  falsifying that zero with a planted undocumented item - so this adds no
  prose; it converts "currently documented" into "cannot regress" (#7614)
- Public documentation no longer links at private items, which had been failing
  the Pages build, and the invocation Pages uses now also runs on every pull
  request. Four pull requests had repaired this class post-merge because
  `cargo doc` ran only on push-to-master, so a broken link was discoverable
  only after it landed (#7618)
- README and SECURITY claims that no longer matched the tree are regrounded
  against it (#7626)
- The `io_uring` feature no longer promises 20-40% faster I/O. Measured on
  kernel 7.1.5 it delivers +1.2% on bulk and -1.2% on fan-out, so the number
  described an expectation rather than the build it was attached to (#7631)

### Maintenance

- Dependency and action updates (#7473, #7474, #7568, #7569, #7570, #7571,
  #7572)
- Give the in-place open chain a single owner with the resolver injected,
  and the bounded drain-and-wait helper a single owner (#7487, #7504)
- Update the Homebrew formulas for v0.6.4 (#7492)

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
