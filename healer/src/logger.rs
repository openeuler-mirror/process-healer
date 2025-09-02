use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

pub fn init_daemon_logging(
    log_directory: &Path,
) -> Result<WorkerGuard, Box<dyn std::error::Error>> {
    let log_file_name_prefix = "healer.log";

    let file_appender = tracing_appender::rolling::daily(log_directory, log_file_name_prefix);
    let (non_blocking_writer, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter_str = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let env_filter = EnvFilter::try_new(&env_filter_str)
        .map_err(|e| format!("Failed to parse RUST_LOG value '{}': {}", env_filter_str, e))?;

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(non_blocking_writer)
        .with_ansi(false)
        .try_init() // try_init() 返回 Result, init() 会 panic on error
        .map_err(|e| format!("Failed to initialize tracing subscriber: {}", e))?;

    tracing::info!(
        "Logging system initialized. Log directory: {}",
        log_directory.display()
    );
    tracing::info!("Log level configured via RUST_LOG='{}'", env_filter_str);

    Ok(guard)
}
