#!/usr/bin/env bash
# Run upstream rsync client to oc-rsync daemon interoperability tests.
#
# This script runs the comprehensive test suite that verifies upstream rsync
# clients (3.0.9, 3.1.3, 3.4.1) can successfully connect to and transfer files
# with the oc-rsync daemon implementation.
#
# Prerequisites:
# 1. Upstream rsync binaries must be built and installed at:
#    - target/interop/upstream-install/3.0.9/bin/rsync
#    - target/interop/upstream-install/3.1.3/bin/rsync
#    - target/interop/upstream-install/3.4.1/bin/rsync
#
# 2. oc-rsync binary must be built at:
#    - target/release/oc-rsync (preferred)
#    - OR target/debug/oc-rsync (fallback)
#
# Usage:
#   bash tools/run_upstream_client_interop_tests.sh [OPTIONS]
#
# Options:
#   --test <name>    Run specific test by name
#   --list           List all available tests
#   --verbose        Show verbose output
#   --help           Show this help message
#
# Examples:
#   # Run all interop tests
#   bash tools/run_upstream_client_interop_tests.sh
#
#   # Run specific test
#   bash tools/run_upstream_client_interop_tests.sh --test test_upstream_3_4_1_client_handshake
#
#   # List all tests
#   bash tools/run_upstream_client_interop_tests.sh --list

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Test configuration
TEST_PACKAGE="core"
TEST_NAME="upstream_client_to_oc_daemon_interop"
SPECIFIC_TEST=""
VERBOSE=""
LIST_TESTS=false

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --test)
            SPECIFIC_TEST="$2"
            shift 2
            ;;
        --list)
            LIST_TESTS=true
            shift
            ;;
        --verbose|-v)
            VERBOSE="--nocapture"
            shift
            ;;
        --help|-h)
            head -n 40 "$0" | tail -n +2 | sed 's/^# //'
            exit 0
            ;;
        *)
            echo -e "${RED}Error: Unknown option: $1${NC}" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
    esac
done

cd "$REPO_ROOT"

# Function to check if binary exists
check_binary() {
    local binary_path="$1"
    local description="$2"

    if [[ -f "$binary_path" && -x "$binary_path" ]]; then
        echo -e "${GREEN}✓${NC} Found: $description at $binary_path"
        return 0
    else
        echo -e "${YELLOW}⚠${NC} Missing: $description at $binary_path"
        return 1
    fi
}

# Function to check prerequisites
check_prerequisites() {
    echo -e "${BLUE}Checking prerequisites...${NC}"

    local all_found=true

    # Check for oc-rsync binary
    if check_binary "target/release/oc-rsync" "oc-rsync (release)"; then
        :
    elif check_binary "target/debug/oc-rsync" "oc-rsync (debug)"; then
        :
    else
        echo -e "${RED}Error: oc-rsync binary not found${NC}"
        echo "Build it with: cargo build --release"
        all_found=false
    fi

    # Check for upstream binaries
    check_binary "target/interop/upstream-install/3.0.9/bin/rsync" "upstream rsync 3.0.9" || all_found=false
    check_binary "target/interop/upstream-install/3.1.3/bin/rsync" "upstream rsync 3.1.3" || all_found=false
    check_binary "target/interop/upstream-install/3.4.1/bin/rsync" "upstream rsync 3.4.1" || all_found=false

    echo ""

    if [[ "$all_found" == "false" ]]; then
        echo -e "${YELLOW}Note: Some tests will be skipped due to missing binaries${NC}"
        echo "To build upstream rsync binaries, see: docs/interop.md"
        echo ""
    fi
}

# Function to list all tests
list_all_tests() {
    echo -e "${BLUE}Available interop tests:${NC}"
    echo ""

    # Extract test names from the test file
    grep -E '^\s*#\[test\]' "$REPO_ROOT/crates/core/tests/$TEST_NAME.rs" -A 2 | \
        grep -E '^\s*fn\s+test_' | \
        sed -E 's/^\s*fn\s+(test_[a-z0-9_]+).*/  \1/' | \
        sort

    echo ""
    echo "Run a specific test with:"
    echo "  bash $0 --test <test_name>"
}

# Main execution
main() {
    if [[ "$LIST_TESTS" == "true" ]]; then
        list_all_tests
        exit 0
    fi

    check_prerequisites

    # Build test command
    local cargo_cmd="cargo test --package $TEST_PACKAGE --test $TEST_NAME"

    if [[ -n "$SPECIFIC_TEST" ]]; then
        cargo_cmd="$cargo_cmd $SPECIFIC_TEST"
        echo -e "${BLUE}Running specific test: $SPECIFIC_TEST${NC}"
    else
        echo -e "${BLUE}Running all interop tests...${NC}"
    fi

    cargo_cmd="$cargo_cmd -- --ignored --show-output $VERBOSE"

    echo ""
    echo -e "${BLUE}Command: $cargo_cmd${NC}"
    echo ""

    # Run the tests
    if eval "$cargo_cmd"; then
        echo ""
        echo -e "${GREEN}✓ All tests passed${NC}"
        exit 0
    else
        echo ""
        echo -e "${RED}✗ Some tests failed${NC}"
        echo ""
        echo "Tips for debugging:"
        echo "  - Check daemon logs in test output"
        echo "  - Run with --verbose for more details"
        echo "  - Run specific failing test with --test <name>"
        exit 1
    fi
}

main
