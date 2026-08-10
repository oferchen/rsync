# Coverage Quick Reference

## Installation (One-Time)

```bash
cargo install cargo-llvm-cov
```

## Generate Coverage

### Using the Script (Recommended)

```bash
# Generate all reports
./scripts/coverage.sh

# Generate and open in browser
./scripts/coverage.sh --open

# HTML only
./scripts/coverage.sh --html-only

# LCOV only (fast, for CI)
./scripts/coverage.sh --lcov-only

# Include JSON
./scripts/coverage.sh --json
```

### Using Cargo Aliases

```bash
# Default coverage run
cargo cov

# HTML report
cargo cov-html

# LCOV report
cargo cov-lcov

# Open HTML in browser
cargo cov-open
```

### Using Make

```bash
make coverage          # All reports
make coverage-open     # Generate and open
make coverage-html     # HTML only
make coverage-lcov     # LCOV only
```

## View Reports

```bash
# HTML (interactive, detailed)
open target/coverage/html/index.html

# LCOV (text format, for CI)
cat target/coverage/lcov.info
```

## Common Tasks

### Clean Coverage Data

```bash
cargo llvm-cov clean --workspace
```

### Coverage for Specific Crate

```bash
cargo llvm-cov --package engine --html
```

### Coverage with Extra Args

```bash
CARGO_LLVM_COV_EXTRA_ARGS="--ignore-filename-regex tests/" ./scripts/coverage.sh
```

## File Locations

| File | Location |
|------|----------|
| Main script | `scripts/coverage.sh` |
| HTML report | `target/coverage/html/index.html` |
| LCOV report | `target/coverage/lcov.info` |
| JSON report | `target/coverage/coverage.json` |
| Configuration | `.cargo/config.toml`, `.lcovrc` |
| Documentation | `scripts/coverage-README.md` |

## CI Integration

```yaml
# GitHub Actions
- run: ./scripts/coverage.sh --lcov-only
- uses: codecov/codecov-action@v3
  with:
    files: ./target/coverage/lcov.info
```

## Coverage Levels

- 🟢 **GOOD**: ≥80% (green)
- 🟡 **MODERATE**: 60-79% (yellow)
- 🔴 **LOW**: <60% (red)

## Troubleshooting

```bash
# Tool not found?
cargo install cargo-llvm-cov

# Stale data?
cargo llvm-cov clean

# Tests failing?
cargo test --workspace

# Need help?
./scripts/coverage.sh --help
```

## Resources

- Full docs: `scripts/coverage-README.md`
- Setup guide: `COVERAGE_SETUP.md`
- cargo-llvm-cov: https://github.com/taiki-e/cargo-llvm-cov
