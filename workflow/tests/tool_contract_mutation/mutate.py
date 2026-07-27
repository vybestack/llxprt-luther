"""Re-run the reviewer's undetected mutations against the reworked contract.

Each mutation is applied to a clean tree, the suite is run, and the tree is
restored. A mutation that does not fail the suite is a gap.
"""

import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path("/Users/acoliver/projects/luther")
WF = ROOT / "workflow"
FIX = WF / "tests/fixtures/tool-contracts/ocr"

OCR = WF / "src/tool_contract/ocr.rs"
TESTS = WF / "src/tool_contract_tests.rs"
YML = ROOT / ".github/workflows/ocr-pr-review.yml"


def edit(path, old, new, count=0):
    def apply():
        text = path.read_text()
        assert old in text, f"anchor not found in {path.name}: {old[:60]}"
        path.write_text(text.replace(old, new) if count == 0 else text.replace(old, new, count))
    return apply


def overwrite(path, content):
    return lambda: path.write_text(content)


MUTATIONS = [
    ("M1 truncated version pin (prefix must not validate)",
     edit(OCR, '"v1.7.16"', '"v1.7.1"')),
    ("M2 swapped capture filenames",
     edit(OCR, '"session-list--json.stdout",\n                        "30425b13',
          '"session-show--json.stdout",\n                        "30425b13')),
    ("M3 falsified session-list fixture content (#174)",
     lambda: (FIX / "session-list--json.stdout").write_text(
         (FIX / "session-list--json.stdout").read_text()
         .replace('"file_path"', '"filePath"')
         .replace('"repo_dir": "/Users/acoliver/projects/luther"',
                  '"repo_dir": "/completely/made/up"'))),
    ("M4 hand-written session-show invention",
     overwrite(FIX / "session-show--json.stdout",
               "Session: not-a-real-id\n  Bogus:  invented field\n")),
    ("M5 hand-edited version.txt",
     overwrite(FIX / "version.txt",
               "open-code-review v1.7.16 and also v2.0.0\nHAND EDITED\n")),
    ("M6 emptied remediation string",
     edit(OCR, 'use_instead: "the session jsonl named by session list --json"',
          'use_instead: ""')),
    ("M7 fabricated subcommand with no capture",
     edit(OCR, "        ],\n    }\n}",
          """            SubcommandContract {
                subcommand: "review".to_string(),
                state_key: StateKey::GitRepositoryRoot,
                result_source: ResultSource::Stdout,
                flags: flags(&[("--totally-invented", FlagBehaviour::Honoured)]),
                required_fields: vec![],
                captures: vec![],
            },
        ],
    }
}""")),
    ("M8 CI installs a different version",
     edit(YML, 'OCR_VERSION: "1.7.16"', 'OCR_VERSION: "1.9.0"')),
    ("M9 wrong state key for session list",
     edit(OCR, 'state_key: StateKey::PathArgument { flag: "--repo" }',
          "state_key: StateKey::LogicalWorkingDirectory")),
    ("M10 claim session show honours --repo",
     edit(OCR, '''(
                        "--repo",
                        FlagBehaviour::AcceptedAndIgnored {
                            use_instead: "the working directory of the invoking process"
                                .to_string(),
                        },
                    ),''', '("--repo", FlagBehaviour::Honoured),')),
    ("M11 drop a required field",
     edit(OCR, '''                    field(
                        "file_path",
                        "locates the durable session jsonl, which is the authoritative \\
                         source for session show",
                    ),\n''', "")),
]


def snapshot():
    backup = ROOT / "workflow/target/_mutbak"
    if backup.exists():
        shutil.rmtree(backup)
    backup.mkdir(parents=True)
    for path in [OCR, TESTS, YML]:
        shutil.copy2(path, backup / path.name)
    shutil.copytree(FIX, backup / "fixtures")
    return backup


def restore(backup):
    for path in [OCR, TESTS, YML]:
        shutil.copy2(backup / path.name, path)
    shutil.rmtree(FIX)
    shutil.copytree(backup / "fixtures", FIX)


def run_suite():
    result = subprocess.run(
        ["cargo", "test", "--lib", "tool_contract"],
        cwd=WF, capture_output=True, text=True)
    return "test result: ok." in result.stdout


def main():
    backup = snapshot()
    gaps = []
    try:
        for name, mutation in MUTATIONS:
            restore(backup)
            try:
                mutation()
            except AssertionError as error:
                print(f"  SKIP {name}: {error}")
                continue
            survived = run_suite()
            status = "UNDETECTED <-- GAP" if survived else "caught"
            print(f"  {status:20} {name}")
            if survived:
                gaps.append(name)
    finally:
        restore(backup)
        shutil.rmtree(backup)

    # Restoring file contents is not enough: cargo keys rebuilds off mtime, so
    # a restored tree can still test a mutated binary. Force a rebuild and
    # confirm the suite is green before reporting, or the results describe a
    # tree that no longer exists.
    OCR.touch()
    if not run_suite():
        print("RESTORED TREE IS NOT GREEN -- results are unreliable")
        return 1

    print()
    if gaps:
        print(f"{len(gaps)} MUTATION(S) SURVIVED:")
        for gap in gaps:
            print(f"  - {gap}")
        return 1
    print(f"All {len(MUTATIONS)} mutations caught.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
