# Filter Rule Coverage Matrix

Coverage of rsync filter rule types across the test suite.

Legend:
- Y = covered
- \- = not covered

## Filter Rule Types (Actions)

| Rule Type | Unit Test | Integration Test | Interop (up->oc) | Interop (oc->up) | Daemon Filter Test |
|-----------|-----------|------------------|-------------------|-------------------|--------------------|
| include (`+`) | Y | Y | Y | Y | Y |
| exclude (`-`) | Y | Y | Y | Y | Y |
| protect (`P`) | Y | - | - | - | - |
| risk (`R`) | Y | - | - | - | - |
| hide (`H`) | Y | - | - | - | - |
| show (`S`) | Y | - | - | - | - |
| clear (`!`) | Y | - | - | - | - |
| merge (`.`) | Y | - | - | - | - |
| dir-merge (`:`) | Y | - | Y | Y | - |
| CVS-exclude (`-C`) | Y | Y | - | - | - |

### Notes

- **protect/risk**: Unit tests in `crates/filters/src/tests.rs` cover protect and risk semantics (blocking deletion, risk overriding protect). The interop harness defines `delete-filter-protect` and `delete-filter-risk` vtypes with full prep/verify logic but no scenario in the matrix actually uses them - they are dead code.
- **hide/show**: Unit tests verify sender-only semantics. No integration or interop tests exercise hide/show via CLI.
- **clear**: Unit tests cover clear with side flags. No end-to-end tests.
- **merge**: Unit tests in `merge/tests.rs` cover file reading, recursive merge, and depth limits. No interop test uses `.` (merge) rules via `--filter='. file'` CLI syntax.
- **dir-merge**: Unit tests in `chain.rs` cover per-directory `.rsync-filter` push/pop. The `-FF` interop test (`test_ff_filter_shortcut`) exercises dir-merge in both directions.
- **CVS-exclude**: Unit tests verify pattern list completeness. Integration test `cvs_exclude_ignores_common_files` exercises `-C` flag. No interop test.

## Pattern Syntax Features

| Pattern Feature | Unit Test | Integration Test | Interop (up->oc) | Interop (oc->up) | Daemon Filter Test |
|-----------------|-----------|------------------|-------------------|-------------------|--------------------|
| Wildcards (`*`) | Y | Y | Y | Y | Y |
| Double-star (`**`) | Y | - | - | - | - |
| Question mark (`?`) | Y | - | - | - | - |
| Character class (`[...]`) | Y | - | - | - | - |
| Trailing slash (dir-only) | Y | Y | Y | Y | - |
| Leading slash (anchored) | Y | - | Y | Y | Y |
| Internal slash (anchored) | Y | - | - | - | - |
| Negation (`!` modifier) | Y | - | - | - | - |

### Notes

- **Wildcards**: Extensively tested at all levels. Glob patterns like `*.log`, `*.tmp` used throughout.
- **Double-star**: Unit test `compiled_rule_complex_glob` uses `**/*.o`. Integration test for `cache/preserved/**` inclusion. No interop coverage.
- **Question mark**: Only tested via escape sequence test (`foo\?bar`). No direct `?` wildcard usage in tests.
- **Character class**: Only the error case (`[` invalid pattern) is tested. No valid `[a-z]` pattern tests.
- **Trailing slash**: Unit tests verify directory-only matching (`foo/`, `build/`). Integration test `exclude_directory_pattern` uses `.git/`. Interop anchored test uses `/logs/`.
- **Leading slash**: Unit tests verify anchored patterns (`/foo/bar`). Interop `daemon-filter-exclude-anchored` tests `/secret` and `/logs/` in both directions.
- **Negation**: Unit tests in `tests.rs::negate_tests` and `merge/tests.rs` cover negated patterns thoroughly. No end-to-end or interop tests.

## Modifiers

| Modifier | Unit Test | Integration Test | Interop (up->oc) | Interop (oc->up) | Daemon Filter Test |
|----------|-----------|------------------|-------------------|-------------------|--------------------|
| Sender-only (`s`) | Y | - | - | - | - |
| Receiver-only (`r`) | Y | - | - | - | - |
| Perishable (`p`) | Y | - | - | - | - |
| Xattr-only (`x`) | Y | - | - | - | - |
| Exclude-only (`e`) | Y | - | - | - | - |
| No-inherit (`n`) | Y | - | - | - | - |
| Word-split (`w`) | Y | - | - | - | - |
| CVS mode (`C`) | Y | - | - | - | - |

### Notes

- All modifiers are tested at the parse level in `merge/tests.rs`. None are tested in integration or interop suites.

## CLI Options

| CLI Option | Integration Test | Interop (up->oc) | Interop (oc->up) | Daemon Filter Test |
|------------|------------------|-------------------|-------------------|--------------------|
| `--exclude=PATTERN` | Y | Y | Y | - |
| `--include=PATTERN` | Y | Y | Y | - |
| `--exclude-from=FILE` | Y | Y | Y | - |
| `--include-from=FILE` | Y | - | - | - |
| `--filter=RULE` | - | Y | Y | - |
| `-F` / `-FF` | - | Y | Y | - |
| `-C` (`--cvs-exclude`) | Y | - | - | - |
| `--delete --exclude` | - | Y | - | Y |
| `--delete-excluded` | - | Y | - | - |

### Notes

- **`--include-from`**: Integration test exists but no interop test.
- **`--filter`**: Used in interop `filter-rule` scenario (as `--exclude=*.tmp`) and standalone daemon filter tests. The `filter` rsyncd.conf directive is tested in multiple daemon tests.
- **`-FF`**: Tested bidirectionally in `test_ff_filter_shortcut`.

## Daemon Config Directives

| Directive | Interop (up->oc) | Interop (oc->up) | Direction Tested |
|-----------|-------------------|-------------------|------------------|
| `exclude = PATTERN` | Y | Y | pull and push |
| `filter = RULE` | Y | Y | pull and push |
| `exclude from = FILE` | Y | Y | pull |
| `include from = FILE` | - | - | - |

### Notes

- **`exclude`**: Tested with glob patterns, anchored patterns, and word-split patterns via rsyncd.conf in multiple daemon tests.
- **`filter`**: Tested with `+ *.txt + *.rs + */ - *` whitelist pattern and `- *.tmp - *.bak` exclude patterns.
- **`exclude from`**: Tested in `test_daemon_filter_from_files` with external exclude file.
- **`include from`**: Not tested in daemon context via rsyncd.conf.

## Interop Test Inventory

### Standard Matrix Scenarios (run per-version)

| Scenario | Flags | Versions |
|----------|-------|----------|
| exclude | `-av --exclude=*.log` | 3.0.9, 3.1.3, 3.4.1 |

### Extended Matrix Scenarios (3.4.1 only)

| Scenario | Flags |
|----------|-------|
| include-exclude | `-rv --include=*.txt --include=*/ --exclude=*` |
| filter-rule | `-av --exclude=*.tmp` |
| merge-filter | `-av -FF` |
| exclude-from | `-av --exclude-from=exclude_patterns.txt` |

### Standalone Daemon Filter Tests

| Test Name | What It Tests | Directions |
|-----------|---------------|------------|
| filter-rules | `--filter='- *.log' --filter='- *.tmp'` push | up->oc |
| delete-excluded | `--delete-excluded --exclude='*.bak'` push | up->oc |
| exclude-include-precedence | `--include='*.txt' --include='*/' --exclude='*'` push | up->oc |
| delete-with-filters | `--delete --exclude='*.keep'` push | up->oc |
| ff-filter-shortcut | `-FF` with `.rsync-filter` dir-merge | up->oc, oc->up |
| daemon-server-side-filter | `exclude=` and `filter=` in rsyncd.conf | pull, push |
| daemon-filter-exclude-glob | `exclude = *.tmp *.o *.log` in rsyncd.conf | oc-pull, up-pull |
| daemon-filter-exclude-anchored | `exclude = /secret /logs/` in rsyncd.conf | oc-pull, up-pull |
| daemon-filter-include-exclude-star | `filter = + *.txt + *.rs + */ - *` in rsyncd.conf | oc-pull, up-pull |
| daemon-filter-directive-types | `filter = - *.tmp - *.bak - *.cache` in rsyncd.conf | oc-pull, up-pull |
| daemon-filter-overlapping-rules | `filter = + important.log + .keep.tmp - *.log - *.tmp` | oc-pull, up-pull |
| daemon-filter-from-files | `exclude from = FILE` in rsyncd.conf | oc-pull, up-pull |
| daemon-filter-push-direction | `exclude = *.dump *.dmp` in rsyncd.conf push | oc-push, up-push |

## Coverage Gaps Summary

### High Priority (no interop coverage)

1. **protect (`P`) and risk (`R`)** - Only unit-tested. The interop vtypes `delete-filter-protect` and `delete-filter-risk` are defined with full prep/verify logic but never wired into any scenario.
2. **`--include-from=FILE`** - Only integration-tested, no interop test.
3. **`include from =` in rsyncd.conf** - Not tested at any level for daemon config.

### Medium Priority (no end-to-end coverage)

4. **hide (`H`) and show (`S`)** - Sender-only filter semantics only unit-tested.
5. **clear (`!`)** - Only unit-tested. Important for rule chain reset behavior.
6. **merge (`.`) via `--filter='. FILE'`** - Only unit-tested for file reading. The `-FF` test covers dir-merge but not explicit merge rules.
7. **Negation (`!` modifier)** - Comprehensive unit tests but no end-to-end or interop testing.
8. **Double-star (`**`)** - Only unit-tested.
9. **All modifiers** (`s`, `r`, `p`, `x`, `e`, `n`, `w`, `C`) - Only parse-level tests.

### Low Priority

10. **Question mark (`?`) wildcard** - Only tested as escaped literal.
11. **Character classes (`[...]`)** - Only error case tested.
12. **CVS-exclude (`-C`)** - Integration test exists but no interop test.
