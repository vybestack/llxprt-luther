/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// CLI module - command line interface for the workflow runtime.
///
/// This module provides the CLI commands using clap derive macros.
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// CLI arguments for the luther-workflow application.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Parser, Debug)]
#[command(name = "luther-workflow")]
#[command(about = "Luther workflow runtime and supervision system")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Available CLI commands.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start a workflow run
    #[command(name = "run")]
    Run(RunArgs),
    /// Check workflow status
    #[command(name = "status")]
    Status(StatusArgs),
    /// Run as a service/daemon
    #[command(name = "service")]
    Service(ServiceArgs),
    /// Manage per-config daemon instances
    #[command(name = "daemon")]
    Daemon(DaemonArgs),
    /// Inspect workflow runs (list/show/tail/ps)
    #[command(name = "runs")]
    Runs(RunsArgs),
    /// Continuously monitor daemon and run status (plain CLI, non-TUI)
    #[command(name = "monitor")]
    Monitor(MonitorArgs),
}

/// Arguments for the run command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// @plan:PLAN-20260408-LLXPRT-FIRST.P20
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to config file
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Perform a dry run without executing
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the GitHub `gh` readiness preflight gate (offline/CI fixtures)
    #[arg(long)]
    pub skip_preflight: bool,
    /// Workflow type ID
    #[arg(short, long, value_name = "ID")]
    pub workflow_type: Option<String>,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Stable run id to use for durable checkpoints/resume
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
    /// Target repository in OWNER/NAME form
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,
    /// Target issue number
    #[arg(long, value_name = "NUMBER")]
    pub issue: Option<String>,
    /// Target checkout/workspace directory
    #[arg(long, value_name = "PATH")]
    pub work_dir: Option<PathBuf>,
    /// Target artifact directory
    #[arg(long, value_name = "PATH")]
    pub artifact_dir: Option<PathBuf>,
}

/// Arguments for the status command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
    /// Run ID to check status for
    #[arg(short, long, value_name = "ID")]
    pub run_id: Option<String>,
    /// Filter heartbeats and runs to a single config id (file stem)
    #[arg(long, value_name = "ID")]
    pub config: Option<String>,
}

/// Arguments for the service command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct ServiceArgs {
    /// Service lifecycle subcommand
    #[command(subcommand)]
    pub command: ServiceCommand,
}

/// Service lifecycle subcommands.
///
/// `run` executes the foreground process supervised by launchd/systemd; the
/// remaining subcommands manage the OS-level service (install/start/stop/
/// status/uninstall) so "daemon mode" is delivered through the platform
/// supervisor rather than self-forking (REQ-EARS-SVC-001).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Subcommand, Debug)]
pub enum ServiceCommand {
    /// Run the service process (foreground, OS-supervised)
    #[command(name = "run")]
    Run(ServiceRunArgs),
    /// Install the platform service (launchd/systemd)
    #[command(name = "install")]
    Install(ServiceInstallArgs),
    /// Start the installed service
    #[command(name = "start")]
    Start,
    /// Stop the running service
    #[command(name = "stop")]
    Stop,
    /// Show the service status
    #[command(name = "status")]
    Status(ServiceStatusArgs),
    /// Uninstall the platform service
    #[command(name = "uninstall")]
    Uninstall,
}

/// Arguments for `service run`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct ServiceRunArgs {
    /// Run in foreground mode
    #[arg(long)]
    pub foreground: bool,
    /// IPC socket path
    #[arg(long, value_name = "PATH")]
    pub socket_path: Option<PathBuf>,
}

/// Arguments for `service install`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct ServiceInstallArgs {
    /// Binary to launch (defaults to the current executable)
    #[arg(long, value_name = "PATH")]
    pub binary: Option<PathBuf>,
    /// Optional config file passed to the supervised process
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

/// Arguments for `service status`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct ServiceStatusArgs {
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the daemon command.
///
/// The `daemon` family supervises one foreground daemon instance per workflow
/// config while allowing aggregate views across configs (issue #48).
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
/// @requirement:REQ-EARS-SVC-001
#[derive(Args, Debug)]
pub struct DaemonArgs {
    /// Daemon lifecycle subcommand
    #[command(subcommand)]
    pub command: DaemonCommand,
}

/// Daemon lifecycle subcommands.
///
/// `start` and `run` both execute in the foreground (no self-daemonization,
/// REQ-EARS-SVC-001); `stop` and `status` operate on persisted per-config
/// state so multiple configs can be supervised and aggregated.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Start a foreground daemon for a config
    #[command(name = "start")]
    Start(DaemonStartArgs),
    /// Run a foreground daemon for a config
    #[command(name = "run")]
    Run(DaemonRunArgs),
    /// Stop a running daemon (single config or all)
    #[command(name = "stop")]
    Stop(DaemonStopArgs),
    /// Show daemon status (single config or aggregate)
    #[command(name = "status")]
    Status(DaemonStatusArgs),
    /// Dry-run discovery of eligible issues for a config
    #[command(name = "discover")]
    Discover(DaemonDiscoverArgs),
    /// List the issue-lease queue (pending/claimed/running/...)
    #[command(name = "queue")]
    Queue(DaemonQueueArgs),
}

/// Arguments for `daemon discover` (dry-run issue discovery).
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
/// @requirement:REQ-DAEMON-DISCOVERY-004
#[derive(Args, Debug)]
pub struct DaemonDiscoverArgs {
    /// Path to config file (config id is its file stem)
    #[arg(short, long, value_name = "PATH")]
    pub config: PathBuf,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `daemon queue` (issue-lease queue listing).
/// @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
/// @requirement:REQ-DAEMON-DISCOVERY-002
#[derive(Args, Debug)]
pub struct DaemonQueueArgs {
    /// Optional path to config file to filter the queue by config id
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Filter to a single lease status (pending/claimed/running/...)
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `daemon start`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Args, Debug)]
pub struct DaemonStartArgs {
    /// Path to config file (config id is its file stem)
    #[arg(short, long, value_name = "PATH")]
    pub config: PathBuf,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Replace an existing daemon for this config (explicit recovery)
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `daemon run`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Args, Debug)]
pub struct DaemonRunArgs {
    /// Path to config file (config id is its file stem)
    #[arg(short, long, value_name = "PATH")]
    pub config: PathBuf,
    /// Path to a daemon scheduler config with multiple targets
    #[arg(long, value_name = "PATH")]
    pub scheduler_config: Option<PathBuf>,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Replace an existing daemon for this config (explicit recovery)
    #[arg(long)]
    pub force: bool,
    /// Run a single discovery/launch pass instead of looping (cron/testing)
    #[arg(long)]
    pub once: bool,
}

/// Arguments for `daemon stop`.
///
/// Exactly one of `--config` or `--all` must be supplied; they are mutually
/// exclusive.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Args, Debug)]
pub struct DaemonStopArgs {
    /// Path to config file to stop (config id is its file stem)
    #[arg(short, long, value_name = "PATH", conflicts_with = "all", required_unless_present = "all")]
    pub config: Option<PathBuf>,
    /// Stop all known daemon instances
    #[arg(long, conflicts_with = "config")]
    pub all: bool,
}

/// Arguments for `daemon status`.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P09
#[derive(Args, Debug)]
pub struct DaemonStatusArgs {
    /// Path to config file to inspect (config id is its file stem)
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the `monitor` command (issue #52).
///
/// A continuous, plain-CLI (non-TUI) watch view that repaints a combined
/// snapshot of daemon health, run counts, a run table, the selected-run detail
/// and recent log lines. Strictly read-only: it never stops daemons or cancels
/// runs.
#[derive(Args, Debug)]
pub struct MonitorArgs {
    /// Filter to a single config id (file stem)
    #[arg(long, value_name = "ID")]
    pub config: Option<String>,
    /// Focus on a specific run id (also marks it as the selected run)
    #[arg(long, value_name = "RUN_ID")]
    pub run: Option<String>,
    /// Filter runs to a single GitHub issue number
    #[arg(long, value_name = "NUMBER", value_parser = clap::value_parser!(i64).range(1..))]
    pub issue: Option<i64>,
    /// Refresh delay between snapshots, in seconds
    #[arg(long, value_name = "SECONDS", default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
    pub interval: u64,
    /// Render exactly N snapshots, then exit normally
    #[arg(long, value_name = "N", conflicts_with = "once", value_parser = clap::value_parser!(u32).range(1..))]
    pub times: Option<u32>,
    /// Render a single snapshot and exit (equivalent to --times 1)
    #[arg(long, conflicts_with = "times")]
    pub once: bool,
    /// Append snapshots instead of clearing/repainting the terminal
    #[arg(long)]
    pub no_clear: bool,
    /// Number of recent log lines to show for the selected run
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub tail: usize,
}

/// Arguments for the `runs` command family.
///
/// The `runs` family provides read-side visibility into workflow runs: listing,
/// per-run drill-down, log tailing, and process liveness (issue #51).
#[derive(Args, Debug)]
pub struct RunsArgs {
    /// Runs inspection subcommand
    #[command(subcommand)]
    pub command: RunsCommand,
}

/// Subcommands for `runs` visibility.
#[derive(Subcommand, Debug)]
pub enum RunsCommand {
    /// List known workflow runs
    #[command(name = "list")]
    List(RunsListArgs),
    /// Show detailed information for a single run
    #[command(name = "show")]
    Show(RunsShowArgs),
    /// Tail the log for a run
    #[command(name = "tail")]
    Tail(RunsTailArgs),
    /// Show workflow and child/agent processes
    #[command(name = "ps")]
    Ps(RunsPsArgs),
    /// List checkpoints for a run
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    #[command(name = "checkpoints")]
    Checkpoints(RunsCheckpointsArgs),
    /// Resume a run from its latest resumable checkpoint
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    #[command(name = "resume")]
    Resume(RunsResumeArgs),
    /// Retry a failed run, optionally from the failed external-wait step
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    #[command(name = "retry")]
    Retry(RunsRetryArgs),
    /// Rewind a run's resume point to an earlier checkpoint
    /// @plan:PLAN-20260623-LUTHER-CONTINUATION
    #[command(name = "rewind")]
    Rewind(RunsRewindArgs),
}

/// Arguments for `runs list`.
#[derive(Args, Debug)]
pub struct RunsListArgs {
    /// Filter to a single config id (file stem)
    #[arg(long, value_name = "ID")]
    pub config: Option<String>,
    /// Filter to a single run state (running/completed/failed/...)
    #[arg(long, value_name = "STATE")]
    pub state: Option<String>,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs show`.
#[derive(Args, Debug)]
pub struct RunsShowArgs {
    /// The run id to show
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs tail`.
///
/// Exactly one of positional `run_id` or `--current` must be supplied.
#[derive(Args, Debug)]
pub struct RunsTailArgs {
    /// The run id whose log should be tailed
    #[arg(value_name = "RUN_ID", required_unless_present = "current")]
    pub run_id: Option<String>,
    /// Tail the currently-active run resolved from heartbeats
    #[arg(long, conflicts_with = "run_id")]
    pub current: bool,
    /// Number of trailing log lines to print
    #[arg(long, value_name = "N", default_value_t = 80)]
    pub lines: usize,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs ps`.
#[derive(Args, Debug)]
pub struct RunsPsArgs {
    /// Filter to a single config id (file stem)
    #[arg(long, value_name = "ID")]
    pub config: Option<String>,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs checkpoints`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Args, Debug)]
pub struct RunsCheckpointsArgs {
    /// The run id whose checkpoints should be listed
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs resume`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Args, Debug)]
pub struct RunsResumeArgs {
    /// The run id to resume
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Permit resuming from a non-whitelisted (e.g. implementation) step
    #[arg(long)]
    pub force: bool,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs retry`.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Args, Debug)]
pub struct RunsRetryArgs {
    /// The run id to retry
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Directory containing workflows/ and workflow-configs/ subdirectories
    #[arg(long, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
    /// Retry from the failed external-wait step using its prior context
    #[arg(long)]
    pub from_failed_step: bool,
    /// Permit retrying a non-whitelisted (e.g. implementation) step
    #[arg(long)]
    pub force: bool,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `runs rewind`.
///
/// Exactly one of `--to-step` or `--to-checkpoint` must be supplied; clap
/// enforces mutual exclusion and that at least one target is present.
/// @plan:PLAN-20260623-LUTHER-CONTINUATION
#[derive(Args, Debug)]
pub struct RunsRewindArgs {
    /// The run id to rewind
    #[arg(value_name = "RUN_ID")]
    pub run_id: String,
    /// Rewind to the checkpoint for this step id
    #[arg(long, value_name = "STEP", required_unless_present = "to_checkpoint")]
    pub to_step: Option<String>,
    /// Rewind to a checkpoint by identity (step_id@timestamp)
    #[arg(long, value_name = "ID", conflicts_with = "to_step")]
    pub to_checkpoint: Option<String>,
    /// Permit rewinding to a non-whitelisted (e.g. implementation) step
    #[arg(long)]
    pub force: bool,
    /// Output in JSON format
    #[arg(long)]
    pub json: bool,
}

