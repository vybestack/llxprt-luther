//! Service specification - plist and systemd unit generation.
//!
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
use std::path::PathBuf;

/// Service specification for daemon/service installation.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// Service name (used for plist/unit file naming)
    pub name: String,
    /// Human-readable label
    pub label: String,
    /// Path to the binary executable
    pub binary_path: PathBuf,
    /// Command line arguments
    pub args: Vec<String>,
    /// Working directory for the service
    pub working_dir: PathBuf,
    /// Environment variables
    pub environment: Vec<(String, String)>,
    /// Log file path (optional)
    pub log_path: Option<PathBuf>,
    /// Error log file path (optional)
    pub error_log_path: Option<PathBuf>,
    /// Whether to keep alive (restart on exit)
    pub keep_alive: bool,
    /// Whether to run at load
    pub run_at_load: bool,
    /// User to run as (for systemd)
    pub user: Option<String>,
    /// Group to run as (for systemd)
    pub group: Option<String>,
}

impl ServiceSpec {
    /// Create a new service specification.
    #[must_use]
    pub fn new(name: impl Into<String>, binary_path: impl Into<PathBuf>) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            label: format!("com.luther.{}", name),
            binary_path: binary_path.into(),
            args: Vec::new(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            environment: Vec::new(),
            log_path: None,
            error_log_path: None,
            keep_alive: true,
            run_at_load: true,
            user: None,
            group: None,
        }
    }

    /// Set the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Add a command line argument.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = dir.into();
        self
    }

    /// Add an environment variable.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Set the log file path.
    #[must_use]
    pub fn with_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    /// Set the error log file path.
    #[must_use]
    pub fn with_error_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.error_log_path = Some(path.into());
        self
    }

    /// Set keep alive behavior.
    #[must_use]
    pub fn with_keep_alive(mut self, keep_alive: bool) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    /// Set run at load behavior.
    #[must_use]
    pub fn with_run_at_load(mut self, run_at_load: bool) -> Self {
        self.run_at_load = run_at_load;
        self
    }

    /// Set the user to run as.
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set the group to run as.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Get the plist file name.
    pub fn plist_file_name(&self) -> String {
        format!("{}.plist", self.label)
    }

    /// Get the systemd unit file name.
    pub fn unit_file_name(&self) -> String {
        format!("{}.service", self.name)
    }
}

/// Build the canonical install specification for the runtime service.
///
/// Default stdout/stderr log paths are placed under
/// [`crate::runtime_paths::get_log_dir`] so an installed service and the error
/// remediation guidance reference the same log location. The service runs the
/// foreground command (`service run`) supervised by launchd/systemd, with
/// keep-alive and run-at-load enabled so the OS restarts it on failure.
///
/// # Arguments
/// * `binary_path` - Path to the executable to launch (e.g. `current_exe()`).
/// * `working_dir` - Working directory for the supervised process.
///
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub fn build_install_spec(
    binary_path: impl Into<PathBuf>,
    working_dir: impl Into<PathBuf>,
) -> ServiceSpec {
    let log_dir = crate::runtime_paths::get_log_dir();
    ServiceSpec::new("luther-workflow", binary_path)
        .with_label("com.luther.workflow")
        .with_arg("service")
        .with_arg("run")
        .with_working_dir(working_dir)
        .with_log_path(log_dir.join("service.out.log"))
        .with_error_log_path(log_dir.join("service.err.log"))
        .with_keep_alive(true)
        .with_run_at_load(true)
}

/// Ensure the parent directories for a spec's stdout/stderr log files exist.
///
/// The install backends only create the LaunchAgents / systemd user-unit
/// directories, but [`build_install_spec`] points stdout/stderr at
/// `get_log_dir()`. On a clean machine that directory may not exist yet, so the
/// first supervised start would fail or silently drop diagnostics. Creating the
/// parents here keeps log capture working from the very first start.
///
/// Both `log_path` and `error_log_path` are optional; `None` paths and paths
/// without a parent component are skipped.
///
/// @plan:PLAN-20260404-INITIAL-RUNTIME.P10
pub fn ensure_log_directories(spec: &ServiceSpec) -> std::io::Result<()> {
    for path in [spec.log_path.as_ref(), spec.error_log_path.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }
    Ok(())
}

/// Generate a launchd plist from a service specification.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// String containing the XML plist content
pub fn generate_launchd_plist(spec: &ServiceSpec) -> String {
    let binary_path_str = spec.binary_path.to_string_lossy();
    let working_dir_str = spec.working_dir.to_string_lossy();

    let mut plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>"#,
        escape_xml(&spec.label),
        escape_xml(&binary_path_str)
    );

    // Add arguments
    for arg in &spec.args {
        plist.push_str(&format!("\n        <string>{}</string>", escape_xml(arg)));
    }

    plist.push_str("\n    </array>");

    // Working directory
    plist.push_str(&format!(
        "\n    <key>WorkingDirectory</key>\n    <string>{}</string>",
        escape_xml(&working_dir_str)
    ));

    // Environment variables
    if !spec.environment.is_empty() {
        plist.push_str("\n    <key>EnvironmentVariables</key>\n    <dict>");
        for (key, value) in &spec.environment {
            plist.push_str(&format!(
                "\n        <key>{}</key>\n        <string>{}</string>",
                escape_xml(key),
                escape_xml(value)
            ));
        }
        plist.push_str("\n    </dict>");
    }

    // Standard output log
    if let Some(log_path) = &spec.log_path {
        plist.push_str(&format!(
            "\n    <key>StandardOutPath</key>\n    <string>{}</string>",
            escape_xml(&log_path.to_string_lossy())
        ));
    }

    // Standard error log
    if let Some(error_log_path) = &spec.error_log_path {
        plist.push_str(&format!(
            "\n    <key>StandardErrorPath</key>\n    <string>{}</string>",
            escape_xml(&error_log_path.to_string_lossy())
        ));
    }

    // Keep alive
    if spec.keep_alive {
        plist.push_str("\n    <key>KeepAlive</key>\n    <true/>");
    }

    // Run at load
    if spec.run_at_load {
        plist.push_str("\n    <key>RunAtLoad</key>\n    <true/>");
    }

    // End plist
    plist.push_str("\n</dict>\n</plist>");

    plist
}

/// Generate a systemd unit file from a service specification.
///
/// # Arguments
/// * `spec` - The service specification
///
/// # Returns
/// String containing the unit file content
pub fn generate_systemd_unit(spec: &ServiceSpec) -> String {
    let binary_path_str = spec.binary_path.to_string_lossy();
    let working_dir_str = spec.working_dir.to_string_lossy();

    // Build the exec start line. The binary path is escaped with the same rules
    // as the arguments so a path containing whitespace is not split by
    // systemd's shell-like ExecStart parser.
    let mut exec_start = escape_systemd_exec_token(&binary_path_str);
    for arg in &spec.args {
        exec_start.push(' ');
        exec_start.push_str(&escape_systemd_exec_token(arg));
    }

    let mut unit = format!(
        "[Unit]
Description={}
After=network.target

[Service]
Type=simple
ExecStart={}
WorkingDirectory={}",
        spec.label, exec_start, working_dir_str
    );

    // User and group
    if let Some(user) = &spec.user {
        unit.push_str(&format!("\nUser={}", user));
    }
    if let Some(group) = &spec.group {
        unit.push_str(&format!("\nGroup={}", group));
    }

    // Restart policy
    if spec.keep_alive {
        unit.push_str("\nRestart=on-failure");
        unit.push_str("\nRestartSec=5");
    }

    // Environment variables
    if !spec.environment.is_empty() {
        for (key, value) in &spec.environment {
            unit.push_str(&format!("\nEnvironment=\"{}={}\"", key, value));
        }
    }

    // Standard output
    if let Some(log_path) = &spec.log_path {
        unit.push_str(&format!(
            "\nStandardOutput=append:{}",
            log_path.to_string_lossy()
        ));
    }

    // Standard error
    if let Some(error_log_path) = &spec.error_log_path {
        unit.push_str(&format!(
            "\nStandardError=append:{}",
            error_log_path.to_string_lossy()
        ));
    }

    // Install section
    unit.push_str("\n\n[Install]\nWantedBy=default.target");

    unit
}

/// Escape a single token (binary path or argument) for a systemd `ExecStart=`
/// line. systemd parses `ExecStart` with shell-like word splitting, so any
/// token containing whitespace must be double-quoted and have backslashes and
/// double quotes escaped.
fn escape_systemd_exec_token(token: &str) -> String {
    if token.contains(' ') || token.contains('\t') {
        format!("\"{}\"", token.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        token.to_string()
    }
}

/// Escape XML special characters.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_launchd_plist() {
        let spec = ServiceSpec::new("luther-monitor", "/usr/local/bin/luther")
            .with_label("com.luther.monitor")
            .with_arg("--daemon")
            .with_working_dir("/var/lib/luther")
            .with_env("LUTHER_HOME", "/var/lib/luther");

        let plist = generate_launchd_plist(&spec);

        assert!(plist.contains("<?xml version="));
        assert!(plist.contains("com.luther.monitor"));
        assert!(plist.contains("/usr/local/bin/luther"));
        assert!(plist.contains("--daemon"));
        assert!(plist.contains("/var/lib/luther"));
        assert!(plist.contains("LUTHER_HOME"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn test_generate_systemd_unit() {
        let spec = ServiceSpec::new("luther-monitor", "/usr/local/bin/luther")
            .with_label("Luther Monitor Service")
            .with_arg("--daemon")
            .with_working_dir("/var/lib/luther")
            .with_env("LUTHER_HOME", "/var/lib/luther")
            .with_user("luther")
            .with_group("luther");

        let unit = generate_systemd_unit(&spec);

        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("Luther Monitor Service"));
        assert!(unit.contains("/usr/local/bin/luther"));
        assert!(unit.contains("--daemon"));
        assert!(unit.contains("/var/lib/luther"));
        assert!(unit.contains("User=luther"));
        assert!(unit.contains("Group=luther"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn test_generate_systemd_unit_quotes_binary_path_with_spaces() {
        let spec = ServiceSpec::new("luther-monitor", "/opt/Luther Apps/bin/luther")
            .with_arg("service")
            .with_arg("run");

        let unit = generate_systemd_unit(&spec);

        // The whitespace-containing binary path must be quoted so systemd does
        // not split it into multiple tokens.
        assert!(unit.contains("ExecStart=\"/opt/Luther Apps/bin/luther\" service run"));
    }

    #[test]
    fn test_generate_systemd_unit_leaves_plain_binary_path_unquoted() {
        let spec = ServiceSpec::new("luther-monitor", "/usr/local/bin/luther").with_arg("run");

        let unit = generate_systemd_unit(&spec);

        assert!(unit.contains("ExecStart=/usr/local/bin/luther run"));
    }

    #[test]
    fn test_service_spec_builder() {
        let spec = ServiceSpec::new("test", "/bin/test")
            .with_label("com.test.service")
            .with_arg("arg1")
            .with_arg("arg2")
            .with_working_dir("/tmp")
            .with_env("KEY", "value")
            .with_keep_alive(false)
            .with_run_at_load(false)
            .with_user("user1")
            .with_group("group1");

        assert_eq!(spec.name, "test");
        assert_eq!(spec.label, "com.test.service");
        assert_eq!(spec.args, vec!["arg1", "arg2"]);
        assert_eq!(spec.working_dir, PathBuf::from("/tmp"));
        assert_eq!(
            spec.environment,
            vec![("KEY".to_string(), "value".to_string())]
        );
        assert!(!spec.keep_alive);
        assert!(!spec.run_at_load);
        assert_eq!(spec.user, Some("user1".to_string()));
        assert_eq!(spec.group, Some("group1".to_string()));
    }

    #[test]
    fn test_plist_file_name() {
        let spec = ServiceSpec::new("test", "/bin/test").with_label("com.luther.test");
        assert_eq!(spec.plist_file_name(), "com.luther.test.plist");
    }

    #[test]
    fn test_unit_file_name() {
        let spec = ServiceSpec::new("test", "/bin/test");
        assert_eq!(spec.unit_file_name(), "test.service");
    }

    #[test]
    fn test_ensure_log_directories_creates_missing_parents() {
        let base = std::env::temp_dir().join(format!(
            "luther-log-dir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let log_dir = base.join("logs");
        // Sanity: the directory must not exist before the call.
        assert!(!log_dir.exists());

        let spec = ServiceSpec::new("luther-test", "/bin/test")
            .with_log_path(log_dir.join("service.out.log"))
            .with_error_log_path(log_dir.join("service.err.log"));

        ensure_log_directories(&spec).expect("create log dirs");
        assert!(log_dir.exists());

        // Idempotent: a second call succeeds even though the dir now exists.
        ensure_log_directories(&spec).expect("idempotent create");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_ensure_log_directories_noop_without_paths() {
        let spec = ServiceSpec::new("luther-test", "/bin/test");
        // No log paths configured: should be a successful no-op.
        ensure_log_directories(&spec).expect("noop without log paths");
    }

    #[test]
    fn test_build_install_spec_defaults() {
        let spec = build_install_spec("/usr/local/bin/luther", "/var/lib/luther");

        assert_eq!(spec.binary_path, PathBuf::from("/usr/local/bin/luther"));
        assert_eq!(spec.working_dir, PathBuf::from("/var/lib/luther"));
        assert_eq!(spec.args, vec!["service", "run"]);
        assert!(spec.keep_alive);
        assert!(spec.run_at_load);

        let log_dir = crate::runtime_paths::get_log_dir();
        assert_eq!(spec.log_path, Some(log_dir.join("service.out.log")));
        assert_eq!(spec.error_log_path, Some(log_dir.join("service.err.log")));
    }

    #[test]
    fn test_build_install_spec_generates_supervisor_directives() {
        let spec = build_install_spec("/usr/local/bin/luther", "/var/lib/luther");

        let plist = generate_launchd_plist(&spec);
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));

        let unit = generate_systemd_unit(&spec);
        assert!(unit.contains("StandardOutput=append:"));
        assert!(unit.contains("StandardError=append:"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy="));
    }
}
