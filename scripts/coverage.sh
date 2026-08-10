#!/usr/bin/env bash
# Coverage Report Generator for rsync
#
# This script generates test coverage reports using cargo-llvm-cov.
#
# REQUIREMENTS:
#   - cargo-llvm-cov must be installed: cargo install cargo-llvm-cov
#
# USAGE:
#   ./scripts/coverage.sh                    # Generate HTML and LCOV reports
#   ./scripts/coverage.sh --open             # Generate and open HTML report in browser
#   ./scripts/coverage.sh --html-only        # Generate only HTML report
#   ./scripts/coverage.sh --lcov-only        # Generate only LCOV report
#   ./scripts/coverage.sh --json             # Also generate JSON report
#
# OUTPUT:
#   - HTML report: target/coverage/html/index.html
#   - LCOV report: target/coverage/lcov.info
#   - JSON report: target/coverage/coverage.json (if --json is passed)
#
# EXAMPLES:
#   # Generate all reports and view in browser
#   ./scripts/coverage.sh --open
#
#   # Generate only LCOV for CI integration
#   ./scripts/coverage.sh --lcov-only
#
# NOTES:
#   - Coverage data is cleaned before each run
#   - Tests run with --workspace to cover all crates
#   - Use CARGO_LLVM_COV_EXTRA_ARGS for additional flags

set -euo pipefail

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Script directory and project root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Output directories
COVERAGE_DIR="$PROJECT_ROOT/target/coverage"
HTML_DIR="$COVERAGE_DIR/html"
LCOV_FILE="$COVERAGE_DIR/lcov.info"
JSON_FILE="$COVERAGE_DIR/coverage.json"

# Parse command line arguments
OPEN_BROWSER=false
HTML_ONLY=false
LCOV_ONLY=false
GENERATE_JSON=false

for arg in "$@"; do
    case $arg in
        --open)
            OPEN_BROWSER=true
            shift
            ;;
        --html-only)
            HTML_ONLY=true
            shift
            ;;
        --lcov-only)
            LCOV_ONLY=true
            shift
            ;;
        --json)
            GENERATE_JSON=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --open         Generate reports and open HTML in browser"
            echo "  --html-only    Generate only HTML report"
            echo "  --lcov-only    Generate only LCOV report"
            echo "  --json         Also generate JSON report"
            echo "  --help, -h     Show this help message"
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $arg${NC}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

# Check if cargo-llvm-cov is installed
if ! command -v cargo-llvm-cov &> /dev/null; then
    echo -e "${RED}Error: cargo-llvm-cov is not installed${NC}"
    echo ""
    echo "Install it with:"
    echo "  cargo install cargo-llvm-cov"
    echo ""
    echo "Or using rustup component (if available):"
    echo "  rustup component add llvm-tools-preview"
    echo "  cargo install cargo-llvm-cov"
    exit 1
fi

echo -e "${BLUE}=== Rsync Test Coverage Report ===${NC}"
echo ""
echo "Project: $PROJECT_ROOT"
echo "Coverage output: $COVERAGE_DIR"
echo ""

# Create coverage directory
mkdir -p "$COVERAGE_DIR"

# Clean previous coverage data
echo -e "${YELLOW}Cleaning previous coverage data...${NC}"
cargo llvm-cov clean --workspace

# Common test arguments
TEST_ARGS=(
    --workspace
    --all-features
)

# Additional arguments from environment
if [ -n "${CARGO_LLVM_COV_EXTRA_ARGS:-}" ]; then
    IFS=' ' read -r -a EXTRA_ARGS <<< "$CARGO_LLVM_COV_EXTRA_ARGS"
    TEST_ARGS+=("${EXTRA_ARGS[@]}")
fi

# Generate reports based on flags
if [ "$LCOV_ONLY" = true ]; then
    echo -e "${GREEN}Generating LCOV report...${NC}"
    cargo llvm-cov "${TEST_ARGS[@]}" \
        --lcov \
        --output-path "$LCOV_FILE"

    echo ""
    echo -e "${GREEN}Coverage report generated successfully!${NC}"
    echo "LCOV: $LCOV_FILE"

elif [ "$HTML_ONLY" = true ]; then
    echo -e "${GREEN}Generating HTML report...${NC}"
    cargo llvm-cov "${TEST_ARGS[@]}" \
        --html \
        --output-dir "$HTML_DIR"

    echo ""
    echo -e "${GREEN}Coverage report generated successfully!${NC}"
    echo "HTML: $HTML_DIR/index.html"

    if [ "$OPEN_BROWSER" = true ]; then
        echo ""
        echo -e "${BLUE}Opening report in browser...${NC}"
        if command -v xdg-open &> /dev/null; then
            xdg-open "$HTML_DIR/index.html"
        elif command -v open &> /dev/null; then
            open "$HTML_DIR/index.html"
        else
            echo -e "${YELLOW}Cannot open browser automatically. Open manually:${NC}"
            echo "  file://$HTML_DIR/index.html"
        fi
    fi

else
    # Generate both HTML and LCOV reports
    echo -e "${GREEN}Generating HTML report...${NC}"
    cargo llvm-cov "${TEST_ARGS[@]}" \
        --html \
        --output-dir "$HTML_DIR"

    echo ""
    echo -e "${GREEN}Generating LCOV report...${NC}"
    cargo llvm-cov "${TEST_ARGS[@]}" \
        --lcov \
        --output-path "$LCOV_FILE" \
        --no-run  # Don't re-run tests, use existing coverage data

    # Generate JSON if requested
    if [ "$GENERATE_JSON" = true ]; then
        echo ""
        echo -e "${GREEN}Generating JSON report...${NC}"
        cargo llvm-cov "${TEST_ARGS[@]}" \
            --json \
            --output-path "$JSON_FILE" \
            --no-run
    fi

    echo ""
    echo -e "${GREEN}Coverage reports generated successfully!${NC}"
    echo "HTML: $HTML_DIR/index.html"
    echo "LCOV: $LCOV_FILE"
    if [ "$GENERATE_JSON" = true ]; then
        echo "JSON: $JSON_FILE"
    fi

    if [ "$OPEN_BROWSER" = true ]; then
        echo ""
        echo -e "${BLUE}Opening report in browser...${NC}"
        if command -v xdg-open &> /dev/null; then
            xdg-open "$HTML_DIR/index.html"
        elif command -v open &> /dev/null; then
            open "$HTML_DIR/index.html"
        else
            echo -e "${YELLOW}Cannot open browser automatically. Open manually:${NC}"
            echo "  file://$HTML_DIR/index.html"
        fi
    fi
fi

# Extract and display coverage summary
echo ""
echo -e "${BLUE}=== Coverage Summary ===${NC}"

# Try to extract summary from LCOV if it exists
if [ -f "$LCOV_FILE" ]; then
    # Parse LCOV file for summary statistics
    TOTAL_LINES=$(grep -c "^DA:" "$LCOV_FILE" || echo "0")
    COVERED_LINES=$(grep "^DA:" "$LCOV_FILE" | grep -v ",0$" | wc -l || echo "0")

    if [ "$TOTAL_LINES" -gt 0 ]; then
        COVERAGE_PERCENT=$(awk "BEGIN {printf \"%.2f\", ($COVERED_LINES / $TOTAL_LINES) * 100}")
        echo "Lines covered: $COVERED_LINES / $TOTAL_LINES ($COVERAGE_PERCENT%)"

        # Color code based on coverage percentage
        COVERAGE_INT=${COVERAGE_PERCENT%.*}
        if [ "$COVERAGE_INT" -ge 80 ]; then
            echo -e "${GREEN}Coverage level: GOOD${NC}"
        elif [ "$COVERAGE_INT" -ge 60 ]; then
            echo -e "${YELLOW}Coverage level: MODERATE${NC}"
        else
            echo -e "${RED}Coverage level: LOW${NC}"
        fi
    fi
fi

echo ""
echo -e "${BLUE}To view the HTML report, open:${NC}"
echo "  file://$HTML_DIR/index.html"
echo ""
echo -e "${BLUE}For CI integration, use the LCOV report:${NC}"
echo "  $LCOV_FILE"
