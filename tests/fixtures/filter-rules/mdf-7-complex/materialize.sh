#!/bin/sh
# MDF-7 fixture materialization step.
#
# The fixture's CVS-bundle subtree needs a literal `.git/` directory to
# exercise upstream's `default_cvsignore` set. Git cannot track a
# nested `.git/` so the fixture ships `dot-git/` on disk; this script
# copies the fixture into a destination directory with the rename
# applied.
#
# Usage:
#     materialize.sh <source-fixture-dir> <materialized-dest-dir>
#
# After running, <materialized-dest-dir>/source/ is the tree the
# transfer consumer should rsync from. The expected lists in
# <source-fixture-dir>/expected/ reference paths AFTER the rename.

set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <source-fixture-dir> <materialized-dest-dir>" >&2
    exit 2
fi

src=$1
dest=$2

if [ ! -d "$src/source" ]; then
    echo "$0: missing $src/source" >&2
    exit 1
fi

mkdir -p "$dest"
# Copy preserving symlinks (-a). cp -R preserves the symlinked
# .rsync-filter in symlinked-rsyncfilter/ as a real symlink on Linux
# and macOS.
cp -R "$src/source" "$dest/source"

if [ -d "$dest/source/cvs/dot-git" ]; then
    mv "$dest/source/cvs/dot-git" "$dest/source/cvs/.git"
fi
