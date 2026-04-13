# context-interpolation.md

Pseudocode for StepContext and string interpolation.

---

```
1  // ============================================================================
2  // StepContext Struct Definition
3  // ============================================================================

4  /// Context for step execution.
5  /// Stores key-value pairs for variable interpolation across steps.
6  struct StepContext {
7      /// Storage for context values: key -> value
8      values: HashMap<String, String>,

9      /// Built-in variables that are always available
10     built_ins: HashMap<String, String>,
11 }

12 // ============================================================================
13 // StepContext Implementation
14 // ============================================================================

15 impl StepContext {
16     /// Create a new empty StepContext.
17     fn new() -> Self {
18         let mut context = StepContext {
19             values: HashMap::new(),
20             built_ins: HashMap::new(),
21         };

22         // Initialize built-in variables (line 22)
23         context.initialize_built_ins();

24         return context;
25     }

26     /// Initialize built-in variables.
27     /// Called automatically during construction.
28     fn initialize_built_ins(&mut self) {
29         // {work_dir} - Current working directory (line 29)
30         let work_dir = std::env::current_dir()
31             .map(|p| p.to_string_lossy().to_string())
32             .unwrap_or_else(|_| "/".to_string());
33         self.built_ins.insert("work_dir".to_string(), work_dir);

34         // {run_id} - Will be set when workflow starts (line 34)
35         self.built_ins.insert("run_id".to_string(), String::new());

36         // {config_id} - Will be set from WorkflowInstance (line 36)
37         self.built_ins.insert("config_id".to_string(), String::new());

38         // {workflow_type_id} - Will be set from WorkflowType (line 38)
39         self.built_ins.insert("workflow_type_id".to_string(), String::new());
40     }

41     /// Set a context value.
42     /// 
43     /// # Arguments
44     /// * `key` - The variable name (without braces)
45     /// * `value` - The value to store
46     fn set(&mut self, key: &str, value: &str) {
47         self.values.insert(key.to_string(), value.to_string());
48     }

49     /// Get a context value.
50     /// Checks built-ins first, then user-set values.
51     /// 
52     /// # Arguments
53     /// * `key` - The variable name (without braces)
54     /// 
55     /// # Returns
56     /// * `Some(String)` - The value if found
57     /// * `None` - If key not defined
58     fn get(&self, key: &str) -> Option<String> {
59         // Check built-ins first (line 59)
60         if let Some(val) = self.built_ins.get(key) {
61             return Some(val.clone());
62         }

63         // Fall back to user-set values (line 63)
64         if let Some(val) = self.values.get(key) {
65             return Some(val.clone());
66         }

67         // Key not found (line 67)
68         return None;
69     }

70     /// Set built-in variable value (used by engine runner).
71     fn set_builtin(&mut self, key: &str, value: &str) {
72         self.built_ins.insert(key.to_string(), value.to_string());
73     }
74 }

75 // ============================================================================
76 // interpolate_string Function
77 // ============================================================================

78 /// Interpolate {key} placeholders in a template string.
79 /// 
80 /// Replaces all occurrences of {key} with the corresponding value from context.
81 /// Undefined keys are left as-is (no error, no replacement).
82 /// No nested/recursive resolution - only one pass.
83 /// 
84 /// # Arguments
85 /// * `template` - The string template with {key} placeholders
86 /// * `context` - The StepContext containing values
87 /// 
88 /// # Returns
89 /// * String with placeholders replaced by values
90 fn interpolate_string(template: &str, context: &StepContext) -> String {
91     // Initialize result with template (line 91)
92     let mut result = template.to_string();

93     // Collect all keys from context (line 93)
94     // Built-ins + user values
95     let mut all_keys: Vec<String> = Vec::new();

96     // Add built-in keys (line 96)
97     for key in context.built_ins.keys() {
98         all_keys.push(key.clone());
99     }

100     // Add user-defined keys (line 100)
101     for key in context.values.keys() {
102         all_keys.push(key.clone());
103     }

104     // Sort keys by length descending (line 104)
105     // This prevents partial replacements (e.g., "{foo}" vs "{foobar}")
106     all_keys.sort_by(|a, b| b.len().cmp(&a.len()));

107     // Iterate over all keys and replace (line 107)
108     for key in all_keys {
109         // Construct placeholder pattern (line 109)
110         let placeholder = format!("{{{}}}", key);

111         // Get the value for this key (line 111)
112         if let Some(value) = context.get(&key) {
113             // Replace all occurrences (line 113)
114             result = result.replace(&placeholder, &value);
115         }
116         // If key not found, leave placeholder as-is (line 116)
117     }

118     // Return interpolated result (line 118)
119     return result;
120 }

121 // ============================================================================
122 // Example Usage
123 // ============================================================================

124 // Example: Interpolating a command string
125 //
126 // let template = "cat {previous_step.stdout} | grep {search_term} > {output_file}";
127 // context.set("previous_step.stdout", "/tmp/out.txt");
128 // context.set("search_term", "ERROR");
129 // context.set("output_file", "errors.log");
130 //
131 // let result = interpolate_string(template, &context);
132 // // result: "cat /tmp/out.txt | grep ERROR > errors.log"

133 // Example: Undefined keys left as-is
134 //
135 // let template = "echo {defined} and {undefined}";
136 // context.set("defined", "hello");
137 //
138 // let result = interpolate_string(template, &context);
139 // // result: "echo hello and {undefined}"

140 // Example: Built-in variables
141 //
142 // let template = "cd {work_dir} && ./run.sh {config_id}";
143 // // Assuming work_dir = "/home/user/project", config_id = "prod-v1"
144 //
145 // let result = interpolate_string(template, &context);
146 // // result: "cd /home/user/project && ./run.sh prod-v1"

147 // Example: Step output access convention
148 //
149 // let template = "Processing: {step1.stdout}";
150 // // ShellExecutor stores: context.set("step1.stdout", "output content");
151 //
152 // let result = interpolate_string(template, &context);
153 // // result: "Processing: output content"
```

---

## Coverage

| Requirement | Lines |
|-------------|-------|
| StepContext struct with HashMap storage | 6-11 |
| Built-in variables: {work_dir} | 29-33 |
| Built-in variables: {run_id} | 34-35 |
| Built-in variables: {config_id} | 36-37 |
| Built-in variables: {workflow_type_id} | 38-39 |
| interpolate_string function | 90-120 |
| Simple {key} replacement | 109-115 |
| Iterate over context HashMap | 93-103 |
| Undefined keys left as-is | 116 |
| No nested/recursive resolution | 107 (single pass) |
| context.set() for storing values | 46-48 |
| context.get() for retrieving values | 58-69 |
| {step_id.stdout} convention support | 100-102, 149-153 |

## Reference

- Plan: PLAN-20260408-STEP-EXEC.P02
- Domain Model: analysis/domain-model.md
