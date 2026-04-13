/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
/// Main entry point for the luther-workflow CLI.

use std::process;

use tracing_subscriber::{fmt, EnvFilter};

use luther_workflow::cli::{parse_args, Commands};
use luther_workflow::engine::instance::WorkflowInstance;
use luther_workflow::engine::runner::{EngineRunner, RunOutcome};
use luther_workflow::monitor::heartbeat::MonitorState;
use luther_workflow::monitor::heartbeat::read_all_heartbeats;
use luther_workflow::persistence::init_database;
use luther_workflow::service::{Service, ServiceConfig};
use luther_workflow::workflow::config_loader::{
    resolve_workflow, resolve_workflow_config, resolve_workflow_type,
};

#[tokio::main]
async fn main() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(env_filter).with_target(false).init();

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
    }
}

/// Handle the run command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_run_command(args: &luther_workflow::cli::RunArgs) {
    // 1. Load config from path (or default fixture root)
    let fixture_root = std::path::PathBuf::from("tests/fixtures");

    let (workflow_type, config, run_ref) = if let Some(config_path) = &args.config {
        // Load from specified path
        let config_id = config_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        
        let workflow_type_id = args
            .workflow_type
            .clone()
            .unwrap_or_else(|| "test-workflow".to_string());

        let workflow_type = match resolve_workflow_type(&workflow_type_id, &fixture_root) {
            Ok(wt) => wt,
            Err(e) => {
                eprintln!("Error: Failed to resolve workflow type '{}': {}", workflow_type_id, e);
                process::exit(1);
            }
        };

        let config = match resolve_workflow_config(&config_id, &fixture_root) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Error: Failed to resolve config '{}': {}", config_id, e);
                process::exit(1);
            }
        };

        let run_ref = luther_workflow::workflow::schema::WorkflowRunRef::new(
            &workflow_type_id,
            &config_id,
            &uuid::Uuid::new_v4().to_string(),
        );
        (workflow_type, config, run_ref)
    } else {
        // Use default: test-workflow with test-config
        let workflow_type_id = args
            .workflow_type
            .clone()
            .unwrap_or_else(|| "test-workflow".to_string());
        let config_id = "test-config".to_string();
        let run_id = uuid::Uuid::new_v4().to_string();

        match resolve_workflow(&workflow_type_id, &config_id, &run_id, &fixture_root) {
            Ok((wt, cfg, rr)) => (wt, cfg, rr),
            Err(e) => {
                eprintln!("Error: Failed to resolve workflow: {}", e);
                process::exit(1);
            }
        }
    };

    // 2. Create run_id (already done in run_ref)
    let run_id = run_ref.run_id.clone();
    println!("Starting workflow run: {}", run_id);
    println!("  Workflow type: {}", workflow_type.workflow_type_id);
    println!("  Config: {}", config.config_id);

    // 3. Initialize checkpoint database
    let db_path = luther_workflow::runtime_paths::get_data_dir().join("checkpoints.db");
    if let Err(e) = init_database(&db_path) {
        eprintln!("Warning: Failed to initialize checkpoint database: {}", e);
    }

    if args.dry_run {
        println!("Dry run mode - workflow would execute the following steps:");
        for step in &workflow_type.steps {
            println!("  - {} ({}): {:?}", 
                step.step_id, 
                step.step_type,
                step.description.as_deref().unwrap_or("No description")
            );
        }
        println!("\nDry run complete. No changes made.");
        process::exit(0);
    }

    // 4. Create EngineRunner with default registry
    let instance = WorkflowInstance::create(workflow_type, config);
    let registry = luther_workflow::engine::executor::ExecutorRegistry::with_defaults();
    let mut runner = EngineRunner::new(instance, registry);

    // 5. Execute workflow
    println!("Executing workflow...");
    match runner.run() {
        Ok(outcome) => {
            // 6. Report results
            match outcome {
                RunOutcome::Success => {
                    println!("\nWorkflow completed successfully!");
                    println!("Run ID: {}", run_id);
                    process::exit(0);
                }
                RunOutcome::Failure { step_id, reason } => {
                    eprintln!("\nWorkflow failed at step '{}'", step_id);
                    eprintln!("Reason: {}", reason);
                    process::exit(1);
                }
                RunOutcome::Abandoned { step_id, reason } => {
                    eprintln!("\nWorkflow abandoned at step '{}'", step_id);
                    eprintln!("Reason: {}", reason);
                    process::exit(1);
                }
                RunOutcome::Interrupted { step_id } => {
                    println!("\nWorkflow interrupted at step '{}'", step_id);
                    println!("Run ID: {} (can be resumed)", run_id);
                    process::exit(130); // 128 + SIGINT (2)
                }
            }
        }
        Err(e) => {
            eprintln!("\nWorkflow execution error: {}", e);
            process::exit(1);
        }
    }
}

/// Handle the status command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_status_command(args: &luther_workflow::cli::StatusArgs) {
    // 1. Read all heartbeat files from data dir
    let heartbeats = match read_all_heartbeats().await {
        Ok(hbs) => hbs,
        Err(e) => {
            eprintln!("Error reading heartbeats: {}", e);
            std::collections::HashMap::new()
        }
    };

    // 2. Display monitor state
    if args.json {
        // JSON output
        let status = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "heartbeats": heartbeats,
        });
        println!("{}", serde_json::to_string_pretty(&status).unwrap());
    } else {
        // Human-readable output
        println!("Luther Workflow Monitor Status");
        println!("==============================");
        println!("Timestamp: {}", chrono::Utc::now().to_rfc3339());
        println!();

        if heartbeats.is_empty() {
            println!("No active runs found.");
            println!("  Status: No heartbeats detected");
        } else {
            println!("Active/Recent Runs:");
            for (run_id, hb) in &heartbeats {
                let state_str = match hb.state {
                    MonitorState::Starting => "starting",
                    MonitorState::Running => "running",
                    MonitorState::Degraded => "degraded",
                    MonitorState::Stopping => "stopping",
                    MonitorState::Stopped => "stopped",
                    MonitorState::Error => "error",
                };
                println!("  Run ID: {}", run_id);
                println!("    State: {}", state_str);
                println!("    Instance: {}", hb.instance_id);
                println!("    Uptime: {} seconds", hb.uptime_secs);
                println!("    Last heartbeat: {}", 
                    chrono::DateTime::from_timestamp(hb.timestamp, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if hb.active_workers > 0 {
                    println!("    Active workers: {}", hb.active_workers);
                }
                println!();
            }
        }

        // Show current run if specified
        if let Some(run_id) = &args.run_id {
            if let Some(hb) = heartbeats.get(run_id) {
                println!("Details for run '{}':", run_id);
                println!("  State: {:?}", hb.state);
                println!("  Active workers: {}", hb.active_workers);
            } else {
                println!("No heartbeat found for run '{}'", run_id);
            }
        }
    }
}

/// Handle the service command.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
async fn handle_service_command(args: &luther_workflow::cli::ServiceArgs) {
    println!("Starting service mode...");
    if !args.foreground {
        println!("Note: Running in foreground mode (daemon mode not yet implemented)");
    }

    let config = ServiceConfig {
        foreground: true,
        ipc_socket_path: args
            .socket_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/tmp/luther.sock".to_string()),
        log_level: "info".to_string(),
    };

    match Service::start(config).await {
        Ok(service) => {
            println!("Service started successfully.");
            println!("Instance ID: {} (Note: Service runs in foreground for now)", 
                service.get_status().await.map(|s| s.instance_id).unwrap_or_default());
            
            // Keep running until interrupted
            println!("Press Ctrl+C to stop...");
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
        Err(e) => {
            eprintln!("Failed to start service: {}", e);
            process::exit(1);
        }
    }
}
