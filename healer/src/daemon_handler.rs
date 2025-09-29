use crate::config::AppConfig;
use crate::logger;
use daemonize::Daemonize;
use std::path::PathBuf;

#[derive(Debug)]
#[allow(dead_code)] // Error fields preserved for error context
pub enum DaemonError {
    Io(std::io::Error),
    Daemonize(daemonize::Error),
}
impl From<std::io::Error> for DaemonError {
    fn from(err: std::io::Error) -> DaemonError {
        DaemonError::Io(err)
    }
}
impl From<daemonize::Error> for DaemonError {
    fn from(err: daemonize::Error) -> DaemonError {
        DaemonError::Daemonize(err)
    }
}

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub pid_file: PathBuf,
    pub log_directory: PathBuf,
    pub working_dir: PathBuf,
}
impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            pid_file: PathBuf::from("/tmp/healer.pid"),
            log_directory: PathBuf::from("/tmp/healer"),
            working_dir: PathBuf::from("/"),
        }
    }
}

pub fn run_as_daemon<F>(
    config: std::sync::Arc<tokio::sync::RwLock<AppConfig>>,
    core_logic_fn: F,
) -> Result<(), DaemonError>
where
    F: FnOnce() + Send + 'static,
{
    let daemon_config = {
        let config_guard = config.blocking_read();
        config_guard.to_daemonize_config()
    };

    println!("Starting the daemon process");
    println!("PID file: {:?}", daemon_config.pid_file);
    println!(
        "Healer-Process Log Directory: {:?}",
        daemon_config.log_directory
    );
    let daemonizer = Daemonize::new()
        .pid_file(&daemon_config.pid_file)
        .chown_pid_file(false)
        .working_directory(&daemon_config.working_dir)
        .umask(0o027)
        .privileged_action(|| {
            //Todo
            println!("Child process: Successfully set the hook fn.");
        });
    match daemonizer.start() {
        Ok(_) => {
            let log_file_path = &daemon_config.log_directory;
            let log_guard = match logger::init_daemon_logging(log_file_path) {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!("Failed to initialize logging: {}. Exiting.", e);
                    std::process::exit(1);
                }
            };

            // 执行核心逻辑
            core_logic_fn();

            // 在进程退出前，显式地等待日志系统完成
            std::mem::drop(log_guard);
            std::thread::sleep(std::time::Duration::from_millis(100));

            Ok(())
        }
        Err(e) => {
            eprintln!("Parents process: Failed to begin the daemon process: {}", e);
            Err(DaemonError::Daemonize(e))
        }
    }
}
