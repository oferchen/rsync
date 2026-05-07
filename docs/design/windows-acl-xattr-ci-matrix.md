# Windows ACL/xattr CI matrix (#1869)

This note specifies the `windows-latest` CI matrix entry that exercises
the Windows ACL and xattr code paths under explicit feature flags. The
default `windows-test` job builds the workspace with `--all-features`
but only tests `core`, `engine`, and `cli`, so the `metadata` crate's
Win32 ACL surface and NTFS Alternate Data Streams xattr surface never
run on Windows today. No wire-protocol changes; no upstream-compat
changes.

## 1. Adjacent issues

- **#1866 Windows ACLs (in flight).** Read/write DACL round-trip via
  the `windows-rs` crate is being landed in
  `crates/metadata/src/acl_windows.rs`. The CI entry below is the
  regression net so #1866 work and any later refactor surface failures
  on master rather than during a release.
- **#1867 Windows xattrs (done).** NTFS ADS-backed xattr storage is
  merged in `crates/metadata/src/xattr_windows.rs`
  (`list_attributes`, `read_attribute`, `write_attribute`,
  `remove_attribute`). CI must keep exercising `FindFirstStreamW` /
  `FindNextStreamW` and the `:$DATA` suffix path so encoding bugs on
  unicode stream names cannot regress silently.
- **#1635 GNU CI cross-reference.** #1635 tracks the
  `x86_64-pc-windows-gnu` cross-check (`windows-gnu-cross-check` in
  `.github/workflows/ci.yml`). That job runs on `ubuntu-latest` and
  proves portable compilation only; it never invokes the Win32 runtime
  surface and cannot reach NTFS ADS (no NTFS volume on Linux runners).
  The MSVC ACL/xattr entry and the GNU cross-check are complementary -
  GNU validates portable build, MSVC validates the runtime FFI surface.

## 2. Proposed matrix entry

A new top-level job `windows-acl-xattr` lives in
`.github/workflows/ci.yml`. Shape:

| Field | Value |
|-------|-------|
| `runs-on` | `windows-latest` |
| `needs` | `lint` |
| `timeout-minutes` | 30 |
| Toolchain | `stable` (matrix expansion deferred to post-#1866) |
| Cache key | `ci-windows-acl-xattr-cargo-...` |
| Build step | `cargo build --locked -p metadata --features acl,xattr` |
| Crate test | `cargo nextest run --locked -p metadata --features acl,xattr` |
| Filter test | `cargo nextest run --locked --workspace --features acl,xattr -E 'test(acl) \| test(xattr) \| test(ads) \| test(stream)'` |

Three rationales for the shape:

- The `metadata` build step proves the `acl` and `xattr` cargo features
  link on MSVC; pure test runs would skip the link step on cache hits.
- The targeted `-p metadata` nextest run keeps wall time bounded and
  pins the regression target for #1866 / #1867.
- The workspace-wide `-E` filter catches future ACL / ADS tests that
  land outside `metadata` (transfer-level integration, daemon path)
  without forcing a workspace-wide nextest.

The job pins `--features acl,xattr` rather than `--all-features` so a
red run is immediately attributable to those two axes.

## 3. Test plan: round-trip ACL/xattr via push/pull

Unit suites cover read and write of one entry. The integration story
needs end-to-end push and pull through the `oc-rsync` binary on
`windows-latest`:

1. **Local push.** `oc-rsync -aAX src/ dst/`. Source tree contains:
   - One file with a non-trivial DACL (deny-ACE before allow-ACE) so
     the ACE ordering preserved by `SetSecurityInfo` is observable.
   - One file with three NTFS ADS streams (`:meta`, `:tag`, and a
     unicode-named stream) so list and round-trip both exercise.
   Assert on the destination: `read_acl` returns the source DACL
   byte-for-byte; `list_attributes` returns the same stream names;
   each stream's content matches.
2. **Local pull.** `oc-rsync -aAX dst/ src2/`. Pulled tree must equal
   `src/` for ACLs and xattrs. Validates symmetry of receiver-side
   metadata application and exposes any sender-side stripping bug.
3. **Daemon push and pull.** Same binary in `--daemon` mode, same
   assertions as steps 1-2. Daemon path uses the same `metadata` crate
   but routes through `crates/daemon/`, so a daemon-side regression
   surfaces here.
4. **Quick-check skip mitigation.** Tests must backdate destination
   files or use distinct sizes so rsync's quick-check does not skip
   the transfer (same pattern as `crates/transfer/tests/`).

Test files target:

- `crates/metadata/tests/acl_windows_roundtrip.rs` - push/pull harness
  against a local temp dir.
- `crates/metadata/tests/xattr_windows_roundtrip.rs` - mirror for ADS.
- `tests/interop/windows_metadata.rs` - workspace-level driver that
  invokes `oc-rsync` with `-aAX` and asserts both metadata classes.

All three are `#[cfg(target_os = "windows")]` and stay no-op on
Linux/macOS, so the existing nextest matrix is unaffected.

## 4. Cross-reference: #1635 GNU CI

#1635's `windows-gnu-cross-check` complements this job:

- `GetSecurityInfo` and `SetSecurityInfo` resolve through `advapi32.dll`,
  which the GNU cross-check links but never invokes.
- NTFS ADS requires a real NTFS volume; Linux cross-check runners do
  not provide one.

The `windows-acl-xattr` entry covers what #1635 cannot: runtime
behaviour on a real Windows host. Together the two jobs close the
matrix gap. Future changes to either side should update both this doc
and the #1635 GNU cross-check note in lockstep.

## 5. Non-goals

- No new wire-protocol features. The job exercises a metadata code
  path whose protocol bytes already exist in protocol 30+ (`-A`/`-X`).
- No daemon CI in this entry. Daemon round-trip belongs to a follow-up
  under the interop matrix in `tools/ci/run_interop.sh`.
- No POSIX ACL coverage. The `exacl`-backed POSIX path is exercised by
  the existing Linux nextest job.

## 6. References

- `.github/workflows/ci.yml` - `windows-acl-xattr` job and
  `windows-gnu-cross-check` job.
- `crates/metadata/src/acl_windows.rs` - Win32 ACL implementation
  (#1866).
- `crates/metadata/src/xattr_windows.rs` - NTFS ADS implementation
  (#1867).
- Issue refs: #1635, #1866, #1867, #1869.
