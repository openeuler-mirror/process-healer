use crate::{
    config::{NetworkMonitorConfig, ProcessConfig},
    event_bus::ProcessEvent,
    monitor::{
        Monitor, ebpf_monitor::EbpfMonitor, network_monitor::NetworkMonitor,
        pid_monitor::PidMonitor,
    },
};
use anyhow::Result;
use std::collections::HashMap;
use std::time::Duration;
use sysinfo::NetworkData;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

// 监控器管理器，负责统一管理不同类型的监控器
pub struct MonitorManager {
    // eBPF 监控器 - 全局单例，始终运行
    ebpf_monitor: Option<EbpfMonitor>,
    // 当前被 eBPF 监控的进程配置
    watched_ebpf_configs: HashMap<String, ProcessConfig>,
    // PID 监控器 - 按需启停
    running_monitors: HashMap<String, JoinHandle<()>>,
    // 网络监控器 - 按需启停
    // running_network_monitors: HashMap<String, JoinHandle<()>>,
    // 事件发送器
    event_sender: broadcast::Sender<ProcessEvent>,
}

impl MonitorManager {
    pub async fn new(event_sender: broadcast::Sender<ProcessEvent>) -> Result<Self> {
        // 初始化全局 eBPF 监控器
        let ebpf_monitor = match EbpfMonitor::new(event_sender.clone()).await {
            Ok(monitor) => {
                info!("MonitorManager: eBPF Monitor initialized successfully.");
                Some(monitor)
            }
            Err(e) => {
                error!("MonitorManager: Failed to initialize eBPF Monitor: {}", e);
                None
            }
        };

        Ok(Self {
            ebpf_monitor,
            watched_ebpf_configs: HashMap::new(),
            running_monitors: HashMap::new(),
            // running_network_monitors: HashMap::new(),
            event_sender,
        })
    }

    // 根据新的配置更新所有监控器
    pub async fn reconcile(&mut self, processes: &[ProcessConfig]) -> Result<()> {
        info!("MonitorManager: Starting reconciliation...");

        // 分离不同类型的监控配置, ebpf和其他的pid  network监视器都略有不同
        let (ebpf_configs, not_ebpf_configs): (Vec<_>, Vec<_>) = processes
            .iter()
            .filter(|p| p.enabled)
            .partition(|p| p.get_ebpf_monitor_config().is_some());

        // 更新 eBPF 监控器
        self.reconcile_ebpf_monitors(ebpf_configs).await?;

        // 更新 PID 监控器
        self.reconcile_monitors(not_ebpf_configs).await?;

        info!("MonitorManager: Reconciliation completed.");
        Ok(())
    }

    // 更新 eBPF 监控器的监控列表
    async fn reconcile_ebpf_monitors(
        &mut self,
        desired_configs: Vec<&ProcessConfig>,
    ) -> Result<()> {
        let Some(ref mut ebpf_monitor) = self.ebpf_monitor else {
            warn!("MonitorManager: eBPF monitor not available, skipping eBPF reconciliation.");
            return Ok(());
        };

        // 构建期望的配置映射
        let desired_configs_map: HashMap<String, ProcessConfig> = desired_configs
            .into_iter()
            .map(|config| (config.name.clone(), config.clone()))
            .collect();

        // 移除不再需要的监控
        let configs_to_remove: Vec<String> = self
            .watched_ebpf_configs
            .keys()
            .filter(|name| !desired_configs_map.contains_key(*name))
            .cloned()
            .collect();

        for name in configs_to_remove {
            if let Some(config) = self.watched_ebpf_configs.remove(&name) {
                if let Some(ebpf_config) = config.get_ebpf_monitor_config() {
                    info!("MonitorManager: Removing eBPF watch for process '{}'", name);
                    if let Err(e) = ebpf_monitor.unwatch_config(ebpf_config).await {
                        error!(
                            "MonitorManager: Failed to unwatch config for '{}': {}",
                            name, e
                        );
                    }
                }
            }
        }

        // 添加新的监控
        for (name, config) in desired_configs_map {
            if !self.watched_ebpf_configs.contains_key(&name) {
                if let Some(ebpf_config) = config.get_ebpf_monitor_config() {
                    info!("MonitorManager: Adding eBPF watch for process '{}'", name);
                    match ebpf_monitor.watch_config(ebpf_config).await {
                        Ok(()) => {
                            self.watched_ebpf_configs.insert(name, config);
                        }
                        Err(e) => {
                            error!(
                                "MonitorManager: Failed to watch config for '{}': {}",
                                name, e
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // 更新 PID 监控器的启停状态
    async fn reconcile_monitors(&mut self, desired_configs: Vec<&ProcessConfig>) -> Result<()> {
        // 构建期望的配置映射
        let desired_configs_map: HashMap<String, &ProcessConfig> = desired_configs
            .into_iter()
            .map(|config| (config.name.clone(), config))
            .collect();

        // 停止不再需要的监控器
        let monitors_to_stop: Vec<String> = self
            .running_monitors
            .keys()
            .filter(|name| !desired_configs_map.contains_key(*name))
            .cloned()
            .collect();

        for name in monitors_to_stop {
            info!(
                "MonitorManager: Stopping not-ebpf monitor for process '{}'",
                name
            );
            if let Some(handle) = self.running_monitors.remove(&name) {
                // 取消任务并等待一小段时间
                handle.abort();
                // 给任务一些时间来清理
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // 启动新的监控器或重启已结束的监控器
        for (name, process_config) in desired_configs_map {
            let should_start = match self.running_monitors.get(&name) {
                Some(handle) => handle.is_finished(),
                None => true,
            };

            if should_start {
                if let Some(pid_config) = process_config.get_pid_monitor_config() {
                    info!(
                        "MonitorManager: Starting PID monitor for process '{}'",
                        name
                    );
                    let monitor = PidMonitor::new(pid_config, self.event_sender.clone());
                    let handle = tokio::spawn(monitor.run());
                    self.running_monitors.insert(name.clone(), handle);
                } else if let Some(network_config) = process_config.get_network_monitor_config() {
                    info!(
                        "MonitorManager: Starting Network monitor for process '{}'",
                        name
                    );
                    let monitor = NetworkMonitor::new(network_config, self.event_sender.clone());
                    let handle = tokio::spawn(monitor.run());
                    self.running_monitors.insert(name.clone(), handle);
                }
            }
        }

        Ok(())
    }

    // 关闭所有监控器
    pub async fn shutdown(&mut self) {
        info!("MonitorManager: Shutting down all monitors...");

        // 停止所有 非ebpf 监控器
        let mut handles_to_wait = Vec::new();
        for (name, handle) in self.running_monitors.drain() {
            info!("MonitorManager: Stopping monitor for '{}'", name);
            handle.abort();
            handles_to_wait.push(handle);
        }

        // 等待所有任务完成，但设置超时
        for handle in handles_to_wait {
            if let Err(e) = tokio::time::timeout(Duration::from_secs(2), handle).await {
                warn!(
                    "MonitorManager: not-ebpf monitor task did not complete within timeout: {:?}",
                    e
                );
            }
        }

        // 关闭 eBPF 监控器，否则守护进程杀不死
        if let Some(mut ebpf_monitor) = self.ebpf_monitor.take() {
            info!("MonitorManager: Shutting down eBPF monitor...");
            ebpf_monitor.shutdown().await;
        }

        info!("MonitorManager: All monitors stopped.");
    }
}
