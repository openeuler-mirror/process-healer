// src/monitor/pid_monitor.rs

use async_trait::async_trait;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use tokio::fs;
use tokio::sync::broadcast;
use tokio::time::{self, Duration as TokioDuration};
use tracing::{debug, warn};
// 从 config 模块引入 PidMonitor 所需的、具体的配置结构体
use super::Monitor;
use crate::config::PidMonitorConfig;
use crate::event_bus::ProcessEvent;
use crate::publisher::Publisher;
use tracing::info;
pub struct PidMonitor {
    config: PidMonitorConfig,
    event_tx: broadcast::Sender<ProcessEvent>,
}

impl PidMonitor {
    pub fn new(config: PidMonitorConfig, event_tx: broadcast::Sender<ProcessEvent>) -> Self {
        Self { config, event_tx }
    }
    pub fn check_interval(&self) -> u64 {
        self.config.interval_secs
    }
    fn publish_process_down(&self, pid: u32) {
        let event = ProcessEvent::ProcessDown {
            name: self.config.name.clone(), //name是被检测的进程的name
            pid,
        };
        debug!(
            "[{}] Publishing ProcessDown event for PID {}",
            self.config.name, pid
        );

        match self.publish(event) {
            Ok(receiver_count) => {
                debug!(
                    "[{}] Sent ProcessDown event for PID {} to {} receivers",
                    self.config.name, pid, receiver_count
                );
            }
            Err(_) => {
                warn!(
                    "[{}] Failed to publish ProcessDown event for PID {}: no active subscribers",
                    self.config.name, pid
                );
            }
        }
    }

    async fn monitor_task_loop(&self) {
        let monitor_name = self.name();
        let interval_secs = self.check_interval();
        let mut interval = time::interval(TokioDuration::from_secs(interval_secs));
        info!(
            "[Monitor] Task for '{}' started with a {}s interval.",
            monitor_name, interval_secs
        );
        loop {
            interval.tick().await;
            self.check_and_publish().await;
        }
    }

    async fn check_and_publish(&self) {
        let monitor_name = &self.config.name;
        let pid_file_path = &self.config.pid_file_path;
        debug!(
            "[{}] Performing health check on PID file: {}",
            monitor_name,
            pid_file_path.display()
        );
        // 异步读取 PID 文件内容
        let pid_str = match fs::read_to_string(pid_file_path).await {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "[{}] Failed to read PID file {}: {}. Assuming process is down.",
                    monitor_name,
                    pid_file_path.display(),
                    e
                );
                return;
            }
        };

        // 解析 PID
        let pid = match pid_str.trim().parse::<i32>() {
            Ok(p) if p > 0 => p,
            _ => {
                warn!(
                    "[{}] Failed to parse a valid PID from file {}. Content: '{}'. Assuming process is down.",
                    monitor_name,
                    pid_file_path.display(),
                    pid_str
                );
                return;
            }
        };

        // 使用信号检查进程是否存在
        let process_pid = Pid::from_raw(pid);
        match kill(process_pid, None) {
            Ok(_) => {
                debug!("[{}] Process (PID: {}) is alive.", monitor_name, pid);
            }
            Err(Errno::ESRCH) => {
                info!(
                    "[{}] Process (PID: {}) not found (ESRCH). Process has exited.",
                    monitor_name, pid
                );
                self.publish_process_down(pid as u32);
            }
            Err(e) => {
                warn!(
                    "[{}] Error checking process (PID: {}): {}. Unable to determine status.",
                    monitor_name, pid, e
                );
            }
        }
    }
}

#[async_trait]
impl Monitor for PidMonitor {
    fn name(&self) -> String {
        self.config.name.clone()
    }
    async fn run(self) {
        self.monitor_task_loop().await;
    }
}

impl Publisher for PidMonitor {
    fn publish(
        &self,
        event: ProcessEvent,
    ) -> Result<usize, broadcast::error::SendError<ProcessEvent>> {
        self.event_tx.send(event)
    }
}
