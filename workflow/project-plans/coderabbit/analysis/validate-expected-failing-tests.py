#!/usr/bin/env python3
"""Validate the expected failing tests manifest shape without running tests."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ALLOWED_GROUPS = {
    "graph/fake E2E",
    "artifact store",
    "GitHub API",
    "evaluator",
    "remediation validator",
    "marker/idempotency",
    "shell safety",
}
VAGUE = re.compile(r"^(fails?|not implemented|future|TBD|TODO)$", re.IGNORECASE)
REQUIRED = [
    "test_binary",
    "test_name",
    "test_filter",
    "group",
    "owner_phase",
    "requirement_id",
    "introduced_in_phase",
    "removal_required_by_phase",
    "expected_failure_mode",
    "expected_assertion",
    "artifact_or_fixture",
]


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: validate-expected-failing-tests.py <manifest>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    manifest = json.loads(path.read_text())
    groups = set(manifest.get("allowed_groups", []))
    if groups != ALLOWED_GROUPS:
        raise SystemExit(f"manifest allowed_groups mismatch: {sorted(groups)}")
    entries = manifest.get("entries")
    if not isinstance(entries, list):
        raise SystemExit("manifest entries must be a list")
    names = set()
    for entry in entries:
        missing = [key for key in REQUIRED if not entry.get(key)]
        if missing:
            raise SystemExit(f"manifest entry missing required fields {missing}: {entry}")
        if entry["group"] not in ALLOWED_GROUPS:
            raise SystemExit(f"unknown manifest group {entry['group']}: {entry['test_name']}")
        if entry["test_filter"] != entry["test_name"]:
            raise SystemExit(f"broad or mismatched filter rejected for {entry['test_name']}")
        if VAGUE.fullmatch(str(entry["expected_failure_mode"]).strip()):
            raise SystemExit(f"vague expected_failure_mode for {entry['test_name']}")
        if VAGUE.fullmatch(str(entry["expected_assertion"]).strip()):
            raise SystemExit(f"vague expected_assertion for {entry['test_name']}")
        key = (entry["test_binary"], entry["test_name"])
        if key in names:
            raise SystemExit(f"duplicate manifest test entry {key}")
        names.add(key)
    print("expected_failing_tests_manifest: PASS")
    print(f"entries={len(entries)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
