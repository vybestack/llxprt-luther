# executor-dispatch.md

Pseudocode for StepExecutor trait, ExecutorRegistry, and dispatch logic.

---

```
1  // ============================================================================
2  // StepExecutor Trait Definition
3  // ============================================================================
4
5  /// Trait for step executors. Each step type has a concrete implementation.
6  trait StepExecutor {
7      /// Execute the step and return an outcome.
8      /// 
9      /// # Arguments
10     /// * `step_def` - The step definition with parameters
11     /// * `context` - Mutable context for value storage and interpolation
12     /// 
13     /// # Returns
14     /// * `Ok(StepOutcome)` - Success, Fixable, or other terminal outcome
15     /// * `Err(ExecutionError)` - Fatal execution error
16     fn execute(
17         &self,
18         step_def: &StepDef,
19         context: &mut StepContext,
20     ) -> Result<StepOutcome, ExecutionError>;
21 }
22
23 // ============================================================================
24 // ExecutorRegistry Struct Definition
25 // ============================================================================

26 /// Registry mapping step_type strings to executor implementations.
27 struct ExecutorRegistry {
28     /// Map of step_type -> boxed executor trait object
29     executors: HashMap<String, Box<dyn StepExecutor>>,
30 }
31
32 // ============================================================================
33 // ExecutorRegistry Implementation
34 // ============================================================================

35 impl ExecutorRegistry {
36     /// Create a new empty registry.
37     fn new() -> Self {
38         return ExecutorRegistry {
39             executors: HashMap::new(),
40         };
41     }
42
43     /// Register an executor for a step type.
44     /// 
45     /// # Arguments
46     /// * `step_type` - The step type string (e.g., "shell", "write_file")
47     /// * `executor` - Boxed trait object implementing StepExecutor
48     fn register(&mut self, step_type: &str, executor: Box<dyn StepExecutor>) {
49         self.executors.insert(step_type.to_string(), executor);
50     }

51     /// Dispatch execution to the appropriate executor.
52     /// 
53     /// # Arguments
54     /// * `step_type` - The step type to look up
55     /// * `step_def` - The full step definition
56     /// * `context` - Mutable execution context
57     /// 
58     /// # Returns
59     /// * `Ok(StepOutcome)` - Outcome from the executor
60     /// * `Err(ExecutionError::UnregisteredStepType)` - No executor registered
61     fn dispatch(
62         &self,
63         step_type: &str,
64         step_def: &StepDef,
65         context: &mut StepContext,
66     ) -> Result<StepOutcome, ExecutionError> {
67         // Look up executor by step_type (line 67)
68         let executor = self.executors.get(step_type);
69
70         // Handle unregistered step type (line 70)
71         if executor.is_none() {
72             return Err(ExecutionError::UnregisteredStepType(step_type.to_string()));
73         }

74         // Get reference to the executor (line 74)
75         let exec = executor.unwrap();
76
77         // Delegate to the executor (line 77)
78         let outcome = exec.execute(step_def, context)?;
79
80         // Return the outcome (line 80)
81         return Ok(outcome);
82     }

83     /// Create registry with built-in default executors.
84     fn with_defaults() -> Self {
85         let mut registry = ExecutorRegistry::new();
86
87         // Register shell executor (line 87)
88         registry.register("shell", Box::new(ShellExecutor));
89
90         // Register write_file executor (line 90)
91         registry.register("write_file", Box::new(WriteFileExecutor));

92         // Register noop executor for testing (line 92)
93         registry.register("noop", Box::new(NoOpExecutor));

94         return registry;
95     }
96 }

97 // ============================================================================
98 // ExecutionError Enum Definition
99 // ============================================================================

100 /// Errors that can occur during step execution.
101 enum ExecutionError {
102     /// Step type has no registered executor. Fatal.
103     UnregisteredStepType(String),
104
105     /// IO error during execution. Fatal.
106     IoError(String),
107
108     /// Command spawn failed. Fatal.
109     CommandSpawnFailed(String),
110
111     /// Invalid parameters provided to executor. Fatal.
112     InvalidParameters(String),
113 }

114 // ============================================================================
115 // NoOpExecutor (for testing)
116 // ============================================================================

117 /// No-op executor that always returns Success.
118 /// Used for tests that don't need real execution.
119 struct NoOpExecutor;
120
121 impl StepExecutor for NoOpExecutor {
122     fn execute(
123         &self,
124         _step_def: &StepDef,
125         _context: &mut StepContext,
126     ) -> Result<StepOutcome, ExecutionError> {
127         // Always return Success (line 127)
128         return Ok(StepOutcome::Success);
129     }
130 }
```

---

## Coverage

| Requirement | Lines |
|-------------|-------|
| StepExecutor trait with execute(context, params) | 16-21 |
| ExecutorRegistry with HashMap storage | 27-30 |
| Register executors by step_type | 48-50 |
| Dispatch: look up step_type | 67-68 |
| Dispatch: call executor | 77-78 |
| Dispatch: return outcome | 80-81 |
| Unregistered type → Fatal (ExecutionError) | 71-73, 103 |
| with_defaults() factory | 84-95 |

## Reference

- Plan: PLAN-20260408-STEP-EXEC.P02
- Domain Model: analysis/domain-model.md
