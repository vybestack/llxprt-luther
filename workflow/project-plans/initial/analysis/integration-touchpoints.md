# Integration Touchpoints

## Existing Repository Touchpoints

- `src/main.rs`: current bootstrap startup path to replace with CLI/monitor entrypoint.
- `src/lib.rs`: module export surface that should expose runtime layers.
- `Cargo.toml`: dependency and lint/test gates that must stay aligned with quality/release controls.

## Runtime Integration Paths

1. CLI -> monitor commands (`run`, `status`, service operations)
2. Monitor -> engine start/restart/shutdown supervision
3. Engine -> workflow type/config loader
4. Engine -> router/guards/checkpoint persistence
5. Engine -> repository preparation before workflow execution
6. Monitor/service -> IPC status/control surfaces

## Replacement/Deprecation Targets

- Replace bootstrap `println!` only startup behavior with command-driven runtime startup.
- Replace implicit in-memory assumptions with persisted run metadata/checkpoint model.

## User-Reachable Access Paths

- Local CLI commands for run/status/service control
- Monitor heartbeat/state exposed for operational introspection
