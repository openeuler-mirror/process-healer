mod config;
mod config_manager;
mod coordinator; // expose dependency coordinator
mod core_logic;
mod daemon_handler;
mod event_bus;
mod logger;
mod monitor;
mod monitor_manager;
mod publisher;
mod service_manager;
mod signal_handler;
mod subscriber;
mod utils;
use config::AppConfig;
use daemon_handler::run_as_daemon;
use std::env;
use tokio::sync::RwLock;

fn main() {
    // Support overriding config path and running in foreground for tests/dev.
    let config_file_path_str =
        env::var("HEALER_CONFIG").unwrap_or_else(|_| "config.yaml".to_string());
    let run_foreground = matches!(
        env::var("HEALER_NO_DAEMON")
            .unwrap_or_else(|_| "0".to_string())
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );

    println!(
        "Attempting to load the config from {}",
        config_file_path_str
    );

    let absolue_config_path = match std::fs::canonicalize(&config_file_path_str) {
        Ok(path) => path,
        Err(e) => {
            eprintln!(
                "Error: No such file or directory about configure '{}': {}",
                config_file_path_str, e
            );
            std::process::exit(1);
        }
    };

    let initial_config = AppConfig::load_from_file(&absolue_config_path).expect("初始配置加载失败");
    let shared_config = std::sync::Arc::new(RwLock::new(initial_config));

    if run_foreground {
        // Minimal stdout logger for foreground mode; respects RUST_LOG.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_ansi(true)
            .try_init();

        // Run core logic directly without daemonizing (useful for tests).
        core_logic::async_runtime(std::sync::Arc::clone(&shared_config), absolue_config_path);
        return;
    }

    // Default: run as daemon.
    let config_for_closure = std::sync::Arc::clone(&shared_config);
    let path_for_closure = absolue_config_path.clone();
    let core_logic_closure =
        move || core_logic::async_runtime(config_for_closure, path_for_closure);
    match run_as_daemon(shared_config, core_logic_closure) {
        Ok(_) => {
            println!("Main program: Core logic quit");
        }
        Err(e) => {
            println!("Main program: Core logic error with {:?}", e);
        }
    }
}
