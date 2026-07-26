#!/usr/bin/env python3
"""Guard: every CI test cell that can reach the oc-rsync binary must build it.

Integration tests spawn the binary through one resolver in `test-support`
(`oc_rsync_bin`, `workspace_bin`, `require_binary`, `OcRsyncCliRunner`, ...).
That resolver considers exactly one path - the profile directory Cargo baked in
via `OUT_DIR` - and aborts when nothing is there.  Cargo only builds the root
`bin` package's `[[bin]]` when the selection reaches that package, so a
package-scoped `cargo nextest run -p engine` never produces it.

Before the resolver was unified, the helpers returned `Option` and callers
printed `skip:` and returned, so a cell that never built the binary reported
`2 passed; 0 failed; finished in 0.00s` - green, and having spawned nothing.
The resolver now panics instead, which converts that class of silent pass into
a hard failure; this guard keeps either outcome from being reintroduced by a
new matrix row, an un-ignored test or a widened `-E` filter.

The check walks every `.github/workflows/*.yml`, expands each job's matrix
(including `include`/`exclude` and `needs_bin`-style conditional steps), and
replays the steps in order.  A `cargo nextest run` / `cargo test` whose package
selection contains a crate that references the resolver, with no preceding step
that builds `oc-rsync` for the same cargo profile and target, is reported.

The crate list is derived, never pinned: it is whatever set of workspace
packages owns a source file referencing a resolver symbol.  A sixth crate that
starts spawning the binary is picked up on the next run.

Usage:
    python3 tools/ci/check_bin_build_coverage.py [--verbose] [--list-crates]

Exit status: 0 when every cell is covered, 1 when any cell is not, 2 when the
inputs could not be analysed (unparseable YAML, no cells found, ...).
"""

from __future__ import annotations

import argparse
import re
import shlex
import sys
from dataclasses import dataclass, field
from itertools import product
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_DIR = REPO_ROOT / ".github" / "workflows"

# Symbols that resolve or spawn the workspace `oc-rsync` binary. A file naming
# any of them is treated as a call site; see `derive_resolver_packages`.
RESOLVER_SYMBOLS = (
    "oc_rsync_bin",
    "workspace_bin",
    "workspace_bin_path",
    "require_binary",
    "locate_workspace_binary",
    "OcRsyncCliRunner",
)
RESOLVER_USE_RE = re.compile(r"\b(?:{})\b".format("|".join(RESOLVER_SYMBOLS)))
# The package that *defines* the resolver is the provider, not a consumer: its
# own unit tests deliberately exercise the missing-binary path.
_SYMBOL_ALTERNATION = "|".join(RESOLVER_SYMBOLS)
RESOLVER_DEF_RE = re.compile(
    rf"\bfn\s+(?:{_SYMBOL_ALTERNATION})\b"
    rf"|\bstruct\s+(?:{_SYMBOL_ALTERNATION})\b"
)

SUGGESTED_FIX = (
    "      - name: Build oc-rsync (for subprocess integration tests)\n"
    "        run: cargo build --locked -p bin --bin oc-rsync"
)


class AnalysisError(Exception):
    """The workflows could not be analysed; never a silent pass."""


# ---------------------------------------------------------------------------
# Minimal YAML reader
#
# Deliberately not PyYAML: this guard must run in a plain lint job with no pip
# install and no network. It covers the subset GitHub workflows use (block
# maps, block sequences, flow sequences/maps, block scalars, quoted scalars)
# and raises on anything it does not understand, so an unsupported construct
# fails the job instead of silently reading as an empty document.
# ---------------------------------------------------------------------------


def _strip_comment(line: str) -> str:
    """Drop a trailing `#` comment, respecting quotes."""
    out: list[str] = []
    quote: str | None = None
    i = 0
    while i < len(line):
        ch = line[i]
        if quote:
            if ch == "\\" and quote == '"':
                out.append(ch)
                i += 1
                if i < len(line):
                    out.append(line[i])
                    i += 1
                continue
            if ch == quote:
                quote = None
            out.append(ch)
        elif ch in "\"'":
            quote = ch
            out.append(ch)
        elif ch == "#" and (not out or out[-1] in " \t"):
            break
        else:
            out.append(ch)
        i += 1
    return "".join(out).rstrip()


def _split_key(text: str) -> tuple[str, str] | None:
    """Split `key: rest` at the first unquoted `:` followed by space or EOL."""
    quote: str | None = None
    i = 0
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == ":" and (i + 1 == len(text) or text[i + 1] in " \t"):
            key = text[:i].strip()
            if not key:
                return None
            if key[0] in "\"'" and key[-1] == key[0] and len(key) > 1:
                key = key[1:-1]
            return key, text[i + 1 :].strip()
        i += 1
    return None


def _split_flow(body: str) -> list[str]:
    """Split a flow collection body on top-level commas."""
    parts: list[str] = []
    depth = 0
    quote: str | None = None
    current: list[str] = []
    for ch in body:
        if quote:
            current.append(ch)
            if ch == quote:
                quote = None
            continue
        if ch in "\"'":
            quote = ch
            current.append(ch)
        elif ch in "[{":
            depth += 1
            current.append(ch)
        elif ch in "]}":
            depth -= 1
            current.append(ch)
        elif ch == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(ch)
    tail = "".join(current).strip()
    if tail or parts:
        parts.append(tail)
    return [p.strip() for p in parts if p.strip() != "" or len(parts) > 1]


def _scalar(token: str):
    """Convert a scalar token to a Python value."""
    text = token.strip()
    if text.startswith("[") and text.endswith("]"):
        return [_scalar(part) for part in _split_flow(text[1:-1])]
    if text.startswith("{") and text.endswith("}"):
        out = {}
        for part in _split_flow(text[1:-1]):
            split = _split_key(part)
            if split is None:
                raise AnalysisError(f"unsupported flow mapping entry: {part!r}")
            out[split[0]] = _scalar(split[1])
        return out
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        return text[1:-1].replace('\\"', '"').replace("\\\\", "\\")
    if len(text) >= 2 and text[0] == "'" and text[-1] == "'":
        return text[1:-1].replace("''", "'")
    if text in ("true", "True", "TRUE"):
        return True
    if text in ("false", "False", "FALSE"):
        return False
    if text in ("null", "Null", "NULL", "~", ""):
        return None
    if re.fullmatch(r"-?\d+", text):
        return int(text)
    return text


class _Reader:
    """Indentation-driven reader over the significant lines of a document."""

    def __init__(self, text: str, origin: str) -> None:
        self.lines = text.split("\n")
        self.origin = origin
        self.pos = 0

    def _fail(self, message: str) -> AnalysisError:
        return AnalysisError(f"{self.origin}:{self.pos + 1}: {message}")

    def _significant(self) -> tuple[int, str, str] | None:
        """Next (indent, comment-stripped content, raw line), or None at EOF."""
        while self.pos < len(self.lines):
            raw = self.lines[self.pos]
            if "\t" in raw[: len(raw) - len(raw.lstrip())]:
                raise self._fail("tab in indentation")
            stripped = raw.strip()
            if stripped == "" or stripped.startswith("#"):
                self.pos += 1
                continue
            if stripped in ("---", "..."):
                self.pos += 1
                continue
            content = _strip_comment(raw).strip()
            if content == "":
                self.pos += 1
                continue
            return len(raw) - len(raw.lstrip(" ")), content, raw
        return None

    def parse_document(self):
        node = self.parse_node(0)
        if self._significant() is not None:
            raise self._fail("trailing content after document")
        return node

    def parse_node(self, min_indent: int):
        item = self._significant()
        if item is None or item[0] < min_indent:
            return None
        indent, content, _raw = item
        if content == "-" or content.startswith("- "):
            return self.parse_sequence(indent)
        return self.parse_mapping(indent)

    def parse_mapping(self, indent: int) -> dict:
        out: dict = {}
        while True:
            item = self._significant()
            if item is None or item[0] < indent:
                break
            cur, content, _raw = item
            if cur > indent:
                raise self._fail(f"unexpected indent {cur}, expected {indent}")
            if content == "-" or content.startswith("- "):
                break
            split = _split_key(content)
            if split is None:
                raise self._fail(f"expected `key:`, found {content!r}")
            key, rest = split
            self.pos += 1
            if rest == "":
                out[key] = self.parse_child(cur)
            elif rest[0] in "|>":
                out[key] = self.read_block_scalar(cur, rest)
            else:
                out[key] = _scalar(rest)
        return out

    def parse_child(self, key_indent: int):
        """Value block belonging to a `key:` line with an empty value."""
        item = self._significant()
        if item is None:
            return None
        indent, content, _raw = item
        if indent > key_indent:
            return self.parse_node(indent)
        # A sequence may sit at the same column as its key.
        if indent == key_indent and (content == "-" or content.startswith("- ")):
            return self.parse_sequence(indent)
        return None

    def parse_sequence(self, indent: int) -> list:
        out: list = []
        while True:
            item = self._significant()
            if item is None or item[0] < indent:
                break
            cur, content, raw = item
            if cur > indent:
                raise self._fail(f"unexpected indent {cur}, expected {indent}")
            if not (content == "-" or content.startswith("- ")):
                break
            after = raw[cur + 1 :]
            lead = len(after) - len(after.lstrip(" "))
            item_col = cur + 1 + lead
            body = content[1:].strip()
            if body == "":
                self.pos += 1
                out.append(self.parse_node(cur + 1))
            elif _split_key(body) is not None:
                # Blank the dash so the entry reads as a mapping at item_col.
                self.lines[self.pos] = " " * item_col + raw[item_col:]
                out.append(self.parse_mapping(item_col))
            else:
                self.pos += 1
                out.append(_scalar(body))
        return out

    def read_block_scalar(self, key_indent: int, header: str) -> str:
        style = header[0]
        if not re.fullmatch(r"[|>][-+]?\d*", header):
            raise self._fail(f"unsupported block scalar header {header!r}")
        body: list[str] = []
        content_indent: int | None = None
        while self.pos < len(self.lines):
            raw = self.lines[self.pos]
            if raw.strip() == "":
                body.append("")
                self.pos += 1
                continue
            indent = len(raw) - len(raw.lstrip(" "))
            if indent <= key_indent:
                break
            if content_indent is None:
                content_indent = indent
            body.append(raw[content_indent:] if len(raw) > content_indent else "")
            self.pos += 1
        while body and body[-1] == "":
            body.pop()
        if style == "|":
            return "\n".join(body)
        folded: list[str] = []
        for line in body:
            if line == "":
                folded.append("\n")
            elif folded and folded[-1] not in ("", "\n"):
                folded[-1] = folded[-1] + " " + line
            else:
                folded.append(line)
        return " ".join(part for part in folded if part != "\n")


def load_yaml(path: Path):
    return _Reader(path.read_text(encoding="utf-8"), str(path)).parse_document()


# ---------------------------------------------------------------------------
# Workspace facts, derived from the repository rather than pinned
# ---------------------------------------------------------------------------


@dataclass
class Workspace:
    """Package layout plus the derived resolver-consumer set."""

    package_dirs: dict[str, Path]
    excluded_dirs: list[Path]
    resolver_packages: set[str]
    bin_owner: str

    def package_at(self, path: Path) -> str | None:
        """The package owning `path`, by nearest ancestor manifest."""
        best: tuple[int, str] | None = None
        for name, directory in self.package_dirs.items():
            try:
                path.relative_to(directory)
            except ValueError:
                continue
            depth = len(directory.parts)
            if best is None or depth > best[0]:
                best = (depth, name)
        return None if best is None else best[1]


def _manifest_package_name(text: str) -> str | None:
    match = re.search(r"^\s*\[package\]\s*$(.*?)(?=^\s*\[|\Z)", text, re.MULTILINE | re.DOTALL)
    if match is None:
        return None
    name = re.search(r'^\s*name\s*=\s*"([^"]+)"', match.group(1), re.MULTILINE)
    return None if name is None else name.group(1)


def _declares_oc_rsync_bin(text: str) -> bool:
    for block in re.findall(r"^\s*\[\[bin\]\]\s*$(.*?)(?=^\s*\[|\Z)", text, re.MULTILINE | re.DOTALL):
        if re.search(r'^\s*name\s*=\s*"oc-rsync"', block, re.MULTILINE):
            return True
    return False


def _workspace_members(root_manifest: str) -> tuple[list[str], list[str]]:
    def entries(field_name: str) -> list[str]:
        match = re.search(rf"^\s*{field_name}\s*=\s*\[(.*?)\]", root_manifest, re.MULTILINE | re.DOTALL)
        return [] if match is None else re.findall(r'"([^"]+)"', match.group(1))

    return entries("members"), entries("exclude")


def discover_workspace() -> Workspace:
    root_manifest = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
    members, excluded = _workspace_members(root_manifest)
    if not members:
        raise AnalysisError("root Cargo.toml declares no [workspace] members")

    package_dirs: dict[str, Path] = {}
    bin_owner: str | None = None
    manifests = [REPO_ROOT / "Cargo.toml"]
    manifests.extend(REPO_ROOT / member / "Cargo.toml" for member in members)
    for manifest in manifests:
        if not manifest.is_file():
            raise AnalysisError(f"workspace member manifest missing: {manifest}")
        text = manifest.read_text(encoding="utf-8")
        name = _manifest_package_name(text)
        if name is None:
            continue
        package_dirs[name] = manifest.parent
        if _declares_oc_rsync_bin(text):
            bin_owner = name
    if bin_owner is None:
        raise AnalysisError('no manifest declares a [[bin]] named "oc-rsync"')

    workspace = Workspace(
        package_dirs, [REPO_ROOT / path for path in excluded], set(), bin_owner
    )
    workspace.resolver_packages = derive_resolver_packages(workspace)
    return workspace


def _resolver_sources(workspace: Workspace) -> list[tuple[Path, bool]]:
    """Workspace `.rs` files naming a resolver symbol, with a defines flag."""
    sources: list[tuple[Path, bool]] = []
    for source in REPO_ROOT.rglob("*.rs"):
        relative = source.relative_to(REPO_ROOT)
        if "target" in relative.parts or ".git" in relative.parts:
            continue
        if any(source.is_relative_to(directory) for directory in workspace.excluded_dirs):
            continue
        try:
            text = source.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if RESOLVER_USE_RE.search(text):
            sources.append((source, bool(RESOLVER_DEF_RE.search(text))))
    return sources


def derive_resolver_packages(workspace: Workspace) -> set[str]:
    """Packages whose sources reach the binary resolver.

    Derived, not pinned: any package that grows a test spawning `oc-rsync`
    joins the set on the next run.

    The package that *defines* the resolver is the provider, not a consumer:
    its own unit tests deliberately probe the missing-binary path, so its
    `src/` is skipped. An integration test under the provider's `tests/` is
    still a consumer, so the exemption cannot hide a real call site.
    """
    sources = _resolver_sources(workspace)
    providers = {
        workspace.package_at(path) for path, defines in sources if defines
    } - {None}

    found: set[str] = set()
    for source, _defines in sources:
        package = workspace.package_at(source)
        if package is None:
            continue
        if package in providers:
            tests_dir = workspace.package_dirs[package] / "tests"
            if not source.is_relative_to(tests_dir):
                continue
        found.add(package)
    if not found:
        raise AnalysisError(
            "no package references the binary resolver; the symbol list in "
            "RESOLVER_SYMBOLS is stale"
        )
    return found


# ---------------------------------------------------------------------------
# Matrix expansion and `${{ }}` evaluation
# ---------------------------------------------------------------------------


class Unresolved(Exception):
    """An expression could not be evaluated from the known context."""


def expand_matrix(matrix) -> list[dict]:
    """Expand a `strategy.matrix` into rows, per GitHub's documented rules."""
    if not matrix:
        return [{}]
    if not isinstance(matrix, dict):
        raise AnalysisError(f"unsupported matrix form: {matrix!r}")
    axes = {k: v for k, v in matrix.items() if k not in ("include", "exclude")}
    for key, values in axes.items():
        if not isinstance(values, list):
            raise AnalysisError(f"matrix axis {key!r} is not a list")
    rows = [dict(zip(axes, combo)) for combo in product(*axes.values())] or [{}]

    for excluded in matrix.get("exclude") or []:
        rows = [r for r in rows if not _matches(r, excluded)]

    for included in matrix.get("include") or []:
        merged_any = False
        for row in rows:
            if any(key in axes and row.get(key) != value for key, value in included.items()):
                continue
            if any(key not in axes and key in row and row[key] != value
                   for key, value in included.items()):
                continue
            row.update(included)
            merged_any = True
        if not merged_any:
            rows.append(dict(included))
    return rows


def _matches(row: dict, filt: dict) -> bool:
    return all(row.get(key) == value for key, value in filt.items())


_TOKEN_RE = re.compile(
    r"\s*(?:(?P<op>==|!=|&&|\|\||[()!])|"
    r"(?P<str>'(?:[^']|'')*')|"
    r"(?P<ident>[A-Za-z_][A-Za-z0-9_.\-]*))"
)


def _lookup(path: str, context: dict):
    if path in ("true", "false"):
        return path == "true"
    parts = path.split(".")
    node = context
    for part in parts:
        if not isinstance(node, dict) or part not in node:
            raise Unresolved(path)
        node = node[part]
    return node


def evaluate(expression: str, context: dict):
    """Evaluate the workflow-expression subset used by these workflows."""
    tokens: list[tuple[str, object]] = []
    pos = 0
    while pos < len(expression):
        if expression[pos].isspace():
            pos += 1
            continue
        match = _TOKEN_RE.match(expression, pos)
        if match is None:
            raise Unresolved(expression)
        pos = match.end()
        if match.group("op"):
            tokens.append(("op", match.group("op")))
        elif match.group("str"):
            tokens.append(("val", match.group("str")[1:-1].replace("''", "'")))
        else:
            tokens.append(("ident", match.group("ident")))
    index = 0

    def parse_or():
        nonlocal index
        value = parse_and()
        while index < len(tokens) and tokens[index] == ("op", "||"):
            index += 1
            value = bool(value) or bool(parse_and())
        return value

    def parse_and():
        nonlocal index
        value = parse_cmp()
        while index < len(tokens) and tokens[index] == ("op", "&&"):
            index += 1
            value = bool(value) and bool(parse_cmp())
        return value

    def parse_cmp():
        nonlocal index
        value = parse_unary()
        while index < len(tokens) and tokens[index][0] == "op" and tokens[index][1] in ("==", "!="):
            op = tokens[index][1]
            index += 1
            other = parse_unary()
            value = (value == other) if op == "==" else (value != other)
        return value

    def parse_unary():
        nonlocal index
        if index < len(tokens) and tokens[index] == ("op", "!"):
            index += 1
            return not bool(parse_unary())
        return parse_primary()

    def parse_primary():
        nonlocal index
        if index >= len(tokens):
            raise Unresolved(expression)
        kind, value = tokens[index]
        if kind == "op" and value == "(":
            index += 1
            inner = parse_or()
            if index >= len(tokens) or tokens[index] != ("op", ")"):
                raise Unresolved(expression)
            index += 1
            return inner
        index += 1
        if kind == "val":
            return value
        if kind == "ident":
            return _lookup(str(value), context)
        raise Unresolved(expression)

    result = parse_or()
    if index != len(tokens):
        raise Unresolved(expression)
    return result


_INTERP_RE = re.compile(r"\$\{\{(.+?)\}\}", re.DOTALL)


def interpolate(text: str, context: dict) -> tuple[str, list[str]]:
    """Substitute `${{ }}` spans; return the text and any unresolved spans."""
    unresolved: list[str] = []

    def replace(match: re.Match) -> str:
        expression = match.group(1).strip()
        try:
            value = evaluate(expression, context)
        except Unresolved:
            unresolved.append(expression)
            return f"<<unresolved:{expression}>>"
        if isinstance(value, bool):
            return "true" if value else "false"
        return "" if value is None else str(value)

    return _INTERP_RE.sub(replace, text), unresolved


def step_runs(condition, context: dict, default_when_unknown: bool) -> bool:
    """Whether a step with this `if:` runs in this cell."""
    if condition is None:
        return True
    if isinstance(condition, bool):
        return condition
    text = str(condition).strip()
    inner = _INTERP_RE.fullmatch(text)
    if inner:
        text = inner.group(1).strip()
    try:
        return bool(evaluate(text, context))
    except Unresolved:
        return default_when_unknown


# ---------------------------------------------------------------------------
# Cargo command analysis
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Selection:
    """The packages a cargo invocation acts on, plus its output location."""

    packages: frozenset[str]
    excluded: frozenset[str]
    whole_workspace: bool
    profile: str
    target: str | None


def _cargo_invocations(script: str) -> list[list[str]]:
    """Every `cargo <subcommand>` argv in a `run:` block."""
    joined = re.sub(r"\\\n", " ", script)
    argvs: list[list[str]] = []
    for line in joined.split("\n"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        for segment in re.split(r"&&|\|\||;", line):
            segment = segment.strip()
            if "cargo " not in segment:
                continue
            segment = segment[segment.index("cargo ") :]
            try:
                argv = shlex.split(segment, comments=False)
            except ValueError:
                argv = segment.split()
            if argv and argv[0] == "cargo":
                argvs.append(argv)
    return argvs


def _parse_selection(argv: list[str], default_package: str | None) -> Selection:
    packages: set[str] = set()
    excluded: set[str] = set()
    whole = False
    profile = "debug"
    target: str | None = None
    is_nextest = len(argv) > 1 and argv[1] == "nextest"
    index = 1
    while index < len(argv):
        arg = argv[index]
        nxt = argv[index + 1] if index + 1 < len(argv) else None
        if arg in ("-p", "--package") and nxt:
            packages.add(nxt)
            index += 2
            continue
        if arg.startswith("--package="):
            packages.add(arg.split("=", 1)[1])
        elif arg == "--exclude" and nxt:
            excluded.add(nxt)
            index += 2
            continue
        elif arg.startswith("--exclude="):
            excluded.add(arg.split("=", 1)[1])
        elif arg in ("--workspace", "--all"):
            whole = True
        elif arg == "--release":
            profile = "release"
        elif arg == "--cargo-profile" and nxt:
            profile = nxt
            index += 2
            continue
        elif arg.startswith("--cargo-profile="):
            profile = arg.split("=", 1)[1]
        elif arg == "--profile" and nxt:
            # `cargo nextest run --profile` selects a *nextest* profile; the
            # cargo profile comes from --cargo-profile/--release.
            if not is_nextest:
                profile = nxt
            index += 2
            continue
        elif arg.startswith("--profile="):
            if not is_nextest:
                profile = arg.split("=", 1)[1]
        elif arg == "--target" and nxt:
            target = nxt
            index += 2
            continue
        elif arg.startswith("--target="):
            target = arg.split("=", 1)[1]
        index += 1
    if not packages and not whole and default_package:
        packages.add(default_package)
    if profile == "dev":
        profile = "debug"
    return Selection(frozenset(packages), frozenset(excluded), whole, profile, target)


def _is_test_command(argv: list[str]) -> bool:
    if len(argv) >= 3 and argv[1] == "nextest" and argv[2] == "run":
        return True
    return len(argv) >= 2 and argv[1] == "test"


def _builds_oc_rsync(argv: list[str], selection: Selection, workspace: Workspace) -> bool:
    """Whether this invocation links `oc-rsync` into the profile directory."""
    if len(argv) < 2:
        return False
    if argv[1] not in ("build", "nextest", "test", "run", "install"):
        return False
    if argv[1] == "nextest" and (len(argv) < 3 or argv[2] != "run"):
        return False
    if "--bin" in argv:
        index = argv.index("--bin")
        if index + 1 < len(argv) and argv[index + 1] == "oc-rsync":
            return True
    if "--bin=oc-rsync" in argv:
        return True
    if "--no-run" in argv:
        return False
    if workspace.bin_owner in selection.excluded:
        return False
    if selection.whole_workspace:
        return True
    if workspace.bin_owner not in selection.packages:
        return False
    # `cargo build -p bin` with a target filter that excludes bins does not
    # produce the binary.
    return not ("--lib" in argv or "--tests" in argv or "--test" in argv)


# ---------------------------------------------------------------------------
# The check
# ---------------------------------------------------------------------------


@dataclass
class Finding:
    workflow: str
    job: str
    row: dict
    step: str
    command: str
    crates: list[str] = field(default_factory=list)
    reason: str = "no preceding step builds oc-rsync"

    def render(self) -> str:
        row = ", ".join(f"{k}={v}" for k, v in self.row.items()) or "<no matrix>"
        return (
            f"{self.workflow}\n"
            f"  job:    {self.job}\n"
            f"  row:    {row}\n"
            f"  step:   {self.step}\n"
            f"  reaches: {', '.join(self.crates) or '-'}\n"
            f"  reason: {self.reason}\n"
            f"  command: {self.command}\n"
            f"  fix:\n{SUGGESTED_FIX}\n"
        )


def _runner_os(runs_on, context: dict) -> str | None:
    if runs_on is None:
        return None
    if isinstance(runs_on, list):
        runs_on = runs_on[0] if runs_on else None
    if not isinstance(runs_on, str):
        return None
    text, _ = interpolate(runs_on, context)
    lowered = text.lower()
    if "windows" in lowered:
        return "Windows"
    if "macos" in lowered or "darwin" in lowered:
        return "macOS"
    if "ubuntu" in lowered or "linux" in lowered:
        return "Linux"
    return None


def check_workflow(path: Path, workspace: Workspace, verbose: bool) -> tuple[list[Finding], int]:
    document = load_yaml(path)
    if not isinstance(document, dict):
        raise AnalysisError(f"{path}: document is not a mapping")
    jobs = document.get("jobs") or {}
    if not isinstance(jobs, dict):
        raise AnalysisError(f"{path}: `jobs` is not a mapping")
    relative = str(path.relative_to(REPO_ROOT)) if path.is_relative_to(REPO_ROOT) else str(path)
    findings: list[Finding] = []
    cells = 0

    for job_id, job in jobs.items():
        if not isinstance(job, dict):
            continue
        steps = job.get("steps")
        if not isinstance(steps, list):
            continue  # reusable-workflow call; the callee is analysed on its own
        matrix = (job.get("strategy") or {}).get("matrix")
        for row in expand_matrix(matrix):
            context = {"matrix": row}
            context["runner"] = {"os": _runner_os(job.get("runs-on"), context)}
            built: set[tuple[str, str | None]] = set()
            for step in steps:
                if not isinstance(step, dict):
                    continue
                script = step.get("run")
                if not isinstance(script, str):
                    continue
                name = interpolate(
                    str(step.get("name") or script.strip().split("\n")[0]), context
                )[0]
                # A build step whose condition cannot be evaluated is not
                # counted as coverage; a test step whose condition cannot be
                # evaluated is assumed to run. Both directions fail loud.
                build_runs = step_runs(step.get("if"), context, default_when_unknown=False)
                test_runs = step_runs(step.get("if"), context, default_when_unknown=True)
                expanded, unresolved = interpolate(script, context)
                for argv in _cargo_invocations(expanded):
                    selection = _parse_selection(argv, workspace.bin_owner)
                    if _is_test_command(argv):
                        if not test_runs:
                            continue
                        cells += 1
                        rendered = " ".join(argv)
                        if verbose:
                            print(f"  [cell] {relative}::{job_id} {row or '{}'} -> {rendered}")
                        if unresolved and any("<<unresolved:" in a for a in argv):
                            findings.append(
                                Finding(
                                    relative, str(job_id), row, name, rendered,
                                    reason="package selection contains an "
                                           "expression this check cannot resolve",
                                )
                            )
                            continue
                        if _builds_oc_rsync(argv, selection, workspace):
                            continue
                        reaching = sorted(selection.packages & workspace.resolver_packages)
                        if not reaching:
                            continue
                        if (selection.profile, selection.target) in built:
                            continue
                        findings.append(
                            Finding(relative, str(job_id), row, name, rendered, reaching)
                        )
                    elif build_runs and _builds_oc_rsync(argv, selection, workspace):
                        built.add((selection.profile, selection.target))
    return findings, cells


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verbose", action="store_true", help="list every test cell examined")
    parser.add_argument(
        "--list-crates",
        action="store_true",
        help="print the derived resolver-consumer crates and exit",
    )
    parser.add_argument(
        "--workflows",
        type=Path,
        default=WORKFLOW_DIR,
        help="directory of workflow files to analyse (default: .github/workflows)",
    )
    args = parser.parse_args()

    try:
        workspace = discover_workspace()
    except AnalysisError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    consumers = sorted(workspace.resolver_packages)
    if args.list_crates:
        for name in consumers:
            print(name)
        return 0

    print(f"resolver-consumer crates (derived): {', '.join(consumers)}")
    print(f"package owning the oc-rsync [[bin]]: {workspace.bin_owner}")

    findings: list[Finding] = []
    cells = 0
    directory = args.workflows.resolve()
    workflows = sorted(directory.glob("*.yml")) + sorted(directory.glob("*.yaml"))
    if not workflows:
        print(f"error: no workflows found under {directory}", file=sys.stderr)
        return 2
    for path in workflows:
        try:
            found, seen = check_workflow(path, workspace, args.verbose)
        except AnalysisError as error:
            print(f"error: {error}", file=sys.stderr)
            return 2
        findings.extend(found)
        cells += seen

    print(f"examined {cells} test cell(s) across {len(workflows)} workflow file(s)")
    if cells == 0:
        print(
            "error: no test cells were found - the workflow reader is broken "
            "and this check would pass vacuously",
            file=sys.stderr,
        )
        return 2

    if not findings:
        print("ok: every test cell that can reach the resolver builds oc-rsync first")
        return 0

    sys.stdout.flush()
    print(file=sys.stderr)
    print(
        f"error: {len(findings)} test cell(s) can reach the oc-rsync resolver "
        "without building the binary.",
        file=sys.stderr,
    )
    print(
        "Such a cell either fails with a missing-binary panic or, for the "
        "skip-style helpers,\nreports `0 failed; finished in 0.00s` having "
        "spawned nothing at all.\n",
        file=sys.stderr,
    )
    for finding in findings:
        print(finding.render(), file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
