use crate::{
    config::AppConfig,
    config_manager::ConfigManager,
    event_bus,
    monitor_manager::MonitorManager,
    service_manager::ServiceManager,
    signal_handler::{SignalEvent, SignalHandler},
};
use anyhow::Result;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::RwLock;
use tracing::{debug, error, info};

pub fn async_runtime(app_config: Arc<RwLock<AppConfig>>, config_path: PathBuf) {
    println!("Async runtime: Starting process monitoring");

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .thread_name("healer")
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Async runtime: Error from {}", e);
            std::process::exit(1);
        }
    };

    rt.block_on(async {
        if let Err(e) = daemon_core_logic(app_config, config_path).await {
            error!("Core logic error: {}", e);
            std::process::exit(1);
        }
    });
}

async fn daemon_core_logic(config: Arc<RwLock<AppConfig>>, config_path: PathBuf) -> Result<()> {
    info!("Application Core Logic: Starting up and initializing components...");

    // 1. 创建事件总线
    // 事件通道拆分：monitors -> coordinator_in, coordinator_out -> healer
    let monitor_event_sender = event_bus::create_event_sender();
    let coordinator_event_sender = event_bus::create_event_sender();
    info!("Application Core Logic: Event bus created.");

    // 2. 初始化各个管理器，包括配置管理器喝监视器管理器
    let config_manager = ConfigManager::new(Arc::clone(&config), config_path);
    let mut monitor_manager = MonitorManager::new(monitor_event_sender.clone()).await?;

    // 3. 启动持久性后台服务
    ServiceManager::spawn_persistent_services(
        &monitor_event_sender,
        &coordinator_event_sender,
        &config,
    );
    info!("Application Core Logic: Persistent services started.");

    // 4. 进行初始配置协调
    {
        let config_guard = config.read().await;
        debug!(
            "Loaded {} process configurations:",
            config_guard.processes.len()
        );
        for (i, process_config) in config_guard.processes.iter().enumerate() {
            debug!(
                "Process {}: name='{}', command='{}'",
                i + 1,
                process_config.name,
                process_config.command
            );
            if let Some(ebpf_config) = process_config.get_ebpf_monitor_config() {
                let executable_name = crate::utils::extract_executable_name(&ebpf_config.command);
                let truncated_name = crate::utils::truncate_process_name(&executable_name);
                debug!(
                    "  eBPF monitoring enabled: executable='{}', truncated='{}'",
                    executable_name, truncated_name
                );
            }
        }
        monitor_manager.reconcile(&config_guard.processes).await?;
    }
    info!("Application Core Logic: Initial reconciliation completed.");

    // 5. 主事件循环 - 等待信号并处理
    loop {
        match SignalHandler::wait_for_signal().await? {
            SignalEvent::ConfigReload => {
                info!("Core Logic: Processing configuration reload...");

                // 重新加载配置
                if let Err(e) = config_manager.reload_config().await {
                    error!("Core Logic: Failed to reload config: {}", e);
                    continue;
                }

                // 重新协调监控器
                let config_guard = config.read().await;
                if let Err(e) = monitor_manager.reconcile(&config_guard.processes).await {
                    error!("Core Logic: Failed to reconcile monitors: {}", e);
                }
            }
            SignalEvent::Shutdown => {
                info!("Core Logic: Initiating graceful shutdown...");
                break;
            }
        }
    }

    // 6. 关闭
    monitor_manager.shutdown().await;
    info!("Application Core Logic: Shutdown completed.");

    // 7. 确保进程正确退出
    info!("Application Core Logic: Exiting process...");
    std::process::exit(0);
}
