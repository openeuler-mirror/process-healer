use crate::{
    config::AppConfig,
    event_bus::ProcessEvent,
    subscriber::{Subscriber, process_healer::ProcessHealer},
};
use nix::errno::Errno;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use std::sync::Arc;
use tokio::signal::unix::{self, SignalKind};
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, error, info, warn};

/// 服务管理器，负责管理持久性后台任务
pub struct ServiceManager;

impl ServiceManager {
    /// 启动所有持久性后台服务
    pub fn spawn_persistent_services(
        event_sender: &broadcast::Sender<ProcessEvent>,
        config: &Arc<RwLock<AppConfig>>,
    ) {
        Self::spawn_process_healer(event_sender, config);
        Self::spawn_zombie_reaper();
    }

    /// 启动进程自愈服务
    fn spawn_process_healer(
        event_sender: &broadcast::Sender<ProcessEvent>,
        config: &Arc<RwLock<AppConfig>>,
    ) {
        let healer_receiver = event_sender.subscribe();
        let healer_config = Arc::clone(config);

        tokio::spawn(async move {
            let mut healer = ProcessHealer::new(healer_receiver, healer_config).await;
            info!("ServiceManager: ProcessHealer service started.");
            loop {
                match healer.event_rx.recv().await {
                    Ok(event) => {
                        healer.handle_event(event).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "ServiceManager: ProcessHealer lagged, missed {} messages",
                            n
                        );
                    }
                    Err(_) => {
                        error!("ServiceManager: ProcessHealer event channel closed, exiting.");
                        break;
                    }
                }
            }
        });
    }

    /// 启动僵尸进程清理服务
    fn spawn_zombie_reaper() {
        tokio::spawn(async {
            info!("ServiceManager: Zombie reaper service started, listening for SIGCHLD.");

            match unix::signal(SignalKind::child()) {
                Ok(mut stream) => loop {
                    stream.recv().await;
                    Self::reap_zombies();
                },
                Err(e) => {
                    error!(
                        "ServiceManager: Failed to register SIGCHLD signal handler: {}",
                        e
                    );
                }
            }
        });
    }

    /// 清理僵尸进程
    fn reap_zombies() {
        loop {
            match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, status)) => {
                    info!(
                        "ServiceManager: Reaped child {} which exited with status {}",
                        pid, status
                    );
                }
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    info!(
                        "ServiceManager: Reaped child {} which was killed by signal {}",
                        pid, signal
                    );
                }
                Ok(WaitStatus::StillAlive) | Err(Errno::ECHILD) => {
                    break; // 没有更多已终止的子进程了
                }
                Ok(other) => {
                    debug!(
                        "ServiceManager: Reaped child with other status: {:?}",
                        other
                    );
                }
                Err(e) => {
                    error!("ServiceManager: waitpid failed: {}", e);
                    break;
                }
            }
        }
    }
}
