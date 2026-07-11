/// Parse CLI arguments.
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P12
use clap::Parser;

use super::args::Cli;

pub fn parse_args() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::super::args::*;
    use clap::{CommandFactory, Parser};
    use std::path::{Path, PathBuf};

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
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "run",
            "--config",
            "/test/config.toml",
            "--dry-run",
            "--workflow-type",
            "test-type",
            "--run-id",
            "run-123",
            "--repo",
            "vybestack/llxprt-luther",
            "--issue",
            "3",
            "--work-dir",
            "/tmp/luther-workspaces/llxprt-luther",
            "--artifact-dir",
            "/tmp/luther-artifacts/llxprt-luther",
        ])
        .expect("run args should parse");
        let Commands::Run(args) = cli.command else {
            panic!("expected run command");
        };
        assert!(args.dry_run);
        assert!(!args.skip_preflight);
        assert_eq!(args.config.as_deref(), Some(Path::new("/test/config.toml")));
        assert_eq!(args.workflow_type.as_deref(), Some("test-type"));
        assert!(args.config_dir.is_none());
        assert_eq!(args.run_id.as_deref(), Some("run-123"));
        assert_eq!(args.repo.as_deref(), Some("vybestack/llxprt-luther"));
        assert_eq!(args.issue.as_deref(), Some("3"));
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
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "status",
            "--json",
            "--config",
            "llxprt-code",
        ])
        .expect("status args should parse");
        let Commands::Status(args) = cli.command else {
            panic!("expected status command");
        };
        assert!(args.json);
        assert_eq!(args.run_id, None);
        assert_eq!(args.config.as_deref(), Some("llxprt-code"));
    }

    #[test]
    fn status_config_filter_parses() {
        // issue #51: status gains a --config filter
        let cli = Cli::try_parse_from(["luther-workflow", "status", "--config", "llxprt-code"])
            .expect("status --config should parse");
        match cli.command {
            Commands::Status(args) => {
                assert_eq!(args.config.as_deref(), Some("llxprt-code"));
            }
            other => panic!("expected status, got {other:?}"),
        }
    }

    #[test]
    fn runs_list_parses_filters() {
        // issue #51
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "list",
            "--config",
            "llxprt-code",
            "--state",
            "running",
            "--json",
        ])
        .expect("runs list should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::List(args),
            }) => {
                assert_eq!(args.config.as_deref(), Some("llxprt-code"));
                assert_eq!(args.state.as_deref(), Some("running"));
                assert!(args.json);
            }
            other => panic!("expected runs list, got {other:?}"),
        }
    }

    #[test]
    fn runs_list_bare_parses() {
        // issue #51
        let cli = Cli::try_parse_from(["luther-workflow", "runs", "list"])
            .expect("runs list should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::List(args),
            }) => {
                assert_eq!(args.config, None);
                assert_eq!(args.state, None);
                assert!(!args.json);
            }
            other => panic!("expected runs list, got {other:?}"),
        }
    }

    #[test]
    fn runs_show_parses() {
        // issue #51
        let cli = Cli::try_parse_from(["luther-workflow", "runs", "show", "run-123", "--json"])
            .expect("runs show should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Show(args),
            }) => {
                assert_eq!(args.run_id, "run-123");
                assert!(args.json);
            }
            other => panic!("expected runs show, got {other:?}"),
        }
    }

    #[test]
    fn runs_tail_parses_run_id_and_lines() {
        // issue #51
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "tail",
            "run-123",
            "--lines",
            "100",
        ])
        .expect("runs tail should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Tail(args),
            }) => {
                assert_eq!(args.run_id.as_deref(), Some("run-123"));
                assert_eq!(args.lines, 100);
                assert!(!args.current);
            }
            other => panic!("expected runs tail, got {other:?}"),
        }
    }

    #[test]
    fn runs_tail_default_lines_is_80() {
        // issue #51
        let cli = Cli::try_parse_from(["luther-workflow", "runs", "tail", "run-123"])
            .expect("runs tail should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Tail(args),
            }) => {
                assert_eq!(args.lines, 80);
            }
            other => panic!("expected runs tail, got {other:?}"),
        }
    }

    #[test]
    fn runs_tail_current_parses() {
        // issue #51
        let cli = Cli::try_parse_from(["luther-workflow", "runs", "tail", "--current"])
            .expect("runs tail --current should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Tail(args),
            }) => {
                assert!(args.current);
                assert_eq!(args.run_id, None);
            }
            other => panic!("expected runs tail, got {other:?}"),
        }
    }

    #[test]
    fn runs_tail_run_id_and_current_conflict() {
        // issue #51: positional run_id and --current are mutually exclusive
        let result =
            Cli::try_parse_from(["luther-workflow", "runs", "tail", "run-123", "--current"]);
        assert!(
            result.is_err(),
            "runs tail with both run_id and --current should fail"
        );
    }

    #[test]
    fn runs_tail_requires_run_id_or_current() {
        let result = Cli::try_parse_from(["luther-workflow", "runs", "tail"]);
        assert!(
            result.is_err(),
            "runs tail without run_id or --current should fail"
        );
    }

    #[test]
    fn runs_ps_parses() {
        // issue #51
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "ps",
            "--config",
            "llxprt-code",
            "--json",
        ])
        .expect("runs ps should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Ps(args),
            }) => {
                assert_eq!(args.config.as_deref(), Some("llxprt-code"));
                assert!(args.json);
            }
            other => panic!("expected runs ps, got {other:?}"),
        }
    }

    #[test]
    fn runs_checkpoints_parses() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "checkpoints",
            "run-123",
            "--json",
        ])
        .expect("runs checkpoints should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Checkpoints(args),
            }) => {
                assert_eq!(args.run_id, "run-123");
                assert!(args.json);
            }
            other => panic!("expected runs checkpoints, got {other:?}"),
        }
    }

    #[test]
    fn runs_resume_parses_with_force() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "resume",
            "run-123",
            "--force",
            "--config-dir",
            "/tmp/cfg",
        ])
        .expect("runs resume should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Resume(args),
            }) => {
                assert_eq!(args.run_id, "run-123");
                assert_eq!(args.config_dir, Some(PathBuf::from("/tmp/cfg")));
                assert!(args.force);
                assert!(!args.json);
            }
            other => panic!("expected runs resume, got {other:?}"),
        }
    }

    #[test]
    fn runs_retry_parses_from_failed_step() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "retry",
            "run-123",
            "--from-failed-step",
            "--config-dir",
            "/tmp/cfg",
        ])
        .expect("runs retry should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Retry(args),
            }) => {
                assert_eq!(args.run_id, "run-123");
                assert_eq!(args.config_dir, Some(PathBuf::from("/tmp/cfg")));
                assert!(args.from_failed_step);
                assert!(!args.force);
            }
            other => panic!("expected runs retry, got {other:?}"),
        }
    }

    #[test]
    fn runs_rewind_parses_to_step() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "rewind",
            "run-123",
            "--to-step",
            "post_pr_iteration_guard",
        ])
        .expect("runs rewind --to-step should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Rewind(args),
            }) => {
                assert_eq!(args.run_id, "run-123");
                assert_eq!(args.to_step.as_deref(), Some("post_pr_iteration_guard"));
                assert!(args.to_checkpoint.is_none());
            }
            other => panic!("expected runs rewind, got {other:?}"),
        }
    }

    #[test]
    fn runs_rewind_to_checkpoint_parses() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "rewind",
            "run-123",
            "--to-checkpoint",
            "watch_pr_checks@2026-06-23T00:00:00Z",
        ])
        .expect("runs rewind --to-checkpoint should parse");
        match cli.command {
            Commands::Runs(RunsArgs {
                command: RunsCommand::Rewind(args),
            }) => {
                assert_eq!(
                    args.to_checkpoint.as_deref(),
                    Some("watch_pr_checks@2026-06-23T00:00:00Z")
                );
                assert!(args.to_step.is_none());
            }
            other => panic!("expected runs rewind, got {other:?}"),
        }
    }

    #[test]
    fn runs_rewind_requires_a_target() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let result = Cli::try_parse_from(["luther-workflow", "runs", "rewind", "run-123"]);
        assert!(result.is_err(), "runs rewind without a target should fail");
    }

    #[test]
    fn runs_rewind_rejects_both_targets() {
        // @plan:PLAN-20260623-LUTHER-CONTINUATION
        let result = Cli::try_parse_from([
            "luther-workflow",
            "runs",
            "rewind",
            "run-123",
            "--to-step",
            "watch_pr_checks",
            "--to-checkpoint",
            "watch_pr_checks@2026-06-23T00:00:00Z",
        ]);
        assert!(
            result.is_err(),
            "runs rewind with both targets should fail (mutually exclusive)"
        );
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
                assert_eq!(start.config, PathBuf::from("llxprt-code.toml"));
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

    #[test]
    fn daemon_run_parses_scheduler_config() {
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "daemon",
            "run",
            "--config",
            "llxprt-code.toml",
            "--scheduler-config",
            "daemon-scheduler.toml",
        ])
        .expect("daemon run --scheduler-config should parse");
        match cli.command {
            Commands::Daemon(DaemonArgs {
                command: DaemonCommand::Run(run),
            }) => {
                assert_eq!(
                    run.scheduler_config,
                    Some(PathBuf::from("daemon-scheduler.toml"))
                );
            }
            other => panic!("expected daemon run, got {other:?}"),
        }
    }

    #[test]
    fn monitor_defaults_parse() {
        // @plan:issue-52
        let cli =
            Cli::try_parse_from(["luther-workflow", "monitor"]).expect("bare monitor should parse");
        match cli.command {
            Commands::Monitor(args) => {
                assert_eq!(args.interval, 2);
                assert_eq!(args.tail, 10);
                assert!(!args.no_clear);
                assert!(!args.once);
                assert_eq!(args.times, None);
                assert_eq!(args.config, None);
                assert_eq!(args.run, None);
                assert_eq!(args.issue, None);
            }
            other => panic!("expected monitor, got {other:?}"),
        }
    }

    #[test]
    fn monitor_once_parses() {
        // @plan:issue-52
        let cli = Cli::try_parse_from(["luther-workflow", "monitor", "--once"])
            .expect("monitor --once should parse");
        match cli.command {
            Commands::Monitor(args) => assert!(args.once),
            other => panic!("expected monitor, got {other:?}"),
        }
    }

    #[test]
    fn monitor_times_parses() {
        // @plan:issue-52
        let cli = Cli::try_parse_from(["luther-workflow", "monitor", "--times", "5"])
            .expect("monitor --times should parse");
        match cli.command {
            Commands::Monitor(args) => assert_eq!(args.times, Some(5)),
            other => panic!("expected monitor, got {other:?}"),
        }
    }

    #[test]
    fn monitor_once_and_times_conflict() {
        // @plan:issue-52
        let result = Cli::try_parse_from(["luther-workflow", "monitor", "--once", "--times", "2"]);
        assert!(result.is_err(), "--once and --times must conflict");
    }

    #[test]
    fn monitor_filters_parse() {
        // @plan:issue-52
        let cli = Cli::try_parse_from([
            "luther-workflow",
            "monitor",
            "--config",
            "llxprt-code",
            "--run",
            "RUN",
            "--issue",
            "1801",
            "--interval",
            "5",
            "--no-clear",
            "--tail",
            "3",
        ])
        .expect("monitor with filters should parse");
        match cli.command {
            Commands::Monitor(args) => {
                assert_eq!(args.config.as_deref(), Some("llxprt-code"));
                assert_eq!(args.run.as_deref(), Some("RUN"));
                assert_eq!(args.issue, Some(1801));
                assert_eq!(args.interval, 5);
                assert!(args.no_clear);
                assert_eq!(args.tail, 3);
            }
            other => panic!("expected monitor, got {other:?}"),
        }
    }

    #[test]
    fn daemon_stop_requires_config_or_all() {
        let result = Cli::try_parse_from(["luther-workflow", "daemon", "stop"]);
        assert!(
            result.is_err(),
            "daemon stop without --config or --all should fail"
        );
    }
}
