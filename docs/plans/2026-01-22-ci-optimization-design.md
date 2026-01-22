# CI Optimization Design

**Date:** 2026-01-22
**Status:** Approved
**Goal:** Optimize CI for speed while maintaining full test coverage and all 10 release artifacts

---

## Summary

| Metric | Current | Target | Method |
|--------|---------|--------|--------|
| PR feedback (first fail) | ~30 min | ~2 min | Split lint from test |
| Full CI pass | ~90 min | ~20 min | Parallel execution |
| Release build | ~65 min | ~35 min | Matrix parallelization |
| Artifacts | 10 | 10 | No change |

---

## Design Decisions

1. **Full coverage always** - All tests run on every PR (no tiered coverage)
2. **Keep all 10 artifacts** - No changes to release output
3. **Balanced parallelization** - High-impact items only
4. **Centralized config** - Reusable workflows + GitHub Variables

---

## Workflow Structure

### Files

```
.github/workflows/
├── ci.yml                    # Main CI (calls reusables)
├── release-cross.yml         # Release builds (matrix parallelized)
├── interop-validation.yml    # Standalone interop (calls reusable)
├── _test-features.yml        # Reusable: feature flag testing
└── _interop.yml              # Reusable: interop with caching
```

### GitHub Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `CACHE_VERSION` | `v1` | Bump to invalidate all caches |
| `UPSTREAM_RSYNC_VERSION` | `3.4.1` | Upstream rsync cache key |

---

## CI Workflow (`ci.yml`)

### Job Dependency Graph

```
lint (ubuntu, 2 min)
    │
    ├──► test (ubuntu, 8 min)
    ├──► windows-test (windows-latest, 8 min)
    ├──► feature-flags (6 matrix jobs via _test-features.yml)
    └──► interop (via _interop.yml, 15 min)
```

### Key Changes

1. Split `lint-and-test` into separate `lint` and `test` jobs
2. Change interop to `needs: lint` (not `needs: lint-and-test`)
3. Use reusable workflows for feature-flags and interop

---

## Reusable Workflow: `_test-features.yml`

Centralizes feature flag test matrix:

- no-default-features
- parallel
- async
- concurrent-sessions
- tracing
- serde

Inputs:
- `cache_version` (string, default: 'v1')
- `runner` (string, default: 'ubuntu-latest')

---

## Reusable Workflow: `_interop.yml`

Centralizes interop testing with upstream rsync caching:

- Caches compiled upstream rsync binary
- Cache key: `upstream-rsync-{version}-{os}`
- Saves ~3 min per run

Inputs:
- `cache_version` (string, default: 'v1')
- `upstream_rsync_version` (string, default: '3.4.1')

---

## Release Workflow (`release-cross.yml`)

### Linux GNU Matrix

```yaml
strategy:
  matrix:
    include:
      - target: x86_64-unknown-linux-gnu
        arch: amd64
      - target: aarch64-unknown-linux-gnu
        arch: arm64
```

**Savings:** ~20 min (parallel instead of sequential)

### macOS Matrix

```yaml
strategy:
  matrix:
    include:
      - target: x86_64-apple-darwin
        arch: x86_64
      - target: aarch64-apple-darwin
        arch: aarch64
```

**Savings:** ~20 min (parallel instead of sequential)

### Artifact Collection

```yaml
- uses: actions/download-artifact@v4
  with:
    pattern: dist-*
    merge-multiple: true
```

---

## Caching Strategy

### Shared Cache Keys

| Job | Shared Key | Specific Key |
|-----|------------|--------------|
| lint | `ci-{os}-cargo-{version}-{lock_hash}` | - |
| test | Same | `test` |
| feature-flags | Same | `features-{name}` |
| interop | Same | `interop` |
| windows | `ci-windows-cargo-{version}-{lock_hash}` | - |

### Upstream rsync Cache

```yaml
key: upstream-rsync-{version}-{os}-{script_hash}
path: target/interop/upstream-src
```

---

## Artifacts Preserved

| Platform | Type | Artifacts |
|----------|------|-----------|
| Linux GNU | deb | amd64, arm64 |
| Linux GNU | rpm | x86_64, aarch64 |
| Linux musl | tarball | x86_64, aarch64 |
| macOS | tarball | x86_64, aarch64 |
| Windows | tarball + zip | x86_64 |
| **Total** | | **10** |

---

## Implementation Plan

1. Create feature branch `ci-optimization`
2. Create `_test-features.yml` reusable workflow
3. Create `_interop.yml` reusable workflow
4. Refactor `ci.yml` (split lint/test, use reusables)
5. Refactor `release-cross.yml` (Linux/macOS matrix)
6. Update `interop-validation.yml` to use reusable
7. Test on feature branch
8. Validate all 10 artifacts produced
9. Merge to master

---

## Rollback Plan

All changes are workflow YAML files. Rollback by reverting the commit.
