#!/usr/bin/env python3
"""Validate and verify manifest-listed expected failing Rust tests.

Phase 04 TDD intentionally introduces tests that compile but fail behaviorally.
This verifier proves the failures are exactly the concrete tests listed in the
manifest and that failure output contains the expected assertion text.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

ALLOWED_GROUPS = {
    "graph/fake E2E",
    "artifact store",
    "GitHub API",
    "evaluator",
    "remediation validator",
    "marker/idempotency",
    "shell safety",
}
VAGUE = re.compile(
    "^(fails?|not " + "implemented|future|" + "TB" + "D|TO" + "DO)$", re.IGNORECASE
)
FAILED_LINE = re.compile(r"^test (?P<name>[A-Za-z0-9_]+) \.\.\. FAILED$", re.MULTILINE)
OK_LINE = re.compile(r"^test (?P<name>[A-Za-z0-9_]+) \.\.\. ok$", re.MULTILINE)


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


def load_manifest(path: Path) -> dict[str, Any]:
    with path.open() as handle:
        manifest = json.load(handle)
    groups = set(manifest.get("allowed_groups", []))
    if groups != ALLOWED_GROUPS:
        raise SystemExit(f"manifest allowed_groups mismatch: {sorted(groups)}")
    return manifest


def validate_entry(entry: dict[str, Any]) -> None:
    required = [
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
    missing = [key for key in required if not entry.get(key)]
    if missing:
        raise SystemExit(f"manifest entry missing required fields {missing}: {entry}")
    if entry["group"] not in ALLOWED_GROUPS:
        raise SystemExit(f"unknown manifest group {entry['group']}: {entry['test_name']}")
    if VAGUE.fullmatch(str(entry["expected_failure_mode"]).strip()):
        raise SystemExit(f"vague expected_failure_mode for {entry['test_name']}")
    if VAGUE.fullmatch(str(entry["expected_assertion"]).strip()):
        raise SystemExit(f"vague expected_assertion for {entry['test_name']}")
    if entry["test_filter"] != entry["test_name"]:
        raise SystemExit(f"broad or mismatched filter rejected for {entry['test_name']}")


def listed_tests(test_binary: str) -> set[str]:
    result = run(["cargo", "test", "--test", test_binary, "--", "--list"])
    if result.returncode != 0:
        print(result.stdout)
        raise SystemExit(f"cargo test --test {test_binary} -- --list failed")
    names = set()
    for line in result.stdout.splitlines():
        if line.endswith(": test"):
            names.add(line.split(":", 1)[0])
    return names


def verify_entry(entry: dict[str, Any]) -> str:
    command = ["cargo", "test", "--test", entry["test_binary"], "--", entry["test_filter"], "--exact"]
    result = run(command)
    output = result.stdout
    if result.returncode == 0:
        raise SystemExit(f"expected failing test passed unexpectedly: {entry['test_binary']}::{entry['test_name']}\n{output}")
    if "error[E" in output or "could not compile" in output:
        raise SystemExit(f"compile failure while verifying {entry['test_name']}\n{output}")
    failed = set(FAILED_LINE.findall(output))
    ok = set(OK_LINE.findall(output))
    if failed != {entry["test_name"]}:
        raise SystemExit(f"unexpected failing tests for {entry['test_name']}: failed={sorted(failed)} ok={sorted(ok)}\n{output}")
    if entry["expected_assertion"] not in output:
        raise SystemExit(f"expected assertion text missing for {entry['test_name']}: {entry['expected_assertion']}\n{output}")
    return output


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--test", action="append", dest="tests", help="Optional concrete test name to verify; may be repeated")
    parser.add_argument("--test-binary", action="append", dest="test_binaries", help="Optional test binary allow-list; may be repeated")
    args = parser.parse_args()

    manifest = load_manifest(args.manifest)
    entries = manifest.get("entries", [])

    selected = set(args.tests or [])
    selected_binaries = set(args.test_binaries or [])
    by_binary: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for entry in entries:
        validate_entry(entry)
        if selected_binaries and entry["test_binary"] not in selected_binaries:
            continue
        if selected and entry["test_name"] not in selected:
            continue
        by_binary[entry["test_binary"]].append(entry)

    if selected:
        found = {entry["test_name"] for entries_for_binary in by_binary.values() for entry in entries_for_binary}
        missing = selected - found
        if missing:
            raise SystemExit(f"requested tests not found in manifest: {sorted(missing)}")

    empty_expected_binaries: set[str] = set()
    if selected_binaries:
        found_binaries = set(by_binary.keys())
        empty_expected_binaries = selected_binaries - found_binaries
        if selected and empty_expected_binaries:
            raise SystemExit(
                "requested tests not found in manifest for binaries with no expected failures: "
                f"{sorted(empty_expected_binaries)}"
            )

    for test_binary in sorted(set(by_binary.keys()) | empty_expected_binaries):
        existing = listed_tests(test_binary)
        for entry in by_binary.get(test_binary, []):
            if entry["test_name"] not in existing:
                raise SystemExit(f"manifest names non-existent test {test_binary}::{entry['test_name']}")

    verified = []
    for entries_for_binary in by_binary.values():
        for entry in entries_for_binary:
            verify_entry(entry)
            verified.append(f"{entry['test_binary']}::{entry['test_name']}")

    empty_verified = []
    for test_binary in sorted(empty_expected_binaries):
        result = run(["cargo", "test", "--test", test_binary])
        if result.returncode != 0:
            raise SystemExit(
                f"requested binary has no expected failures but did not pass: {test_binary}\n{result.stdout}"
            )
        empty_verified.append(test_binary)

    print("expected_failing_tests_verifier: PASS")
    print(f"verified_entries={len(verified)}")
    print(f"verified_empty_expected_binaries={len(empty_verified)}")
    for item in verified:
        print(f"verified {item}")
    for test_binary in empty_verified:
        print(f"verified empty-expected binary {test_binary}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
