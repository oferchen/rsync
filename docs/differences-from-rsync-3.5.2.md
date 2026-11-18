# Differences from rsync 3.5.2

oc-rsync aims to behave exactly like rsync 3.5.2: the same options, wire bytes,
messages and exit codes. This page lists the places where it does not, on
purpose. Anything that differs and is not listed here is a defect; please
report it.

The page has four parts:

1. [Behaviour kept on purpose](#1-behaviour-kept-on-purpose)
2. [Upstream fixes adopted ahead of an rsync release](#2-upstream-fixes-adopted-ahead-of-an-rsync-release)
3. [oc-rsync extensions](#3-oc-rsync-extensions)
4. [Platform differences](#4-platform-differences)

Performance work that changes no output, exit code or wire byte (parallel
hashing, io_uring, reflinks, buffer pools) is not a difference in this sense
and is described in the [README](../README.md).

## 1. Behaviour kept on purpose

Each row says what oc-rsync does, why, and how to see it.

| What | Why | How to observe |
|---|---|---|
| Protocols 28 to 33 only. rsync 3.5.2 still accepts peers down to protocol 20. | Protocols below 28 belong to rsync releases older than 2.6.1 (2004) and are not tested against. | A connection that would run at protocol 27 or lower is refused. `--protocol=20` to `27` is still accepted on a local copy, where the value has no effect. |
| A daemon `hosts deny` rule that names an address refuses a peer whose address text cannot be parsed (for example `08.1.2.3`, taken from the environment of a daemon run over a remote shell). rsync 3.5.2 lets such a peer through, because the address never matches. | A deny rule that cannot be proved not to name the peer must refuse it. `hosts allow` still admits only a peer it can prove. | Run a daemon over a remote shell with `hosts deny = 8.1.2.3` and a peer reported as `08.1.2.3`: oc-rsync refuses the connection. |
| A daemon `hosts deny` rule that names a host refuses a peer whose name could not be resolved. | oc-rsync applies `daemon chroot` once at startup, so a reverse lookup can fail inside a chroot that has no resolver configuration. Failing open there would disarm the rule. | With `hosts deny = bad.example.org` and reverse lookup unavailable, the connection is refused. |
| The daemon's default syslog tag is `oc-rsyncd`. rsync uses `rsyncd`. | Hosts that run both daemons keep their log entries apart. | `journalctl -t oc-rsyncd`. Set `syslog tag = rsyncd` to get rsync's tag. |
| `--version` prints an oc-rsync banner: the oc-rsync version and revision, a `Compatible with rsync ...` line, a `Source:` line where rsync prints `Web site:`, a toolchain line, and extra capability and optimization entries. | The banner must say which program is running and which rsync release it is compatible with. | `oc-rsync --version`. |
| A server built without ACL or xattr support that is asked for `-A` or `-X` drops the option, warns, and carries on. rsync 3.5.2 refuses the transfer. | The rest of the transfer is still useful, and the warning names what was not preserved. | Push `-A` to an oc-rsync built without the `acl` feature: the warning appears and the files are copied. |
| On macOS, a file name is compared in Unicode NFC form when a destination directory is matched against the file list. rsync converts names only under `--iconv`. | APFS and HFS+ can return a name in a different normalization form than the sender used, which would make an existing file look new. | Names that differ only in normalization form are matched as the same file on a macOS destination. |

## 2. Upstream fixes adopted ahead of an rsync release

These are fixes that the rsync project has accepted, or has open as a pull
request, and that are not in rsync 3.5.2.

No fix of this kind is in this release yet.

Pending, not in this release:

- Permission changes the destination refuses become non-fatal when
  permissions were not asked for (rsync pull request 1178).
- When the sender declines the requested member of a hard-link group, another
  member is requested instead of leaving the group untouched (rsync pull
  request 1179).
- A filter or merge file that cannot be read in full ends the run with an
  error instead of being accepted silently (rsync pull request 1165).

## 3. oc-rsync extensions

Nothing in this part changes how oc-rsync talks to rsync. Every extension is
either local to the process that sets it, or is negotiated so that it stays
off against rsync.

### Options

`oc-rsync --help` lists these under their own heading, `oc-rsync extensions`,
after the options shared with rsync. They are never sent to an rsync peer.

| Group | Options |
|---|---|
| Connection | `--connect-program`, `--tcp-fastopen`, `--password-command` |
| Embedded SSH client (`ssh://`) | `--aes`, `--no-aes`, `--ssh-cipher`, `--ssh-connect-timeout`, `--ssh-keepalive`, `--ssh-identity`, `--ssh-no-agent`, `--ssh-strict-host-key-checking`, `--ssh-ipv6`, `--ssh-port`, `--jump-host` |
| QUIC transport (`quic://`) | `--quic`, `--quic-ca`, `--quic-cert`, `--quic-key`, `--quic-cc`, `--quic-window`, `--quic-cipher` |
| I/O backends | `--io-uring`, `--no-io-uring`, `--no-io-uring-sqpoll`, `--io-uring-depth`, `--zero-copy`, `--no-zero-copy`, `--cow`, `--no-cow`, `--reflink`, `--sparse-detect` |
| CPU and threads | `--simd`, `--parallel-delta`, `--no-parallel-delta`, `--rayon-threads`, `--checksum-threads`, `--tokio-threads` |
| macOS | `--apple-double-skip` |
| Status reports | `--io-uring-status`, `--lsm-status` |

The man page, [`oc-rsync.1.md`](./oc-rsync.1.md), describes each one.

### Transports

- **Embedded SSH client.** An `ssh://` operand connects with a built-in SSH
  client instead of running `ssh`. It reads `~/.ssh/config`. See
  [`ssh-transport.md`](./ssh-transport.md) and
  [`ssh-config-embedded.md`](./ssh-config-embedded.md). A `host:path` operand
  still runs the remote shell, as rsync does.
- **QUIC.** A `quic://` operand, or `--quic`, reaches an oc-rsync daemon over
  QUIC. Only oc-rsync daemons listen on it. See
  [`quic-transport.md`](./quic-transport.md).

### Daemon configuration

These `oc-rsyncd.conf` parameters do not exist in rsync, which logs an unknown
parameter and ignores it:

- `parallel delta scan`
- `quic cert file`, `quic key file`, `quic client ca file`, `quic port`,
  `quic retry`, `quic address`

### Output

`--debug` accepts seven words that rsync does not have: `IOURING`, `CLONE`,
`SOCKOPT`, `IOCP`, `SECCOMP`, `LANDLOCK` and `PIPELINE`, plus `SANDBOX` as a
short form for `SECCOMP` and `LANDLOCK`. `-v` and `-vv` print exactly what
rsync prints. From `-vvv`, the `SECCOMP` and `LANDLOCK` lines are added.

### Sandboxing

On Linux, an oc-rsync server and daemon restrict themselves to the transfer's
directories with Landlock where the kernel supports it. A daemon built with
the `daemon-seccomp` feature also installs a system-call filter. rsync does
neither.
`OC_RSYNC_NO_LANDLOCK`, `OC_RSYNC_DAEMON_LANDLOCK`, `OC_RSYNC_NO_SECCOMP` and
`OC_RSYNC_DAEMON_SECCOMP` turn the layers off; `oc-rsync --lsm-status` reports
what is active.

### Environment variables

[`oc-extension-env-reference.md`](./oc-extension-env-reference.md) is the
reference: it gives each variable's values, default and scope. None of them is
read by rsync. Only `OC_CONSECUTIVE_MATCH`, described below, can change wire
bytes.

| Area | Variables |
|---|---|
| Buffer pool | `OC_RSYNC_BUFFER_POOL_SIZE`, `OC_RSYNC_BYTE_BUDGET`, `OC_RSYNC_BUFFER_POOL_MEMORY_CAP`, `OC_BUFFER_POOL_BLOCK_SIZE`, `OC_RSYNC_BUFFER_POOL_STATS` |
| Concurrency | `OC_RSYNC_DISK_COMMIT_CHANNEL_CAP`, `OC_RSYNC_PIPELINE_WINDOW`, `OC_RSYNC_PARALLEL_STAT_THRESHOLD`, `OC_RSYNC_PARALLEL_CHECKSUM`, `OC_RSYNC_PARALLEL_DELTA_SCAN` |
| I/O backends | `OC_RSYNC_DISABLE_IOURING`, `OC_RSYNC_ZERO_COPY`, `OC_RSYNC_MMAP_TO_SQPOLL_THRESHOLD_BYTES`, `OC_RSYNC_ADAPTIVE_BASIS_DISPATCH`, `OC_RSYNC_WIN_CHUNK_BYTES`, `OC_RSYNC_WINDOWS_RIO`, `OC_RSYNC_FORCE_ROOTLESS_CONTAINER` |
| Daemon | `OC_RSYNC_CONFIG`, `OC_RSYNC_SECRETS`, `OC_RSYNC_DAEMON_ADDRESS_FAMILY`, `OC_RSYNC_ASYNC_DAEMON`, and the four sandbox variables above |
| Transports | `OC_RSYNC_ASYNC_SSH`, `OC_RSYNC_QUIC_CC`, `OC_RSYNC_QUIC_WINDOW` |
| File list | `OC_RSYNC_LAZY_FLIST`, `OC_RSYNC_PULL_INC_RECURSE` |
| Branding | `OC_RSYNC_BRAND` |

### Consecutive-match delta (opt-in, oc-rsync to oc-rsync)

`OC_CONSECUTIVE_MATCH=1` is the one extension that changes wire bytes. It
shortens the per-block strong checksum by requiring a block match to follow a
matching neighbour. It engages only when both ends are oc-rsync and both set
the variable. It is off by default. Against rsync, or against an oc-rsync that
has not set it, the transfer is byte-identical to rsync's. Details are in the
[environment reference](./oc-extension-env-reference.md#oc_consecutive_match)
and in [`design/zsync-inspired-matching.md`](./design/zsync-inspired-matching.md).

To observe it, run the same transfer with and without the variable on both
ends and compare `--stats`: the bytes sent by the receiving side drop.

## 4. Platform differences

rsync 3.5.2 has no native Windows build; it runs there under Cygwin or MSYS2.
oc-rsync builds natively on Windows and macOS, and some features differ by
platform. The platform documents are the reference:

- [Platform support](./user/platform-support.md): tiers, I/O and metadata
  matrices, and the known limitations for Windows, macOS, Linux musl and
  aarch64.
- [Windows support matrix](./user/windows-support-matrix.md) and
  [Windows feature matrix](./user/windows-feature-matrix.md): what each option
  does on Windows.
- [ACLs and extended attributes across platforms](./user/xattr-acl-cross-platform.md).
- [Platform notes](./platform-notes.md): Windows behaviour in detail.
