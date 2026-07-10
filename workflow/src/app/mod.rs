//! Application-level command dispatch for the `luther-workflow` binary.
//!
//! The binary root (`main.rs`) parses CLI arguments and dispatches into the
//! cohesive command-handler modules declared here. Each submodule owns one
//! command family (run, status, service, daemon, runs, monitor) or a shared
//! support concern (wait-state persistence, config-token interpolation).
mod config_tokens;
mod daemon;
mod monitor;
mod run;
mod runs;
mod service;
mod status;
mod wait_state;

// Each submodule owns its explicit `use` imports and reaches sibling helpers
// through `super::<module>::<item>` paths. `mod.rs` intentionally re-exports
// only the command entry points that `main.rs` dispatches to, keeping the
// binary's internal API surface narrow.
pub use daemon::handle_daemon_command;
pub use monitor::handle_monitor_command;
pub use run::handle_run_command;
pub use runs::handle_runs_command;
pub use service::handle_service_command;
pub use status::handle_status_command;
