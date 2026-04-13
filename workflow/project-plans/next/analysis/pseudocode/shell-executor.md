# shell-executor.md

Pseudocode for ShellExecutor implementation.

---

```
1  // ============================================================================
2  // ShellExecutor Struct Definition
3  // ============================================================================

4  /// Executor for shell command steps.
5  /// Runs commands via `sh -c` with variable interpolation.
6  struct ShellExecutor;

7  // ============================================================================
8  // ShellExecutor Implementation
9  // ============================================================================

10 impl StepExecutor for ShellExecutor {
11     fn execute(
12         &self,
13         step_def: &StepDef,
14         context: &mut StepContext,
15     ) -> Result<StepOutcome, ExecutionError> {
16         // -------------------------------------------------------------------------
17         // 1. Parameter Extraction (lines 17-42)
18         // -------------------------------------------------------------------------

19         // Get parameters as JSON object (line 19)
20         let params = step_def.parameters.as_ref();

21         // Check parameters exist (line 21)
22         if params.is_none() {
23             return Err(ExecutionError::InvalidParameters(
24                 "Missing parameters object".to_string()
25             ));
26         }
27         let params = params.unwrap();

28         // Extract "command" parameter (line 28)
29         let command_param = params.get("command");
30         if command_param.is_none() {
31             return Err(ExecutionError::InvalidParameters(
32                 "Missing 'command' parameter".to_string()
33             ));
34         }
35         let command_template = command_param.unwrap().as_str();
36         if command_template.is_none() {
37             return Err(ExecutionError::InvalidParameters(
38                 "'command' must be a string".to_string()
39             ));
40         }
41         let command_template = command_template.unwrap();

42         // Extract optional "working_dir" parameter (line 42)
43         let working_dir = params.get("working_dir");
44         let working_dir = working_dir.and_then(|v| v.as_str());

45         // Extract optional "timeout_seconds" parameter (line 45)
46         let timeout_param = params.get("timeout_seconds");
47         let timeout_seconds = timeout_param.and_then(|v| v.as_u64());

48         // -------------------------------------------------------------------------
49         // 2. Variable Interpolation (lines 49-53)
50         // -------------------------------------------------------------------------

51         // Interpolate {key} placeholders in command string (line 51)
52         let interpolated_command = interpolate_string(command_template, context);

53         // -------------------------------------------------------------------------
54         // 3. Command Setup (lines 54-70)
55         // -------------------------------------------------------------------------

56         // Create std::process::Command with `sh -c` (line 56)
57         let mut cmd = std::process::Command::new("sh");
58         cmd.arg("-c");
59         cmd.arg(&interpolated_command);

60         // Set working directory if provided (line 60)
61         if working_dir.is_some() {
62             let interpolated_wd = interpolate_string(working_dir.unwrap(), context);
63             cmd.current_dir(&interpolated_wd);
64         }

65         // Configure stdout/stderr capture (line 65)
66         cmd.stdout(std::process::Stdio::piped());
67         cmd.stderr(std::process::Stdio::piped());

68         // Configure timeout if provided (line 68)
69         if timeout_seconds.is_some() {
70             // Note: Actual timeout implementation would use async or kill logic
71             // For pseudocode: mark intent to timeout after N seconds
72             cmd.timeout(std::time::Duration::from_secs(timeout_seconds.unwrap()));
73         }

74         // -------------------------------------------------------------------------
75         // 4. Command Execution (lines 75-85)
76         // -------------------------------------------------------------------------

77         // Spawn the child process (line 77)
78         let child_result = cmd.spawn();

79         // Handle spawn failure (line 79)
80         if child_result.is_err() {
81             let err_msg = format!("Failed to spawn command: {}", child_result.unwrap_err());
82             return Err(ExecutionError::CommandSpawnFailed(err_msg));
83         }
84         let mut child = child_result.unwrap();

85         // Wait for completion (line 85)
86         let output_result = child.wait_with_output();

87         // Handle wait/output error (line 87)
88         if output_result.is_err() {
89             let err_msg = format!("Command execution failed: {}", output_result.unwrap_err());
90             return Err(ExecutionError::IoError(err_msg));
91         }
92         let output = output_result.unwrap();

93         // -------------------------------------------------------------------------
94         // 5. Output Capture and Context Storage (lines 94-108)
95         // -------------------------------------------------------------------------

96         // Capture stdout as string (line 96)
97         let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();

98         // Capture stderr as string (line 98)
99         let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();

100         // Store stdout in context under step_id.stdout key (line 100)
101         let stdout_key = format!("{}.stdout", step_def.step_id);
102         context.set(&stdout_key, &stdout_str);

103         // Store stderr in context under step_id.stderr key (line 103)
104         let stderr_key = format!("{}.stderr", step_def.step_id);
105         context.set(&stderr_key, &stderr_str);

106         // Store exit code in context (line 106)
107         let exit_code_key = format!("{}.exit_code", step_def.step_id);
108         context.set(&exit_code_key, &output.status.code().unwrap_or(-1).to_string());

109         // -------------------------------------------------------------------------
110         // 6. Exit Code Mapping to StepOutcome (lines 110-122)
111         // -------------------------------------------------------------------------

112         // Get exit code (line 112)
113         let exit_code = output.status.code();

114         // Map exit code to StepOutcome (line 114)
115         if exit_code == Some(0) {
116             // Exit code 0 -> Success (line 116)
117             return Ok(StepOutcome::Success);
118         } else {
119             // Non-zero exit code -> Fixable (line 119)
120             // The error is recoverable, may trigger remediation
121             return Ok(StepOutcome::Fixable);
122         }
123     }
124 }
```

---

## Coverage

| Requirement | Lines |
|-------------|-------|
| Extract "command" from params | 28-41 |
| Extract optional "working_dir" | 42-44 |
| Extract optional "timeout_seconds" | 45-47 |
| Interpolate variables with {key} | 51-52 |
| Run std::process::Command | 56-73 |
| Spawn failure → Fatal | 77-83 |
| Capture stdout | 96-97, 100-102 |
| Capture stderr | 98-99, 103-105 |
| Store outputs in context | 100-108 |
| Exit code 0 → Success | 115-117 |
| Non-zero → Fixable | 118-122 |

## Reference

- Plan: PLAN-20260408-STEP-EXEC.P02
- Domain Model: analysis/domain-model.md
