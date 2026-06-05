from pathlib import Path

root = Path('/Users/acoliver/projects/luther/workflow')
log = (root / 'project-plans/coderabbit/.completed/P21A-command-output.txt').read_text()
content = """# Phase 21a Verification Results
## Verdict: FAIL

Blocking issue:

- Required deferred-implementation negative grep failed. The exact command returned exit 0 and matched `project-plans/coderabbit/analysis/artifact-schema-contract.md:301` containing the forbidden phrase `not yet` in the required search scope.

Semantic verification:

- Not accepted as complete because one required Phase 21 deferred-implementation detection gate failed.
- The independent command run did confirm the P21 marker exists, the P21 marker contains PASS, formatting/clippy/tests/build/targeted integration tests/dry-run/expected-failing manifest/todo-unimplemented grep passed.
- Semantic checklist evidence is not sufficient for a PASS while the required negative grep still finds a forbidden deferred marker in `project-plans/coderabbit/analysis`.

Full exact independent command output follows.

""" + log
(root / 'project-plans/coderabbit/.completed/P21A.md').write_text(content)
print(root / 'project-plans/coderabbit/.completed/P21A.md')
