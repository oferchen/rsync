# Windows ACL/xattr CI Matrix Audit

Tracking issue: #1869

## 1. Summary

Windows CI builds the workspace with `--all-features` but exercises only
`core`, `engine`, and `cli` tests; the `metadata` crate, which owns all
ACL/xattr surface area, is never tested on `windows-latest`. ACL and xattr
compile to no-op stubs on Windows yet still emit user-visible warnings, and
no job verifies those warnings, error mappings, or stub APIs. This audit
lists existing coverage, locates the Windows code paths, and recommends
matrix entries to close the gap.

## 2. Current Windows CI matrix

Source of truth: `.github/workflows/`. Of 16 workflow files, only two run on
`windows-latest`:

- `ci.yml:151-197` - job `windows-test` (matrix: stable, beta, nightly).
  Builds `--workspace --all-features`, then runs `cargo nextest run -p core
  -p engine -p cli --all-features`. The `metadata` crate is intentionally
  excluded by the comment on line 185.
- `release-cross.yml:599` - job `build-windows`. Release artifact build
  only; no test step.

`ci.yml:202-226` adds a `windows-gnu-cross-check` job that runs `cargo check`
for `x86_64-pc-windows-gnu` on `ubuntu-latest`. This catches compile errors
but executes nothing.

`coverage.yml:40` runs only on `ubuntu-latest`. The `_test-features.yml`
reusable workflow (`ci.yml:141-147`) hard-codes `runs-on: ubuntu-latest`
(`_test-features.yml:25`) and gates `io_uring` and `copy_file_range` to Linux
via `linux_only: true`, but no entry currently isolates `acl` or `xattr` as
named feature combinations on any OS.

Skipped on Windows today: every test in `crates/metadata/`, every interop
job (all run on `ubuntu-latest`), and the feature-flag matrix.

## 3. Current ACL/xattr code paths on Windows

`crates/metadata/Cargo.toml:26-39` declares `xattr = ["dep:xattr"]` and
`acl = ["dep:exacl"]`. Both upstream crates are unix-only, so the cfg gates
in `crates/metadata/src/lib.rs` route Windows builds to stub modules:

- `lib.rs:71-94` - `acl_exacl` is selected only on linux/macos/freebsd;
  ios/tvos/watchos use `acl_stub`; everything else (Windows, Android,
  illumos, etc.) falls through to `acl_noop`.
- `lib.rs:131-143` - `xattr` and `nfsv4_acl` are gated on `all(unix,
  feature = "xattr")`; Windows always picks `xattr_stub` and
  `nfsv4_acl_stub`.
- `crates/metadata/src/acl_noop.rs:19-66` - emits a one-time `warning:
  ACLs are not supported on this platform` via `Once`, then returns
  `Ok(())` from `sync_acls` and `apply_acls_from_cache`.
  `get_rsync_acl` returns `RsyncAcl::from_mode` (lines 43-49).
- `crates/metadata/src/xattr_stub.rs:14-60` - mirrors the same pattern
  for `sync_xattrs`, `read_xattrs_for_wire`, and
  `apply_xattrs_from_list`.
- `crates/metadata/src/mapping_win.rs:1-40` - Windows ownership mapping
  stub used by `--usermap`/`--groupmap`/`--chown`.
- `crates/metadata/src/apply_batch.rs:546-569` - the only Windows-gated
  test in the crate (`windows_readonly_attribute`); covers permission
  bits, not ACL/xattr.
- `crates/metadata/src/stat_cache.rs:686` - additional `#[cfg(windows)]`
  stat handling, unrelated to ACL/xattr semantics.

Stub unit tests exist (`acl_noop.rs:68-120`, `xattr_stub.rs:62-113`) but
run only when nextest targets `metadata`, which Windows CI never does.

## 4. Gap analysis

| Surface                              | Linux | macOS | Windows |
|--------------------------------------|-------|-------|---------|
| `metadata` crate unit tests          | yes   | no    | no      |
| `acl_noop` stub tests                | n/a   | n/a   | no      |
| `xattr_stub` stub tests              | n/a   | n/a   | no      |
| `mapping_win` parse-error tests      | n/a   | n/a   | no      |
| `--xattrs` / `-X` CLI surface        | yes   | yes   | no      |
| `--acls` / `-A` CLI surface          | yes   | yes   | no      |
| `windows_readonly_attribute` test    | n/a   | n/a   | yes     |
| Warning-emission `Once` semantics    | n/a   | n/a   | no      |
| `--no-default-features` build check  | yes   | no    | no      |

macOS shares the gap for `metadata` tests (`ci.yml:263-267`) but exercises
`acl_exacl`/`xattr` rather than stubs, so failure modes differ.

## 5. Recommendation

Add the matrix entries below. Complexity reflects expected churn in
workflow YAML and any test gating (`#[cfg(windows)]` filters) needed to
keep Linux-only assertions from running on Windows.

| Entry                              | Runner          | Features              | Filter                              | Effort |
|------------------------------------|-----------------|-----------------------|-------------------------------------|--------|
| `windows-metadata-stubs` (new job) | windows-latest  | `--all-features`      | `-p metadata -E 'test(acl) + test(xattr) + test(mapping_win) + test(windows_)'` | S |
| Extend `windows-test` package set  | windows-latest  | `--all-features`      | add `-p metadata` to line 187       | S |
| `windows-no-acl-xattr`             | windows-latest  | `--no-default-features --features "zstd,lz4,parallel"` | `-p metadata -p cli` | M |
| `windows-stub-warnings` (optional) | windows-latest  | `--all-features`      | `-E 'test(stub_emits_warning)'` plus new tests asserting `Once` semantics | M |
| Add `acl`/`xattr` rows to `_test-features.yml` | ubuntu+windows  | `--features acl,xattr` | `-p metadata`                       | L |

The first two entries (S) are mechanical edits to `ci.yml`; the third (M)
adds a new job and verifies a `--no-default-features` Windows binary. The
optional fourth needs small new tests around the `Once` warn-once
behavior. The fifth (L) requires turning `_test-features.yml` into a
cross-OS strategy with a parallel `windows-latest` runner.

## 6. References

- Workflows: `.github/workflows/ci.yml:151-227`,
  `.github/workflows/_test-features.yml:22-86`,
  `.github/workflows/coverage.yml:40-69`,
  `.github/workflows/release-cross.yml:599`.
- Code: `crates/metadata/Cargo.toml:26-39`,
  `crates/metadata/src/lib.rs:71-170`,
  `crates/metadata/src/acl_noop.rs`,
  `crates/metadata/src/xattr_stub.rs`,
  `crates/metadata/src/mapping_win.rs`,
  `crates/metadata/src/apply_batch.rs:546`.
- Upstream parity: `target/interop/upstream-src/rsync-3.4.1/options.c:1854`
  (warn-on-missing-ACL behavior matched by `acl_noop.rs:19-24`).
