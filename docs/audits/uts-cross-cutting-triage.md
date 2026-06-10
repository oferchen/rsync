# UTS series cross-cutting root-cause triage

Date: 2026-06-10
Scope: PRs #5510, #5516, #5519, #5520, #5523, #5525-#5544 (the Upstream
Testsuite interop fix series, against upstream rsync 3.4.3 / 3.4.4
`runtests.py` invoked through TCP daemon and `--rsync-bin2`).

This audit groups the series by *root cause*, not by Stream bucket
(A/B'/B''/C/D/E/F). Several streams share the same defect class; recording
that overlap lets future work prevent the class instead of fixing it case
by case.

## Cluster 1 - Parser-stores-string, transfer-time-drops-it

Pattern: a daemon directive is parsed off `oc-rsyncd.conf` and stored as
`Option<String>` on `ModuleDefinition`. The string is never converted to
the effective transfer-time form, so every push or pull through the module
silently runs with the default. Upstream parses the same directive at
load time and stores the converted form on a parallel globals struct (e.g.
`daemon_chmod_modes`).

PRs in this cluster:

| PR | Directive | Surface bug |
|----|-----------|-------------|
| #5527 (UTS-11) | `munge symlinks = yes` | Receiver wrote unmunged target, sender did not strip the prefix |
| #5530 (UTS-7) | `incoming chmod` / `outgoing chmod` | Receiver mode-finalize and sender flist emission did not apply the spec |
| #5534 (UTS-17) | same chmod directives via spec parser parity | Same as above, refined into a single chmod parse audit |
| #5519 (UTS-16) | client positional dest under daemon push | Server wrote into module root instead of `upload/realdir/`; sandbox refusal never fired |
| #5536 (UTS-6 receiver leg) | `--stats` -> goodbye `NDX_DEL_STATS` | Receiver never stashed delete stats for goodbye emission |
| #5520 (UTS-6 sender leg) | `--stats` -> sender wire | Daemon long-form parser dropped `--stats` |

Sample fix shape (from #5530): add the parsed value as a typed field on
`ServerConfig` / `ServerConfigBuilder`, populate it inside
`build_server_config()` from `module.incoming_chmod()` / equivalent, then
plumb the field into the existing transfer-time site
(`MetadataOptions::with_chmod` on the receiver,
`GeneratorContext::create_entry` on the sender). The transfer-time site
must consume the new field unconditionally; no `Option::None` short
circuit that bypasses the call.

Why repeated: the parser is feature-complete by inspection (every
directive name is recognised), so audits that stop at the parser layer
miss the missing wiring at the transfer layer. The audit tool reads
"directive X parses" as "directive X works"; it does not check that the
parsed value is consumed by the transfer engine.

Prevention:

- Make `ModuleDefinition` fields private. Force every consumer through a
  typed accessor that returns the converted value (`ChmodModifiers`,
  `bool`, `PathBuf`) rather than a raw string. The accessor lives in
  `metadata` / `transfer`, not in the parser; calling it on `ServerConfig`
  is the wiring proof.
- Add a daemon-interop test fixture that writes `oc-rsyncd.conf` with the
  directive set, runs a transfer, and asserts the directive's observable
  effect (mode bits, symlink target prefix, `NDX_DEL_STATS` on the wire).
  These are the tests the upstream `daemon-*` suite already provides; we
  should mirror them as unit-scoped regression so a missing wire-up
  shows up before the upstream suite catches it.

## Cluster 2 - Field-vs-method shadowing on `ModuleRuntime` Deref

Pattern: `ModuleDefinition` exposes the same name as both a public
`Option<String>` field and a (test-only) accessor method. `ModuleRuntime`
derefs to `ModuleDefinition`, so the method call goes through the field
in name resolution and trips one of:

- E0599 ("no method named ... in the current scope") in release builds
  when the method is gated behind `#[cfg(test)]`.
- "field used as a function" type error on stable.

PRs in this cluster:

| PR | Site | Note |
|----|------|------|
| #5530 (UTS-7 follow-up commit) | `module.incoming_chmod()` -> field-as-call error | Fixed by switching to `module.incoming_chmod.as_deref()` |
| #5534 (UTS-17 follow-up commit) | Same accessor pattern on release builds | Same fix: prefer the field, drop the `#[cfg(test)]` accessor in callers |

Sample fix shape: drop the accessor call entirely. Use the field
directly with `.as_deref()`, matching the surrounding pattern that other
directives (e.g. `module.fake_super`, `module.temp_dir`,
`module.dont_compress`) already use.

Why repeated: the accessor was introduced as a convenience for unit
tests and never deleted when callers were added in non-test code. The
shadowing is invisible on the development machine where the test cfg is
active.

Prevention:

- Apply `#[deny(clippy::needless_borrow)]` and a custom lint or
  hand-rolled rule that flags any method on `ModuleDefinition` whose
  body is `self.field_name.as_deref()`; replace with direct field
  access. The accessor is then dead and can be deleted.
- Move every `#[cfg(test)]` accessor on `ModuleDefinition` into a
  helper trait under `#[cfg(test)] mod test_helpers`. Production code
  cannot call it then; if a production caller appears, the import
  fails to resolve.
- Add a release-mode doctest that builds the daemon arg path with
  a chmod directive; the build fail will be loud.

## Cluster 3 - Test assertion encoded the bug, not upstream

Pattern: the regression test asserted oc-rsync's (wrong) behaviour.
Fixing the implementation flips the test. Without a corresponding
upstream reference in the test name and body, reviewers cannot tell
which side is correct.

PRs in this cluster:

| PR | Test that codified the bug | Upstream truth |
|----|---------------------------|-----------------|
| #5526 (UTS-3) | `deny_takes_precedence_over_allow` | `access.c:264-292` short-circuits on allow match |
| #5526 follow-up | `module_definition_hostname_deny_takes_precedence` chunk | Same; chunk reassertion was wrong direction |
| #5526 follow-up | `module_definition_mixed_ip_and_hostname_acl` chunk | Same |
| #5523 (UTS-19) | receiver had no mode-0 sentinel consumer; pass-through tests never noticed | `generator.c:1348-1354` (mode==0 && missing_args==2 path) |

Sample fix shape: rename the test after the upstream contract it pins
(`module_definition_allow_match_short_circuits_deny`,
`module_definition_matches_upstream_rsync_fns_allow_list`), and cite the
upstream file + line in the test docblock. Add tests for *both*
directions of the contract (allow-only match, deny-only match,
fall-through), so a future drift cannot retitle one direction to
match.

Why repeated: oc-rsync's daemon ACL semantics were originally derived
by inspection of behaviour, not from upstream source. The tests then
froze the inferred semantics. Same for `--delete-missing-args`: the
receiver had no mode-0 consumer, and the integration test that did
exist (run a single push, see no error) read clean.

Prevention:

- Require every regression test under `crates/daemon/`,
  `crates/transfer/receiver/`, `crates/transfer/sender/` to cite an
  upstream `.c` file and function in a `// upstream:` comment in the
  test body, identical to the existing rule for code comments
  (CLAUDE.md, "Reference upstream source in comments").
- For new daemon features, write the upstream-mirroring assertion
  before the oc-rsync implementation. The test must pass under
  upstream rsync (in interop harness) before it is accepted as the
  oc-rsync regression.

## Cluster 4 - Wire-format off-by-default-vs-upstream-default-on

Pattern: upstream emits the wire frame unconditionally under a set of
runtime preconditions; oc-rsync gates the same emission on an
additional flag that defaults to off.

PRs in this cluster:

| PR | Frame | oc-rsync gate | Upstream gate |
|----|-------|---------------|---------------|
| #5536 (UTS-6 receiver) | `NDX_DEL_STATS` goodbye | `do_stats && flags.delete` | `delete_mode \|\| force_delete \|\| read_batch` (3.4.4 dropped `INFO_GTE(STATS, 2)`) |
| #5543 (URV-6.a) | same | Same surface fix, post-3.4.4 audit | Same |
| #5542 (URV-6.b) | same, incremental-flist driver | The pipelined-incremental path did not call `delete_extraneous_files` at all | Upstream calls `do_delete_pass()` on every driver |

Sample fix shape: drop the `do_stats` precondition; gate solely on
`delete_mode`. Mirror the audit at every receiver driver
(`pipelined.rs`, `pipelined_incremental.rs`, `sync.rs`), and confirm
the stash-and-emit hands off through a single helper that all three
drivers call.

Why repeated: the wire-frame emission predicate was lifted from
upstream pre-3.4.4 sources, then upstream relaxed the predicate without
oc-rsync syncing. Same story for the per-driver delete-pass: the
pipelined-incremental driver was added later and the delete sweep was
not retrofitted.

Prevention:

- Stand up an "upstream-3.4.x conformance" audit doc (URV-6 is the
  precedent) that lists every wire frame and the upstream predicate
  for emission. The doc is checked into `docs/audits/` and updated on
  every upstream release. New receiver / sender drivers are accepted
  only when they share the predicate helper with the existing drivers.
- Add a wire-differential fuzzer (already a known gap: see
  `project_wire_differential_fuzzing.md`). A daemon-mode push with
  `--delete` and `--stats` flipped both ways should produce the same
  wire bytes as upstream for both combinations.

## Cluster 5 - Daemon arg quoting / escaping divergence

Pattern: upstream's `safe_arg()` escapes shell/wildcard metacharacters
when shipping the option to the receiving daemon, and `unbackslash_arg()`
reverses it on the receiver. oc-rsync either dropped one side of the
pair or did not honour both forms.

PRs in this cluster:

| PR | Bug | Upstream reference |
|----|-----|---------------------|
| #5531 (UTS-8) | `--groupmap=*:GID` wildcard stripped by client-side daemon arg builder, then never restored | `options.c:safe_arg()` + `uidlist.c:parse_name_map()` |
| #5544 (URV-2.a) | non-protect_args daemon mode: literal `\*` reached parser because no `unbackslash_arg()` was applied | `io.c:1295-1306`, `clientserver.c:1073` |

Sample fix shape: implement both halves as one symmetric helper. The
client always escapes via `safe_arg_for_daemon()`; the daemon always
unescapes the phase-1 args (those before the `.` CWD marker) via
`unbackslash_arg()`. Tests round-trip the same byte sequences upstream
produces, captured via `tcpdump` against the host.

Why repeated: the escape and unescape are wired through different code
paths and crates. One side can change without the other noticing.

Prevention:

- Pin a contract test: for every well-known metacharacter, the
  client-emitted bytes must round-trip through the daemon's
  `unbackslash_arg` to the original. The test is a single table-driven
  unit test under `crates/daemon/src/daemon/sections/module_access/`.
- When upstream lands a new escape (3.4.4 added the `unescape`
  parameter to `read_args()`), the upstream conformance audit should
  surface it as a TODO line that maps to an oc-rsync follow-up.

## Cluster 6 - Sandbox / openat2 confinement gaps

Pattern: a confinement primitive is too strict or too loose for the
upstream contract.

PRs in this cluster:

| PR | Bug | Fix |
|----|-----|-----|
| #5510 (URV-3 / -K) | `RESOLVE_NO_SYMLINKS` rejected legitimate in-tree symlinks for `--copy-dirlinks` | `enter_follow_dirlinks` drops `RESOLVE_NO_SYMLINKS`, keeps `RESOLVE_BENEATH` |
| #5519 (UTS-16) | Daemon receiver without chroot did not refuse leaf-symlink dest | `open_sandbox_for_dest_strict` promotes ELOOP/ENOTDIR/EXDEV to error on daemon connection |
| #5540 (URV-5.a) | Daemon alt-basis path `--copy-dest=../../etc` accepted on chroot-disabled module | Reject any `..` component in relative alt-basis, then join |

Sample fix shape: the sandbox primitive ships with both a strict and a
follow variant; the caller picks the variant. Path admission filters
sit *before* the sandbox open so the easy cases reject without a
syscall.

Why repeated: SEC-1 was sized around CVE-2026-29518 / CVE-2026-43619
(receiver-side TOCTOU). The same primitive then had to absorb
`-K` (which needs follow-symlinks-in-tree) and alt-basis
(which needs `..`-aware portable fallback). Each new caller exposed a
gap.

Prevention:

- For every receiver / generator syscall site that opens a path
  derived from wire input, add a checklist item to the audit doc:
  "what does this path admit? What does upstream admit at this site?".
  The audit doc lives under `docs/audits/`; the checklist is the spec
  for a future SEC-2 sandbox API consolidation.

## Defense-in-depth audit: rust-landlock as the outer confinement ring

Confinement strength order, strongest first:

1. **rust-landlock** (kernel ruleset, process-wide; see
   <https://github.com/landlock-lsm/rust-landlock>). Already wired at
   `crates/fast_io/src/landlock.rs` behind the `landlock` Cargo feature,
   public entry `restrict_to_module_paths()`. SEC-1.p design at
   `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`.
2. **openat2(RESOLVE_BENEATH)** (per-syscall, Linux 5.6+). Used by
   `DirSandbox::enter()` and `enter_follow_dirlinks()`.
3. **Lexical `..` rejection** (portable fallback for older kernels,
   macOS, the BSDs, Cygwin). Used by #5540's alt-basis admission gate.

Each layer should sit *under* the next. Landlock is the preferred outer
ring because it survives ambient privilege changes (post-chroot,
post-setuid) that rust-landlock's ruleset was specifically designed to
weather. The per-syscall RESOLVE_BENEATH is the middle ring; lexical
rejection is the floor that runs on every platform.

### Cluster 6 mapping

| PR | Layer that landed | Landlock coverage today | Follow-up |
|----|-------------------|--------------------------|-----------|
| #5510 (URV-3 / -K) | per-syscall RESOLVE_BENEATH variant (follow-dirlinks) | SEC-1.p ruleset already permits read of every basis-dir path; no extension needed | none |
| #5519 (UTS-16) | per-syscall sandbox strict-on-daemon | SEC-1.p ruleset restricts the receiver to the module path; redundant on Landlock-enabled builds, primary defence on the others | URV-5.b (#3623): assert ruleset is armed before strict open |
| #5540 (URV-5.a) | lexical `..` rejection | SEC-1.p does *not* yet add the `--copy-dest` / `--link-dest` / `--compare-dest` relative paths to the read-only ruleset; if the lexical gate is ever bypassed, Landlock will refuse the open with EACCES | URV-5.c (#3624): extend `restrict_to_module_paths()` to admit relative alt-basis paths after the same `..` audit; URV-LDL-1 (#3625): same plumbing as a generic helper |

For #5540 specifically, the right shape is the layered one: lexical
rejection rejects the obvious `../../etc` first, RESOLVE_BENEATH covers
the kernels that have it, and rust-landlock catches any future
admission-path regression that ever lets a `..` through. URV-5.b /
URV-5.c / URV-LDL-1 are the tasks that close the layering.

### Cluster 1 mapping (parser-stores-string)

Landlock cannot help here. The defect is wiring a parsed directive to
its transfer-time consumer; it is not a confinement question. Listed
for completeness so the audit explicitly does not over-extend the
ruleset for non-confinement classes.

### Cluster 4 mapping (wire predicates)

Same conclusion: Landlock is irrelevant. NDX_DEL_STATS gating is a wire
correctness question, not a filesystem confinement question.

### Cluster 5 mapping (escape / unescape parity)

Landlock can mitigate the worst-case downstream effect of a botched
unescape: even if a daemon arg sneaks through with a literal `*` that
expands to an attacker-chosen path, the ruleset still refuses paths
outside the module root. This is *not* a primary defence (the parser
fix in #5544 is) but it is the reason Landlock should be enabled in
default builds: it converts every wire-handling regression into an
audit-loggable refusal rather than a successful traversal.

### Recommended follow-ups

- URV-5.b (#3623): assert `restrict_to_module_paths()` was called
  before `open_sandbox_for_dest_strict` accepts a daemon connection.
  The assertion is conditional on the `landlock` feature; on non-
  Landlock builds it is a no-op.
- URV-5.c (#3624): extend `restrict_to_module_paths()` to admit
  validated relative `--copy-dest` / `--link-dest` /
  `--compare-dest` paths under the module root. The validation is
  the same `..` audit that #5540 implemented.
- URV-LDL-1 (#3625): factor the ruleset extension into a generic
  helper so future receiver / sender directives that need to admit a
  bounded set of host paths route through one API.
- Bake target: enable the `landlock` feature on the Linux musl and
  Linux GNU release builds shipped from CI. macOS and Windows fall
  through to the per-syscall and lexical layers as today.

## Cluster 7 - Documentation-only / packaging churn (not interop bugs)

PRs in this cluster do not fix interop; they document or pin the
environment so the testsuite runs at all. Listed for completeness.

| PR | Scope |
|----|-------|
| #5538 | Pin upstream rsync 3.4.4 source build in interop scripts |
| #5539 | Same for workflow YAML matrices |
| #5532 | Add a runtime test for `path = / use chroot = no` (the parser fix landed earlier in #5522) |
| #5533 | Pin Scenario B of the GHSA-rjfm-3w2m-jf4f testsuite as a regression |
| #5541 | Pull-file coverage gap behind a `path = /` module |

No shared root cause; these stand on their own.

## Cross-cutting recommendations (ranked)

1. **Make every parsed daemon directive flow through a typed
   `ServerConfig` field, never a raw `Option<String>` on
   `ModuleDefinition`.** This kills Cluster 1 and Cluster 2 in one
   move.  The mechanical follow-up is to delete every `#[cfg(test)]`
   accessor on `ModuleDefinition` and replace callers with direct
   field reads via `Deref`.
2. **Adopt an upstream-conformance audit doc per upstream release.**
   URV-6 already follows this template. The doc enumerates every wire
   frame, every directive, every escape, with the upstream predicate.
   New oc-rsync code that diverges from the doc is rejected at review.
3. **Mirror the upstream daemon-* testsuite as unit-scoped regression
   tests.** Cluster 4 and the wiring half of Cluster 1 only surfaced
   because we now run `runtests.py` against the daemon. A targeted
   `crates/daemon/src/tests/upstream/` directory that translates each
   upstream `*_test.py` into a Rust integration test would catch the
   class before interop.
4. **Symmetric round-trip tests on every escape/unescape pair.**
   Cluster 5 reappeared as soon as upstream added a new escape; a
   single table-driven round-trip would have caught both halves.
5. **Receiver-driver wiring parity invariant.** Every receiver driver
   (`pipelined`, `pipelined_incremental`, `sync`) must call the same
   helpers in the same order. A property test that introspects the
   call graph (or a doctest that lists the helpers per driver) makes
   the omission in #5542 impossible.

## Cluster assignment cheat-sheet

| PR | Stream | Cluster(s) |
|----|--------|------------|
| #5510 | (sandbox) | 6 |
| #5516 | A | (cli-only, no overlap) |
| #5519 | E | 1, 6 |
| #5520 | C | 1, 4 |
| #5523 | F | 3 |
| #5525 | (alt-dest) | (lstat parity, no overlap) |
| #5526 | B' | 3 |
| #5527 | B'' | 1 |
| #5528 | B' | (format parity, no overlap) |
| #5529 | B' | (alias matcher, no overlap) |
| #5530 | B'' | 1, 2 |
| #5531 | D | 5 |
| #5532 | B' | 7 |
| #5533 | (sec test) | 7 |
| #5534 | B'' | 1, 2 |
| #5535 | C | (zlib, no overlap) |
| #5536 | C | 1, 4 |
| #5537 | A | 7 |
| #5538 | (upstream pin) | 7 |
| #5539 | (workflow pin) | 7 |
| #5540 | (sandbox) | 6 |
| #5541 | (sec test) | 7 |
| #5542 | (receiver driver) | 4 |
| #5543 | (post-3.4.4 audit) | 4 |
| #5544 | (escape parity) | 5 |

The 1+2 and 4 columns carry the highest repeat count and are the
primary targets for the prevention work above.
