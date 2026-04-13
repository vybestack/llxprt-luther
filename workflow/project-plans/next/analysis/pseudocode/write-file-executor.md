# write-file-executor.md

Pseudocode for WriteFileExecutor implementation.

---

```
1  // ============================================================================
2  // WriteFileExecutor Struct Definition
3  // ============================================================================

4  /// Executor for write_file steps.
5  /// Writes content to a file with variable interpolation.
6  struct WriteFileExecutor;

7  // ============================================================================
8  // WriteFileExecutor Implementation
9  // ============================================================================

10 impl StepExecutor for WriteFileExecutor {
11     fn execute(
12         &self,
13         step_def: &StepDef,
14         context: &mut StepContext,
15     ) -> Result<StepOutcome, ExecutionError> {
16         // -------------------------------------------------------------------------
17         // 1. Parameter Extraction (lines 17-56)
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

28         // Extract "path" parameter (line 28)
29         let path_param = params.get("path");
30         if path_param.is_none() {
31             return Err(ExecutionError::InvalidParameters(
32                 "Missing 'path' parameter".to_string()
33             ));
34         }
35         let path_template = path_param.unwrap().as_str();
36         if path_template.is_none() {
37             return Err(ExecutionError::InvalidParameters(
38                 "'path' must be a string".to_string()
39             ));
40         }
41         let path_template = path_template.unwrap();

42         // Extract "content" parameter (line 42)
43         let content_param = params.get("content");
44         if content_param.is_none() {
45             return Err(ExecutionError::InvalidParameters(
46                 "Missing 'content' parameter".to_string()
47             ));
48         }
49         let content_template = content_param.unwrap().as_str();
50         if content_template.is_none() {
51             return Err(ExecutionError::InvalidParameters(
52                 "'content' must be a string".to_string()
53             ));
54         }
55         let content_template = content_template.unwrap();

56         // Extract optional "mkdir" parameter (line 56)
57         let mkdir_param = params.get("mkdir");
58         let mkdir = mkdir_param.and_then(|v| v.as_bool()).unwrap_or(false);

59         // -------------------------------------------------------------------------
60         // 2. Variable Interpolation (lines 60-66)
61         // -------------------------------------------------------------------------

62         // Interpolate {key} placeholders in path string (line 62)
63         let interpolated_path = interpolate_string(path_template, context);

64         // Interpolate {key} placeholders in content string (line 64)
65         let interpolated_content = interpolate_string(content_template, context);

66         // -------------------------------------------------------------------------
67         // 3. Parent Directory Creation (lines 67-88)
68         // -------------------------------------------------------------------------

69         // Check if mkdir is requested or if we should create parent anyway (line 69)
70         if mkdir {
71             // Get parent directory path (line 71)
72             let parent_path = std::path::Path::new(&interpolated_path).parent();

73             // Check if parent exists (line 73)
74             if parent_path.is_some() {
75                 let parent = parent_path.unwrap();

76                 // Check if parent directory needs creation (line 76)
77                 if !parent.exists() {
78                     // Create parent directories recursively (line 78)
79                     let create_result = std::fs::create_dir_all(parent);

80                     // Handle directory creation error (line 80)
81                     if create_result.is_err() {
82                         let err_msg = format!(
83                             "Failed to create parent directory '{}': {}",
84                             parent.display(),
85                             create_result.unwrap_err()
86                         );
87                         return Err(ExecutionError::IoError(err_msg));
88                     }
89                 }
90             }
91         }

92         // -------------------------------------------------------------------------
93         // 4. File Write Operation (lines 93-112)
94         // -------------------------------------------------------------------------

95         // Create parent directories if they don't exist (best effort) (line 95)
96         let parent_path = std::path::Path::new(&interpolated_path).parent();
97         if parent_path.is_some() {
98             let parent = parent_path.unwrap();
99             if !parent.exists() {
100                 // Create parent directories (line 100)
101                 let _ = std::fs::create_dir_all(parent);
102                 // Ignore errors here - the write will fail if truly inaccessible
103             }
104         }

105         // Write file using std::fs::write (line 105)
106         let write_result = std::fs::write(&interpolated_path, &interpolated_content);

107         // Handle write error (line 107)
108         if write_result.is_err() {
109             let err_msg = format!(
110                 "Failed to write file '{}': {}",
111                 interpolated_path,
112                 write_result.unwrap_err()
113             );
114             return Err(ExecutionError::IoError(err_msg));
115         }

116         // -------------------------------------------------------------------------
117         // 5. Context Storage and Success (lines 117-125)
118         // -------------------------------------------------------------------------

119         // Store written path in context (line 119)
120         let path_key = format!("{}.path", step_def.step_id);
121         context.set(&path_key, &interpolated_path);

122         // Store written content length in context (line 122)
123         let len_key = format!("{}.content_length", step_def.step_id);
124         context.set(&len_key, &interpolated_content.len().to_string());

125         // Return Success outcome (line 125)
126         return Ok(StepOutcome::Success);
127     }
128 }
```

---

## Coverage

| Requirement | Lines |
|-------------|-------|
| Extract "path" from params | 28-41 |
| Extract "content" from params | 42-55 |
| Extract optional "mkdir" | 56-58 |
| Interpolate path string | 62-63 |
| Interpolate content string | 64-65 |
| Create parent directories (if mkdir=true) | 70-91 |
| Write file with std::fs::write | 105-106 |
| IO error → Fatal (ExecutionError::IoError) | 107-115 |
| Return Success on completion | 125-127 |
| Store output values in context | 119-124 |

## Reference

- Plan: PLAN-20260408-STEP-EXEC.P02
- Domain Model: analysis/domain-model.md
