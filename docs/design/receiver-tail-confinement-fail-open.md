# Receiver peer-tail confinement: the fail-open, and how to close it

Implementation spec for U350-4d.2. Companion to
`upstream-3.5.0-path-confinement-model.md` (the model) and
`path-confinement-resolver-api.md` (the resolver this routes onto).

## 1. The defect, measured

A daemon receiver resolves a client-supplied path tail beneath the operator's
module root. **On a platform without `openat2(2)`** that confinement is not
merely weaker - it is absent, the transfer reports success, and **both upstream
versions refuse the same transfer at exit 3 in the same configuration.**

⚠ **The platform qualifier is load-bearing; the config qualifier is not.**
Everything measured below is macOS. The same portable arm runs on the BSDs and
on Linux kernels predating `openat2`. On Linux **with** `openat2` the walk
produces `EXDEV` and oc refuses correctly. The accurate claim is *"the stock
non-chroot daemon module silently serves an escape on any platform without
`openat2`"* - **not** "on every platform". What is *not* qualified is the
configuration: this is the default module, not a hardened-off one.

Measured 2026-08-14, macOS 15 (aarch64), real daemons, `use chroot = false`,
`read only = false`, and **no other directive** - so `munge symlinks` takes its
upstream auto default (`!use_chroot` -> on, `clientserver.c:997-998`). This is
the stock non-chroot module.

```
fixture:  <root>/module          module root
          <root>/outside         sibling, outside the module
          <root>/module/escape -> ../outside

client:   -a payload/ rsync://127.0.0.1:PORT/mod/escape/
```

| daemon | exit | client stderr | `outside/payload.txt` |
|---|---|---|---|
| rsync 3.4.4 | **3** | `change_dir#1 "escape/" (in mod) failed: Capabilities insufficient (107)` | absent |
| rsync 3.5.0 | **3** | `change_dir#1 "escape/" (in mod) failed: Too many levels of symbolic links (62)` | absent |
| **oc-rsync** | **0** | *(empty)* | **PRESENT** |

3.5.0's `ELOOP` is the `O_NOFOLLOW` confined walk refusing the component;
3.4.4 refuses by a different mechanism, not chased here. Either way the
divergence is oc-only and lands in the default deployment.

⚠ **The detector has been failing in CI the whole time, in a cell nobody
reads.** `transfer` is absent from the *required* `macOS (stable)` cell, but
`.github/workflows/_test-features.yml` runs a cross-OS matrix whose `iconv` row
is `-p protocol -p transfer -p engine -p core -p cli -p daemon --features
iconv` on `[ubuntu-latest, macos-latest, windows-latest]`. Both
`sandbox::symlink_race_tests::anchored_mode_*` have been red there on every PR
- observed on #7318, which touches only a citation comment in
`crates/protocol/src/flist/read/name.rs`, so the red is master's. The escape
detector fired continuously; the cell is not required, so nobody looked.

⚠ **The daemon log is silent too.** The full log for the escaping run holds the
access line, the client-args line, the landlock/seccomp notices,
`receiving file list`, and the byte totals. There is **no** confinement
warning of any kind - not the `continuing without path confinement` text, not
anything else. Which arm ran instead of the warn arm is *not* established; only
the absence of the log line is measured. So "loud, but only in the daemon's log"
overstates it: on this path it is silent on both sides.

### Why

`DirSandbox::open_dest_anchor` walks the peer tail one component at a time.
The two platform arms differ in kind, not degree:

| arm | mechanism | in-tree symlink | escaping symlink |
|---|---|---|---|
| Linux `openat2` | `RESOLVE_BENEATH` | followed | **`EXDEV`** |
| portable | `openat(O_DIRECTORY \| O_NOFOLLOW)` per component | `ENOTDIR` | `ENOTDIR` |

The portable arm refuses a symlink *without resolving it*, so it never learns
where the component pointed. Measured on the same fixtures:

```
open_dest_anchor(module, "link")    -> ENOTDIR (20)     // must be FOLLOWED
open_dest_anchor(module, "escape")  -> ENOTDIR (20)     // must be REFUSED
```

**Identical errno for the case that must succeed and the case that must fail.**
No predicate exists at that layer to tell them apart.

`open_sandbox_for_dest_anchored`
(`crates/transfer/src/receiver/transfer/setup/sandbox.rs`) then classifies:

```
EXDEV        -> hard Err          "escapes module root"
ENOENT       -> Ok(None)          first-run push, tail created later
_            -> warn + Ok(None)   "continuing without path confinement"
```

`ENOTDIR` falls into the third arm. `Ok(None)` means *no sandbox*, and the
receiver proceeds with **path-based** syscalls on the fused
`module_root/peer_tail` string - which resolve through the planted symlink by
definition. The warning goes to the daemon's log, not the client's stderr.

> The defect is not the missing `openat2`. It is that **"I could not confine"
> is treated as "proceed unconfined"** rather than as a refusal.

### Can the client plant the symlink itself? Not on a default module.

Measured with the same harness, pushing a source tree that *contains*
`escape -> ../outside` and reading back what the module received:

| daemon | config | target as written into the module | escape |
|---|---|---|---|
| oc-rsync | default | `/rsyncd-munged/../outside` | no |
| rsync 3.4.4 | default | `/rsyncd-munged/../outside` | no |
| rsync 3.5.0 | default | `/rsyncd-munged/../outside` | no |
| oc-rsync | `munge symlinks = false` | `../outside` **verbatim** | **YES** |
| rsync 3.4.4 | `munge symlinks = false` | `outside` *(sanitised)* | no |
| rsync 3.5.0 | `munge symlinks = false` | `outside` *(sanitised)* | no |

oc implements the munging default correctly, so **a client cannot plant a
traversable escape link on a default module.** The trigger for section 1 is a
symlink that arrives by some other route - which is any directory symlink an
operator or a co-tenant process puts inside a served module.

The last two rows are a **separate defect in a different layer** - see
section 5. They are recorded here because the same harness found them, not
because this task fixes them.

`use chroot = true` unprivileged is not a divergence: oc, 3.4.4 and 3.5.0 all
refuse the connection (`@ERROR: chroot failed`, exit 5).

### Scope of the claim

| claim | status |
|---|---|
| macOS end-to-end escape, default config, exit 0, payload outside | **measured** |
| 3.4.4 and 3.5.0 refuse it at exit 3, same config | **measured** |
| no confinement warning anywhere in oc's daemon log for that run | **measured** (full log read) |
| a client cannot plant the link on a default module | **measured** |
| Linux refuses the same tail (`EXDEV` -> hard `Err`) | **measured**, unit level |
| Linux end-to-end also refuses | inferred from the above |
| which oc arm runs in place of the warn arm | **not established** |

## 2. Upstream, and what oc must copy

Upstream keeps the operator half and the peer half in *different mechanisms*:

- the module root is opened plainly, because an operator who writes
  `path = /srv/backup` has authorised every component of it, symlinked or not
  (`syscall.c:85-90` `open_anchor_dirfd()`, and the comment at `:3189-3193`);
- the peer tail is walked by `ds_descend()` (`syscall.c:2891-2965`), which
  **resolves** each component and then decides: a relative in-tree target is
  spliced back into the walk (`:2961`), an absolute target is refused
  (`:2953`), a `..` above the anchor is refused (`:2896`).

Resolving is what makes the distinction expressible. That is the whole fix.

## 3. The change

### 3.1 Route the tail walk onto the shared resolver

Replace the per-component `openat_dir` walk in
`DirSandbox::open_dest_anchor` with the confined walk landed in PR #7325
(`ConfinedWalk` / `open_dest_anchor_confined`). It already implements
`ds_descend` exactly: follows relative in-tree targets, refuses absolute ones,
treats `..` as movement and refuses a pop above the anchor, shares one
symlink-hop budget, and caps depth at `DS_MAXDEPTH`.

⚠ This is a genuine compile dependency. `open_dest_anchor_confined` does not
exist on master; #7325 must land first.

### 3.2 Carry a dotdot policy as a parameter

`open_sandbox_for_dest_anchored` currently refuses `..` in a peer tail via
`Component::ParentDir => EXDEV`. The resolver instead treats `..` as movement,
refusing only a pop above the anchor. Routing naively **drops the front-door
check**.

Upstream parameterises exactly this, and says so
(`syscall.c:3236-3240`):

> *"A caller may explicitly allow literal `..` components when the fd itself is
> the confinement boundary: `secure_walk_at()` resolves each one by popping its
> held-dirfd stack and refuses a pop above the anchor. Other callers retain the
> front-door validation used by `secure_relative_open()`."*

with `int allow_dotdot` at `:3243` and the guard at `:3255`. A peer-supplied
tail is *not* a re-anchored path, so this site keeps the front-door refusal.
Same shape as `LeafPolicy` in U350-4d.1: what reads as one global rule is a
per-call parameter upstream.

### 3.3 Make the classifier fail closed

Once the walk resolves components, `ENOTDIR` from the tail no longer means
"unknown". Narrow the `_ =>` arm so an unclassified refusal on a **daemon**
receiver is fatal rather than a downgrade. Keep `ENOENT` soft - a first-run
push legitimately creates the tail later.

### 3.4 The gate is a separate decision - do not fold it in

Upstream hardens **every non-chrooted receiver**:

```
syscall.c:100-114   secure_relpath_active()
                    ... !am_chrooted && (am_daemon || !am_sender)
```

oc gates on `is_daemon_connection` alone
(`receiver/transfer/setup/context.rs:543-553`), so ordinary local and SSH
receivers fail open on refusal. `Activation::hardened()` (U350-4c.5,
PR #7322) already encodes upstream's predicate and deliberately does **not**
change this gate.

Widening it is a real behaviour change for non-daemon users and needs its own
evidence. Land 3.1-3.3 first; decide the gate separately.

⚠ The sender side is already correct - `generator/context.rs:665` excludes the
non-daemon sender, matching upstream's deliberate `--copy-links` carve-out. Do
not "fix" it.

## 4. Gates

1. The existing daemon-sender symlink-swap escape test passes unchanged.
2. End-to-end: the fixture in section 1 **refuses**, and `outside/` stays
   empty, on a platform without `openat2`.
3. An in-tree symlinked subdirectory in the module still transfers - the
   availability half. Refusing everything would pass gate 2 while breaking
   ordinary module layouts. This is the failure mode task 598 already recorded
   against the resolver: 3.5.0 **follows** a relative in-tree target
   (`ds_descend`, `syscall.c:2961`) and refuses only absolute targets
   (`:2953`) and `..` above the anchor (`:2896`). A refuse-all reading would
   break plain `oc-rsync -a src/ dst/` where `dst/sub` is a symlink.
4. `..` in a peer tail still yields the front-door refusal after routing.
5. The two `anchored_mode_*` tests (PR #7328) tighten from mechanism-branched
   to a single asserted answer on both arms, and their `⚠ KNOWN GAP` doc
   comments are deleted rather than reworded.
6. **Cross-implementation**: the section 1 fixture, driven against a real
   `rsync` 3.5.0 daemon in the same harness, must refuse. It does today
   (exit 3, `ELOOP`), so this gate is a live oracle rather than a recorded
   expectation - if oc and 3.5.0 ever agree by both being wrong, it fires.

Gate 3 is the one a security-only reading misses, and gate 5 is how this task
proves it actually closed the thing #7328 could only document. Gate 6 exists
because the escape was found by *comparing* against upstream, not by reading
oc alone - keep the oracle in the suite rather than transcribing its verdict.

⚠ The harness has one environment trap worth carrying: an `rsync` daemon with
`max connections` set needs an explicit `lock file` inside the fixture, or it
fails `@ERROR: failed to open lock file /var/run/rsyncd.lock` at exit 5 and
every cell reports a clean "no escape" for the wrong reason. oc does not need
it, so the upstream control fails while the oc cell looks fine - the exact
shape of a control that validates nothing.

## 5. Adjacent defect: no `sanitize_path` on a received symlink target

Deliberately **not** fixed here - it is a file-list-receive defect, not a
path-walk defect, and the fix lands in a different file.

Upstream carries a second, independent defence for the case where an operator
turns munging off. `flist.c:1182`, inside `recv_file_entry()`:

```c
if (sanitize_paths && !munge_symlinks && *bp)
        sanitize_path(bp, bp, "", lastdir_depth, SP_DEFAULT);
```

`sanitize_paths` is set for every daemon module (`clientserver.c:994-995`, from
the normalised module path length), so with munging off upstream still strips
the leading `../` from an incoming symlink target - which is exactly the
`../outside` -> `outside` rewrite in the table above.

oc has the function (`crates/transfer/src/sanitize_path.rs`) and its module doc
**cites this very line**, but the daemon receive path never applies it to
symlink targets. Consequence, measured: under `munge symlinks = false` a client
holding only write access plants `../outside` verbatim and escapes in two
transfers, exit 0, empty stderr, no out-of-band step.

Fixing that is worth its own change, because it has its own upstream anchor,
its own gate (`munge symlinks = false` module receives a `../` target and stores
it sanitised), and it would still be needed if this task's walk were perfect.

## 6. Consumers

`fast_io::confinement` (U350-4c.5) and the 4c.2/4c.4 chain are `pub mod` with
zero production callers, so `dead_code` cannot fire on them. U350-4d.1 and this
task are their only planned consumers. If this stalls, that chain is an orphan
of the class task 249 catalogues - escalate rather than let it sit.
