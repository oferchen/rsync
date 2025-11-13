#!/usr/bin/env python3
from __future__ import annotations

import argparse
import collections
import json
import re
from pathlib import Path

Entry = collections.namedtuple(
    "Entry", ["long", "short", "arg_info", "arg_ptr", "value"]
)

def strip_comments(text: str) -> str:
    return re.sub(r"/\*.*?\*/", "", text, flags=re.S)

def parse_variable_defaults(source: str) -> dict[str, str]:
    defaults: dict[str, str] = {}
    for match in re.finditer(r"^int\s+([A-Za-z0-9_]+)\s*=\s*([^;]+);", source, flags=re.M):
        name = match.group(1)
        value = match.group(2).strip()
        defaults[name] = value
    return defaults


def parse_entries(source: str) -> list[Entry]:
    text = strip_comments(source)
    prefix = "static struct poptOption long_options[] = {"
    start = text.index(prefix) + len(prefix)
    end = text.index("\n};", start)
    body = text[start:end]
    entries: list[Entry] = []
    for match in re.finditer(r"\{[^{}]*\}", body):
        entry_str = match.group(0)
        entry = parse_entry(entry_str)
        if entry:
            entries.append(entry)
    return entries


def parse_entry(entry: str) -> Entry | None:
    entry = entry.strip()
    if entry in ("{0,0,0,0,0,0,0}", "{0, 0, 0, 0, 0, 0, 0}"):
        return None
    pattern = re.compile(
        r'^\{\s*(?:"([^"]*)"|0)\s*,\s*(?:\'([^\']*)\'|0)\s*,\s*([A-Z0-9_]+)\s*,\s*([^,]+)\s*,\s*([^,]+)\s*,'
    )
    match = pattern.match(entry)
    if not match:
        raise ValueError(f"Unable to parse entry: {entry}")
    long_raw, short_raw, arg_info, arg_ptr, value = match.groups()
    long_opt = None
    if long_raw and long_raw != '""':
        long_opt = long_raw.strip('"')
    short_opt = short_raw if short_raw else None
    if long_opt is None and short_opt is None:
        return None
    return Entry(long_opt, short_opt, arg_info.strip(), arg_ptr.strip(), value.strip())

def parse_help_options(help_text: str) -> tuple[set[str], set[str]]:
    long_opts: set[str] = set()
    short_opts: set[str] = set()
    for match in re.finditer(r"--([A-Za-z0-9][A-Za-z0-9-]*)", help_text):
        long_opts.add(match.group(1))
    for match in re.finditer(r"(?<!-)\B-([A-Za-z0-9])", help_text):
        short_opts.add(match.group(1))
    return long_opts, short_opts

def categorize(option: str | None) -> str:
    if not option:
        return "general"
    name = option
    if name.startswith('no-'):
        name = name[3:]
    if any(key in name for key in (
        'delete', 'remove', 'prune', 'max-delete', 'ignore-missing-args'
    )):
        return 'deletion'
    if any(key in name for key in (
        'archive', 'recursive', 'dirs', 'mkpath', 'implied', 'relative', 'one-file-system'
    )):
        return 'traversal'
    if any(key in name for key in (
        'perms', 'owner', 'group', 'acls', 'xattr', 'chmod', 'times', 'numeric', 'chown', 'usermap', 'groupmap', 'omit'
    )):
        return 'metadata'
    if any(key in name for key in (
        'compress', 'checksum', 'bwlimit', 'block-size', 'whole-file', 'append', 'sparse', 'preallocate', 'inplace', 'partial',
        'progress', 'stats', 'log', 'fsync'
    )):
        return 'transfer'
    if any(key in name for key in (
        'filter', 'include', 'exclude', 'files-from', 'from0', 'cvs'
    )):
        return 'filters'
    if any(key in name for key in (
        'daemon', 'config', 'password', 'motd', 'module'
    )):
        return 'daemon'
    if any(key in name for key in (
        'ipv', 'rsh', 'rsync-path', 'connect', 'port', 'address', 'sock', 'timeout', 'contimeout', 'protocol', 'remote',
        'blocking'
    )):
        return 'connection'
    if any(key in name for key in (
        'debug', 'verbose', 'info', 'msgs2stderr', 'outbuf', 'out-format', 'itemize'
    )):
        return 'logging'
    return 'general'

def describe_default(arg_ptr: str, value: str, defaults: dict[str, str]) -> str:
    if not arg_ptr.startswith('&'):
        return 'n/a'
    var = arg_ptr[1:]
    default_value = defaults.get(var)
    if default_value is None:
        return 'n/a'
    normalized_default = default_value.strip()
    if value == '0' and normalized_default != '0':
        return f"enabled by default ({normalized_default})"
    if normalized_default == '0':
        return 'disabled by default'
    return f"default {normalized_default}"

def status_for(entry: Entry, implemented_longs: set[str], implemented_shorts: set[str]) -> tuple[str, str]:
    notes: list[str] = []
    status = 'missing'
    if entry.long and entry.long in implemented_longs:
        status = 'implemented'
    elif entry.short and entry.short in implemented_shorts:
        status = 'implemented'
    if entry.long and entry.long.startswith('no-'):
        notes.append('Negates the corresponding positive option.')
    if entry.long in {'del', 'no-W', 'no-c', 'no-i', 'no-d', 'no-r', 'no-p', 'no-o', 'no-g'}:
        notes.append('Alias maintained for compatibility.')
    return status, '; '.join(notes)

def make_option_identifier(entry: Entry) -> str:
    if entry.long:
        return f"--{entry.long}"
    if entry.short:
        return f"-{entry.short} (short-only)"
    raise ValueError('Entry without long or short option.')

def main() -> None:
    parser = argparse.ArgumentParser(description="Generate rsync option parity matrix.")
    parser.add_argument("options", type=Path, help="Path to rsync 3.4.1 options.c")
    parser.add_argument("oc_help", type=Path, help="Path to oc-rsync --help output")
    parser.add_argument("--format", choices=["json", "yaml"], default="yaml")
    args = parser.parse_args()

    options_text = args.options.read_text()
    help_text = args.oc_help.read_text()

    defaults = parse_variable_defaults(options_text)
    entries = parse_entries(options_text)
    implemented_longs, implemented_shorts = parse_help_options(help_text)

    matrix = []
    for entry in entries:
        identifier = make_option_identifier(entry)
        category = categorize(entry.long)
        upstream_default = describe_default(entry.arg_ptr, entry.value, defaults)
        status, extra_notes = status_for(entry, implemented_longs, implemented_shorts)
        matrix.append({
            "option": identifier,
            "short": entry.short or "",
            "category": category,
            "upstream_default": upstream_default,
            "status": status,
            "notes": extra_notes,
        })

    if args.format == "json":
        print(json.dumps(matrix, indent=2))
    else:
        print("options:")
        for item in matrix:
            print(f"  - option: {item['option']}")
            print(f"    short: {item['short']}")
            print(f"    category: {item['category']}")
            print(f"    upstream_default: {item['upstream_default']}")
            print(f"    status: {item['status']}")
            notes = item['notes']
            if notes:
                print(f"    notes: {notes}")
            else:
                print("    notes: \"\"")

if __name__ == "__main__":
    main()
