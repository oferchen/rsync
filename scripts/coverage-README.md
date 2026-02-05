# Test Coverage Guide

This document describes how to generate and analyze test coverage for the rsync project.

## Prerequisites

Install `cargo-llvm-cov`:

```bash
cargo install cargo-llvm-cov
```

Verify installation:

```bash
cargo llvm-cov --version
```

## Quick Start

### Generate All Reports

```bash
./scripts/coverage.sh
```

This generates:
- HTML report: `target/coverage/html/index.html`
- LCOV report: `target/coverage/lcov.info`

### Generate and Open HTML Report

```bash
./scripts/coverage.sh --open
```

### Generate Only HTML

```bash
./scripts/coverage.sh --html-only
```

### Generate Only LCOV (for CI)

```bash
./scripts/coverage.sh --lcov-only
```

### Include JSON Report

```bash
./scripts/coverage.sh --json
```

## Using Cargo Aliases

Convenient aliases are configured in `.cargo/config.toml`:

```bash
# Run coverage with default settings
cargo cov

# Generate HTML report
cargo cov-html

# Generate LCOV report
cargo cov-lcov

# Generate and open HTML report
cargo cov-open
```

## Advanced Usage

### Environment Variables

Control coverage behavior with environment variables:

```bash
# Pass additional arguments to cargo-llvm-cov
export CARGO_LLVM_COV_EXTRA_ARGS="--ignore-filename-regex tests/"
./scripts/coverage.sh
```

### Exclude Specific Crates

```bash
cargo llvm-cov --workspace --exclude xtask --html
```

### Coverage for Specific Crate

```bash
cargo llvm-cov --package engine --html
```

### Integration Tests Only

```bash
cargo llvm-cov --workspace --tests --html
```

### Unit Tests Only

```bash
cargo llvm-cov --workspace --lib --html
```

## Understanding Coverage Reports

### HTML Report

The HTML report (`target/coverage/html/index.html`) provides:

- **Overall coverage percentage** for the entire workspace
- **Per-file coverage** with color-coded lines:
  - Green: Executed lines
  - Red: Not executed lines
  - Gray: Non-executable lines (comments, declarations)
- **Function-level coverage** showing which functions are tested
- **Branch coverage** showing which conditional branches are tested

### LCOV Report

The LCOV report (`target/coverage/lcov.info`) is a standard format that:

- Works with CI/CD tools (GitHub Actions, GitLab CI)
- Integrates with coverage services (Codecov, Coveralls)
- Can be processed by various coverage analyzers

### Coverage Metrics

The script displays a summary including:

- **Lines covered**: Number and percentage of executed lines
- **Coverage level**:
  - GOOD: ≥80% coverage (green)
  - MODERATE: 60-79% coverage (yellow)
  - LOW: <60% coverage (red)

## CI/CD Integration

### GitHub Actions

```yaml
- name: Install cargo-llvm-cov
  run: cargo install cargo-llvm-cov

- name: Generate coverage
  run: ./scripts/coverage.sh --lcov-only

- name: Upload to Codecov
  uses: codecov/codecov-action@v3
  with:
    files: ./target/coverage/lcov.info
    fail_ci_if_error: true
```

### GitLab CI

```yaml
coverage:
  script:
    - cargo install cargo-llvm-cov
    - ./scripts/coverage.sh --lcov-only
  artifacts:
    reports:
      coverage_report:
        coverage_format: cobertura
        path: target/coverage/lcov.info
```

## Coverage Configuration

### LCOV Configuration

The `.lcovrc` file configures LCOV report generation:

- Excludes external dependencies and build artifacts
- Enables branch and function coverage
- Handles unexecuted blocks gracefully

### Cargo Configuration

The `.cargo/config.toml` includes:

- Coverage-specific environment variables
- Convenient cargo aliases for common operations

## Improving Coverage

### Identify Gaps

1. Generate HTML report: `./scripts/coverage.sh --open`
2. Browse to low-coverage files (red in the summary)
3. Look for red lines in the file view
4. Write tests to exercise those code paths

### Coverage Goals

Recommended coverage targets:

- **Core libraries**: 80%+ (engine, protocol, flist)
- **Utilities**: 70%+ (checksums, filters, match)
- **CLI/Frontend**: 60%+ (harder to test, more integration-focused)
- **Overall project**: 75%+

### Excluding Code from Coverage

Use `#[cfg(not(coverage))]` to exclude code:

```rust
#[cfg(not(coverage))]
fn debug_only_function() {
    // This won't be counted in coverage
}
```

Or use coverage-specific ignore attributes:

```rust
#[cfg_attr(coverage_nightly, coverage(off))]
fn unreachable_panic() {
    panic!("This should never be called");
}
```

## Troubleshooting

### "cargo-llvm-cov: command not found"

Install the tool:

```bash
cargo install cargo-llvm-cov
```

### Coverage data is stale

Clean and regenerate:

```bash
cargo llvm-cov clean
./scripts/coverage.sh
```

### Tests fail during coverage

Run tests normally first to identify issues:

```bash
cargo test --workspace
```

Then generate coverage:

```bash
./scripts/coverage.sh
```

### Low coverage for a well-tested module

Some reasons:
- Generated code (build.rs outputs) counted in coverage
- Integration tests in separate crates don't count
- Code only executed in specific feature configurations

### Report won't open in browser

Open manually:

```bash
file:///path/to/rsync/target/coverage/html/index.html
```

Or use a specific browser:

```bash
firefox target/coverage/html/index.html
google-chrome target/coverage/html/index.html
```

## Performance Considerations

Coverage collection adds overhead:

- Tests run slower (typically 2-3x)
- More disk I/O for profiling data
- Increased memory usage

For large test suites:

1. Use `--lcov-only` for CI (faster than HTML generation)
2. Run coverage on specific crates during development
3. Use full workspace coverage for releases/PRs only

## Best Practices

1. **Run coverage regularly**: Catch coverage regressions early
2. **Review new code**: Ensure new features have tests
3. **Focus on critical paths**: Prioritize coverage for core logic
4. **Don't chase 100%**: Some code is hard to test (error paths, panics)
5. **Use in CI**: Automate coverage reporting for all PRs
6. **Track trends**: Monitor coverage over time, not just absolute numbers

## Resources

- [cargo-llvm-cov documentation](https://github.com/taiki-e/cargo-llvm-cov)
- [LLVM Coverage Mapping Format](https://llvm.org/docs/CoverageMappingFormat.html)
- [Rust testing guide](https://doc.rust-lang.org/book/ch11-00-testing.html)
