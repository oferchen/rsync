# Receiver peer-tail confinement: the fail-open, and how to close it

Implementation spec for U350-4d.2. Companion to
`upstream-3.5.0-path-confinement-model.md` (the model) and
`path-confinement-resolver-api.md` (the resolver this routes onto).

## 1. The defect, measured

A daemon receiver resolves a client-supplied path tail beneath the operator's
module root. On a platform without `openat2(2)` that confinement is not merely
weaker - it is absent, and the failure is silent to the client.

Measured 2026-08-14, macOS 15 (aarch64), real `oc-rsync --daemon`
(`use chroot = false`, `read only = false`), real client:

```
fixture:  <root>/module          module root
          <root>/outside         sibling, outside the module
          <root>/module/escape -> ../outside

client:   oc-rsync -a src/ rsync://127.0.0.1:PORT/mod/escape/

result:   exit                       = 0
          client stderr              = (empty)
          <root>/outside/payload.txt = PRESENT
```

The payload landed outside the served module. The transfer reported success.

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

### Scope of the claim

| claim | status |
|---|---|
| macOS end-to-end: file lands outside the module, exit 0 | **measured** |
| Linux refuses the same tail (`EXDEV` -> hard `Err`) | **measured**, unit level |
| Linux end-to-end also refuses | inferred from the above |
| a client can plant the escape symlink itself via `--links` | **not established** |

The last row separates "an operator whose module already contains a symlink is
exposed" from "any client with write access escapes in two transfers". Settle
it before the PR body characterises reach; the harness for it is standing up
anyway.

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
   ordinary module layouts.
4. `..` in a peer tail still yields the front-door refusal after routing.
5. The two `anchored_mode_*` tests (PR #7328) tighten from mechanism-branched
   to a single asserted answer on both arms, and their `⚠ KNOWN GAP` doc
   comments are deleted rather than reworded.

Gate 3 is the one a security-only reading misses, and gate 5 is how this task
proves it actually closed the thing #7328 could only document.

## 5. Consumers

`fast_io::confinement` (U350-4c.5) and the 4c.2/4c.4 chain are `pub mod` with
zero production callers, so `dead_code` cannot fire on them. U350-4d.1 and this
task are their only planned consumers. If this stalls, that chain is an orphan
of the class task 249 catalogues - escalate rather than let it sit.
