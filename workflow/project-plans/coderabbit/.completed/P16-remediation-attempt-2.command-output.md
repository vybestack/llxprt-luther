# Phase 16 Remediation Attempt 2 Command Output

## GitHub API contract tests

```text
$ cargo test --test github_api_contract_tests -- github_api_contract
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/github_api_contract_tests.rs (target/debug/deps/github_api_contract_tests-2db7dc2f1c547aec)

running 2 tests
test github_api_contract_negative_fixture_rejects_missing_contract ... ok
test github_api_contract_validator_accepts_phase_02_contract ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.44s
exit=0
```

## GitHub API fixture validator

```text
$ cargo run --bin github_api_contract_validator -- project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.04s
     Running `target/debug/github_api_contract_validator project-plans/coderabbit/analysis/github-api-contract.md tests/fixtures/github_api_contract`
github_api_contract_validator: PASS
exit=0
```

## P16 marker audit

```text
$ cargo test --test pr_followup_marker_audit_tests -- p16_markers_cover_all_touched_items
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/pr_followup_marker_audit_tests.rs (target/debug/deps/pr_followup_marker_audit_tests-541da5180a19145e)

running 1 test
test p16_markers_cover_all_touched_items ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.00s
exit=0
```

## Substantive fake PR follow-up scenarios

```text
$ cargo test --test pr_followup_workflow_integration
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/pr_followup_workflow_integration.rs (target/debug/deps/pr_followup_workflow_integration-52387906893a32be)

running 13 tests
test post_pr_fake_clean_success_reaches_marker_and_log_completion ... ok
test post_pr_fake_coderabbit_valid_remediation ... ok
test post_pr_fake_all_terminal_ci_failed_remediation_rechecks_head ... ok
test post_pr_fake_empty_not_ready_fatal ... ok
test post_pr_fake_invalid_out_of_scope_only_marks_then_completes ... ok
test post_pr_fake_failed_and_unknown_terminal ... ok
test post_pr_fake_failed_and_pending_terminal ... ok
test post_pr_fake_local_verification_failure_loops_to_remediation ... ok
test post_pr_fake_malformed_non_empty_remediation_result_rejected ... ok
test post_pr_fake_marker_partial_failure_terminal ... ok
test post_pr_fake_marker_retry_idempotency ... ok
test post_pr_fake_pending_marker_carry_forward_head_a_to_b ... ok
test post_pr_fake_unknown_timeout_without_concrete_failures_fatal ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
exit=0
```

## Post-PR graph test discovery

```text
$ cargo test --test e2e_workflow_integration -- post_pr --list
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-71f98b4686e2b204)
llxprt_dry_run_step_list_includes_full_post_pr_contract: test
post_pr_duplicate_transition_negative_fixtures_are_detected: test
post_pr_exact_p17_routing_contract_is_present: test
post_pr_failure_terminal_is_terminal_and_fatal_routes_target_it: test
post_pr_fake_executor_contract_never_returns_abandon: test
post_pr_fake_runner_fatal_outcome_ends_at_failure_terminal: test
post_pr_guard_enumerates_all_steps_and_forbids_abandon_routes: test
post_pr_negative_fixture_detects_fatal_route_to_successful_cleanup: test
post_pr_no_direct_create_pr_to_log_completion: test
post_pr_reachable_transitions_are_unique_by_from_and_effective_condition: test
post_pr_steps_have_artifact_root_step_order_and_path_contract: test

11 tests, 0 benchmarks
exit=0
```

## Dry-run test discovery

```text
$ cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list --list
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-71f98b4686e2b204)
llxprt_dry_run_step_list_current_toml_lists_existing_steps: test
llxprt_dry_run_step_list_includes_full_post_pr_contract: test

2 tests, 0 benchmarks
exit=0
```

## Current-TOML dry-run list gate

```text
$ cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list_current_toml_lists_existing_steps --exact
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-71f98b4686e2b204)

running 1 test
test llxprt_dry_run_step_list_current_toml_lists_existing_steps ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out; finished in 0.00s
exit=0
```

## Expected P17-owned dry-run contract failure

```text
$ cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list_includes_full_post_pr_contract --exact
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.05s
     Running tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-71f98b4686e2b204)

running 1 test
test llxprt_dry_run_step_list_includes_full_post_pr_contract ... FAILED

failures:

---- llxprt_dry_run_step_list_includes_full_post_pr_contract stdout ----

thread 'llxprt_dry_run_step_list_includes_full_post_pr_contract' (196085774) panicked at tests/e2e_workflow_integration.rs:1066:9:
dry-run step list missing capture_pr_identity
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    llxprt_dry_run_step_list_includes_full_post_pr_contract

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 26 filtered out; finished in 0.00s

error: test failed, to rerun pass `--test e2e_workflow_integration`
exit=101
```

## Mixed dry-run filter expected current-TOML failure

```text
$ cargo test --test e2e_workflow_integration -- llxprt_dry_run_step_list
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running tests/e2e_workflow_integration.rs (target/debug/deps/e2e_workflow_integration-71f98b4686e2b204)

running 2 tests
test llxprt_dry_run_step_list_current_toml_lists_existing_steps ... ok
test llxprt_dry_run_step_list_includes_full_post_pr_contract ... FAILED

failures:

---- llxprt_dry_run_step_list_includes_full_post_pr_contract stdout ----

thread 'llxprt_dry_run_step_list_includes_full_post_pr_contract' (196085801) panicked at tests/e2e_workflow_integration.rs:1066:9:
dry-run step list missing capture_pr_identity
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    llxprt_dry_run_step_list_includes_full_post_pr_contract

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 25 filtered out; finished in 0.00s

error: test failed, to rerun pass `--test e2e_workflow_integration`
exit=101
```

## Expected-failing manifest validation

```text
$ python3 project-plans/coderabbit/analysis/validate-expected-failing-tests.py project-plans/coderabbit/analysis/expected-failing-tests.json
expected_failing_tests_manifest: PASS
entries=7
exit=0
```

## Required expected-failing verifier command

```text
$ python3 project-plans/coderabbit/analysis/verify-expected-failing-tests.py --manifest project-plans/coderabbit/analysis/expected-failing-tests.json --test-binary e2e_workflow_integration --test-binary pr_followup_workflow_integration
expected_failing_tests_verifier: PASS
verified_entries=7
verified_empty_expected_binaries=1
verified e2e_workflow_integration::post_pr_no_direct_create_pr_to_log_completion
verified e2e_workflow_integration::post_pr_guard_enumerates_all_steps_and_forbids_abandon_routes
verified e2e_workflow_integration::post_pr_failure_terminal_is_terminal_and_fatal_routes_target_it
verified e2e_workflow_integration::post_pr_exact_p17_routing_contract_is_present
verified e2e_workflow_integration::post_pr_steps_have_artifact_root_step_order_and_path_contract
verified e2e_workflow_integration::post_pr_fake_runner_fatal_outcome_ends_at_failure_terminal
verified e2e_workflow_integration::llxprt_dry_run_step_list_includes_full_post_pr_contract
verified empty-expected binary pr_followup_workflow_integration
exit=0
```

## All-targets build

```text
$ cargo build --all-targets
warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib) generated 12 warnings
warning: unused variable: `malformed_toml`
   --> tests/config_binding_integration.rs:192:9
    |
192 |     let malformed_toml = r#"
    |         ^^^^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_malformed_toml`
    |
    = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: variable does not need to be mutable
  --> tests/per_edge_loop_tests.rs:48:13
   |
48 |         let mut outcomes = self.outcomes.lock().unwrap();
   |             ----^^^^^^^^
   |             |
   |             help: remove this `mut`
   |
   = note: `#[warn(unused_mut)]` (part of `#[warn(unused)]`) on by default

warning: method `call_count` is never used
  --> tests/e2e_workflow_integration.rs:38:8
   |
30 | impl SharedMockExecutor {
   | ----------------------- method in this implementation
...
38 |     fn call_count(&self, step_id: &str) -> usize {
   |        ^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `std::path::PathBuf`
 --> tests/hello_world_workflow_integration.rs:6:5
  |
6 | use std::path::PathBuf;
  |     ^^^^^^^^^^^^^^^^^^
  |
  = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused imports: `NoOpExecutor` and `StepContext`
 --> tests/hello_world_workflow_integration.rs:8:59
  |
8 | use luther_workflow::engine::executor::{ExecutorRegistry, NoOpExecutor, StepContext};
  |                                                           ^^^^^^^^^^^^  ^^^^^^^^^^^

warning: unused import: `luther_workflow::engine::transition::StepOutcome`
  --> tests/hello_world_workflow_integration.rs:13:5
   |
13 | use luther_workflow::engine::transition::StepOutcome;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

warning: variable does not need to be mutable
  --> tests/cli_e2e_integration.rs:11:9
   |
11 |     let mut cmd = Command::new(env!("CARGO_BIN_EXE_luther-workflow"));
   |         ----^^^
   |         |
   |         help: remove this `mut`
   |
   = note: `#[warn(unused_mut)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "config_binding_integration") generated 1 warning (run `cargo fix --test "config_binding_integration" -p luther-workflow` to apply 1 suggestion)
warning: `luther-workflow` (test "per_edge_loop_tests") generated 1 warning (run `cargo fix --test "per_edge_loop_tests" -p luther-workflow` to apply 1 suggestion)
warning: `luther-workflow` (test "e2e_workflow_integration") generated 1 warning
warning: `luther-workflow` (test "hello_world_workflow_integration") generated 3 warnings (run `cargo fix --test "hello_world_workflow_integration" -p luther-workflow` to apply 3 suggestions)
warning: `luther-workflow` (test "cli_e2e_integration") generated 1 warning (run `cargo fix --test "cli_e2e_integration" -p luther-workflow` to apply 1 suggestion)
warning: unused import: `EngineError`
 --> tests/engine_execution_integration.rs:9:39
  |
9 | use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
  |                                       ^^^^^^^^^^^
  |
  = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused imports: `resolve_workflow_config` and `resolve_workflow_type`
  --> tests/engine_execution_integration.rs:11:48
   |
11 | use luther_workflow::workflow::config_loader::{resolve_workflow_config, resolve_workflow_type};
   |                                                ^^^^^^^^^^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^^^^^^^^

warning: unused import: `std::collections::HashMap`
  --> tests/engine_execution_integration.rs:15:5
   |
15 | use std::collections::HashMap;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unused import: `std::path::PathBuf`
  --> tests/engine_execution_integration.rs:16:5
   |
16 | use std::path::PathBuf;
   |     ^^^^^^^^^^^^^^^^^^

warning: unused variable: `initial_loop_count`
   --> tests/engine_execution_integration.rs:323:9
    |
323 |     let initial_loop_count = 0;
    |         ^^^^^^^^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_initial_loop_count`
    |
    = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `EngineError`
  --> tests/engine_resume_integration.rs:11:39
   |
11 | use luther_workflow::engine::runner::{EngineError, EngineRunner, RunOutcome};
   |                                       ^^^^^^^^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `luther_workflow::engine::transition::StepOutcome`
  --> tests/engine_resume_integration.rs:12:5
   |
12 | use luther_workflow::engine::transition::StepOutcome;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unused import: `luther_workflow::persistence::checkpoint::PersistenceError`
  --> tests/engine_resume_integration.rs:13:5
   |
13 | use luther_workflow::persistence::checkpoint::PersistenceError;
   |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unused import: `std::fs`
   --> src/adapters/git.rs:334:9
    |
334 |     use std::fs;
    |         ^^^^^^^
    |
    = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `tempfile::TempDir`
   --> src/adapters/git.rs:335:9
    |
335 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused import: `tempfile::TempDir`
   --> src/monitor/heartbeat.rs:300:9
    |
300 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused import: `tempfile::TempDir`
   --> src/monitor/ipc.rs:283:9
    |
283 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused import: `std::path::PathBuf`
   --> src/service/launchd.rs:259:9
    |
259 |     use std::path::PathBuf;
    |         ^^^^^^^^^^^^^^^^^^

warning: unused import: `std::path::PathBuf`
   --> src/service/systemd.rs:325:9
    |
325 |     use std::path::PathBuf;
    |         ^^^^^^^^^^^^^^^^^^

warning: unused variable: `workspace`
   --> src/repo/mod.rs:213:13
    |
213 |         let workspace = Workspace::prepare(&config, "/tmp/test-repo").await;
    |             ^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_workspace`
    |
    = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: variable does not need to be mutable
   --> src/repo/mod.rs:227:13
    |
227 |         let mut manager = BranchManager::new(&config);
    |             ----^^^^^^^
    |             |
    |             help: remove this `mut`
    |
    = note: `#[warn(unused_mut)]` (part of `#[warn(unused)]`) on by default

warning: unused variable: `manager`
   --> src/repo/mod.rs:227:13
    |
227 |         let mut manager = BranchManager::new(&config);
    |             ^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_manager`

warning: unused variable: `params`
   --> src/repo/mod.rs:230:13
    |
230 |         let params = BranchParams {
    |             ^^^^^^ help: if this is intentional, prefix it with an underscore: `_params`

warning: function `setup_test_dir` is never used
   --> src/monitor/heartbeat.rs:302:8
    |
302 |     fn setup_test_dir() {
    |        ^^^^^^^^^^^^^^

warning: function `get_assignee` is never used
  --> tests/live_workflow_integration.rs:51:4
   |
51 | fn get_assignee() -> String {
   |    ^^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `RunMetadata`
  --> tests/persistence_integration.rs:14:74
   |
14 |     load_checkpoint, run_metadata_from_ref, save_checkpoint, Checkpoint, RunMetadata, SqliteStore,
   |                                                                          ^^^^^^^^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: `luther-workflow` (test "engine_execution_integration") generated 5 warnings (run `cargo fix --test "engine_execution_integration" -p luther-workflow` to apply 5 suggestions)
warning: `luther-workflow` (test "engine_resume_integration") generated 3 warnings (run `cargo fix --test "engine_resume_integration" -p luther-workflow` to apply 3 suggestions)
warning: `luther-workflow` (lib test) generated 23 warnings (12 duplicates) (run `cargo fix --lib -p luther-workflow --tests` to apply 10 suggestions)
warning: `luther-workflow` (test "live_workflow_integration") generated 1 warning
warning: `luther-workflow` (test "persistence_integration") generated 1 warning (run `cargo fix --test "persistence_integration" -p luther-workflow` to apply 1 suggestion)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.06s
exit=0
```

## Library tests

```text
$ cargo test --lib
warning: unused import: `std::fs`
   --> src/adapters/git.rs:334:9
    |
334 |     use std::fs;
    |         ^^^^^^^
    |
    = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `tempfile::TempDir`
   --> src/adapters/git.rs:335:9
    |
335 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused doc comment
   --> src/engine/runner.rs:125:9
    |
125 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
126 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
127 | /         for (key, value) in &instance.config.variables {
128 | |             context.set(key, value);
129 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment
    = note: `#[warn(unused_doc_comments)]` (part of `#[warn(unused)]`) on by default

warning: unused doc comment
   --> src/engine/runner.rs:132:9
    |
132 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
133 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
134 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
135 | |             let path = std::path::PathBuf::from(work_dir_str);
136 | |             std::fs::create_dir_all(&path).map_err(|e| {
137 | |                 EngineError::InvalidState(format!(
...   |
141 | |             context.set_work_dir(path);
142 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:201:9
    |
201 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
202 | |         /// @requirement:REQ-LF-PROF-003
    | |________________________________________^
203 | /         for (key, value) in &instance.config.variables {
204 | |             context.set(key, value);
205 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:208:9
    |
208 | /         /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
209 | |         /// @requirement:REQ-LF-WS-001
    | |______________________________________^
210 | /         if let Some(work_dir_str) = instance.config.variables.get("work_dir") {
211 | |             let path = std::path::PathBuf::from(work_dir_str);
212 | |             std::fs::create_dir_all(&path).map_err(|e| {
213 | |                 EngineError::InvalidState(format!(
...   |
218 | |             context.set_work_dir(path);
219 | |         }
    | |_________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:302:13
    |
302 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
303 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
304 | /             if outcome == StepOutcome::Abandon {
305 | |                 let run_outcome = RunOutcome::Abandoned {
306 | |                     step_id: current_step_id.clone(),
307 | |                     reason: "Loop limit exceeded".to_string(),
...   |
310 | |                 return Ok(run_outcome);
311 | |             }
    | |_____________- rustdoc does not generate documentation for expressions
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:314:13
    |
314 | /             /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
315 | |             /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________^
316 |               let next_step = self.resolve_next_step(&current_step_id, &outcome)?;
    |               -------------------------------------------------------------------- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused doc comment
   --> src/engine/runner.rs:350:21
    |
350 | /                     /// @plan:PLAN-20260408-LLXPRT-FIRST.P15
351 | |                     /// @requirement:REQ-LF-FAIL-001
    | |____________________________________________________^
352 | /                     let run_outcome = match outcome {
353 | |                         StepOutcome::Success => RunOutcome::Success,
354 | |                         StepOutcome::Fatal => RunOutcome::Failure {
355 | |                             step_id: current_step_id.clone(),
...   |
365 | |                         },
366 | |                     };
    | |______________________- rustdoc does not generate documentation for statements
    |
    = help: use `//` for a plain comment

warning: unused import: `tempfile::TempDir`
   --> src/monitor/heartbeat.rs:300:9
    |
300 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused import: `tempfile::TempDir`
   --> src/monitor/ipc.rs:283:9
    |
283 |     use tempfile::TempDir;
    |         ^^^^^^^^^^^^^^^^^

warning: unused import: `std::path::PathBuf`
   --> src/service/launchd.rs:259:9
    |
259 |     use std::path::PathBuf;
    |         ^^^^^^^^^^^^^^^^^^

warning: unused import: `std::path::PathBuf`
   --> src/service/systemd.rs:325:9
    |
325 |     use std::path::PathBuf;
    |         ^^^^^^^^^^^^^^^^^^

warning: unused variable: `workspace`
   --> src/repo/mod.rs:213:13
    |
213 |         let workspace = Workspace::prepare(&config, "/tmp/test-repo").await;
    |             ^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_workspace`
    |
    = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default

warning: variable does not need to be mutable
   --> src/repo/mod.rs:227:13
    |
227 |         let mut manager = BranchManager::new(&config);
    |             ----^^^^^^^
    |             |
    |             help: remove this `mut`
    |
    = note: `#[warn(unused_mut)]` (part of `#[warn(unused)]`) on by default

warning: unused variable: `manager`
   --> src/repo/mod.rs:227:13
    |
227 |         let mut manager = BranchManager::new(&config);
    |             ^^^^^^^^^^^ help: if this is intentional, prefix it with an underscore: `_manager`

warning: unused variable: `params`
   --> src/repo/mod.rs:230:13
    |
230 |         let params = BranchParams {
    |             ^^^^^^ help: if this is intentional, prefix it with an underscore: `_params`

warning: field `max_retries` is never read
  --> src/engine/runner.rs:83:5
   |
75 | pub struct EngineRunner {
   |            ------------ field in this struct
...
83 |     max_retries: u32,
   |     ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: field `config` is never read
  --> src/monitor/mod.rs:42:5
   |
41 | pub struct Monitor {
   |            ------- field in this struct
42 |     config: process::MonitorConfig,
   |     ^^^^^^

warning: field `path` is never read
   --> src/monitor/mod.rs:158:5
    |
157 | pub struct SingletonLock {
    |            ------------- field in this struct
158 |     path: String,
    |     ^^^^
    |
    = note: `SingletonLock` has a derived impl for the trait `Debug`, but this is intentionally ignored during dead code analysis

warning: function `setup_test_dir` is never used
   --> src/monitor/heartbeat.rs:302:8
    |
302 |     fn setup_test_dir() {
    |        ^^^^^^^^^^^^^^

warning: field `created_branches` is never read
   --> src/repo/mod.rs:138:5
    |
136 | pub struct BranchManager<'a> {
    |            ------------- field in this struct
137 |     config: &'a RepositoryConfig,
138 |     created_branches: Vec<String>,
    |     ^^^^^^^^^^^^^^^^

warning: field `socket_path` is never read
   --> src/service/mod.rs:207:5
    |
206 | pub struct IpcClient {
    |            --------- field in this struct
207 |     socket_path: String,
    |     ^^^^^^^^^^^

warning: `luther-workflow` (lib test) generated 23 warnings (run `cargo fix --lib -p luther-workflow --tests` to apply 10 suggestions)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.06s
     Running unittests src/lib.rs (target/debug/deps/luther_workflow-cec7d813c7133e38)

running 67 tests
test adapters::git::tests::resolve_workspace_path_with_template ... ok
test adapters::git::tests::resolve_workspace_path_shared_strategy ... ok
test adapters::git::tests::resolve_workspace_path_per_run_strategy ... ok
test cli::tests::run_args_parsing ... ok
test cli::tests::service_args_parsing ... ok
test cli::tests::status_args_parsing ... ok
test engine::dagrs_runtime::tests::dagrs_runtime_can_be_created ... ok
test engine::dagrs_runtime::tests::dagrs_runtime_execute_step_succeeds ... ok
test engine::dagrs_runtime::tests::dagrs_runtime_load_workflow_succeeds ... ok
test engine::dagrs_runtime::tests::dagrs_runtime_resume_run_succeeds ... ok
test engine::runner::tests::engine_error_display_formats_correctly ... ok
test engine::dagrs_runtime::tests::dagrs_runtime_start_run_returns_id ... ok
test engine::runner::tests::run_outcome_variants_exist ... ok
test engine::transition::tests::step_outcome_display_formats_correctly ... ok
test engine::transition::tests::step_outcome_variants_exist ... ok
test engine::transition::tests::transition_can_be_created ... ok
test cli::tests::cli_parses_without_error ... ok
test engine::transition::tests::transition_def_can_be_created ... ok
test monitor::heartbeat::tests::test_heartbeat_creation ... ok
test monitor::heartbeat::tests::test_get_heartbeat_path ... ok
test monitor::heartbeat::tests::test_heartbeat_serialization ... ok
test monitor::ipc::tests::test_ipc_endpoint_creation ... ok
test monitor::ipc::tests::test_ipc_request_serialization ... ok
test monitor::ipc::tests::test_ipc_response_serialization ... ok
test monitor::ipc::tests::test_shared_state ... ok
test monitor::process::tests::test_calculate_backoff_exponential ... ok
test monitor::process::tests::test_calculate_backoff_fixed ... ok
test monitor::process::tests::test_process_state_restart_tracking ... ok
test monitor::tests::test_restart_policy_exponential_backoff ... ok
test monitor::tests::test_select_profile ... ok
test monitor::tests::test_restart_tracker ... ok
test persistence::artifacts::tests::artifact_record_can_be_created ... ok
test persistence::artifacts::tests::artifact_record_with_size ... ok
test persistence::artifacts::tests::default_artifacts_root_returns_path ... ok
test persistence::checkpoint::tests::checkpoint_can_be_created ... ok
test persistence::artifacts::tests::get_artifacts_dir_is_deterministic ... ok
test persistence::checkpoint::tests::checkpoint_mark_interrupted ... ok
test persistence::checkpoint::tests::checkpoint_with_snapshot ... ok
test persistence::checkpoint::tests::persistence_error_variants_exist ... ok
test repo::tests::test_branch_manager_branch_name_generation ... ok
test runtime_paths::tests::get_artifacts_root_returns_valid_path ... ok
test runtime_paths::tests::get_data_dir_returns_valid_path ... ok
test runtime_paths::tests::get_config_dir_returns_valid_path ... ok
test runtime_paths::tests::get_run_dir_includes_run_id ... ok
test repo::tests::test_shared_workspace_returns_same_path ... ok
test service::launchd::tests::test_get_launch_agents_dir ... ok
test repo::tests::test_repository_config_from_toml ... ok
test service::launchd::tests::test_is_service_installed_check ... ok
test service::launchd::tests::test_get_plist_path ... ok
test service::spec::tests::test_generate_launchd_plist ... ok
test service::spec::tests::test_unit_file_name ... ok
test service::spec::tests::test_plist_file_name ... ok
test service::spec::tests::test_generate_systemd_unit ... ok
test service::systemd::tests::test_get_systemd_user_dir ... ok
test service::spec::tests::test_service_spec_builder ... ok
test service::systemd::tests::test_get_unit_path ... ok
test service::tests::test_failure_type_variants ... ok
test service::tests::test_service_config ... ok
test service::systemd::tests::test_is_service_installed_check ... ok
test service::tests::test_service_error_diagnostics ... ok
test tests::exposes_project_name ... ok
test engine::runner::tests::engine_runner_can_be_created ... ok
test persistence::checkpoint::tests::save_and_load_events ... ok
test persistence::checkpoint::tests::save_and_load_checkpoint ... ok
test persistence::checkpoint::tests::checkpoint_preserves_counters ... ok
test service::systemd::tests::test_is_systemd_available ... ok
test monitor::process::tests::test_singleton_lock_acquire_and_release ... ok

test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
exit=0
```

## Negative placeholder grep

```text
$ grep -R -n -E '@pseudocode placeholder|@pseudocode TBD|TODO API|json_path TBD|fixture TBD|assertion TBD|todo!|unimplemented!' tests/e2e_workflow_integration.rs tests/pr_followup_workflow_integration.rs tests/pr_followup_marker_audit_tests.rs tests/smoke_test.rs project-plans/coderabbit/analysis config/workflows/llxprt-issue-fix-v1.toml
exit=1
```

VERDICT: PASS
