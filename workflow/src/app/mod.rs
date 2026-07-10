//! Application-level command dispatch for the `luther-workflow` binary.
//!
//! The binary root (`main.rs`) parses CLI arguments and dispatches into the
//! cohesive command-handler modules declared here. Each submodule owns one
//! command family (run, status, service, daemon, runs, monitor) or a shared
//! support concern (wait-state persistence, config-token interpolation).
use std::path::Path;
use std::process;

use luther_workflow::adapters::github::{run_preflight, GithubError, SystemGithubCommandRunner};
use luther_workflow::adapters::github_issues::SystemGithubIssueQuery;
use luther_workflow::adapters::llxprt::{
    run_preflight as run_llxprt_preflight, LlxprtError, SystemLlxprtCommandRunner,
};
use luther_workflow::daemon::discovery::{discover, DiscoveryResult};
use luther_workflow::daemon::scheduler::{RunSummary, SchedulerTarget};
use luther_workflow::daemon::{
    is_daemon_alive, stop_daemon, DaemonState, DaemonStatus, DaemonStore, StopOutcome,
};
use luther_workflow::engine::executor::ExecutorRegistry;
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::monitor::heartbeat::read_all_heartbeats;
use luther_workflow::monitor::heartbeat::MonitorState;
use luther_workflow::monitor::snapshot::{
    render_snapshot, resolve_snapshot_count, separator_line, DaemonSummary, MonitorFilter,
    MonitorSnapshot, RunCounts, CLEAR_SCREEN,
};
use luther_workflow::persistence::init_database;
use luther_workflow::persistence::leases::{
    list_all_leases, list_leases_by_config, IssueLease, LeaseStatus,
};
use luther_workflow::persistence::{
    get_run_with_conn, get_wait_state, list_artifacts, list_wait_states, load_checkpoint_with_conn,
    load_events, load_recent_events, persist_run_with_conn, upsert_wait_state,
    write_wait_state_artifact, EventRecord, RunMetadata, RunStatus, SqliteStore, WaitKind,
    WaitStateRecord,
};
use luther_workflow::service::{Service, ServiceConfig};
use luther_workflow::workflow::config_loader::{
    load_daemon_scheduler_config, resolve_discovery_config, resolve_workflow,
    resolve_workflow_config, resolve_workflow_type, validate_artifact_dependencies,
    validate_workflow_tokens,
};
use luther_workflow::workflow::schema::{
    StepDef, WorkflowConfig, WorkflowType, DEFAULT_MAX_CHILD_MERGE_WAIT_SECONDS,
};
use serde_json::{Map, Value};

use luther_workflow::workflow::target_profile::{
    apply_target_profile_overrides, target_profile_validation_required, validate_target_profile,
    TargetProfileOverrides,
};

mod config_tokens;
mod daemon;
mod monitor;
mod run;
mod runs;
mod service;
mod status;
mod wait_state;

pub use config_tokens::*;
pub use daemon::*;
pub use monitor::*;
pub use run::*;
pub use runs::*;
pub use service::*;
pub use status::*;
pub use wait_state::*;
