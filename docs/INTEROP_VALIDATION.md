# Interoperability Validation

This document describes the interoperability validation framework used to ensure that `oc-rsync` maintains compatibility with upstream rsync in terms of exit codes and error message formats.

## Overview

The interop validation framework ensures long-term compatibility with upstream rsync by:

1. **Exit Code Validation**: Verifies that `oc-rsync` returns the same exit codes as upstream rsync for identical error conditions
2. **Message Format Validation**: Ensures that error messages match upstream format (accounting for Rust-specific differences)

The framework tests against three upstream rsync versions:
- **3.0.9** (legacy compatibility)
- **3.1.3** (common production version)
- **3.4.1** (latest stable)

## Architecture

```
tests/interop/
├── exit_codes/
│   ├── scenarios.toml           # Test scenario definitions
│   ├── golden-3.0.9.toml        # Expected exit codes for 3.0.9
│   ├── golden-3.1.3.toml        # Expected exit codes for 3.1.3
│   └── golden-3.4.1.toml        # Expected exit codes for 3.4.1
└── messages/
    ├── golden-3.0.9.toml        # Expected messages for 3.0.9
    ├── golden-3.1.3.toml        # Expected messages for 3.1.3
    └── golden-3.4.1.toml        # Expected messages for 3.4.1

xtask/src/commands/interop/
├── exit_codes/
│   ├── scenarios.rs             # Scenario loading and filtering
│   ├── runner.rs                # Scenario execution engine
│   └── golden.rs                # Golden file management
├── messages/
│   ├── extractor.rs             # Message extraction from stderr
│   ├── normalizer.rs            # Message normalization
│   └── golden.rs                # Golden message database
└── shared/
    ├── upstream.rs              # Upstream binary detection
    └── util.rs                  # Common utilities
```

## Exit Code Coverage

The framework validates all 25 exit codes defined in upstream rsync:

| Code | Meaning | Test Coverage |
|------|---------|---------------|
| 0 | Success | ✓ (help, version, dry-run) |
| 1 | Syntax or usage error | ✓ (invalid options, missing args) |
| 2 | Protocol incompatibility | ✓ (unsupported protocol) |
| 3 | Errors selecting input/output files | ✓ (missing files, exclusions) |
| 4 | Requested action not supported | ⚠ (platform-dependent) |
| 5 | Error starting client-server protocol | ✓ (daemon connection) |
| 10 | Error in socket I/O | ⚠ (requires network failure) |
| 11 | Error in file I/O | ✓ (permission denied) |
| 12 | Error in protocol data stream | ⚠ (requires corruption) |
| 13 | Errors with program diagnostics | ⚠ (requires log file error) |
| 14 | Error in IPC code | ⚠ (internal error) |
| 15 | Sibling process crashed | ⚠ (requires process injection) |
| 16 | Sibling terminated abnormally | ⚠ (requires signal) |
| 19 | Received SIGUSR1 | ⚠ (requires signal) |
| 20 | Received SIGINT/SIGTERM/SIGHUP | ⚠ (requires signal) |
| 21 | waitpid() failed | ⚠ (internal error) |
| 22 | Error allocating memory | ⚠ (requires OOM) |
| 23 | Partial transfer | ✓ (permission on subset) |
| 24 | Files vanished | ⚠ (timing-sensitive) |
| 25 | Max delete limit | ✓ (--max-delete) |
| 30 | Timeout in data send/receive | ⚠ (timing-sensitive) |
| 35 | Timeout waiting for daemon | ⚠ (requires daemon setup) |
| 124 | Remote shell failed | ⚠ (requires SSH) |
| 125 | Remote shell killed | ⚠ (requires SSH + signal) |
| 126 | Remote command not executable | ⚠ (requires SSH) |
| 127 | Remote command not found | ✓ (nonexistent shell) |

**Legend:**
- ✓ = Reliably testable
- ⚠ = Skipped (timing-sensitive, platform-dependent, or requires special setup)

## Message Normalization

Messages from `oc-rsync` include Rust-specific metadata that upstream rsync does not have. The normalizer handles these differences:

### Rust Source Suffix

**Upstream rsync:**
```
rsync: error in file IO [sender]
```

**oc-rsync:**
```
rsync: error in file IO at crates/core/src/message.rs:123 [sender=0.5.0]
```

**Normalized (both):**
```
rsync: error in file IO [sender]
```

### Normalization Rules

1. **Strip Rust source locations**: `at crates/*/src/*.rs:123` → removed
2. **Strip version from role trailers**: `[sender=0.5.0]` → `[sender]`
3. **Normalize absolute paths**: `/tmp/xyz` → `<path>`
4. **Normalize whitespace**: Multiple spaces → single space, trim edges
5. **Preserve role trailers**: `[sender]`, `[receiver]`, `[generator]`, etc.

## Usage

### Local Validation

```bash
# Build upstream rsync binaries (required first time)
bash tools/ci/run_interop.sh

# Validate exit codes against all upstream versions
cargo xtask interop exit-codes

# Validate exit codes for a specific version
cargo xtask interop exit-codes --version 3.4.1

# Validate with verbose output
cargo xtask interop exit-codes --verbose

# Validate message formats
cargo xtask interop messages

# Run all validations (exit codes + messages)
cargo xtask interop all
```

### Regenerating Golden Files

When upstream behavior changes or new scenarios are added, regenerate the golden files:

```bash
# Regenerate exit code golden files
cargo xtask interop exit-codes --regenerate

# Regenerate message golden files
cargo xtask interop messages --regenerate

# Regenerate for a specific version only
cargo xtask interop exit-codes --regenerate --version 3.4.1
```

**⚠️ Important:** Always review changes before committing regenerated golden files. Unexpected changes may indicate regressions in `oc-rsync`.

### Adding New Test Scenarios

Edit `tests/interop/exit_codes/scenarios.toml`:

```toml
[[scenario]]
name = "my_new_test"
exit_code = 11
args = ["rsync", "source", "dest"]
setup = "mkdir -p source && chmod 000 source"
description = "Test permission denied on source directory"
skip = false  # Set to true for timing-sensitive tests
```

Then regenerate golden files:

```bash
cargo xtask interop exit-codes --regenerate
```

## CI Integration

### Automatic Validation

The `.github/workflows/interop-validation.yml` workflow runs automatically on:

- **Push to master/main**: Validates every commit
- **Pull requests**: Ensures changes don't break compatibility
- **Nightly schedule**: Detects drift in upstream versions

### Workflow Jobs

1. **validate-exit-codes**: Runs exit code validation against all upstream versions
2. **validate-messages**: Runs message format validation
3. **regenerate-goldens**: Manual workflow to regenerate golden files (workflow_dispatch only)

### Manual Golden Regeneration in CI

To regenerate golden files in CI:

1. Go to Actions → Interop Validation
2. Click "Run workflow"
3. Download the `regenerated-golden-files` artifact
4. Review changes and commit if appropriate

## Troubleshooting

### "No upstream rsync binaries found"

**Problem:** The validation can't find upstream rsync binaries.

**Solution:**
```bash
# Build the upstream binaries first
bash tools/ci/run_interop.sh

# Verify they exist
ls -la target/interop/upstream-install/*/bin/rsync
```

### "Golden file not found"

**Problem:** Golden files haven't been generated yet.

**Solution:**
```bash
# Generate golden files for the first time
cargo xtask interop exit-codes --regenerate
cargo xtask interop messages --regenerate
```

### Exit Code Mismatch

**Problem:** A scenario returns a different exit code than expected.

**Investigation:**
1. Run with `--verbose` to see detailed execution:
   ```bash
   cargo xtask interop exit-codes --verbose
   ```

2. Check if the scenario is reliable:
   - Some scenarios are timing-sensitive and marked with `skip = true`
   - Platform-specific behavior may cause differences

3. If the mismatch is legitimate:
   - Check if it's a regression in `oc-rsync`
   - Or regenerate goldens if upstream behavior changed

### Message Format Differences

**Problem:** Messages don't match after normalization.

**Investigation:**
1. Run with `--verbose` to see the actual differences:
   ```bash
   cargo xtask interop messages --verbose
   ```

2. Check the normalization rules in `xtask/src/commands/interop/messages/normalizer.rs`

3. Common causes:
   - New role trailers not recognized
   - Path patterns not covered by normalization
   - Whitespace differences

## Design Rationale

### Why Golden Files?

Golden files provide:
- **Stability**: Upstream behavior is captured at specific versions
- **Traceability**: Changes are visible in version control
- **Debugging**: Easy to diff expected vs actual
- **Independence**: Tests run without network access

### Why Multiple Upstream Versions?

Testing against 3.0.9, 3.1.3, and 3.4.1 ensures:
- **Legacy compatibility**: Scripts written for old rsync still work
- **Production coverage**: Most servers run 3.1.x
- **Future-proofing**: Latest version shows upcoming changes

### Why Separate Exit Code and Message Tests?

- **Exit codes** are a hard contract (scripts depend on them)
- **Messages** are softer (informational, can vary slightly)
- Separating them allows different tolerance levels

## Maintenance

### When to Update

Regenerate golden files when:
1. **Upstream rsync releases a new version** you want to test against
2. **New test scenarios** are added to `scenarios.toml`
3. **oc-rsync behavior changes** legitimately (not a regression)

### Review Checklist

Before committing regenerated golden files:

- [ ] Reviewed diff for unexpected changes
- [ ] Verified changes are intentional (not regressions)
- [ ] Tested locally with `--verbose` for failures
- [ ] Updated documentation if behavior changed
- [ ] All CI jobs pass

## Future Enhancements

Potential improvements to the framework:

1. **Protocol version testing**: Test different protocol versions (28-32)
2. **Performance benchmarks**: Compare transfer speeds with upstream
3. **Daemon mode testing**: Test `rsyncd` compatibility
4. **SSH transport testing**: Validate remote transfers
5. **Incremental transfer validation**: Test `--checksum`, `--update`, etc.

## See Also

- [tools/ci/run_interop.sh](../../tools/ci/run_interop.sh) - Upstream binary build script
- [.github/workflows/interop-validation.yml](../../.github/workflows/interop-validation.yml) - CI workflow
- [tests/interop/exit_codes/scenarios.toml](../../tests/interop/exit_codes/scenarios.toml) - Test scenarios
- [CLAUDE.md](../../CLAUDE.md) - Overall project architecture
