use anyhow::Result;
use tokio::signal::unix::{self, SignalKind};
use tracing::info;

/// 信号处理器，负责处理系统信号
pub struct SignalHandler;

#[derive(Debug)]
pub enum SignalEvent {
    /// 配置重载信号 (SIGHUP)
    ConfigReload,
    /// 关闭信号 (SIGTERM, SIGINT)
    Shutdown,
}

impl SignalHandler {
    /// 等待下一个信号事件
    pub async fn wait_for_signal() -> Result<SignalEvent> {
        let mut hup_signal = unix::signal(SignalKind::hangup())?;
        let mut term_signal = unix::signal(SignalKind::terminate())?;
        let mut int_signal = unix::signal(SignalKind::interrupt())?;

        tokio::select! {
            _ = hup_signal.recv() => {
                info!("SignalHandler: Received SIGHUP, triggering configuration reload.");
                Ok(SignalEvent::ConfigReload)
            }
            _ = term_signal.recv() => {
                info!("SignalHandler: Received SIGTERM, initiating graceful shutdown.");
                Ok(SignalEvent::Shutdown)
            }
            _ = int_signal.recv() => {
                info!("SignalHandler: Received SIGINT (Ctrl+C), initiating graceful shutdown.");
                Ok(SignalEvent::Shutdown)
            }
        }
    }
}
