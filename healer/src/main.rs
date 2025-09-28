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
use std::path::PathBuf;
use tokio::sync::RwLock;

use clap::Parser;

/// Command line options for healer
#[derive(Debug, Parser)]
#[command(author, version, about = "Process self-healing daemon", long_about = None)]
struct Cli {
    /// Path to configuration file (YAML). If not provided, search order applies.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Run in foreground (do not daemonize). Equivalent to env HEALER_NO_DAEMON=1
    #[arg(long)]
    foreground: bool,

    /// Print the path that was selected for configuration and exit
    #[arg(long)]
    print_config_path: bool,
}

fn candidate_config_paths(explicit: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(p) = explicit {
        return vec![p];
    }

    let mut cands = Vec::new();
    // 1. Environment variable
    if let Ok(p) = env::var("HEALER_CONFIG") {
        cands.push(PathBuf::from(p));
    }
    // 2. Current working directory
    cands.push(PathBuf::from("./config.yaml"));
    cands.push(PathBuf::from("./healer.yaml"));
    // 3. /etc/healer/
    cands.push(PathBuf::from("/etc/healer/config.yaml"));
    cands.push(PathBuf::from("/etc/healer/healer.yaml"));
    // 4. XDG config home if set
    if let Ok(home) = env::var("XDG_CONFIG_HOME") {
        cands.push(PathBuf::from(home).join("healer/config.yaml"));
    }
    // 5. ~/.config/healer/config.yaml
    if let Some(home_dir) = dirs_next::home_dir() {
        cands.push(home_dir.join(".config/healer/config.yaml"));
    }
    cands
}

fn resolve_config_path(cli: &Cli) -> PathBuf {
    if let Some(explicit) = &cli.config {
        return explicit.clone();
    }
    if let Ok(env_path) = env::var("HEALER_CONFIG") {
        return PathBuf::from(env_path);
    }
    for cand in candidate_config_paths(None) {
        if cand.exists() {
            return cand;
        }
    }
    // Fallback default (will likely fail later if missing)
    PathBuf::from("config.yaml")
}

fn main() {
    let cli = Cli::parse();

    // Determine final config path
    let raw_config_path = resolve_config_path(&cli);
    println!("Config resolution: using {:?}", raw_config_path);

    if cli.print_config_path {
        println!("{:?}", raw_config_path);
        return;
    }

    // Expand & canonicalize for safety
    let absolute_config_path = match std::fs::canonicalize(&raw_config_path) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("Error: cannot access config {:?}: {}", raw_config_path, e);
            std::process::exit(1);
        }
    };

    let initial_config =
        AppConfig::load_from_file(&absolute_config_path).expect("初始配置加载失败");
    let shared_config = std::sync::Arc::new(RwLock::new(initial_config));

    // Detect foreground from either flag or env
    let env_foreground = matches!(
        env::var("HEALER_NO_DAEMON")
            .unwrap_or_else(|_| "0".into())
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    let run_foreground = cli.foreground || env_foreground;

    if run_foreground {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_ansi(true)
            .try_init();
        core_logic::async_runtime(std::sync::Arc::clone(&shared_config), absolute_config_path);
        return;
    }

    let config_for_closure = std::sync::Arc::clone(&shared_config);
    let path_for_closure = absolute_config_path.clone();
    let core_logic_closure =
        move || core_logic::async_runtime(config_for_closure, path_for_closure);
    match run_as_daemon(shared_config, core_logic_closure) {
        Ok(_) => println!("Main program: Core logic quit"),
        Err(e) => println!("Main program: Core logic error with {:?}", e),
    }
}
