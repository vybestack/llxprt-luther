/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// CLI module - command line interface for the workflow runtime.
///
/// This module provides the CLI commands using clap derive macros.

use clap::{Parser, Subcommand, Args};
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
}

/// Arguments for the run command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to config file
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Perform a dry run without executing
    #[arg(long)]
    pub dry_run: bool,
    /// Workflow type ID
    #[arg(short, long, value_name = "ID")]
    pub workflow_type: Option<String>,
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
    /// Run in foreground mode
    #[arg(long)]
    pub foreground: bool,
    /// IPC socket path
    #[arg(long, value_name = "PATH")]
    pub socket_path: Option<PathBuf>,
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
        let args = RunArgs {
            config: Some(PathBuf::from("/test/config.toml")),
            dry_run: true,
            workflow_type: Some("test-type".to_string()),
        };
        assert!(args.dry_run);
        assert_eq!(args.config, Some(PathBuf::from("/test/config.toml")));
        assert_eq!(args.workflow_type, Some("test-type".to_string()));
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
    fn service_args_parsing() {
        // @plan:PLAN-20260404-INITIAL-RUNTIME.P12
        let args = ServiceArgs {
            foreground: true,
            socket_path: Some(PathBuf::from("/tmp/test.sock")),
        };
        assert!(args.foreground);
        assert_eq!(args.socket_path, Some(PathBuf::from("/tmp/test.sock")));
    }
}
