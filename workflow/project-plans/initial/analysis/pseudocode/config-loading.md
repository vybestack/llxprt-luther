# Pseudocode: Config Loading and Binding

1. Receive `workflow_type_id`, `config_id`, and generated `run_id` from CLI/monitor startup.
2. Resolve workflow type path candidates (`.toml` primary, optional `.json` equivalent).
3. Resolve workflow config path candidates (`.toml` primary, optional `.json` equivalent).
4. If required files are missing, return structured startup error.
5. Parse workflow type into typed schema.
6. Parse workflow config into typed schema.
7. Validate workflow topology/transitions/guard references.
8. Validate config references and guard limits.
9. Validate workflow config `workflow_type_id` matches requested workflow type.
10. Build `WorkflowRunRef(workflow_type_id, config_id, run_id)`.
11. Persist initial run metadata containing all identifiers.
12. Return bound runtime context to engine runner.
