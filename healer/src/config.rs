use crate::daemon_handler::DaemonConfig;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

// 顶层配置结构体

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[allow(dead_code)] // Reserved for future use
    pub log_level: Option<String>,
    pub log_directory: Option<PathBuf>,
    pub pid_file_directory: Option<PathBuf>,
    pub processes: Vec<ProcessConfig>,
    pub working_directory: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProcessConfig {
    pub name: String,
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub run_as_user: Option<String>,
    pub run_as_root: bool,
    #[serde(default)]
    #[allow(dead_code)] // Reserved for future use
    pub working_dir: Option<PathBuf>,
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub recovery: RecoveryConfig,
    #[serde(default)]
    pub dependencies: Vec<RawDependency>,
}

// ---------------- Dependency Config ----------------

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum RawDependency {
    Simple(String),
    Detailed(DependencyConfig),
}

#[derive(Deserialize, Debug, Clone)]
pub struct DependencyConfig {
    pub target: String,
    #[serde(default = "default_kind")]
    pub kind: DependencyKind,
    #[serde(default = "default_true")]
    pub hard: bool,
    #[serde(default = "default_max_wait_secs")]
    pub max_wait_secs: u64,
    #[serde(default = "default_on_failure")]
    pub on_failure: OnFailure,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DependencyKind {
    Requires,
    After,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    Abort,
    Skip,
    Degrade,
}

fn default_kind() -> DependencyKind {
    DependencyKind::Requires
}
fn default_true() -> bool {
    true
}
fn default_max_wait_secs() -> u64 {
    30
}
fn default_on_failure() -> OnFailure {
    OnFailure::Abort
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MonitorConfig {
    Pid(PidMonitorFields),
    Ebpf(EbpfMonitorFields),
    Network(NetworkMonitorFields),
}

#[derive(Deserialize, Debug, Clone)]
pub struct PidMonitorFields {
    pub pid_file_path: PathBuf,
    pub interval_secs: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NetworkMonitorFields {
    pub target_url: String,
    pub interval_secs: u64,
}
#[derive(Deserialize, Debug, Clone)]
pub struct EbpfMonitorFields {}

// #[derive(Deserialize, Debug, Clone)]
// pub struct RecoveryConfig {
//     pub retries: u32,
//     pub retry_window_secs: u64,
//     pub cooldown_secs: u64,
// }

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RecoveryConfig {
    Regular(RegularHealerFields),
    NotRegular(NotREgularHealerFields),
}

#[derive(Deserialize, Debug, Clone)]
pub struct RegularHealerFields {
    pub retries: u32,
    pub retry_window_secs: u64,
    pub cooldown_secs: u64,
}
// 占位以后没有也可以删掉
#[derive(Deserialize, Debug, Clone)]
pub struct NotREgularHealerFields {}

#[derive(Debug, Clone)]
pub struct PidMonitorConfig {
    pub name: String,
    pub pid_file_path: PathBuf,
    pub interval_secs: u64,
}
#[derive(Debug, Clone)]
pub struct EbpfMonitorConfig {
    pub name: String,
    pub command: String,
}
#[derive(Debug, Clone)]
pub struct NetworkMonitorConfig {
    pub name: String,
    pub target_url: String, // 目标URL
    pub interval_secs: u64, //检查的频率间隔
}
impl Default for RecoveryConfig {
    fn default() -> Self {
        Self::Regular(RegularHealerFields::default())
    }
}
impl Default for RegularHealerFields {
    fn default() -> Self {
        Self {
            retries: 3,
            retry_window_secs: 60,
            cooldown_secs: 180,
        }
    }
}
impl ProcessConfig {
    pub fn get_pid_monitor_config(&self) -> Option<PidMonitorConfig> {
        if let MonitorConfig::Pid(pid_fields) = &self.monitor {
            Some(PidMonitorConfig {
                name: self.name.clone(),
                pid_file_path: pid_fields.pid_file_path.clone(),
                interval_secs: pid_fields.interval_secs,
            })
        } else {
            None
        }
    }

    pub fn get_ebpf_monitor_config(&self) -> Option<EbpfMonitorConfig> {
        if let MonitorConfig::Ebpf(_ebpf_fields) = &self.monitor {
            Some(EbpfMonitorConfig {
                name: self.name.clone(),
                command: self.command.clone(),
            })
        } else {
            None
        }
    }

    pub fn get_network_monitor_config(&self) -> Option<NetworkMonitorConfig> {
        if let MonitorConfig::Network(net_fields) = &self.monitor {
            Some(NetworkMonitorConfig {
                name: self.name.clone(),
                target_url: net_fields.target_url.clone(),
                interval_secs: net_fields.interval_secs,
            })
        } else {
            None
        }
    }

    pub fn resolved_dependencies(&self) -> Vec<DependencyConfig> {
        self.dependencies
            .iter()
            .map(|raw| match raw {
                RawDependency::Simple(name) => DependencyConfig {
                    target: name.clone(),
                    kind: default_kind(),
                    hard: default_true(),
                    max_wait_secs: default_max_wait_secs(),
                    on_failure: default_on_failure(),
                },
                RawDependency::Detailed(d) => d.clone(),
            })
            .collect()
    }
}

impl AppConfig {
    pub fn load_from_file(config_file_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let config_content = fs::read_to_string(config_file_path)?;
        let loaded_config: AppConfig = serde_yaml::from_str(&config_content)?;
        Ok(loaded_config)
    }

    pub fn get_process_config_for(&self, process_name: &str) -> Option<&ProcessConfig> {
        self.processes
            .iter()
            .find(|&p| p.name == process_name)
            .map(|p_config| p_config)
    }

    pub fn to_daemonize_config(&self) -> DaemonConfig {
        let daemon_config = DaemonConfig {
            pid_file: self
                .pid_file_directory
                .as_ref()
                .map(|pid_file| pid_file.join("healer.pid"))
                .unwrap_or_else(|| PathBuf::from("/tmp/healer.pid")),
            log_directory: self
                .log_directory
                .clone()
                .unwrap_or_else(|| PathBuf::from("/tmp/healer")),
            working_dir: self
                .working_directory
                .clone()
                .unwrap_or_else(|| PathBuf::from("/")),
        };
        daemon_config
    }
}
