# Audit: `--fake-super` and `--super` privilege paths

Closes #1839.

## Scope

Audit oc-rsync's `--fake-super`, `--super`, and `--copy-as` implementations against upstream rsync 3.4.1 to identify divergences in privilege handling that could cause backup/restore round-trips, daemon module behaviour, or file-creation safety to deviate from upstream.

Upstream code referenced (relative to `target/interop/upstream-src/rsync-3.4.1/`):

- `options.c` — option parsing for `-M--fake-super`, `--super`, `--copy-as`.
- `rsync.c` — `am_root` and `local_server` / `read_only` semantics.
- `xattrs.c:340-720` — `set_stat_xattr` / `read_stat_xattr` round-trip via `user.rsync.%stat`.
- `syscall.c:90-174` — `do_open_nofollow` / `do_mkstemp` / `do_mknod` placeholder substitution under `am_root < 0`.
- `clientserver.c:1080-1107` — daemon `fake super = yes` demotes client `am_root` and rewrites `--super` to `--fake-super`.
- `receiver.c`, `generator.c`, `main.c` — sender/receiver-side dispatch.

## Findings summary

| # | Severity | Area | Title |
|---|----------|------|-------|
| F1 | HIGH | metadata | `load_fake_super` is exported but never called |
| F2 | HIGH | engine/local_copy | Special-file executors invoke real syscalls unconditionally |
| F3 | HIGH | daemon | `fake super = yes` is parsed but no read site demotes `am_root` |
| F4 | MEDIUM | cli | No `-XX` / repeated `--fake-super` validation |
| F5 | MEDIUM | metadata | chmod path doesn't honour fake-super; touch-up-dirs missing |
| F6 | MEDIUM | metadata | `am_root()` is binary instead of upstream tri-state |
| F7 | MEDIUM | core | Remote invocation builder unconditionally pushes `--fake-super` |
| F8 | MEDIUM | metadata | `--copy-as` is parsed but never calls `setuid`/`setgid` |
| F9 | LOW | metadata | `FakeSuperStat::encode` skips `to_wire_mode` |
| F10 | LOW | metadata | rdev `(0,0)` is decoded to `None` and lost on round-trip |

Total: 3 HIGH, 5 MEDIUM, 2 LOW.

## Implementation map

| Concern | Upstream | oc-rsync |
|---|---|---|
| `--fake-super` option | `options.c` `OPT_FAKE_SUPER` | `crates/cli/src/arguments/parser.rs` |
| `user.rsync.%stat` xattr name | `xattrs.c` `XSTAT_ATTR` | `crates/metadata/src/fake_super.rs` `FAKE_SUPER_XATTR` |
| Encode privileged stat → xattr | `xattrs.c:set_stat_xattr` | `metadata::fake_super::store_fake_super` |
| Decode xattr → stat | `xattrs.c:read_stat_xattr` | `metadata::fake_super::load_fake_super` |
| `am_root < 0` placeholder mode | `syscall.c:90-174` | (missing — see F2) |
| Daemon `fake super = yes` | `clientserver.c:1080-1107` | `crates/daemon/src/rsyncd_config/parser.rs:319` (parsed only) |
| `--copy-as` privilege drop | `rsync.c:do_as_root()` | `LocalCopyOptions::with_copy_as` (parsed only) |
| Remote invocation flag pass-through | `options.c:server_options` | `crates/core/src/client/remote/invocation/builder.rs:346-347` |

## Findings

### F1 (HIGH) — `load_fake_super` is exported but never called

**Evidence.** `pub fn load_fake_super(path: &Path) -> io::Result<Option<FakeSuperStat>>` is defined at `crates/metadata/src/fake_super.rs:199` and re-exported from `crates/metadata/src/lib.rs:204`. A workspace-wide search for callers turns up only the definition, the non-Linux stub at `crates/metadata/src/fake_super.rs:321`, and one stub-test at `crates/metadata/src/fake_super.rs:483-485`. No production call site reads the `user.rsync.%stat` xattr to reconstruct a `FakeSuperStat`.

**Impact.** Files written by an oc-rsync sender with `--fake-super` carry the `user.rsync.%stat` xattr verbatim, but a subsequent `oc-rsync --fake-super` restore never reads it back. Restored files end up with the live receiver-process uid/gid/perms instead of the originals encoded in the xattr. Round-trip with upstream rsync's `--fake-super` is silently broken in one direction.

**Recommended fix.** Wire `load_fake_super` into the receiver/generator metadata-application path, ahead of `apply_metadata_from_file_entry`, so a restored entry's stat fields are taken from the xattr when present. Mirror upstream `xattrs.c:read_stat_xattr` (called from `set_file_attrs`).

---

### F2 (HIGH) — Special-file executors invoke real syscalls unconditionally

**Evidence.** `crates/engine/src/local_copy/executor/special/{device,fifo,symlink}.rs` create the destination using `mknod`, `mkfifo`, and `symlinkat` directly. Upstream `syscall.c:90-174` (`do_open_nofollow`, `do_mkstemp`, `do_mknod`) substitutes a regular `0600` placeholder file when `am_root < 0` (the in-memory marker for `--fake-super`); the privileged stat is then carried in the `user.rsync.%stat` xattr, not in the inode.

**Impact.** Under `--fake-super`, oc-rsync attempts privileged syscalls (`mknod`) that fail without `CAP_MKNOD`, breaking `--fake-super` backups of device nodes and FIFOs on unprivileged accounts — the entire reason the feature exists.

**Recommended fix.** Introduce a `should_fake_super(opts) → bool` helper and gate the special-file executors so that, when fake-super is active, they create a 0600 regular placeholder via the temp-file path and then call `store_fake_super` to record the original stat. Cite `syscall.c:do_mknod()` in the new branch.

---

### F3 (HIGH) — Daemon `fake super = yes` is parsed but no read site demotes `am_root` or rewrites `--super`

**Evidence.** Parser writes the value at `crates/daemon/src/rsyncd_config/parser.rs:319-320`, and tests assert it round-trips (`crates/daemon/src/rsyncd_config/tests.rs:118,493,507`). But there is no consumer that, on connection setup, (a) demotes the receiving process's effective root view, (b) rewrites the negotiated `--super` flag from the client into `--fake-super`, or (c) refuses real ownership preservation. Upstream does all three at `clientserver.c:1080-1107`.

**Impact.** A daemon module configured with `fake super = yes` (the canonical "give me fake-super semantics regardless of what the client asks for" knob) silently behaves as if the option were absent. Operators relying on this directive for unprivileged daemon backups get real-uid/gid behaviour with no warning.

**Recommended fix.** In the daemon module-handoff path, when `module.fake_super()` is true:

1. Force the receiver-side `LocalCopyOptions::fake_super(true)`.
2. Strip any incoming `--super` flag from the negotiated args.
3. Lower the reported `am_root` state to the upstream "fake" tri-state (see F6).

---

### F4 (MEDIUM) — No `-XX` / repeated `--fake-super` validation

**Evidence.** Upstream accepts `-MM` / `--fake-super` repeatedly without diagnostic, but `-XX` (`--xattrs --xattrs`) is treated specially: it enables `XATTR_NOSYS_IS_OK`. oc-rsync's CLI parser does not distinguish `-X` from `-XX`, and silently accepts `--fake-super` repetition without error.

**Impact.** A user passing `-XX` (a documented upstream convenience) gets the same behaviour as `-X`, and `--fake-super --fake-super` does not toggle anything. Lower severity because no documented upstream behaviour is altered besides `-XX`.

**Recommended fix.** Track flag-occurrence counts in the parser; map `-X` count ≥ 2 to set the `xattr_nosys_is_ok` flag. Document repetition handling in the CLI rustdoc.

---

### F5 (MEDIUM) — chmod path doesn't honour fake-super; touch-up-dirs missing

**Evidence.** `apply_chmod_to_dest` at `crates/metadata/src/apply/permissions.rs` calls `fchmodat` directly. Under fake-super, upstream stores chmod-derived modes in `user.rsync.%stat` instead. There is also no equivalent of upstream `generator.c:touch_up_dirs()` that re-applies parent-directory metadata at the end of the transfer.

**Impact.** With `--chmod` + `--fake-super`, mode bits are written to the inode (often failing silently for non-owner) instead of being captured in the xattr. Parent-directory mtime/perms are not retouched after recursive transfers.

**Recommended fix.** Route `--chmod` through `store_fake_super` when fake-super is active. Add a post-transfer pass that walks the synced directory tree and applies deferred metadata (matches upstream `touch_up_dirs`).

---

### F6 (MEDIUM) — `am_root()` is binary instead of upstream tri-state

**Evidence.** `metadata::am_root()` returns `bool`. Upstream `am_root` is a tri-state `int`: `1` (real root), `0` (regular user), `-1` (fake-super marker forced by `--fake-super` or daemon `fake super = yes`). Several upstream branches discriminate on the negative case (`syscall.c`, `generator.c`).

**Impact.** Code that needs the "fake-super forces unprivileged behaviour even if EUID == 0" branch cannot express it. Today the implementation falls back to checking the option flag directly, which works but couples privilege checks to option layout.

**Recommended fix.** Introduce `enum AmRoot { Real, Regular, Fake }` (or `Option<AmRootState>`) and replace `am_root() -> bool` callers. Cite `rsync.c:am_root` in rustdoc.

---

### F7 (MEDIUM) — Remote invocation builder unconditionally pushes `--fake-super`

**Evidence.** `crates/core/src/client/remote/invocation/builder.rs:346-347` appends `--fake-super` to the remote arg list whenever the local option is set, without considering whether the remote is the sender or receiver. Upstream only sends `--fake-super` to the remote when the *remote* is doing the receiving (it has no effect on the sender side).

**Impact.** When oc-rsync is the receiver pulling from a remote upstream sender with `--fake-super` set locally, the flag is pushed to the remote sender, which ignores it. Harmless today but wastes wire bytes and could confuse a strict argument-validating remote.

**Recommended fix.** Match upstream `options.c:server_options`: only forward `--fake-super` to the remote when the remote is the receiver (i.e., we are the sender or doing a daemon push). Add a regression test that diffs our remote-invocation arg vector against an upstream `rsync -e ... --fake-super` invocation captured via `RSYNC_CHECKSUM_LIST=…` or `strace`.

---

### F8 (MEDIUM) — `--copy-as` is parsed but never calls `setuid`/`setgid`

**Evidence.** `LocalCopyOptions::with_copy_as` at `crates/engine/src/local_copy/options/metadata/setters.rs:64` stores a `CopyAsIds`, but no executor or daemon path actually calls `setuid`/`setgid`/`setgroups` to switch identity around file I/O. Upstream brackets every privileged file op with `do_as_root()` (`rsync.c`).

**Impact.** `--copy-as=user[:group]` is accepted on the command line and silently has no effect. A user expecting the upstream behaviour (write files as a specific uid/gid via temporary euid switch) gets ownership of the running process instead.

**Recommended fix.** Implement an RAII guard `CopyAsGuard::enter(ids) -> guard` that switches euid/egid via libc and restores in `Drop`. Wrap the receiver's file-creation and metadata-application sections. Audit re-entrancy under rayon (the guard must be thread-local or per-task).

---

### F9 (LOW) — `FakeSuperStat::encode` skips `to_wire_mode`

**Evidence.** `FakeSuperStat::encode` at `crates/metadata/src/fake_super.rs` writes the raw `mode_t` to the xattr buffer. Upstream `xattrs.c:set_stat_xattr` runs the mode through `to_wire_mode()` to strip filesystem-specific bits.

**Impact.** When the source mode contains `S_IFMT` bits or filesystem-specific bits that upstream would mask, oc-rsync's xattr round-trip can carry garbage that upstream restoring side will misinterpret. Low probability in practice because most stat sources already give canonical bits.

**Recommended fix.** Apply `to_wire_mode` (or the rust equivalent) before encoding. Add a property test asserting `encode(decode(buf)) == buf` for arbitrary `mode_t`.

---

### F10 (LOW) — rdev `(0,0)` is decoded to `None` and lost on round-trip

**Evidence.** `FakeSuperStat::decode` returns `rdev: None` when the encoded major/minor are both zero. A regular file legitimately has `rdev == (0,0)` and that distinction is lost on round-trip.

**Impact.** A device node `mknod /dev/null c 1 3` encoded then decoded keeps its rdev. A regular file with `rdev (0,0)` (the usual case) decodes to `None` — fine for files, but the type info is now smuggled through `S_IFMT` only. If the mode is corrupted (F9), the file type is unrecoverable.

**Recommended fix.** Encode rdev unconditionally and let the consumer use `S_IFMT(mode)` to decide whether to apply it. Couple with F9 fix.

## Follow-up tasks (file as separate issues)

| # | Title |
|---|---|
| 1 | Wire `load_fake_super` into receiver metadata-application path (F1) |
| 2 | Substitute 0600 placeholder in special-file executors under fake-super (F2) |
| 3 | Implement daemon `fake super = yes` consumer (F3) |
| 4 | Add `-XX` repetition handling in CLI parser (F4) |
| 5 | Route `--chmod` through fake-super xattr encoding (F5) |
| 6 | Replace `am_root() -> bool` with tri-state enum (F6) |
| 7 | Gate remote `--fake-super` pass-through on receiver direction (F7) |
| 8 | Implement `--copy-as` setuid/setgid bracket (F8) |
| 9 | Apply `to_wire_mode` in `FakeSuperStat::encode` (F9) |
| 10 | Encode rdev unconditionally in `FakeSuperStat` (F10) |
