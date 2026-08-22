#!/usr/bin/env python3
"""Fail if two pull_request workflows declare the same check-run name.

A branch-protection rule matches a required status check purely by NAME. When
two workflow files declare a job of the same name and both can fire on one pull
request, GitHub publishes two check runs under that single name and the ruleset
is satisfied by whichever it resolves - which may be the one that did no work.

That is not hypothetical. On PR #7433 the context
`upstream-testsuite / upstream testsuite (root)` resolved to `failure` from
ci.yml and `success` from ci-skip.yml on the same commit.

The trap this guards is that `on.pull_request.paths` is an ANY-match over the
changed files, so two workflows with *disjoint path lists* still both fire on a
pull request that touches one file from each list - the common code+docs shape.
Disjoint path sets are necessary but not sufficient for a unique publisher; only
name uniqueness is sufficient, so name uniqueness is what this asserts.

Matrices are expanded before comparing, because two *different* templates can
still collide: ci.yml's `nextest (${{ matrix.toolchain }})` over
`toolchain: [stable, beta, nightly]` produces `nextest (stable)`, which is
exactly the literal name a stand-in workflow declares. Comparing the unexpanded
templates would call that pair unique and miss four of the ten real collisions.
"""

from __future__ import annotations

import itertools
import re
import sys
from pathlib import Path

import yaml

MATRIX_REF = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")

WORKFLOW_DIR = Path(__file__).resolve().parents[2] / ".github" / "workflows"


def triggers_on_pull_request(workflow: dict) -> bool:
    """Report whether a parsed workflow declares a top-level pull_request trigger.

    PyYAML resolves the unquoted YAML 1.1 key ``on`` to the boolean ``True``,
    so the trigger block is looked up under both spellings.
    """
    triggers = workflow.get("on", workflow.get(True))
    if isinstance(triggers, str):
        return triggers == "pull_request"
    if isinstance(triggers, list):
        return "pull_request" in triggers
    if isinstance(triggers, dict):
        return "pull_request" in triggers
    return False


def called_workflow_path(uses: str) -> Path | None:
    """Resolve a local reusable-workflow reference to a path, else None.

    Only `./.github/workflows/x.yml` references are resolvable; a reference to
    another repository is out of scope and cannot collide with a local name.
    """
    if not uses.startswith("./"):
        return None
    return WORKFLOW_DIR.parents[1] / uses[2:]


def matrix_combinations(matrix: dict) -> list[dict]:
    """Enumerate the variable bindings a `strategy.matrix` produces.

    List-valued keys form the cartesian product; `include` entries are extra
    combinations. `exclude` is ignored, which can only ever over-report a
    combination - and over-reporting a name is the safe direction for a gate
    whose job is to prove uniqueness.
    """
    axes = {k: v for k, v in matrix.items() if k not in ("include", "exclude") and isinstance(v, list)}
    combos = [
        dict(zip(axes, values)) for values in itertools.product(*axes.values())
    ] if axes else [{}]
    for extra in matrix.get("include") or []:
        if isinstance(extra, dict):
            combos.append(extra)
    return combos


def expand_job_names(name: str, job_id: str, job: dict) -> set[str]:
    """Return every check-run name one job declaration can publish.

    A job with an explicit `name` uses that template verbatim, with any
    `${{ matrix.x }}` reference substituted. A job with a matrix and no `name`
    takes GitHub's default of `<job id> (<values joined by ", ">)`.
    """
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return {name}

    explicit = "name" in job
    names = set()
    for combo in matrix_combinations(matrix):
        if explicit:
            names.add(MATRIX_REF.sub(lambda m: str(combo.get(m.group(1), m.group(0))), name))
        elif combo:
            names.add(f"{job_id} ({', '.join(str(v) for v in combo.values())})")
        else:
            names.add(job_id)
    return names


def check_names(path: Path, workflow: dict) -> set[str]:
    """Return the check-run names a workflow publishes.

    A plain job publishes its `name` (defaulting to the job id). A job that
    delegates via `uses:` publishes one check per job of the called workflow,
    named `<caller job name> / <called job name>` - the form that appears in a
    branch-protection ruleset.
    """
    names: set[str] = set()
    for job_id, job in (workflow.get("jobs") or {}).items():
        if not isinstance(job, dict):
            continue
        caller_names = expand_job_names(job.get("name", job_id), job_id, job)
        uses = job.get("uses")
        if not uses:
            names |= caller_names
            continue
        called_path = called_workflow_path(uses)
        if called_path is None or not called_path.is_file():
            names |= caller_names
            continue
        called = yaml.safe_load(called_path.read_text()) or {}
        for sub_id, sub in (called.get("jobs") or {}).items():
            if not isinstance(sub, dict):
                names |= {f"{c} / {sub_id}" for c in caller_names}
                continue
            sub_names = expand_job_names(sub.get("name", sub_id), sub_id, sub)
            names |= {f"{c} / {s}" for c in caller_names for s in sub_names}
    return names


def main() -> int:
    publishers: dict[str, list[str]] = {}
    for path in sorted(WORKFLOW_DIR.glob("*.yml")):
        workflow = yaml.safe_load(path.read_text()) or {}
        if not triggers_on_pull_request(workflow):
            continue
        for name in check_names(path, workflow):
            publishers.setdefault(name, []).append(path.name)

    collisions = {n: f for n, f in publishers.items() if len(f) > 1}
    if not collisions:
        print(f"OK: {len(publishers)} pull_request check names, each with one publisher")
        return 0

    print("Duplicate check-run names across pull_request workflows:", file=sys.stderr)
    for name, files in sorted(collisions.items()):
        print(f"  {name!r} declared by {', '.join(sorted(files))}", file=sys.stderr)
    print(
        "\nTwo workflows declaring one name can both fire on a single pull request,"
        "\nletting a required check be satisfied by a run that did no work."
        "\nGive each check exactly one declaring workflow.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
