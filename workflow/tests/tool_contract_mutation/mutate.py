"""Re-run the reviewer's undetected mutations against the reworked contract.

Each mutation is applied to a clean tree, the suite is run, and the tree is
restored. A mutation that does not fail the suite is a gap.
"""

import hashlib
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[3]
WF = ROOT / "workflow"
FIX = WF / "tests/fixtures/tool-contracts/ocr"

OCR = WF / "src/tool_contract/ocr.rs"
TC = WF / "src/tool_contract.rs"
TESTS = WF / "src/tool_contract_tests.rs"
YML = ROOT / ".github/workflows/ocr-pr-review.yml"
DOC = ROOT / "workflow/docs/architecture/tool-contracts.md"

# Generous next to a suite that runs in seconds, but bounded: an unbounded wait
# on a hung suite would skip the restore and leave falsified fixtures in place.
SUITE_TIMEOUT_SECONDS = 300

DIGEST = ROOT / "workflow/src/digest.rs"

INVENTED_LIST = """[
  {
    "session_id": "00000000-0000-0000-0000-000000000000",
    "file_path": "/invented/never/ran/the/binary.jsonl",
    "repo_dir": "/invented/repo"
  }
]
"""


def truncate_required_fields(subcommand):
    """Empty one subcommand's required_fields, leaving the rest intact.

    Rewrites only the vec belonging to that subcommand's constructor so the
    result still compiles; a mutation that does not compile proves nothing.
    """
    source = OCR.read_text()
    marker = f'subcommand: "{subcommand}".to_string(),'
    index = source.index(marker)
    field_start = source.index("required_fields: vec![", index)
    depth, cursor = 0, field_start + len("required_fields: vec!")
    field_end = None
    for position in range(cursor, len(source)):
        if source[position] == "[":
            depth += 1
        elif source[position] == "]":
            depth -= 1
            if depth == 0:
                field_end = position + 1
                break
    if field_end is None:
        # Leaving this implicit bound field_end only when the scan succeeded,
        # so an unbalanced vec raised UnboundLocalError from the write below
        # and read as a bug in the script rather than a mutation that could
        # not be applied. Both mean "this mutation proves nothing", but only
        # one says so.
        raise MutationNotApplied(
            f"required_fields for {subcommand} has no balanced closing bracket"
        )
    OCR.write_text(
        source[:field_start] + "required_fields: vec![]" + source[field_end:])


class MutationNotApplied(RuntimeError):
    """A mutation could not be applied, so its result proves nothing."""


def crlf(name):
    """Rewrite a capture with CRLF endings, as a Windows checkout could.

    The digest is over raw bytes, so this must be caught: otherwise the same
    committed content would verify on one platform and fail on another, and
    the failure would look like a fabricated capture.
    """
    path = FIX / name
    data = path.read_bytes().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")
    path.write_bytes(data)


def fabricate(name, content):
    """Replace a capture and refresh only that capture's recorded digest.

    Two captures are byte-identical, so a global digest replace would corrupt
    both contract entries and be caught as a stale digest rather than as
    fabrication. Refreshing per file is what a maintainer following the
    documented re-capture procedure would actually do, and is therefore the
    honest test of whether content is constrained.
    """

    path = FIX / name
    old_digest = hashlib.sha256(path.read_bytes()).hexdigest()
    path.write_text(content)
    new_digest = hashlib.sha256(path.read_bytes()).hexdigest()

    source = OCR.read_text()
    marker = f'"{name}",'
    index = source.index(marker)
    before, after = source[:index], source[index:]
    if old_digest not in after:
        # Raised rather than asserted: under python -O an assert vanishes, the
        # digest refresh is skipped, and the suite then fails because the
        # digest is stale instead of because the content is unconstrained.
        # That would report a pass for the wrong reason.
        raise MutationNotApplied(f"digest for {name} not found after its filename")
    OCR.write_text(before + after.replace(old_digest, new_digest, 1))


def edit(path, old, new, count=0):
    def apply():
        text = path.read_text()
        if old not in text:
            # Not an assert: stripped under python -O, str.replace would be a
            # no-op and the mutation would be scored UNDETECTED without ever
            # having been applied.
            raise MutationNotApplied(
                f"anchor not found in {path.name}: {old[:60]}")
        path.write_text(text.replace(old, new) if count == 0 else text.replace(old, new, count))
    return apply


def overwrite(path, content):
    return lambda: path.write_text(content)


MUTATIONS = [
    ("M1 truncated version pin (prefix must not validate)",
     edit(OCR, '"v1.7.16"', '"v1.7.1"')),
    ("M2 swapped capture filenames",
     edit(OCR, '"session-list--json.stdout",\n                "30425b13',
          '"session-show--json.stdout",\n                "30425b13')),
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
     edit(OCR, "subcommands: vec![session_list(), session_show(), review()],",
          """subcommands: vec![session_list(), session_show(), review(), fabricated()],
    }
}

fn fabricated() -> SubcommandContract {
    SubcommandContract {
        subcommand: "totally invented".to_string(),
        state_key: StateKey::LogicalWorkingDirectory,
        result_source: ResultSource::Stdout,
        flags: flags(&[("--totally-invented", FlagBehaviour::Honoured)]),
        required_fields: vec![field("invented", "nothing at all really")],
        captures: vec![],""")),
    ("M8 CI installs a different version",
     edit(YML, 'OCR_VERSION: "1.7.16"', 'OCR_VERSION: "1.9.0"')),
    # The realistic fabrication threat: a maintainer who invents a capture
    # WILL refresh its digest, because the re-capture procedure says to. These
    # mutations therefore refresh digests per file rather than globally --
    # two captures are byte-identical, so a global replace corrupts both and
    # would be caught for the wrong reason.
    ("M12 fabricated session list captures with digests refreshed",
     lambda: (fabricate("session-list--json.stdout", INVENTED_LIST),
              fabricate("session-list--json--foreign-cwd-with-repo.stdout",
                        INVENTED_LIST))),
    ("M13 fabricated session show capture with digest refreshed",
     lambda: fabricate(
         "session-show--json.stdout",
         "Session: 8e17b8ad-373c-4742-8cf7-99b239de7ed3 (invented)\n")),
    ("M14 fabricated denial capture with digest refreshed",
     lambda: fabricate("session-show--foreign-cwd-with-repo.stderr",
                       "I made this up: /sessions/tmp/\n")),
    ("M15 bare version token with digest refreshed",
     lambda: fabricate("version.txt", "v1.7.16\n")),
    ("M16 padded null passes as the negative control",
     lambda: fabricate("session-list--json--foreign-cwd.stdout", "   null   \n")),
    ("M17 flag declared honoured with no evidence",
     edit(OCR, '("--repo", FlagBehaviour::Honoured),',
          '("--repo", FlagBehaviour::Honoured),\n                    ("--totally-invented", FlagBehaviour::Honoured),',
          1)),
    ("M18 contract claims a different tool",
     edit(OCR, 'tool: "open-code-review".to_string(),', 'tool: "git".to_string(),')),
    ("M19 provenance denies the tool was run",
     edit(OCR, '"captured by running the ocr binary resolved through PATH and \\',
          '"hand-written from the issue tracker, never run; \\')),
    ("M20 session show required fields emptied",
     lambda: truncate_required_fields("session show")),
    ("M21 factually false remediation",
     edit(OCR, 'use_instead: "the session jsonl named by session list --json"',
          'use_instead: "pass --json again, it works eventually"')),
    ("M22 justification emptied",
     edit(OCR, '"identifies the session a caller then reads evidence from"', '""')),
    ("M23 version check accepts empty input",
     edit(TC, "pub fn verify_version(&self, observed: &str) -> Result<(), ContractViolation> {",
          "pub fn verify_version(&self, observed: &str) -> Result<(), ContractViolation> {\n        if observed.is_empty() {\n            return Ok(());\n        }")),
    ("M28 claim --limit is ignored despite the capture",
     edit(OCR, '("--limit", FlagBehaviour::Honoured)',
          '("--limit", FlagBehaviour::AcceptedAndIgnored { use_instead: "nothing".to_string() })')),
    ("M29 documented mutation count drifts from the battery",
     edit(DOC, "All 34 fail the suite", "All 99 fail the suite")),
    ("M30 duplicate flag recorded twice",
     edit(OCR, '("--limit", FlagBehaviour::Honoured)',
          '("--limit", FlagBehaviour::Honoured), ("--limit", FlagBehaviour::Rejected)')),
    ("M31 multi-subcommand remediation names an unhonoured flag",
     edit(OCR, 'use_instead: "the session jsonl named by session list --json"',
          'use_instead: "the session jsonl named by session list --json or session show --repo"')),
    ("M32 review's state key claims the working directory (#179)",
     edit(OCR, 'state_key: StateKey::GitRoot',
          'state_key: StateKey::LogicalWorkingDirectory')),
    ("M33 preview exclusion header loses its measured wording (#174)",
     edit(OCR, 'name: "Excluded from review".to_string()',
          'name: "Excluded".to_string()')),
    ("M34 review's durable artifact is described as stdout (#195)",
     edit(OCR, 'description: "the session jsonl written under the Git root\'s store slug; \\\n                          stdout carries progress, not the reviewable result"',
          'description: "stdout"')),
    ("M27 capture rewritten with CRLF line endings",
     lambda: crlf("version.txt")),
    ("M25 flag recorded that only prefixes a real one",
     edit(OCR, '("--json", FlagBehaviour::Honoured)',
          '("--jsonl", FlagBehaviour::Honoured)', 1)),
    ("M26 remediation points at an uncaptured flag",
     edit(OCR, 'use_instead: "the session jsonl named by session list --json"',
          'use_instead: "session list --uncaptured-flag"')),
    ("M24 digest silently truncates large input",
     edit(DIGEST, "hasher.update(bytes);",
          "hasher.update(&bytes[..bytes.len().min(1_000_000)]);")),
    ("M9 wrong state key for session list",
     edit(OCR, 'state_key: StateKey::PathArgumentAbsoluteCleaned { flag: "--repo" }',
          "state_key: StateKey::LogicalWorkingDirectory")),
    ("M10 claim session show honours --repo",
     edit(OCR, '''(
                "--repo",
                FlagBehaviour::AcceptedAndIgnored {
                    use_instead: "the working directory of the invoking process".to_string(),
                },
            ),''', '("--repo", FlagBehaviour::Honoured),')),
    ("M11 drop a required field",
     edit(OCR, '''            field(
                "file_path",
                "locates the durable session jsonl, which is the authoritative \\
                     source for session show",
            ),\n''', "")),
]


def snapshot():
    # Outside the repository, because an interrupted run whose only backup
    # lives in a build directory can leave a mutation in the working tree with
    # nothing to restore from. That happened, and a mutated contract survived
    # into a later run.
    backup = pathlib.Path(tempfile.mkdtemp(prefix="luther-mutation-backup-"))
    for path in [OCR, TC, TESTS, YML, DOC, DIGEST]:
        shutil.copy2(path, backup / path.name)
    shutil.copytree(FIX, backup / "fixtures")
    return backup


def restore(backup):
    for path in [OCR, TC, TESTS, YML, DOC, DIGEST]:
        shutil.copy2(backup / path.name, path)
    # Copy over the fixtures rather than removing the directory first: an
    # interruption mid-restore then leaves stale content rather than no
    # content, and every capture is overwritten anyway.
    for source in (backup / "fixtures").iterdir():
        shutil.copy2(source, FIX / source.name)
    for existing in FIX.iterdir():
        if not (backup / "fixtures" / existing.name).exists():
            existing.unlink()


def run_suite():
    """Return "green", "failed", or "broken".

    A mutation that does not compile is not evidence that a guard works, so a
    compile error must never be scored as a catch.
    """
    try:
        result = subprocess.run(
            ["cargo", "test", "--lib", "--", "tool_contract", "digest"],
            cwd=WF, capture_output=True, text=True, timeout=SUITE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        # Without a bound, a hung suite would leave the tree mutated and never
        # reach the restore in main's finally block - the worst outcome
        # available, because the working tree would silently hold falsified
        # fixtures. A timeout is scored "broken" rather than "caught": a
        # mutation that stalls the suite is not evidence that a guard works.
        return "broken"
    except OSError:
        # Covers a missing or non-executable cargo. Catching only
        # TimeoutExpired let these propagate out of run_suite, past the
        # per-mutation handling, and abort the script - which skips the
        # restore and leaves the mutated tree in place. Scored "broken" for the
        # same reason a timeout is: the suite never ran.
        return "broken"
    if "test result: ok." in result.stdout:
        return "green"
    if "test result: FAILED" in result.stdout:
        return "failed"
    return "broken"


def main():
    backup = snapshot()
    gaps = []
    try:
        for name, mutation in MUTATIONS:
            restore(backup)
            try:
                mutation()
            except (MutationNotApplied, AssertionError, ValueError) as error:
                # A mutation whose anchor has drifted tests nothing. Treating
                # it as a skip lets the battery report success while silently
                # covering less than it claims, so it is a failure.
                print(f"  STALE ANCHOR <-- INVALID   {name}: {error}")
                gaps.append(f"{name} [stale anchor]")
                continue
            outcome = run_suite()
            status = {
                "green": "UNDETECTED <-- GAP",
                "failed": "caught",
                "broken": "DID NOT COMPILE <-- INVALID",
            }[outcome]
            print(f"  {status:26} {name}")
            if outcome != "failed":
                gaps.append(f"{name} [{outcome}]")
    finally:
        restore(backup)
        shutil.rmtree(backup)

    # Restoring file contents is not enough: cargo keys rebuilds off mtime, so
    # a restored tree can still test a mutated binary. Force a rebuild and
    # confirm the suite is green before reporting, or the results describe a
    # tree that no longer exists.
    OCR.touch()
    if run_suite() != "green":
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
