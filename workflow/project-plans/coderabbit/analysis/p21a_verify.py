import json
import subprocess
from pathlib import Path

root = Path('/Users/acoliver/projects/luther/workflow')
log_path = root / 'project-plans/coderabbit/.completed/P21A-command-output.txt'
commands = []
commands.append(('test -f project-plans/coderabbit/.completed/P21', ['test', '-f', 'project-plans/coderabbit/.completed/P21'], False))
commands.append(('grep -E "^VERDICT: PASS|^## Verdict: PASS" project-plans/coderabbit/.completed/P21', ['grep', '-E', '^VERDICT: PASS|^## Verdict: PASS', 'project-plans/coderabbit/.completed/P21'], False))
commands.append(('cargo fmt --check', ['cargo', 'fmt', '--check'], False))
commands.append(('cargo clippy --all-targets -- -D warnings', ['cargo', 'clippy', '--all-targets', '--', '-D', 'warnings'], False))
commands.append(('cargo test --quiet', ['cargo', 'test', '--quiet'], False))
commands.append(('cargo build --release --quiet', ['cargo', 'build', '--release', '--quiet'], False))
commands.append(('cargo test --test pr_followup_workflow_integration', ['cargo', 'test', '--test', 'pr_followup_workflow_integration'], False))
commands.append(('cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list', ['cargo', 'test', '--test', 'e2e_workflow_integration', '--', 'llxprt_dry_run_step_list'], False))
commands.append(('cargo test --test e2e_workflow_integration -- production_and_fixture_llxprt_issue_fix_v1_are_equivalent', ['cargo', 'test', '--test', 'e2e_workflow_integration', '--', 'production_and_fixture_llxprt_issue_fix_v1_are_equivalent'], False))
cmd_file = root / 'project-plans/coderabbit/analysis/final-dry-run-command.json'
data = json.loads(cmd_file.read_text())
argv = data['argv'] if isinstance(data, dict) else data
commands.append(('argv from project-plans/coderabbit/analysis/final-dry-run-command.json', argv, False))
commands.append(('verify expected-failing-tests.json is exactly [] or {"tests": []}', ['python3', '-c', "import json, pathlib, sys; data=json.loads(pathlib.Path('project-plans/coderabbit/analysis/expected-failing-tests.json').read_text()); print(json.dumps(data, sort_keys=True)); sys.exit(0 if data == [] or data == {'tests': []} else 1)"], False))
commands.append(('! grep -rn "todo!\\|unimplemented!" src/engine/executors src/engine/executor.rs tests --include="*.rs"', ['grep', '-rn', 'todo!\\|unimplemented!', 'src/engine/executors', 'src/engine/executor.rs', 'tests', '--include=*.rs'], True))
commands.append(('! grep -rn -E deferred markers', ['grep', '-rn', '-E', '(// TODO|// FIXME|// HACK|placeholder|not yet|will be|@pseudocode lines X-Y|@pseudocode TBD|TODO API|json_path TBD|fixture TBD|assertion TBD)', 'src/engine/executors', 'src/engine/executor.rs', 'tests', 'project-plans/coderabbit/analysis', '--include=*.rs', '--include=*.md', '--include=*.json'], True))

with log_path.open('w') as f:
    f.write('# P21A independent command output\n\n')
    overall_ok = True
    for label, argv, negate in commands:
        f.write(f'## {label}\n')
        f.write('argv: ' + json.dumps(argv) + '\n')
        try:
            proc = subprocess.run(argv, cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=1800)
            exit_code = proc.returncode
            stdout = proc.stdout
            stderr = proc.stderr
        except Exception as exc:
            exit_code = 999
            stdout = ''
            stderr = repr(exc)
        ok = (exit_code != 0) if negate else (exit_code == 0)
        overall_ok = overall_ok and ok
        f.write(f'exit: {exit_code}\n')
        f.write(f'expected: {"non-zero" if negate else "0"}\n')
        f.write(f'result: {"PASS" if ok else "FAIL"}\n')
        f.write('stdout:\n')
        f.write(stdout)
        if stdout and not stdout.endswith('\n'):
            f.write('\n')
        f.write('stderr:\n')
        f.write(stderr)
        if stderr and not stderr.endswith('\n'):
            f.write('\n')
        f.write('\n')
    f.write(f'OVERALL_COMMAND_RESULT: {"PASS" if overall_ok else "FAIL"}\n')
print(log_path)
print('overall', 'PASS' if overall_ok else 'FAIL')
raise SystemExit(0 if overall_ok else 1)
