# Upstream rsync 3.5.0 rrsync model

Read-only extraction of the restricted-shell rule set oc-rsync must mirror.
Source of truth is `target/interop/upstream-src/rsync-3.5.0/support/rrsync`
(1003 lines, up from 391 in 3.4.4), its man page `support/rrsync.1.md`, the
generator `packaging/cull-options`, the 19 `testsuite/rrsync-*_test.py` files,
and the two rsync options that exist only to serve it - `--drop-D` and
`--confine-root`.

This is the sibling of `upstream-3.5.0-path-confinement-model.md`. That document
covers what the *server* does with a path; this one covers what the *wrapper*
does before the server ever runs. They meet at exactly two places: the
`--confine-root` the wrapper passes, and the `/proc/self/fd/N` pins the wrapper
writes into the argv.

It records what upstream *does*, not what oc-rsync currently does. Citations are
anchored on function names; line numbers are a hint and move between releases.

## 0. What rrsync is, structurally

rrsync is not an rsync mode. It is a `command=` forced-command wrapper: sshd
puts the client's requested command in `SSH_ORIGINAL_COMMAND` and runs
`rrsync [flags] DIR` instead. rrsync parses that string, decides whether to
allow it, **rewrites** it, `chdir`s into DIR, and execs a real rsync.

Three structural facts that shape everything else:

1. **The input is a single string, not an argv.** `main()` re-tokenises it with
   `re.findall(r'(?:[^\s\\]+|\\.[^\s\\]*)+', command)` and de-backslashes
   values itself (`support/rrsync:553`, `DE_BACKSLASH_RE`). There is no shell.
2. **rrsync validates only what appears in that string.** Anything travelling
   over the rsync protocol - filter rules, `--files-from` names streamed down
   the connection - is invisible to it. That gap is why 3.5.0 had to add
   `--confine-root` (section 7).
3. **The decision is per-option, and the default is refuse.** An option not in
   the table breaks the parse loop, which falls through to a generic failure
   (`main()`, the `break # Generate generic failure` arms). Unknown is denied.

The invocation contract is checked before anything else (`main():484-496`):

| Check | Behaviour |
|---|---|
| `SSH_ORIGINAL_COMMAND` unset | die "Not invoked via sshd" |
| command is exactly `true` | exit 0 (connectivity probe, e.g. rsbackup) |
| first word is not `rsync` | die "does not run rsync" |
| second word is not `--server` | die "`--server` option is not the first arg" |
| no bare `.` argument seen | die "invalid rsync-command syntax or options" |

The `.` argument is the pivot: everything before it is parsed as options,
everything after it as transfer arguments. The command finally executed is

```python
cmd = (RSYNC, *rsync_opts, '--', '.', *rsync_args)
```

The literal `--` is what makes a transfer argument beginning with `-` safe;
rrsync does not need to reject those (`main():655`, and the note at
`main():558`).

## 1. Direction control

Direction is inferred, not declared (`main():499`):

```python
am_sender = command.startswith("--sender ")   # Restrictive on purpose!
```

The trailing space matters. `--sender` in any other position is not recognised
as a pull, and the comment says this is deliberate - the failure mode of
guessing wrong is to treat a pull as a push, which the `-ro`/`-wo` checks then
refuse.

| Wrapper flag | Rule |
|---|---|
| `-ro` | die unless `am_sender`; also implies `-no-del` and skips the lock (`__main__:997-1000`) |
| `-wo` | die if `am_sender` |
| neither | both directions allowed |

`-ro` and `-wo` are a mutually exclusive argparse group, so they cannot combine.

Two consequences drawn from the same flag (`main():505-512`):

- `-wo`, or any push, sets `long_opts['sender'] = -1`. A push must not be able
  to smuggle in a `--sender` that would flip the server into a reader.
- `-ro` sets `long_opts['log-file'] = -1`. A read-only endpoint must not be able
  to write anywhere, and `--log-file` is a write.

## 2. The option table

This is the core. rrsync carries two tables: a string of allowed short-option
*letters*, and a dict of long options mapped to a *check class*.

### 2.1 Check classes

| Value | Meaning |
|---|---|
| `-1` | **DENIED.** die "option `--x` has been disabled on this server." |
| `0` | allowed, takes no argument |
| `1` | allowed, argument passed through **unchecked** |
| `2` | allowed, argument checked **only when receiving** (`not am_sender`) |
| `3` | allowed, argument **always** checked |

"Checked" means `validated_arg()` - section 4.

### 2.2 Long options, 3.5.0 (91 entries)

**DENIED unconditionally (`-1`), 5:**
`copy-devices`, `daemon`, `debug`, `no-munge-links`, `write-devices`.

`debug` is the one entry 3.5.0 changed (it was `1` in 3.4.4). `no-munge-links`
is denied so `-munge` cannot be undone by the peer; `daemon` because the wrapper
is not a daemon launcher; `copy-devices`/`write-devices` because they let a
transfer read or write device nodes.

**Argument ALWAYS checked (`3`), 2:**
`files-from`, `log-file`.

**Argument checked WHEN RECEIVING (`2`), 6:**
`backup-dir`, `compare-dest`, `copy-dest`, `link-dest`, `partial-dir`,
`temp-dir`.

This is the operator-path family. The `2` class exists because these name a
directory the *receiver* writes into or reads a basis from; on the sending side
the same option is inert, so upstream does not pay the validation cost.

**Argument passed through UNCHECKED (`1`), 22:**
`block-size`, `bwlimit`, `checksum-choice`, `checksum-seed`, `compress-choice`,
`compress-level`, `compress-threads`, `groupmap`, `iconv`, `info`, `log-format`,
`max-alloc`, `max-delete`, `max-size`, `min-size`, `modify-window`,
`only-write-batch`, `skip-compress`, `stderr`, `suffix`, `timeout`, `usermap`.

These are not pathnames, so there is nothing to confine. Note `only-write-batch`
sits here even though it names a file - upstream's own classification, not an
inference.

**Allowed, no argument (`0`), 56:**
`append`, `copy-unsafe-links`, `delay-updates`, `delete`, `delete-after`,
`delete-before`, `delete-delay`, `delete-during`, `delete-excluded`,
`delete-missing-args`, `dirs`, `existing`, `fake-super`, `force`, `from0`,
`fsync`, `fuzzy`, `group`, `hard-links`, `ignore-errors`, `ignore-existing`,
`ignore-missing-args`, `ignore-times`, `inplace`, `links`, `list-only`,
`mkpath`, `msgs2stderr`, `munge-links`, `new-compress`, `no-W`,
`no-implied-dirs`, `no-msgs2stderr`, `no-r`, `no-relative`, `no-specials`,
`numeric-ids`, `old-compress`, `one-file-system`, `open-noatime`, `owner`,
`partial`, `perms`, `preallocate`, `recursive`, `remove-sent-files`,
`remove-source-files`, `safe-links`, `sender`, `server`, `size-only`,
`specials`, `stats`, `super`, `times`, `use-qsort`.

### 2.3 Short options

```python
short_disabled        = 's'
short_disabled_subdir = 'KLk'
short_no_arg          = 'ACDEHIJKLNORSUWXbcdgklmnopqrstuvxyz'   # DO NOT REMOVE ANY
short_with_num        = '@B'                                    # DO NOT REMOVE ANY
```

| Letter | Option | When disabled | Why |
|---|---|---|---|
| `s` | `--secluded-args` / `--protect-args` | **always** | args then arrive over the protocol, where rrsync cannot see or validate them |
| `K` | `--keep-dirlinks` | restricted dir (`DIR != '/'`) | follows a symlink at a destination directory |
| `L` | `--copy-links` | restricted dir | dereferences symlinks, including ones escaping the tree |
| `k` | `--copy-dirlinks` | restricted dir | same, for directory symlinks |
| `b` | `--backup` | `-no-overwrite` only | see section 3 |

A disabled letter is detected *inside a cluster*: `short_disabled_re` is
`^-[<allowed>]*([<disabled>])`, so `-logDtpreLiLsfxCIvu` is caught on its `L`
even though every other letter is fine (`main():536`). This is the same
bundle-scanning problem oc-rsync fixed for daemon `refuse options`.

The two DO-NOT-REMOVE lists are the *accept* side and are generated, not
curated - see section 5.

The allowed-cluster regex is
`^-(?=.)[<allowed>]*(e\d*\.\w*)?$` (`main():537`). The trailing group is the
capability blob rsync appends (`-e.iLsfxCIvu`), which is **not** a set of
options. 3.5.0 added a matching correction on the `-R` scan: it strips that
group before looking for `R`, so a capability letter cannot be misread as
`--relative` (`main():570-580`). oc-rsync has hit the mirror-image of this bug
twice already (tasks 166/169 and 362).

### 2.4 Rewrites - options rrsync ADDS

Rewrites are the interesting half, because a refusal that breaks every normal
invocation is not a policy, it is an outage.

| Injected option | Condition | Site |
|---|---|---|
| `--drop-D` | `DIR != '/'` **and** `not am_sender` | `main():618-635` |
| `--confine-root=<cwd>` | `DIR != '/'` (both directions) | `main():637-644` |
| `--munge-links` | wrapper flag `-munge` | `main():646` |
| `--ignore-existing` | wrapper flag `-no-overwrite` | `main():649` |
| `/proc/self/fd/N` path rewrites | per validated path argument, Linux only | `validated_arg()` |
| `.` | when no transfer argument survived | `main():652-653` |

## 3. The 3.5.0 behaviour changes

### 3.1 Device/special creation: `--drop-D`, NOT `--no-D`

**Correction worth stating loudly: upstream's own `NEWS.md` says rrsync "forces
`--no-D`" in two places (the CVE-2026-53783 entry and the BEHAVIOR CHANGES
entry). The shipped code does not. It forces `--drop-D`, and
`rrsync-specials-denied_test.py` asserts that `--no-D` must NOT appear.** The
NEWS text is stale relative to the code. Mirror the code.

The history, from `rrsync-archive-mode_test.py` and `rrsync-specials-denied_test.py`:

1. 3.5 development first added `D` to `short_disabled_subdir` (`'KLk'` →
   `'DKLk'`). That rejected plain `rsync -a` outright for every restricted
   rrsync in **both** directions, because `-a` is `-rlptgoD` and the client
   always sends a bundled short string containing `D`.
2. The next attempt forced `--no-D`. That desynchronises the file list:
   `preserve_devices`/`preserve_specials` also frame the wire's rdev fields, and
   only one end of the connection gets the wrapper's option. A FIFO hangs the
   transfer at protocol 29 and corrupts it at 30; a device breaks **every**
   protocol.
3. The shipped answer is `--drop-D`, a new rsync 3.5.0 option
   (`options.c:688-689`, consumed at `generator.c:2031`). It refuses the
   *creation* while leaving the wire format alone: entries fall through to the
   "skipping non-regular file" path, exactly as `--no-D` reaches it, but the
   rdev fields still exist on both sides.

Two scoping rules the tests pin explicitly:

- **Receiver only.** Creation happens where files are written, so a sender has
  nothing to deny and `--drop-D` there would be a no-op that still risks
  desynchronising the list. `rrsync-specials-denied` part 4 fails if either
  `--drop-D` or `--no-D` appears on a `--server --sender` command.
- **The client's own `-D`/`--specials` is left in place** alongside `--drop-D`.
  Stripping it is what breaks the list.

This is a **rewrite, not a refusal**, and that distinction is the whole point.

### 3.2 `--copy-unsafe-links` denied in a restricted dir

`main():526-528`:

```python
if args.dir != '/':
    short_disabled += short_disabled_subdir
    long_opts['copy-unsafe-links'] = -1
```

`rrsync-copy-unsafe-links-denied_test.py` demonstrates the primitive first: a
symlink inside the served tree pointing at `../outside/secret` is dereferenced
by `--copy-unsafe-links`, so the outside content lands in the puller's copy as a
regular file. Then it asserts the refusal. Note there is no short letter for
this one, so unlike `-L`/`-k`/`-K` the long-option table is the only lever.

### 3.3 `-no-overwrite` widened

3.4.4's `-no-overwrite` only added `--ignore-existing`. 3.5.0 recognises that
`--ignore-existing` protects **only the live destination file**, and additionally
denies every option that can reach a *different* existing object
(`main():515-524`):

| Also denied under `-no-overwrite` | Why |
|---|---|
| `--log-file` | appends to an arbitrary existing file |
| `--partial-dir` | consumes / overwrites an existing partial |
| `--delay-updates` | same, via the implicit `.~tmp~` directory |
| `--backup-dir` and short `-b` | publishing a backup onto an existing name deletes what is there (`backup.c make_backup()`), and deletion backs files up first (`delete.c`), so an unrelated `--delete` can land on a protected name |

`short_disabled += 'b'` must happen **before** the regex build - the code
carries that ordering as a comment. The man page documents the user-visible
cost: resumable uploads with an explicit `--partial-dir`, `--delay-updates`, and
server-side logging are unavailable under `-no-overwrite`.

### 3.4 New wrapper flag `-absolute`

`rrsync -absolute /path/to/root` accepts a transfer arg spelled with the
restricted directory's absolute server path - `/path/to/root/dir1` as an alias
for `dir1`. It affects **transfer args only** (`opt == 'arg'`), not option
values, and only when `DIR != '/'` (`validated_arg():697`). The absolute
spelling is stripped back to a relative one before exec (`validated_arg():939-944`).

### 3.5 Logging hardened

3.4.4 opened `LOGFILE` with `open(LOGFILE, 'a')` after an `os.path.isfile()`
test - a symlink at that name was followed. 3.5.0's `safe_open_logfile()`
(`support/rrsync:455-471`) does `lstat` → reject non-regular → `open(O_NOFOLLOW)`
→ `fstat` → compare `(st_dev, st_ino)`, and returns `None` on any mismatch. It
fails silently to "no logging", never to "log elsewhere".
Pinned by `rrsync-logfile-symlink_test.py`.

### 3.6 The lock is explicitly not a security control

`lock_or_die()` (`support/rrsync:950-964`) now distinguishes "another instance
holds it" (`EWOULDBLOCK`/`EAGAIN`/`EACCES` → die) from "flock is unavailable on
this fd/platform" (Solaris returns `EBADF` for `flock()` on a directory fd →
proceed without the lock). The comment states the reasoning: the single-run lock
is a best-effort convenience, cf. `-no-lock`, not a security control.

## 4. The restricted-directory rule, and how a path argument is validated

`validated_arg(opt, arg, typ, wild)` (`support/rrsync:681-947`) is the whole
confinement. This is the FIXED version; the broken one is section 4.6.

### 4.1 Normalisation, before any check

1. De-backslash, unless the value came from the transfer-arg branch, which
   braceexpand already de-backslashed (`validated_arg():682-683`).
2. `--files-from` sentinel: the exact string `-` means "read the list from
   stdin" and is returned untouched. A pull with a *client-local*
   `--files-from` sends literally `--files-from=-`, so treating it as a pathname
   breaks every such pull. Under `-wo`, any *other* `--files-from` value is
   refused outright - a write-only endpoint must not read a server-side list
   back to the peer (`validated_arg():686-691`).
3. Strip a leading `./`, collapse `//`, and `lstrip('/')` - **unless** this is an
   `-absolute` transfer arg naming the root.
4. If `DIR != '/'`, refuse any `..` component:
   `HAS_DOT_DOT_RE = (^|/)\.\.(/|$)`, message "do not use .. in ... (anchor the
   path at the root of your restricted dir)".
5. Transfer args only: `glob.glob()`, falling back to the literal string when
   nothing matches (`validated_arg():704-709`). Each glob result is validated
   independently.

### 4.2 The realpath check

For each candidate, when `DIR != '/'`, the arg is not `.`, and the check class
applies (`typ == 3`, or `typ == 2 and not am_sender`):

```python
real_arg = os.path.realpath(arg)
if arg != real_arg and not real_arg.startswith(args.dir_slash):
    die('unsafe arg:', orig_arg, [arg, real_arg])
```

Trailing `/` and `/.` are split off first and re-appended after
(`validated_arg():714-721, 935-938`) - they are meaningful to rsync and must
survive every rewrite. `args.dir` is itself a `realpath()` taken at startup
(`__main__:994`).

Note the shape: a path that resolves to *itself* passes without a prefix check.
Only a path that realpath actually *changed* has to land under `DIR/`.

### 4.3 The pin - what makes the realpath check binding

The realpath check alone is CVE-2026-53783: between `realpath()` and the exec'd
rsync's own resolution, an attacker can rename a component into a symlink
escaping the tree. `rrsync-symlink_test.py` measures the window - against a
pristine 3.4.4 rrsync a rename-based flipper leaked the outside marker 8 times
as sender and 11 as receiver over a 5-second race.

The fix is to stop passing a *name*. rrsync opens the validated path and hands
rsync `/proc/self/fd/N`, which the kernel binds to the **inode**:

```python
fd = os.open(real_arg, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
#   IsADirectoryError -> retry with O_DIRECTORY
pinned_path = os.readlink('/proc/self/fd/%d' % fd)
if not pinned_path.startswith(args.dir_slash) and pinned_path != args.dir:
    die('post-pin path escaped tree (race?):', ...)
```

Four properties, each load-bearing:

- **`O_RDONLY`, not `O_PATH`, for the leaf.** The code carries a CRITICAL
  comment: `/proc/self/fd/N` for an `O_PATH` fd **re-resolves the path** on open,
  so the race window would stay open across the exec. A regular fd is
  inode-bound. (Directory *prefix* pins in `pin_dir()` do use `O_PATH`, because
  there the magic link is only a prefix and is never reopened as the file
  itself; `O_PATH` also means a mode-0111 parent still works.)
- **`O_NOFOLLOW`** refuses a symlink that raced in at the leaf.
- **`O_NONBLOCK`** so a special file that raced in after the `lstat` cannot
  block the open.
- **readlink-verify after the open.** A parent-component flip that landed on an
  in-tree symlink pointing outside surfaces here.
- **Failure is fatal, not a fallback.** `OSError` from the open →
  "post-realpath open failed (race detected)". `ENOENT` in `--sender` mode →
  the same, because a source must exist.

The fds are held across the exec via `make_inheritable()` (which prefers
`F_SETFD` because `os.set_inheritable()`'s `ioctl(FIONCLEX)` is rejected by
`O_PATH` descriptors on older kernels) and `subprocess.run(..., pass_fds=...)`.
Directory pins are deduplicated by `(st_dev, st_ino)` so a glob or multi-arg
command shares one fd (`pin_dir()`, `pinned_dirs`).

### 4.4 Platform gate

`_probe_proc_self_fd()` (`support/rrsync:175-193`) requires
`sys.platform.startswith(('linux','android'))` **and then still probes** twice -
a directory (`/`) and a regular file (the script itself) - checking that
`readlink('/proc/self/fd/N') == realpath(path)`.

The comment states why an `isdir('/proc/self/fd')` test is insufficient, and the
two platforms that fail in *opposite* directions:

- **NetBSD** makes the entry a symlink for directories only; a directory-only
  probe would claim support that is not there and every file pull would die in
  the post-pin check.
- **Cygwin**'s readlink returns the right path, but *opening* the magic link
  re-resolves it - rename the directory out from under the held fd and the link
  reaches the replacement. The pin silently protects nothing.

Where the probe fails, rrsync passes the realpath-validated name, exactly as
3.4.4 did. **The confinement is weaker there by design**, and the race tests
`test_skipped()` rather than fail. Where `/proc/self/fd` exists but a readlink
fails anyway (seccomp, restricted `/proc`), that is an anomaly and rrsync
**fails closed** (`validated_arg():900-908`).

### 4.5 Which pin, per argument shape

`sender_pinned_arg()` (`support/rrsync:382-453`) exists because the obvious
rewrite is wrong for a sender. A sender `lstat()`s its source argument, and
`lstat` of a procfs magic link is **always** `S_IFLNK`, so handing it the leaf's
pin makes rsync describe the argument as a symlink and send no data. That is the
regression `rrsync-pull-delivers-content_test.py` pins.

| Argument shape | Pin handed to rsync |
|---|---|
| receiver destination, or any option value | leaf pin, `/proc/self/fd/N` (`KEEP_LEAF_PIN`) |
| sender arg with trailing `/` or `/.` | leaf pin - rsync **opens** this one and does follow a symlink there, so the pin is load-bearing (`rrsync-sender-leaf-flip`) |
| sender arg, any other shape | **parent** pin: `/proc/self/fd/<dirfd>/<leaf>` |
| sender arg under `--relative`, client `/./` present | pin the anchor, spell the rest after the client's marker: `/proc/self/fd/<dirfd>/./<suffix>` |
| sender arg under `--relative`, no `/./` | pin the restricted root; **components below it stay raceable** - a stated limit |
| leaf is `.`/`..`/empty, or the tree root | `LEAF_PIN_UNUSABLE`: keep the realpath-validated name (what 3.4.4 passes) |

Under `--relative` the transmitted name is the whole argument rather than its
basename, which is why the pin has to move up; rsync honours the **first**
`/./`, so rrsync splits there and keeps the client's marker rather than inserting
an earlier one.

Whichever directory is pinned, rrsync then re-resolves the leaf beneath the held
fd and compares `(st_dev, st_ino)` against the leaf it opened. A mismatch is
"post-pin path changed (race?)" - fail closed.

**Leaves rrsync deliberately never opens** (`validated_arg():767-781`): a sender
arg whose `lstat` says it is neither a regular file nor a directory. `O_RDONLY`
on a FIFO blocks until a writer appears, which wedged rrsync before exec; a
dangling symlink resolves to nothing and was reported as a race. Both are shapes
rsync only *describes*. They still get the parent pin - handing over the bare
name is precisely CVE-2026-53783, measured at 3 leaks in 83 raced pulls. This
decision is **not** gated on `HAVE_PROC_SELF_FD`, because it is about what rsync
does with the argument, not about whether a pin is available.

### 4.6 What the BROKEN 3.4.4 validation did

For contrast, so the fix is unambiguous. 3.4.4's `validated_arg()`:

- ran the same `realpath()` prefix check, then
- **returned the name string**, which the exec'd rsync re-resolved from scratch.

Every component went back into play between check and use. There was no pin, no
post-check verification, no leaf-shape reasoning, and no distinction between an
option value and a transfer arg beyond the type class.

## 5. How the option table stays in sync with rsync's own

`packaging/cull-options` (byte-identical between 3.4.4 and 3.5.0) is a Python
script that **scrapes `options.c`** for everything a client can send to a server
and prints the block between the `### START/END of options data produced by the
cull-options script. ###` markers. The patterns it matches:

| Pattern in `options.c` | Produces |
|---|---|
| `argstr[x++] = 'C'` (excluding `.`, `i`, `e`) | a `short_no_arg` letter |
| `asprintf(..., "-X%lu")` | a `short_with_num` letter |
| `args[ac++] = "--opt"` | `long_opts['opt'] = 0` |
| ...followed by `args[ac++] = safe_arg("", var);` | upgrades it to `2` |
| `return "--x-dest";` | `2` |
| `--opt=` in an `asprintf`/`args[ac++]`/`safe_arg`/`fmt` | `1` |
| name starting `min-`/`max-` | forced to `1` |

It seeds a hand-written dict for options popt accepts but that no code path
emits verbatim (`fake-super`, `no-munge-links`, `copy-devices`,
`write-devices`, ...), and hard-sets `files-from` to `3`.

### The shipped table is NOT what the generator produces

MEASURED, not inferred: running upstream's own `packaging/cull-options --python`
against upstream's own 3.5.0 `options.c` and diffing the result against the
block shipped in `support/rrsync` gives **three** differences.

```
-  'compress-threads': 1,     shipped only - generator does not emit it
-  'debug': -1,               shipped
+  'debug': 1,                generated
-  'dirs': 0,                 shipped only - generator does not emit it
```

Verified causes:

- **`compress-threads`** is a client-side popt entry only (`options.c:783`,
  `POPT_ARG_INT` into `do_compression_threads`) with no `args[ac++]` emit site,
  so the scraper never sees it. A hand-added permissive extra.
- **`dirs`** likewise has no emit site: `-d` goes to the server through
  `argstr[x++] = 'd'` (`options.c:2806`), so the generator learns the *letter*
  but never the long name. (The only `args[ac++]` match on "dirs" is
  `--no-implied-dirs`, a different option.) Also a hand-added permissive extra.
- **`debug: -1`** is a hand-applied **denial** that a regeneration would revert
  to `1`, re-opening exactly the disclosure route `NEWS.md:171-174` documents.

So the real relation is

> **shipped = generated ∪ {hand-added allows} \ {hand-applied denies}**

and *nothing in the upstream tree records which entry is which*. The
START/END markers claim the whole block is generated; three entries are not.

Upstream's only guard against the denial silently reverting is a string
assertion in a test: `rrsync-debug-denied_test.py` checks that the literal
`"'debug': -1,"` is still in the file, commented "a cull-options regeneration
probably restored it". There is no guard at all on the two hand-added allows.

**Consequence for the oc-rsync drift test (task 582): it cannot be "regenerate
and compare".** That would either fail permanently on the hand-added entries or,
if reconciled by regenerating, silently revert a security denial. It has to be a
three-way check - committed table vs derived-from-upstream set vs an explicit,
reviewed override list - so an upstream change surfaces as a diff a human reads
rather than as a silent policy revert. Same shape as task 410; see consequence 8.

## 6. The `/proc/self/fd/N` pin, seen from the server

The server side is specified in `upstream-3.5.0-path-confinement-model.md`
section 4; repeated here only as the contract between the two halves.

- `fd_pin_tail()` (`syscall.c:149`) splits a `/proc/<self|pid>/fd` prefix,
  returning `""` for the pin directory itself and a `/`-leading tail otherwise.
- `is_exact_fd_pin()` (`syscall.c:176`) accepts only the **bare, all-digit**
  entry. A planted name like `.../fd/outside-secret` is not a pin. A pinned
  *parent* spelled `.../fd/7/<leaf>` is handled by the walk resolving the magic
  link and checking the components past it.
- `abspath_outside_confinement()` (`syscall.c:197`) judges a pin by what it
  points **at**, not by its spelling - a pin is outside the root by
  construction. `!am_daemon` gates the whole pin branch: a daemon has
  `module_dir`, never `confine_root`.
- **A pin that cannot be resolved is refused, not waved through.**

## 7. `--confine-root` - the option rrsync passes for the invisible paths

`main():637-644`, unconditional for a restricted dir, **both directions**:

```python
rsync_opts.append('--confine-root=' + os.getcwd())
```

The reason is stated in the code and in `NEWS.md`: filter rules travel over the
protocol, not in the argv rrsync validates, so a client can name a dir-merge
file outside the restricted dir and have the server read it in as rules. A pull
needs neither `--delete` nor verbosity for that, and an exclude-only merge (the
`-` modifier) makes every line a pattern, so nothing fails to parse and the peer
reads the file's contents off which of *its own* names went missing from the file
list. Redacting the diagnostic does not close it. `--confine-root` bounds the
server's own resolution of such paths - the only end that can see them - and it
is passed in both directions because a dir-merge is read by whichever side the
rule applies to.

Server-side validation (`options.c:2382-2399`):

- `am_daemon` → `confine_root = NULL`. A daemon already has `module_dir`, and
  honouring a peer-supplied root there could only *loosen* the module.
- must be absolute, else "--confine-root must be an absolute path".
- **cannot be combined with `--insecure-links`** - the opt-out short-circuits
  the walk that enforces the root, so the pair would silently mean no
  confinement at all. Upstream says so explicitly rather than letting it pass.

`rrsync-merge-file-confine_test.py` asserts the confinement must **not** cost
the transfer: refusing outright would hand the peer a denial of service. The
out-of-root merge silently finds nothing; an in-tree `.rsync-filter` still works
(the control). It also drives `--confine-root` directly, without the wrapper, for
a shape rrsync happens to reject: a source argument reached through a symlink
makes rsync's tracked cwd deeper than the kernel's, so a merge file's own
relative symlink resolves from one directory while the confinement check walks
from another.

## 8. Auxiliary receiver directories - create, require, or stand in

An option path whose leaf does not exist yet cannot be pinned, and leaving the
name for rsync to resolve lets the *same transfer* plant a symlink there first.
3.5.0 splits these three ways by what rsync itself does with a missing one
(`support/rrsync:265-270`):

| Family | Missing-leaf behaviour | Mechanism |
|---|---|---|
| `--backup-dir` (0777), `--partial-dir` (0700) | rsync creates it on demand, so rrsync creates it **first**, beneath the already-pinned parent | `create_pinned_dir()` |
| `--temp-dir` | rsync requires it, so a missing one stays an error | die "receiver option path does not exist" |
| `--link-dest`, `--compare-dest`, `--copy-dest` | rsync only reads through it, and "first run has no basis" must keep working | `pinned_empty_dir()` |

`create_pinned_dir()` (`support/rrsync:315-376`) walks down from the restricted
dir one component at a time with `O_NOFOLLOW|O_DIRECTORY` at every step, creating
what is missing. It builds a whole missing hierarchy, because rsync's
`backup.c make_bak_dir()` does - stopping at "the immediate parent must exist"
would refuse a first-use dated/nested name that works today. The `0700` for
`--partial-dir` is deliberate and asserted: a partial file is an incomplete copy
of the peer's data and rsync does not publish it to the rest of the machine.

`pinned_empty_dir()` (`support/rrsync:272-313`) is the clever one: it creates
`.rrsync-empty-basis.<pid>` inside the restricted dir, opens it
`O_NOFOLLOW|O_DIRECTORY`, then **unlinks it while holding the fd**. rsync is
handed an inode with no path at all, so there is no name for an in-band symlink
to take over, and a basis lookup cannot tell an empty directory from a missing
one. If the `rmdir` fails, that is fatal - a placeholder the peer can still reach
and fill is not one that can be safely handed over. The test asserts no
`.rrsync-empty-basis*` is left behind.

**Without the `/proc/self/fd` primitive there is no safe answer for either
family, so both are refused** ("receiver option path does not exist"), and
first-use `--backup-dir`/`--partial-dir`/alt-dest simply do not work on those
platforms (`validated_arg():816-832`). Every relevant test branches on
`proc_self_fd_pins()` and asserts the refusal on the other arm.

For an ordinary missing receiver *destination* leaf (not one of these families),
rrsync pins the existing parent and hands rsync
`/proc/self/fd/<parent>/<leaf>`; if the parent does not exist either (a deeper
`-R` path) it falls through unpinned, as before (`validated_arg():833-853`).

## 9. Test map - what each upstream test pins

| Test | Rule exercised |
|---|---|
| `rrsync-archive-mode` | A subdir-restricted rrsync must ACCEPT plain `rsync -a` in both directions. The anti-regression for "deny `-D` outright". Stub rsync (`true`); option acceptance only. |
| `rrsync-specials-denied` | `--drop-D` is forced on the RECEIVING side only; the client's `-D`/`--specials` is left alongside it; `--no-D` must NOT appear; a pushed FIFO and a pushed (fake-super) device are both denied while the transfer still succeeds. |
| `rrsync-copy-unsafe-links-denied` | The primitive (an escaping symlink is dereferenced, exfiltrating outside content) plus the refusal in a restricted dir. |
| `rrsync-debug-denied` | `-M--debug=...` / `--remote-option=--debug=...` refused; `-vvv` still works (verbosity reaches FILTER2 by another route and must NOT be caught); **and the literal `'debug': -1,` must still be in the file** - the cull-options drift guard. |
| `rrsync-pull-delivers-content` | A pull must deliver file CONTENT, not a symlink. The sender-`lstat`-vs-magic-link regression. Push is the control, so the fix cannot be "drop the pinning". |
| `rrsync-pull-arg-shapes` | 17 pull argument shapes (16 below protocol 30) each deliver the exact tree a pristine 3.4.4 rrsync delivers, with kinds and contents checked: plain file, deep file, dir, `dir/`, `dir/.`, `-R` deep file, `-R dir/`, `-R` with client `/./`, terminal `/./` and `/./.`, symlinked parent, `-R --no-implied-dirs` (proto ≥ 30 only), search-only (mode 0111) parent, dangling symlink, symlink to a file, two args - plus a FIFO that must neither wedge the wrapper nor hang the list. |
| `rrsync-symlink` | The realpath-vs-exec TOCTOU on an INTERMEDIATE component, raced against a rename-based flipper, sender direction. Skipped without `/proc/self/fd`. |
| `rrsync-sender-leaf-flip` | The LAST component raced: no outside CONTENT reaches the client, for both a plain file arg (rsync transmits the symlink, does not read through it) and a trailing-slash directory (whose leaf pin IS load-bearing). |
| `rrsync-sender-parent-pin` | The deterministic version: a stub inherits the pin, blocks, the parent is swapped for an outside symlink, then the stub reports what the arg names. Covers the two shapes rrsync never opens - dangling symlink and FIFO. Fails outright if the parent pin is removed. |
| `rrsync-alt-dest-inband-pivot` | A missing `--copy-dest`/`--compare-dest` must not be readable through a symlink the same transfer plants; the unlinked empty-basis stand-in keeps first-run alt-dest working; an existing basis still works. |
| `rrsync-backup-dir-inband-pivot` | The same for `--backup-dir` (create-and-pin, so the pivot can only unlink the name); missing `--temp-dir` refused and not created; first-use `--backup-dir`, nested `bak/2026/07`, `--partial-dir` at mode 0700, and first-run `--link-dest` all still work; no basis placeholder left behind. |
| `rrsync-merge-file-confine` | `--confine-root` bounds a peer-named dir-merge file; the transfer must still SUCCEED (refusing is a DoS); an in-tree `.rsync-filter` control; plus the symlinked-source-arg cwd-divergence case driven directly. |
| `rrsync-files-from-stdin` | `--files-from=-` is a sentinel, not a path, and must pass through; a real out-of-tree list file is still refused, and refused *for the pathname*, not for a command-shape error. |
| `rrsync-write-only-files-from` | `-wo` refuses a remote `--files-from` and leaks no record from it; a client-local `--files-from` upload still works and still selects. |
| `rrsync-logfile-symlink` | `LOGFILE` symlink is not followed. |
| `rrsync-no-overwrite-logfile` | `-no-overwrite` refuses `--log-file`; conditional on the flag (a wrapper without it must accept the same option). |
| `rrsync-no-overwrite-partial-dir` | Same for `--partial-dir`, with an existing partial file as the victim. |
| `rrsync-no-overwrite-delay-updates` | Same for `--delay-updates`, victim in `.~tmp~`. |
| `rrsync-no-overwrite-backup-collision` | Same for backup mode, and the refusal must come from `short_disabled` (`-b` arrives inside the client's bundle) not the long table. The negative control shows the collision really does defeat `--ignore-existing`. |

Two patterns worth adopting wholesale:

1. **Every refusal test has a negative control** running the identical command
   through a wrapper *without* the flag, so an unconditional denial cannot pass
   as a policy.
2. **Every security assertion is placed before its controls**, so a control
   failing for an unrelated reason cannot abort the run before the escape check.

## 10. Consequences for oc-rsync

Recorded here so the implementation tasks (U350-8b..8e) start from the right
premises.

1. **`--drop-D` is a prerequisite, not an optional extra.** oc-rsync does not
   have it. A restricted-shell subcommand cannot ship correct device/special
   denial without first adding `--drop-D`/`--no-drop-D` (`options.c:688-689`) and
   the generator gate (`generator.c:2031`). Forcing `--no-D` instead is not a
   smaller version of the same thing - it corrupts the file list. This is a
   separate task and it blocks 8c.

2. **`--confine-root` is a prerequisite too.** Task 548 is still pending. oc has
   an internal `confine_root` field on the generator's source opener
   (`crates/transfer/src/generator/open_source.rs:43`), set only for daemon
   connections (`crates/transfer/src/generator/context.rs:666`) - that is the
   daemon's module boundary, which is precisely the case upstream makes
   `--confine-root` *inert* for. There is no CLI option and no non-daemon path.

3. **The wrapper must not carry its own path validator.** That is the whole
   lesson of CVE-2026-53783 - a separate string-based validator that could be
   walked out of. The restricted-shell subcommand must reach the same resolver
   the transfer path uses (U350-4c). Concretely: the wrapper contributes the
   *root* and the *pin*; the resolver contributes the *walk*.

4. **The pin is Linux-only and that is upstream's answer, not a gap to close.**
   Mirroring means: probe (platform check **plus** two runtime probes, one
   directory and one regular file), pin where it works, fall through to the
   validated name where it does not - and **fail closed** where `/proc/self/fd`
   exists but misbehaves. Being stricter than upstream (refusing on non-Linux)
   is a divergence that breaks macOS and BSD users. Being laxer is a hole.

5. **Two places oc cannot straightforwardly mirror upstream:**
   - **Windows.** `/proc/self/fd`, `O_PATH`, `flock` on a directory fd, and
     `SSH_ORIGINAL_COMMAND`-style forced commands all have no direct analogue.
     The honest position is the same one upstream takes for the BSDs: the
     unhardened path, stated as such. This overlaps task 614 and should be
     settled once, not twice.
   - **The Python-specific hooks.** `braceexpand` (optional; absent, rrsync just
     de-backslashes), `glob.glob()` semantics for transfer args, and
     `os.path.realpath()`'s exact `..`-collapse behaviour. A Rust implementation
     must pick a glob and a normaliser and then *test against the real 3.5.0
     rrsync*, because these are the places where "looks equivalent" silently is
     not. The `..` refusal happens before globbing, which limits the blast
     radius, but the glob still decides which names get validated at all.

6. **`--secluded-args`/`-s` must stay denied.** It moves arguments onto the
   protocol where the wrapper cannot see them. oc-rsync's daemon `protect_args`
   default work (task 230) is a different question; this one is unconditional.

7. **The bundle-scanning rule is the same one oc already fixed for
   `refuse options`.** A disabled letter must be caught anywhere inside a
   cluster, and the trailing `-e.<caps>` blob must be excluded from the scan on
   both the deny check and the `-R` detection. Reuse the machinery from PR #7262
   rather than writing a second scanner.

8. **The option table drift test must be THREE-WAY, not regenerate-and-compare.**
   Measured in section 5: upstream's shipped table differs from its own
   generator's output in three entries - two hand-added allows and one
   hand-applied denial - with nothing recording which is which. A
   regenerate-and-compare test would either fail forever or silently revert the
   denial. oc-rsync's equivalent needs a committed table, a set derived from
   upstream at test time, and an explicit reviewed override list, asserting both
   directions: every option oc's client can send to a server is accounted for,
   and every intended denial is still a denial. `rrsync-debug-denied` is the
   model for the denial half; nothing upstream guards the allow half.

   **This is the same drift problem as task 410** (daemon `refuse options`
   `SHORT_OPTIONS` must match upstream `long_options[]` exactly), and the two
   should share one mechanism rather than growing two tables that drift apart
   independently. The difference is only in what is enumerated - task 410
   enumerates upstream's `long_options[]`, this one enumerates what a client can
   *send to a server*, which is the narrower set `packaging/cull-options`
   scrapes. Both need the same shape of test: derive the expected set from the
   upstream source at test time, diff it against the committed table, and fail
   on either direction of difference. Settle the mechanism once, in whichever of
   410 / 8e lands first.

9. **A native subcommand is strictly better positioned than the Python script on
   one axis**, and this should be used rather than merely matched: oc-rsync
   controls both the wrapper and the server, in one process image if it wants.
   The exec boundary that forces the pin to be spelled as a filesystem path
   (`/proc/self/fd/N`) is an artefact of rrsync being a separate program. Any
   design that keeps the fd in-process instead must still handle the case where
   a real `oc-rsync --server` is exec'd - and must not quietly become a *third*
   path-resolution policy. Surface this as a decision rather than picking it
   silently.

## References

- `target/interop/upstream-src/rsync-3.5.0/support/rrsync` and `support/rrsync.1.md`
- `target/interop/upstream-src/rsync-3.5.0/packaging/cull-options`
- `target/interop/upstream-src/rsync-3.5.0/testsuite/rrsync-*_test.py` (19 files)
- `options.c` - `--drop-D` / `--no-drop-D` (688-689), `--confine-root` (690),
  validation (2382-2399); `generator.c:2031` (`drop_devices` gate)
- `syscall.c` - `fd_pin_tail()`, `is_exact_fd_pin()`, `abspath_outside_confinement()`
- `NEWS.md` - the `support/rrsync` block under SECURITY FIXES, and the rrsync
  entry under BEHAVIOR CHANGES. **Both say `--no-D`; the code says `--drop-D`.**
- Sibling document: `docs/design/upstream-3.5.0-path-confinement-model.md`
- CVE-2026-53783 (HIGH): rrsync restricted-directory escape
