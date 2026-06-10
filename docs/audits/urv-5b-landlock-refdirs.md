# URV-5.b: ref_dir inclusion in SEC-1.p Landlock allowlist

**Status:** Audit complete. Two-part gap confirmed. Follow-up filed as URV-5.b.1.

**Date:** 2026-06-10

**Scope:** Verify that daemon-admitted `ref_dirs` from `--copy-dest`, `--link-dest`,
and `--compare-dest` are inside the kernel-enforced Landlock allowlist engaged by
the SEC-1.p layer.

## Verdict

ref_dirs are **NOT** covered. Two independent gaps stack:

1. **Admission gap.** `validate_client_paths_in_module`
   (`crates/daemon/src/daemon/sections/module_access/transfer.rs:131-193`) only
   inspects `--temp-dir`, `--partial-dir`, and `--backup-dir`. Absolute
   `--copy-dest=/foo`, `--link-dest=/foo`, and `--compare-dest=/foo` arguments are
   never matched and flow straight into `cfg.reference_directories` at
   `crates/daemon/src/daemon/sections/module_access/client_args.rs:392-418` and
   `:495-509`.
2. **Allowlist gap.** `engage_landlock_sandbox`
   (`crates/daemon/src/daemon/sections/module_access/transfer.rs:217-297`) hard-codes
   the ruleset roots to `vec![module.path.as_path()]` at line 253. Even if the
   admission layer were widened to accept out-of-module ref_dirs, the Landlock
   ruleset would still deny every read against them with `EACCES`.

The call ordering in `process_approved_module`
(`transfer.rs:506-739`) is:

```
validate_client_paths_in_module     // line 659 (does NOT see ref-dest flags)
apply_privilege_restrictions_...    // line 670 (chroot + uid/gid drop)
build_server_config                 // line 712 (populates cfg.reference_directories)
build_daemon_filter_rules           // line 720
engage_landlock_sandbox             // line 737 (roots = [module.path] only)
```

`cfg.reference_directories` is fully populated by line 712, three frames before
the Landlock ruleset is built at line 737. The information required to widen the
allowlist is available; the call simply does not consume it.

## Concrete exploit on `use chroot = no`

Daemon config:

```
[scratch]
    path = /srv/scratch
    use chroot = no
```

Client invocation: `rsync ./local.txt rsync://host/scratch/ --copy-dest=/etc`

Current behaviour:

- `validate_client_paths_in_module` returns `Ok(true)` (no `--temp-dir` /
  `--partial-dir` / `--backup-dir` to inspect).
- No chroot, so `/etc` is reachable.
- `cfg.reference_directories` gains `{ path: "/etc", kind: Copy }`.
- Landlock allowlist = `[/srv/scratch]`; first read against `/etc` returns
  `EACCES`.
- Net result: the operator sees a generic kernel-issued permission denial from
  the receiver path instead of the daemon's explicit
  `@ERROR: ... outside module root` reply.

The SEC-1.p design note in `transfer.rs:125-128` is explicit that REJECT at
admission time is preferred over widening the kernel allowlist:

> rsync's own chroot mode behaves the same way, and expanding the writable
> surface to honour an attacker-supplied prefix undermines the whole point of
> the sandbox.

URV-5.a (PR #5540, open) closes the relative-path side of this gap by rejecting
`..` components in `--copy-dest=REL`, `--link-dest=REL`, `--compare-dest=REL`.
It does **not** address absolute ref-dest paths.

## Interaction with URV-5.a

URV-5.a (`crates/daemon/src/daemon/sections/module_access/client_args.rs`,
PR #5540) tightens relative-path ref-dest admission only. Absolute paths still
bypass both layers:

| Form | URV-5.a guard | Landlock allowlist |
| --- | --- | --- |
| `--copy-dest=../etc` (relative) | reject | n/a |
| `--copy-dest=etc` (relative, clean) | accept, joined under module root | covered (subpath) |
| `--copy-dest=/etc` (absolute) | **pass-through** | **not covered** (EACCES at first read) |

The absolute-path row is the URV-5.b gap.

## Fix proposal (follow-up URV-5.b.1)

Preferred: extend `validate_client_paths_in_module` to inspect `--copy-dest`,
`--link-dest`, and `--compare-dest` in both two-arg
(`--copy-dest /abs/path`) and `=value` (`--copy-dest=/abs/path`) forms.
Apply the same canonicalisation + `starts_with(module_root)` check already used
for the backup/temp/partial trio. Reject with
`@ERROR: --copy-dest path '<raw>' is outside module root` on miss.

This matches the SEC-1.p section-10 recommendation (REJECT over widen) and
re-uses the existing helper, keeping the Landlock ruleset minimal. The
allowlist at `transfer.rs:253` stays `vec![module.path.as_path()]`; admitted
ref_dirs are now guaranteed to be subpaths of `module.path`, which
`PathBeneath::new(module.path, ...)` already covers transitively.

Alternative (rejected): widen `engage_landlock_sandbox` to thread
`cfg.reference_directories` into `restrict_to_module_paths`. This contradicts
the SEC-1.p design ("expanding the writable surface ... undermines the whole
point of the sandbox") and trades a clean `@ERROR` reply for an opaque
`EACCES` deep in the receiver path.

## Estimated cost

- ~30 LoC in `validate_client_paths_in_module` (three flag matches + two parse
  branches for `=value`).
- 6-8 new unit tests in `crates/daemon/src/daemon/sections/module_access/tests.rs`
  mirroring the existing backup/temp/partial coverage:
  `validate_rejects_copy_dest_outside_module`,
  `validate_rejects_link_dest_outside_module`,
  `validate_rejects_compare_dest_outside_module`, plus `=value` form coverage.
- Updated rustdoc on `validate_client_paths_in_module` to list ref-dest flags.

Total: ~80-100 LoC including tests. Files in URV-5.b.1 rather than this audit
commit because URV-5.a (PR #5540) is still open and modifies the same admission
path; landing URV-5.b.1 ahead of URV-5.a would force a merge conflict on the
admission helper.

## Citations

- `crates/fast_io/src/landlock.rs:94-129` - `restrict_to_module_paths` signature.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:131-193` - URV-5.a
  guard (absolute-path branch, ref-dest flags absent).
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:247-253` - Landlock
  roots construction (`module.path` only).
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:506-739` - call
  ordering in `process_approved_module`.
- `crates/daemon/src/daemon/sections/module_access/client_args.rs:392-418`,
  `:495-509` - ref_dir admission into `cfg.reference_directories`.
- `crates/daemon/src/daemon/sections/module_access/client_args.rs:255-258` -
  relative ref_dir resolution against module root (URV-5.a covers `..`).
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` - section 10
  REJECT-over-widen rationale.
