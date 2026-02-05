# Code Coverage Setup for Rsync

This document summarizes the test coverage infrastructure set up for the rsync project.

## What Was Installed

### Tools
- **cargo-llvm-cov** v0.6.21 is already installed and verified

## Files Created/Modified

### New Files

1. **`scripts/coverage.sh`** - Main coverage script
   - Generates HTML and LCOV reports
   - Supports multiple output formats (HTML, LCOV, JSON)
   - Includes coverage summary with color-coded results
   - Auto-detects and opens browser for HTML reports
   - Configurable via command-line flags and environment variables

2. **`scripts/coverage-README.md`** - Comprehensive documentation
   - Installation instructions
   - Usage examples
   - CI/CD integration guides
   - Troubleshooting tips
   - Best practices

3. **`.lcovrc`** - LCOV configuration
   - Excludes external dependencies and build artifacts
   - Enables branch and function coverage
   - Configures coverage report generation

4. **`.gitattributes`** - Git line ending configuration
   - Ensures shell scripts use LF line endings on all platforms
   - Ensures Makefiles use LF line endings

### Modified Files

1. **`.cargo/config.toml`** - Added coverage configuration
   - Coverage-specific environment variables
   - Convenient cargo aliases: `cov`, `cov-html`, `cov-lcov`, `cov-open`

2. **`scripts/Makefile`** - Added coverage targets
   - `make coverage` - Generate all reports
   - `make coverage-open` - Generate and open HTML report
   - `make coverage-html` - Generate only HTML report
   - `make coverage-lcov` - Generate only LCOV report

## How to Use

### Quick Start

Generate coverage reports:
```bash
./scripts/coverage.sh
```

View reports:
```bash
# HTML report
open target/coverage/html/index.html

# Or generate and auto-open
./scripts/coverage.sh --open
```

### Using Cargo Aliases

```bash
# Generate HTML report
cargo cov-html

# Generate LCOV report
cargo cov-lcov

# Generate and open HTML
cargo cov-open
```

### Using Make

```bash
# Generate all reports
make coverage

# Generate and open
make coverage-open
```

### Command-Line Options

```bash
# Show help
./scripts/coverage.sh --help

# Generate only HTML report
./scripts/coverage.sh --html-only

# Generate only LCOV report (faster, good for CI)
./scripts/coverage.sh --lcov-only

# Generate JSON report as well
./scripts/coverage.sh --json

# Generate and open in browser
./scripts/coverage.sh --open
```

## Output Locations

All coverage data is written to `target/coverage/`:

- **HTML report**: `target/coverage/html/index.html`
- **LCOV report**: `target/coverage/lcov.info`
- **JSON report**: `target/coverage/coverage.json` (if `--json` flag used)

These are automatically ignored by git (in `target/` directory).

## Features

### Multi-Format Support
- **HTML**: Interactive, browsable coverage report with syntax highlighting
- **LCOV**: Standard format for CI/CD integration (Codecov, Coveralls)
- **JSON**: Structured data for custom processing

### Smart Reporting
- Parses LCOV to display coverage summary
- Color-coded results (GOOD/MODERATE/LOW)
- Shows lines covered and percentage

### Developer-Friendly
- Clean output with progress indicators
- Auto-detects and opens browser on Linux/macOS
- Helpful error messages with suggestions
- No-rerun flag for fast report generation

### CI/CD Ready
- Fast LCOV-only mode for CI pipelines
- Environment variable support for custom args
- Exit codes respect test failures
- Works with standard coverage services

## Configuration

### Environment Variables

```bash
# Pass additional arguments to cargo-llvm-cov
export CARGO_LLVM_COV_EXTRA_ARGS="--ignore-filename-regex tests/"
./scripts/coverage.sh
```

### Coverage Exclusions

Configure in `.lcovrc`:
- External dependencies excluded
- Build artifacts excluded
- Cargo registry excluded

## CI/CD Integration Examples

### GitHub Actions

```yaml
- name: Generate coverage
  run: ./scripts/coverage.sh --lcov-only

- name: Upload to Codecov
  uses: codecov/codecov-action@v3
  with:
    files: ./target/coverage/lcov.info
```

### GitLab CI

```yaml
coverage:
  script:
    - ./scripts/coverage.sh --lcov-only
  artifacts:
    reports:
      coverage_report:
        coverage_format: cobertura
        path: target/coverage/lcov.info
```

## Architecture

### Coverage Script Flow

1. **Validation**: Check cargo-llvm-cov is installed
2. **Cleanup**: Remove previous coverage data
3. **Test Execution**: Run tests with instrumentation
4. **Report Generation**: Generate requested formats
5. **Summary**: Parse and display coverage metrics

### Test Arguments

The script runs coverage with:
- `--workspace` - Cover all crates
- `--all-features` - Test all feature combinations
- Additional args from `CARGO_LLVM_COV_EXTRA_ARGS`

### Report Generation

- First run generates primary data
- Subsequent formats use `--no-run` to avoid re-running tests
- Faster multi-format generation

## Best Practices

1. **Run Locally**: Generate coverage before pushing
2. **Review HTML**: Identify untested code paths
3. **Track Trends**: Monitor coverage over time
4. **Focus on Core**: Prioritize critical path coverage
5. **Automate**: Run in CI for every PR

## Coverage Goals

Recommended targets:
- **Core libraries** (engine, protocol): 80%+
- **Utilities** (checksums, filters): 70%+
- **CLI/Frontend**: 60%+
- **Overall project**: 75%+

## Maintenance

### Updating cargo-llvm-cov

```bash
cargo install cargo-llvm-cov --force
```

### Cleaning Coverage Data

```bash
cargo llvm-cov clean --workspace
```

### Regenerating Reports

Just re-run the script:
```bash
./scripts/coverage.sh
```

## Troubleshooting

See `scripts/coverage-README.md` for detailed troubleshooting guide.

Common issues:
- **Tool not found**: Install `cargo install cargo-llvm-cov`
- **Stale data**: Run `cargo llvm-cov clean`
- **Tests fail**: Run `cargo test` first to debug

## Resources

- Script: `scripts/coverage.sh`
- Documentation: `scripts/coverage-README.md`
- Configuration: `.cargo/config.toml`, `.lcovrc`
- Makefile targets: `scripts/Makefile`

## Next Steps

1. Run initial coverage: `./scripts/coverage.sh --open`
2. Review the HTML report
3. Identify low-coverage areas
4. Write tests to improve coverage
5. Add coverage reporting to CI/CD
6. Set coverage thresholds for PRs

## Notes

- Coverage adds ~2-3x overhead to test execution time
- Use `--lcov-only` for faster CI runs
- HTML reports are best for local development
- LCOV reports are standard for CI integration
- Don't chase 100% coverage - focus on critical paths
