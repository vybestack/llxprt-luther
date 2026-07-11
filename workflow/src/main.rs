/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// Main entry point for the luther-workflow CLI.
mod app;

use app::{
    handle_daemon_command, handle_monitor_command, handle_run_command, handle_runs_command,
    handle_service_command, handle_status_command,
};
use luther_workflow::cli::{parse_args, Commands};

#[tokio::main]
async fn main() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();

    let cli = parse_args();

    match cli.command {
        Commands::Run(args) => {
            handle_run_command(&args).await;
        }
        Commands::Status(args) => {
            handle_status_command(&args).await;
        }
        Commands::Service(args) => {
            handle_service_command(&args).await;
        }
        Commands::Daemon(args) => {
            handle_daemon_command(&args).await;
        }
        Commands::Runs(args) => {
            handle_runs_command(&args).await;
        }
        Commands::Monitor(args) => {
            handle_monitor_command(&args).await;
        }
    }
}
