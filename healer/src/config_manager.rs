use crate::config::AppConfig;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// 配置管理器，负责配置的加载和热更新
pub struct ConfigManager {
    config: Arc<RwLock<AppConfig>>,
    config_path: std::path::PathBuf,
}

impl ConfigManager {
    pub fn new(config: Arc<RwLock<AppConfig>>, config_path: std::path::PathBuf) -> Self {
        Self {
            config,
            config_path,
        }
    }

    // 重新加载配置文件
    pub async fn reload_config(&self) -> Result<()> {
        info!(
            "ConfigManager: Reloading configuration from {:?}",
            self.config_path
        );
        // 先加载到临时变量，避免持锁期间做IO
        let load_start = std::time::Instant::now();
        let load_result = AppConfig::load_from_file(&self.config_path);
        debug!(
            elapsed_ms = load_start.elapsed().as_millis() as u64,
            "ConfigManager: load_from_file completed"
        );

        match load_result {
            Ok(new_config) => {
                debug!("ConfigManager: Attempting to acquire write lock for config swap");
                let lock_start = std::time::Instant::now();
                let mut config_guard = match tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    self.config.write(),
                )
                .await
                {
                    Ok(g) => g,
                    Err(_) => {
                        warn!("ConfigManager: Timeout waiting for write lock (possible read-lock held across await). Falling back to blocking acquire with periodic logging.");
                        let mut waited_ms = 0u64;
                        loop {
                            match self.config.try_write() {
                                Ok(g) => break g,
                                Err(_) => {
                                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                    waited_ms += 50;
                                    if waited_ms % 500 == 0 {
                                        debug!(
                                            waited_ms,
                                            "ConfigManager: still waiting for write lock..."
                                        );
                                    }
                                }
                            }
                        }
                    }
                };
                debug!(
                    wait_ms = lock_start.elapsed().as_millis() as u64,
                    "ConfigManager: Acquired write lock, swapping config"
                );
                *config_guard = new_config;
                info!("ConfigManager: Configuration reloaded successfully.");
                Ok(())
            }
            Err(e) => {
                error!("ConfigManager: Failed to reload configuration: {}", e);
                Err(anyhow::anyhow!("Failed to reload configuration: {}", e))
            }
        }
    }
}
