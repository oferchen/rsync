# shellcheck shell=sh

resolve_zig_binary() {
    cross_dir=$1

    if [ -n "${ZIG:-}" ]; then
        if command -v "$ZIG" >/dev/null 2>&1; then
            command -v "$ZIG"
            return
        elif [ -x "$ZIG" ]; then
            printf '%s\n' "$ZIG"
            return
        fi
        echo "error: ZIG environment variable points to an invalid binary: $ZIG" >&2
        exit 127
    fi

    if command -v zig >/dev/null 2>&1; then
        command -v zig
        return
    fi

    "${cross_dir}/resolve-zig.sh"
}
