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
    #[arg(short, long, value_name = "PATH", conflicts_with = "all")]
    pub config: Option<PathBuf>,
    /// Stop all known daemon instances
    #[arg(long)]
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

/// Parse CLI arguments.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
pub fn parse_args() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_without_error() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        // Test that CLI can be constructed without panicking
        Cli::command().debug_assert();
    }

    #[test]
    fn run_args_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        // @plan:PLAN-20260408-LLXPRT-FIRST.P20
        let args = RunArgs {
            config: Some(PathBuf::from("/test/config.toml")),
            dry_run: true,
            skip_preflight: false,
            workflow_type: Some("test-type".to_string()),
            config_dir: None,
            run_id: Some("run-123".to_string()),
            repo: Some("vybestack/llxprt-luther".to_string()),
            issue: Some("3".to_string()),
            work_dir: Some(PathBuf::from("/tmp/luther-workspaces/llxprt-luther")),
            artifact_dir: Some(PathBuf::from("/tmp/luther-artifacts/llxprt-luther")),
        };
        assert!(args.dry_run);
        assert_eq!(args.config, Some(PathBuf::from("/test/config.toml")));
        assert_eq!(args.workflow_type, Some("test-type".to_string()));
        assert!(args.config_dir.is_none());
        assert_eq!(args.run_id, Some("run-123".to_string()));
        assert_eq!(args.repo, Some("vybestack/llxprt-luther".to_string()));
        assert_eq!(args.issue, Some("3".to_string()));
        assert_eq!(
            args.work_dir,
            Some(PathBuf::from("/tmp/luther-workspaces/llxprt-luther"))
        );
        assert_eq!(
            args.artifact_dir,
            Some(PathBuf::from("/tmp/luther-artifacts/llxprt-luther"))
        );
    }

    #[test]
    fn status_args_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        let args = StatusArgs {
            json: true,
            run_id: Some("run-123".to_string()),
        };
        assert!(args.json);
        assert_eq!(args.run_id, Some("run-123".to_string()));
    }

    #[test]
    fn service_run_args_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "service",
            "run",
            "--foreground",
            "--socket-path",
            "/tmp/test.sock",
        ])
        .expect("service run should parse");
        match cli.command {
            Commands::Service(ServiceArgs {
                command: ServiceCommand::Run(run),
            }) => {
                assert!(run.foreground);
                assert_eq!(run.socket_path, Some(PathBuf::from("/tmp/test.sock")));
            }
            other => panic!("expected service run, got {other:?}"),
        }
    }

    #[test]
    fn service_install_args_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "service",
            "install",
            "--binary",
            "/usr/local/bin/luther",
        ])
        .expect("service install should parse");
        match cli.command {
            Commands::Service(ServiceArgs {
                command: ServiceCommand::Install(install),
            }) => {
                assert_eq!(install.binary, Some(PathBuf::from("/usr/local/bin/luther")));
                assert_eq!(install.config, None);
            }
            other => panic!("expected service install, got {other:?}"),
        }
    }

    #[test]
    fn service_status_json_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        let cli = Cli::try_parse_from(["luther-workflow", "service", "status", "--json"])
            .expect("service status should parse");
        match cli.command {
            Commands::Service(ServiceArgs {
                command: ServiceCommand::Status(status),
            }) => {
                assert!(status.json);
            }
            other => panic!("expected service status, got {other:?}"),
        }
    }

    #[test]
    fn service_bare_lifecycle_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        for (sub, label) in [
            ("start", "start"),
            ("stop", "stop"),
            ("uninstall", "uninstall"),
        ] {
            let cli = Cli::try_parse_from(["luther-workflow", "service", sub])
                .unwrap_or_else(|_| panic!("service {label} should parse"));
            match cli.command {
                Commands::Service(ServiceArgs { command }) => match (label, command) {
                    ("start", ServiceCommand::Start)
                    | ("stop", ServiceCommand::Stop)
                    | ("uninstall", ServiceCommand::Uninstall) => {}
                    (_, other) => panic!("unexpected subcommand for {label}: {other:?}"),
                },
                other => panic!("expected service command, got {other:?}"),
            }
        }
    }

    #[test]
    fn daemon_run_requires_config() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let result = Cli::try_parse_from(["luther-workflow", "daemon", "run"]);
        assert!(result.is_err(), "daemon run without --config should fail");
    }

    #[test]
    fn daemon_run_parses_config_and_force() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "run",
            "--config",
            "config/workflow-configs/llxprt-code.toml",
            "--force",
        ])
        .expect("daemon run should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Run(run),
            }) => {
                assert!(run.force);
                assert_eq!(
                    run.config,
                    PathBuf::from("config/workflow-configs/llxprt-code.toml")
                );
            }
            other => panic!("expected daemon run, got {other:?}"),
        }
    }

    #[test]
    fn daemon_start_parses_config_dir() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "start",
            "--config",
            "llxprt-code.toml",
            "--config-dir",
            "/tmp/cfg",
        ])
        .expect("daemon start should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Start(start),
            }) => {
                assert!(!start.force);
                assert_eq!(start.config_dir, Some(PathBuf::from("/tmp/cfg")));
            }
            other => panic!("expected daemon start, got {other:?}"),
        }
    }

    #[test]
    fn daemon_stop_config_and_all_conflict() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let result = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "stop",
            "--config",
            "llxprt-code.toml",
            "--all",
        ]);
        assert!(
            result.is_err(),
            "daemon stop with both --config and --all should fail"
        );
    }

    #[test]
    fn daemon_stop_all_parses() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let cli = Cli::try_parse_from(["luther-workflow", "daemon", "stop", "--all"])
            .expect("daemon stop --all should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Stop(stop),
            }) => {
                assert!(stop.all);
                assert_eq!(stop.config, None);
            }
            other => panic!("expected daemon stop, got {other:?}"),
        }
    }

    #[test]
    fn daemon_status_json_parses() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P09
        let cli = Cli::try_parse_from(["luther-workflow", "daemon", "status", "--json"])
            .expect("daemon status should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Status(status),
            }) => {
                assert!(status.json);
                assert_eq!(status.config, None);
            }
            other => panic!("expected daemon status, got {other:?}"),
        }
    }

    #[test]
    fn daemon_discover_parses_config_and_json() {
        // @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "discover",
            "--config",
            "llxprt-code.toml",
            "--config-dir",
            "/tmp/cfg",
            "--json",
        ])
        .expect("daemon discover should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Discover(args),
            }) => {
                assert!(args.json);
                assert_eq!(args.config, PathBuf::from("llxprt-code.toml"));
                assert_eq!(args.config_dir, Some(PathBuf::from("/tmp/cfg")));
            }
            other => panic!("expected daemon discover, got {other:?}"),
        }
    }

    #[test]
    fn daemon_queue_parses_optional_filters() {
        // @plan:PLAN-20260415-DAEMON-DISCOVERY.P05
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "queue",
            "--status",
            "running",
            "--json",
        ])
        .expect("daemon queue should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Queue(args),
            }) => {
                assert!(args.json);
                assert_eq!(args.config, None);
                assert_eq!(args.status.as_deref(), Some("running"));
            }
            other => panic!("expected daemon queue, got {other:?}"),
        }
    }

    #[test]
    fn daemon_run_parses_once_flag() {
        // @plan:PLAN-20260415-DAEMON-DISCOVERY.P06
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "run",
            "--config",
            "llxprt-code.toml",
            "--once",
        ])
        .expect("daemon run --once should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Run(run),
            }) => {
                assert!(run.once);
            }
            other => panic!("expected daemon run, got {other:?}"),
        }
    }
}
