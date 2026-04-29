# Phase 02: Pseudocode

## Phase ID

`PLAN-20260408-LLXPRT-FIRST.P02`

## Prerequisites

- Required: Phase 01a (Analysis Verification) completed
- Verification: Phase 01 analysis document reviewed and approved

## Purpose

Create numbered pseudocode for all new and modified components. Every line is numbered. Implementation phases MUST reference these line numbers.

---

## Component 1: Enhanced ShellExecutor

File: `src/engine/executors/shell.rs`

```
001  FUNCTION execute(context, params) -> Result<StepOutcome, EngineError>
002    command_template = EXTRACT "command" FROM params OR RETURN Err(missing command)
003    interpolated_command = interpolate_string(command_template, context)
004
005    // --- Stdin setup ---
006    stdin_data: Option<String> = None
007    IF params HAS "stdin" THEN
008      stdin_data = Some(interpolate_string(params["stdin"], context))
009    ELSE IF params HAS "stdin_file" THEN
010      file_path = context.work_dir / params["stdin_file"]
011      IF NOT file_path.exists() THEN
012        RETURN Ok(StepOutcome::Fatal) WITH diagnostic "stdin_file not found: {file_path}"
013      END IF
014      stdin_data = Some(read_file(file_path))
015      IF read fails THEN
016        RETURN Ok(StepOutcome::Fatal) WITH diagnostic "cannot read stdin_file: {error}"
017      END IF
018    END IF
019
020    // --- Spawn command ---
021    cmd = Command::new("sh").arg("-c").arg(interpolated_command)
022    cmd.current_dir(context.work_dir)
023    cmd.stdout(Stdio::piped).stderr(Stdio::piped)
024    IF stdin_data IS Some THEN
025      cmd.stdin(Stdio::piped)
026    END IF
027
028    child = cmd.spawn() OR RETURN Err(spawn failed)
029
030    IF stdin_data IS Some(data) THEN
031      WRITE data TO child.stdin
032      DROP child.stdin  // close to signal EOF
033    END IF
034
035    output = child.wait_with_output() OR RETURN Err(wait failed)
036
037    // --- Capture output ---
038    stdout_str = String::from_utf8_lossy(output.stdout)
039    stderr_str = String::from_utf8_lossy(output.stderr)
040    exit_code = output.status.code()
041
042    context.set("stdout", stdout_str)
043    context.set("stderr", stderr_str)
044    IF exit_code IS Some(code) THEN
045      context.set("exit_code", code.to_string())
046    END IF
047
048    // --- JSON output parsing ---
049    IF params HAS "output_format" AND params["output_format"] == "json" THEN
050      parsed_json = serde_json::from_str(stdout_str)
051      IF parse fails THEN
052        context.set("json_parse_error", error.to_string())
053        RETURN Ok(StepOutcome::Fatal)
054      END IF
055
056      IF params HAS "context_map" THEN
057        FOR EACH (var_name, dot_path) IN params["context_map"] DO
058          value = extract_dot_path(parsed_json, dot_path)
059          IF value IS None THEN
060            top_keys = get_top_level_keys(parsed_json)
061            context.set("json_path_error", "path '{dot_path}' not found, available keys: {top_keys}")
062            RETURN Ok(StepOutcome::Fatal)
063          END IF
064          context.set(var_name, json_value_to_string(value))
065        END FOR
066      END IF
067    END IF
068
069    // --- Outcome determination ---
070    IF exit_code != Some(0) THEN
071      // Check exit_code_map first (REQ-LF-SHELL-010)
072      IF params HAS "exit_code_map" THEN
073        map = params["exit_code_map"] AS HashMap<i32, String>
074        IF map CONTAINS exit_code.unwrap() THEN
075          outcome = parse_outcome_name(map[exit_code.unwrap()])
076          RETURN Ok(outcome)
077        END IF
078      END IF
079      // Unmapped non-zero exit: Fixable (REQ-LF-SHELL-007)
080      RETURN Ok(StepOutcome::Fixable)
081    END IF
074
075    IF params HAS "outcome_on_stdout" THEN
076      FOR EACH (pattern_string, outcome_name) IN params["outcome_on_stdout"] DO
077        IF stdout_str CONTAINS pattern_string THEN
078          outcome = parse_outcome_name(outcome_name)  // "success" -> Success, "fixable" -> Fixable, etc.
079          RETURN Ok(outcome)
080        END IF
081      END FOR
082      // No pattern matched, exit was 0 -> default Success (REQ-LF-SHELL-006)
083    END IF
084
085    RETURN Ok(StepOutcome::Success)
086  END FUNCTION
087
088  FUNCTION extract_dot_path(json_value, dot_path) -> Option<&Value>
089    parts = dot_path.split('.')
090    current = json_value
091    FOR EACH part IN parts DO
092      IF part starts with '.' THEN skip (leading dot) END IF
093      current = current.get(part)
094      IF current IS None THEN RETURN None END IF
095    END FOR
096    RETURN Some(current)
097  END FUNCTION
098
099  FUNCTION json_value_to_string(value) -> String
100    MATCH value
101      String(s) => s
102      Number(n) => n.to_string()
103      Bool(b) => b.to_string()
104      Array(_) | Object(_) => serde_json::to_string(value)
105      Null => ""
106    END MATCH
107  END FUNCTION
108
109  FUNCTION parse_outcome_name(name) -> StepOutcome
110    MATCH name.to_lowercase()
111      "success" => StepOutcome::Success
112      "fixable" => StepOutcome::Fixable
113      "fatal" => StepOutcome::Fatal
114      "retryable" => StepOutcome::Retryable
115      "abandon" => StepOutcome::Abandon
116      _ => StepOutcome::Success  // unknown name defaults to success
117    END MATCH
118  END FUNCTION
```

---

## Component 2: VerifyExecutor

File: `src/engine/executors/verify.rs`

```
001  STRUCT CheckResult
002    check_type: String        // "lint", "typecheck", "test", "format", "build"
003    passed: bool
004    exit_code: i32
005    errors: Vec<ErrorRecord>
006    raw_stdout: String
007    raw_stderr: String
008  END STRUCT
009
010  STRUCT ErrorRecord
011    file: Option<String>
012    line: Option<u32>
013    column: Option<u32>
014    message: String
015    severity: Option<String>
016    // Test-specific fields
017    test_name: Option<String>
018    assertion_kind: Option<String>
019    expected: Option<String>
020    actual: Option<String>
021  END STRUCT
022
023  STRUCT VerifyReport
024    passed: bool
025    summary: String
026    checks: Vec<CheckResult>
027  END STRUCT
028
029  STRUCT VerifyExecutor
030  END STRUCT
031
032  IMPL StepExecutor FOR VerifyExecutor
033    FUNCTION execute(context, params) -> Result<StepOutcome, EngineError>
034      checks_array = EXTRACT "checks" FROM params AS array OR RETURN Err(missing checks)
035      check_commands = EXTRACT "check_commands" FROM params AS object OR default empty map
036
037      work_dir = context.work_dir()
038      results: Vec<CheckResult> = Vec::new()
039      all_passed = true
040
041      FOR EACH check_type IN checks_array DO
042        command = resolve_check_command(check_type, check_commands)
043        IF command IS None THEN
044          RETURN Err(StepExecutionError "unknown check type: {check_type}")
045        END IF
046
047        // Run the check command
048        cmd = Command::new("sh").arg("-c").arg(command)
049        cmd.current_dir(work_dir)
050        cmd.stdout(Stdio::piped).stderr(Stdio::piped)
051
052        output = cmd.output()
053        IF output IS Err THEN
054          // Cannot spawn command -> Fatal (REQ-LF-VERIFY-008)
055          context.set("verify_error", "Failed to run {check_type}: {error}")
056          RETURN Ok(StepOutcome::Fatal)
057        END IF
058
059        stdout = String::from_utf8_lossy(output.stdout)
060        stderr = String::from_utf8_lossy(output.stderr)
061        exit_code = output.status.code().unwrap_or(-1)
062
063        // Parse output based on check type
064        errors = parse_check_output(check_type, stdout, stderr, exit_code)
065        passed = exit_code == 0
066
067        IF NOT passed THEN all_passed = false END IF
068
069        results.push(CheckResult {
070          check_type, passed, exit_code, errors, raw_stdout: stdout, raw_stderr: stderr
071        })
072      END FOR
073
074      // Build summary (REQ-LF-VERIFY-004)
075      summary = build_summary(results)
076
077      // Build report (REQ-LF-VERIFY-005)
078      report = VerifyReport { passed: all_passed, summary: summary.clone(), checks: results }
079
080      // Write report to file (REQ-LF-VERIFY-003)
081      report_json = serde_json::to_string_pretty(report)
082      luther_dir = work_dir / ".luther"
083      create_dir_all(luther_dir)
084      write_file(luther_dir / "verify-report.json", report_json)
085
086      // Set context variables (REQ-LF-VERIFY-002, REQ-LF-VERIFY-004, REQ-LF-VERIFY-009)
087      context.set("verify_passed", if all_passed { "true" } else { "false" })
088      context.set("verify_summary", summary)
089
090      // Set per-check-type error context vars
091      FOR EACH result IN report.checks DO
092        IF NOT result.errors.is_empty() THEN
093          error_json = serde_json::to_string(result.errors)
094          MATCH result.check_type
095            "test" => context.set("test_failures", error_json)
096            "build" => context.set("build_errors", error_json)
097            "typecheck" => context.set("type_errors", error_json)
098            "lint" => context.set("lint_errors", error_json)
099            "format" => context.set("format_errors", error_json)
100          END MATCH
101        END IF
102      END FOR
103
104      IF all_passed THEN
105        RETURN Ok(StepOutcome::Success)
106      ELSE
107        RETURN Ok(StepOutcome::Fixable)
108      END IF
109    END FUNCTION
110  END IMPL
111
112  FUNCTION resolve_check_command(check_type, custom_commands) -> Option<String>
113    // Check custom commands first
114    IF custom_commands HAS check_type THEN
115      RETURN Some(custom_commands[check_type])
116    END IF
117
118    // Fall back to defaults for Node/TypeScript
119    MATCH check_type
120      "lint" => Some("npm run lint -- --format json 2>&1 || true")
121      "typecheck" => Some("npx tsc --noEmit 2>&1")
122      "test" => Some("npx vitest run --reporter=json 2>&1")
123      "format" => Some("npx prettier --check . 2>&1")
124      "build" => Some("npm run build 2>&1")
125      _ => None
126    END MATCH
127  END FUNCTION
128
129  FUNCTION parse_check_output(check_type, stdout, stderr, exit_code) -> Vec<ErrorRecord>
130    IF exit_code == 0 THEN RETURN vec![] END IF
131
132    MATCH check_type
133      "typecheck" => parse_typescript_errors(stdout, stderr)
134      "test" => parse_test_results(stdout, stderr)
135      "lint" => parse_lint_errors(stdout, stderr)
136      "format" => parse_format_errors(stdout, stderr)
137      "build" => parse_build_errors(stdout, stderr)
138      _ => vec![ErrorRecord { message: stderr, ..default }]
139    END MATCH
140  END FUNCTION
141
142  FUNCTION parse_typescript_errors(stdout, stderr) -> Vec<ErrorRecord>
143    // TypeScript errors: "src/foo.ts(10,5): error TS2322: Type X is not assignable to Y"
144    errors = Vec::new()
145    combined = stdout + stderr
146    FOR EACH line IN combined.lines() DO
147      IF line matches regex r"^(.+)\((\d+),(\d+)\): error (TS\d+): (.+)$" THEN
148        errors.push(ErrorRecord { file: match[1], line: match[2], column: match[3], message: match[5], severity: "error" })
149      END IF
150    END FOR
151    IF errors.is_empty() AND combined.len() > 0 THEN
152      errors.push(ErrorRecord { message: combined.trim(), ..default })
153    END IF
154    RETURN errors
155  END FUNCTION
156
157  FUNCTION parse_test_results(stdout, stderr) -> Vec<ErrorRecord>
158    // Try JSON parse first (vitest --reporter=json)
159    IF serde_json::from_str(stdout) IS Ok(json) THEN
160      errors = Vec::new()
161      IF json HAS "testResults" THEN
162        FOR EACH test_file IN json["testResults"] DO
163          FOR EACH test IN test_file["assertionResults"] DO
164            IF test["status"] == "failed" THEN
165              errors.push(ErrorRecord {
166                file: test_file["name"],
167                test_name: test["fullName"],
168                message: test["failureMessages"].join("\n"),
169                assertion_kind: "assertion",
170              })
171            END IF
172          END FOR
173        END FOR
174      END IF
175      RETURN errors
176    END IF
177    // Fallback: just return raw output as a single error
178    RETURN vec![ErrorRecord { message: (stdout + stderr).trim(), ..default }]
179  END FUNCTION
180
181  FUNCTION parse_lint_errors(stdout, stderr) -> Vec<ErrorRecord>
182    // Try JSON parse (eslint --format json)
183    IF serde_json::from_str(stdout) IS Ok(json_array) THEN
184      errors = Vec::new()
185      FOR EACH file_result IN json_array DO
186        FOR EACH msg IN file_result["messages"] DO
187          errors.push(ErrorRecord {
188            file: file_result["filePath"],
189            line: msg["line"],
190            column: msg["column"],
191            message: msg["message"],
192            severity: if msg["severity"] == 2 { "error" } else { "warning" },
193          })
194        END FOR
195      END FOR
196      RETURN errors
197    END IF
198    RETURN vec![ErrorRecord { message: (stdout + stderr).trim(), ..default }]
199  END FUNCTION
200
201  FUNCTION parse_format_errors(stdout, stderr) -> Vec<ErrorRecord>
202    // Prettier --check outputs unformatted filenames, one per line
203    errors = Vec::new()
204    combined = stdout + stderr
205    FOR EACH line IN combined.lines() DO
206      IF line starts with "[warn]" THEN
207        file_path = line.trim_start("[warn]").trim()
208        errors.push(ErrorRecord { file: file_path, message: "File is not formatted" })
209      ELSE IF line.ends_with(".ts") OR line.ends_with(".tsx") OR line.ends_with(".js") THEN
210        errors.push(ErrorRecord { file: line.trim(), message: "File is not formatted" })
211      END IF
212    END FOR
213    IF errors.is_empty() AND combined.len() > 0 THEN
214      errors.push(ErrorRecord { message: combined.trim(), ..default })
215    END IF
216    RETURN errors
217  END FUNCTION
218
219  FUNCTION parse_build_errors(stdout, stderr) -> Vec<ErrorRecord>
220    // Generic: try to extract TypeScript-style errors, fall back to raw
221    errors = parse_typescript_errors(stdout, stderr)
222    IF errors.is_empty() AND (stdout.len() + stderr.len()) > 0 THEN
223      errors.push(ErrorRecord { message: (stdout + stderr).trim(), ..default })
224    END IF
225    RETURN errors
226  END FUNCTION
227
228  FUNCTION build_summary(results) -> String
229    parts = Vec::new()
230    FOR EACH r IN results DO
231      status = if r.passed { "pass" } else { format!("{} errors", r.errors.len()) }
232      parts.push(format!("{}: {}", r.check_type, status))
233    END FOR
234    RETURN parts.join(", ")
235  END FUNCTION
```

---

## Component 3: Namespaced Context

File: `src/engine/executor.rs`

```
001  STRUCT StepContext
002    work_dir: PathBuf
003    run_id: String
004    variables: HashMap<String, String>
005    current_step_id: Option<String>    // NEW: tracks which step is executing
006  END STRUCT
007
008  FUNCTION StepContext::new(work_dir, run_id) -> Self
009    Self { work_dir, run_id, variables: HashMap::new(), current_step_id: None }
010  END FUNCTION
011
012  FUNCTION StepContext::set_current_step_id(step_id: &str)
013    self.current_step_id = Some(step_id.to_string())
014  END FUNCTION
015
016  FUNCTION StepContext::set(key, value)
017    IF self.current_step_id IS Some(step_id) THEN
018      // Store namespaced version
019      namespaced_key = format!("{}.{}", step_id, key)
020      self.variables.insert(namespaced_key, value)
021    END IF
022    // Always store bare key (last-write-wins for unnamespaced lookups)
023    self.variables.insert(key, value)
024  END FUNCTION
025
026  FUNCTION StepContext::get(key) -> Option<&String>
027    // Direct lookup first (handles both "step_id.var" and bare "var")
028    IF self.variables.contains_key(key) THEN
029      RETURN self.variables.get(key)
030    END IF
031    RETURN None
032  END FUNCTION
033
034  FUNCTION interpolate_string(template, context) -> String
035    // Collect all keys from context variables and built-ins
036    all_keys = context.variables.keys().collect()
037    all_keys.push("work_dir")
038    all_keys.push("run_id")
039
040    // Sort by length descending (longest first to prevent partial replacement)
041    all_keys.sort_by_key(|k| Reverse(k.len()))
042
043    result = template
044    FOR EACH key IN all_keys DO
045      placeholder = format!("{{{}}}", key)
046      IF let Some(value) = context.get(key) THEN
047        result = result.replace(placeholder, value)
048      END IF
049    END FOR
050
051    RETURN result
052  END FUNCTION
```

---

## Component 4: Per-edge Loop Limits

File: `src/workflow/schema.rs` (TransitionDef change)

```
001  STRUCT TransitionDef
002    from: String
003    to: String
004    condition: Option<String>
005    max_iterations: Option<u32>     // NEW: per-edge loop limit
006  END STRUCT
```

File: `src/engine/transition.rs` (TransitionDef change)

```
007  STRUCT TransitionDef  // local version in transition.rs
008    from: String
009    to: String
010    condition: Option<String>
011    max_iterations: Option<u32>     // NEW: per-edge loop limit
012  END STRUCT
```

File: `src/persistence/checkpoint.rs` (StateSnapshot change)

```
013  STRUCT StateSnapshot
014    retry_count: u32
015    loop_count: u32                         // KEPT: backward compat + global fallback
016    edge_loop_counts: HashMap<String, u32>  // NEW: per-edge counts, keyed "from:to"
017    context: HashMap<String, Value>
018    status: String
019  END STRUCT
```

File: `src/engine/runner.rs` (EngineRunner changes)

```
020  STRUCT EngineRunner
021    instance: WorkflowInstance
022    retry_count: u32
023    edge_loop_counts: HashMap<String, u32>  // CHANGED: was loop_count: u32
024    max_retries: u32
025    max_loops: u32                           // KEPT: global fallback
026    conn: RefCell<Connection>
027    interrupted: RefCell<bool>
028    registry: ExecutorRegistry
029    context: StepContext
030  END STRUCT
031
032  FUNCTION EngineRunner::new(instance, registry) -> Self
033    max_loops = instance.config.guard_limits.max_iterations.unwrap_or(10)
034    edge_loop_counts = HashMap::new()
035    // ... rest unchanged
036  END FUNCTION
037
038  FUNCTION EngineRunner::run() -> Result<RunOutcome, EngineError>
039    // ... checkpoint resume logic (load edge_loop_counts from snapshot) ...
040
041    current_step_id = self.instance.current_state.clone()
042
043    LOOP
044      // Check interrupt
045      // ...
046
047      // Set current step on context for namespaced storage
048      self.context.set_current_step_id(current_step_id)
049
050      // Execute step
051      outcome = self.execute_step(current_step_id)
052
053      // Persist checkpoint (include edge_loop_counts)
054      checkpoint = self.create_checkpoint(current_step_id, "completed")
055      save_checkpoint(checkpoint)
056      append_event(...)
057
058      // Handle terminal outcomes (Fatal, Abandon)
059      // ...
060
061      // Resolve next step
062      next_step = self.resolve_next_step(current_step_id, outcome)
063
064      MATCH next_step
065        Some(next_step_id) =>
066          // Find the transition def for this edge
067          edge_key = format!("{}:{}", current_step_id, next_step_id)
068          transition_def = find_transition(current_step_id, outcome, transitions)
069
070          // Get per-edge limit (or global fallback)
071          edge_limit = transition_def.max_iterations.unwrap_or(self.max_loops)
072
073          // Check if this is a backward edge
074          IF self.is_loop_back(current_step_id, next_step_id) THEN
075            current_count = self.edge_loop_counts.get(edge_key).unwrap_or(0)
076            IF current_count >= edge_limit THEN
077              RETURN Ok(RunOutcome::Abandoned {
078                step_id: current_step_id,
079                reason: format!("Per-edge loop limit ({}) exceeded on edge {}",
080                                edge_limit, edge_key)
081              })
082            END IF
083            self.edge_loop_counts.insert(edge_key, current_count + 1)
084          END IF
085
086          current_step_id = next_step_id
087          self.instance.transition_to(current_step_id)
088
089        None =>
090          RETURN Ok(RunOutcome::Success)
091      END MATCH
092    END LOOP
093  END FUNCTION
094
095  FUNCTION EngineRunner::create_checkpoint(step_id, status) -> Checkpoint
096    snapshot = StateSnapshot {
097      retry_count: self.retry_count,
098      loop_count: self.edge_loop_counts.values().sum(),  // backward compat
099      edge_loop_counts: self.edge_loop_counts.clone(),
100      context: HashMap::new(),
101      status,
102    }
103    Checkpoint::with_snapshot(self.instance.run_id, step_id, snapshot)
104  END FUNCTION
105
106  FUNCTION EngineRunner::loop_count() -> u32
107    // Backward compat: return sum of all edge counts
108    self.edge_loop_counts.values().sum()
109  END FUNCTION
110
111  // find_transition: lookup TransitionDef matching from/condition
112  FUNCTION find_transition(from, outcome, transitions) -> Option<&TransitionDef>
113    outcome_str = outcome.to_string()
114    FOR EACH t IN transitions DO
115      IF t.from == from THEN
116        IF t.condition == Some(outcome_str) OR (t.condition IS None AND outcome == Success) THEN
117          RETURN Some(t)
118        END IF
119      END IF
120    END FOR
121    RETURN None
122  END FUNCTION
```

---

## Component 5: Workflow TOML Structure

File: `config/workflows/llxprt-issue-fix-v1.toml` (data, not code)

```
001  workflow_type_id = "llxprt-issue-fix-v1"
002
003  // Step definitions for the 15-step workflow:
004  // select_issue, fetch_issue, setup_workspace,
005  // create_plan, evaluate_plan,
006  // implement, evaluate_impl,
007  // run_tests, remediate,
008  // push_changes, generate_pr_description, create_pr,
009  // abandon_and_log, log_completion
010
011  // Transitions with per-edge max_iterations on loop-back edges:
012  // evaluate_plan -> create_plan [condition="fixable", max_iterations=5]
013  // run_tests -> remediate [condition="fixable"]
014  // remediate -> run_tests [condition="success", max_iterations=5]
015  // * -> abandon_and_log [condition="fatal"]
016  // * -> abandon_and_log [condition="abandon"]
```

File: `config/workflow-configs/llxprt-code.toml` (data, not code)

```
017  config_id = "llxprt-code"
018  workflow_type_id = "llxprt-issue-fix-v1"
019
020  [runtime]
021  timeout_seconds = 7200
022  max_retries = 3
023
024  [repository]
025  workspace_strategy = "temp_clone"
026  branch_template = "issue{issue_number}"
027  base_branch = "main"
028
029  [guards]
030  max_iterations = 10
031  max_file_changes = 200
032  max_tokens = 500000
033  max_cost = 50.0
034
035  // Variables section for profile mappings and repo config
036  // These get loaded into StepContext at run start
037  [variables]
038  target_repo = "vybestack/llxprt-code"
039  assignee = "acoliver"
040  profile_planning = "opusthinking"
041  profile_evaluating = "gpt54xhigh"
042  profile_implementing = "opusthinking"
043  profile_remediating = "sonnetthinking"
```

---

## Pseudocode Line Index

| Component | Lines | File |
|---|---|---|
| ShellExecutor.execute() | 001-093 | shell.rs |
| extract_dot_path() | 088-097 | shell.rs |
| json_value_to_string() | 099-107 | shell.rs |
| parse_outcome_name() | 109-118 | shell.rs |
| CheckResult, ErrorRecord, VerifyReport structs | 001-028 | verify.rs |
| VerifyExecutor.execute() | 032-110 | verify.rs |
| resolve_check_command() | 112-127 | verify.rs |
| parse_check_output() | 129-140 | verify.rs |
| parse_typescript_errors() | 142-155 | verify.rs |
| parse_test_results() | 157-179 | verify.rs |
| parse_lint_errors() | 181-199 | verify.rs |
| parse_format_errors() | 201-217 | verify.rs |
| parse_build_errors() | 219-226 | verify.rs |
| build_summary() | 228-235 | verify.rs |
| StepContext (namespaced) | 001-052 | executor.rs |
| TransitionDef (schema) | 001-006 | schema.rs |
| TransitionDef (transition) | 007-012 | transition.rs |
| StateSnapshot | 013-019 | checkpoint.rs |
| EngineRunner (per-edge) | 020-122 | runner.rs |
| Workflow TOML structure | 001-016 | llxprt-issue-fix-v1.toml |
| Workflow config structure | 017-043 | llxprt-code.toml |

## Verification Commands

```bash
# No code changes in this phase
cargo build --all-targets
cargo test
```

## Success Criteria

- Every requirement has corresponding pseudocode lines
- All pseudocode lines are numbered
- Line numbers are referenced in the Pseudocode Line Index
- No code written — pseudocode only

## Failure Recovery

This phase has no code changes. No rollback needed.
