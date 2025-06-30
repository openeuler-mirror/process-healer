use crate::config::AppConfig;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

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

        match AppConfig::load_from_file(&self.config_path) {
            Ok(new_config) => {
                let mut config_guard = self.config.write().await;
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

    // 获取当前配置的只读引用
    pub fn get_config(&self) -> Arc<RwLock<AppConfig>> {
        Arc::clone(&self.config)
    }
}
