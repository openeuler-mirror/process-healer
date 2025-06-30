use crate::{
    config::{self, NetworkMonitorConfig},
    event_bus::ProcessEvent,
    monitor::Monitor,
};
use async_trait::async_trait;
use reqwest::Response;
use std::{io::Read, result};
use tokio::{io::AsyncReadExt, sync::broadcast};
use tokio::{net::TcpStream, time};
use tracing::{debug, error, info, warn};
pub struct NetworkMonitor {
    config: NetworkMonitorConfig,
    event_tx: broadcast::Sender<ProcessEvent>,
}
impl NetworkMonitor {
    pub fn new(config: NetworkMonitorConfig, event_tx: broadcast::Sender<ProcessEvent>) -> Self {
        Self {
            config,
            event_tx,
        }
    }
    pub fn check_interval(&self) -> u64 {
        self.config.interval_secs
    }
    async fn check_and_publish(&self) {
        let client = reqwest::Client::new();
        let check_result = client.get(&self.config.target_url).send().await;
        match check_result {
            Ok(response) => match response.status().is_success() {
                true => {
                    debug!("[NetMonitor] {} is healthy", self.config.name);
                }
                false => {
                    warn!(
                        "[NetMonitor] {} is unhealthy, status: {}",
                        self.config.name,
                        response.status()
                    );
                }
            },
            Err(e) => {
                if e.is_connect() {
                    warn!("[NetMonitor] {} is unreachable: {}", self.config.name, e);
                } else if e.is_timeout() {
                    warn!("[NetMonitor] {} request timed out: {}", self.config.name, e);
                } else {
                    warn!(
                        "[NetMonitor] {} encountered an error: {}",
                        self.config.name, e
                    );
                }
                self.publish_process_disconnected();
                //TODO 不能确定这几个事件究竟是否是需要重连，考虑设置成多个不同event发送
            }
        }
    }
    async fn monitor_task_loop(&self) {
        let mut interval = time::interval(time::Duration::from_secs(self.check_interval()));

        info!("[NetMonitor] Task for '{}' started.", self.config.name);
        loop {
            interval.tick().await;
            self.check_and_publish().await;
        }
    }
    fn publish_process_disconnected(&self) {
        let event = ProcessEvent::ProcessDisconnected {
            name: self.config.name.clone(), //name是被检测的进程的name
            url: self.config.target_url.clone(),
            //和PidMonitor比起，稍微不太一样的是Pid的config内部含的是pid file的地址。需要去读取才可以用，而target_url是可以直接使用的
        };
        debug!(
            "[{}] Publishing ProcessDisconnected event for HTTP {}",
            self.config.name, self.config.target_url
        );

        match self.event_tx.send(event) {
            Ok(receiver_count) => {
                debug!(
                    "[{}] Sent ProcessDisconnected event for HTTP {} to {} receivers",
                    self.config.name, self.config.target_url, receiver_count
                );
            }
            Err(_) => {
                warn!(
                    "[{}] Failed to publish ProcessDisconnected event for HTTP {}: no active subscribers",
                    self.config.name, self.config.target_url
                );
            }
        }
    }
}

#[async_trait]
impl Monitor for NetworkMonitor {
    async fn run(self) {
        self.monitor_task_loop().await;
    }
    fn name(&self) -> String {
        self.config.name.clone()
    }
}
