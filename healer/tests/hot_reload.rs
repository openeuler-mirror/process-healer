use healer::config::{
    AppConfig, MonitorConfig, PidMonitorFields, ProcessConfig, RecoveryConfig, RegularHealerFields,
};
use healer::subscriber::process_healer::ProcessHealer;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

fn make_process(
    name: &str,
    command: impl Into<String>,
    args: Vec<String>,
    pid_dir: &Path,
) -> ProcessConfig {
    ProcessConfig {
        name: name.to_string(),
        enabled: true,
        command: command.into(),
        args,
        run_as_user: None,
        run_as_root: true,
        working_dir: None,
        monitor: MonitorConfig::Pid(PidMonitorFields {
            pid_file_path: pid_dir.join(format!("{name}.pid")),
            interval_secs: 1,
        }),
        recovery: RecoveryConfig::Regular(RegularHealerFields {
            retries: 3,
            retry_window_secs: 60,
            cooldown_secs: 30,
        }),
        dependencies: vec![],
    }
}

fn make_config(base_dir: &Path, processes: Vec<ProcessConfig>) -> AppConfig {
    AppConfig {
        log_level: None,
        log_directory: Some(base_dir.join("logs")),
        pid_file_directory: Some(base_dir.join("pids")),
        processes,
        working_directory: Some(base_dir.to_path_buf()),
    }
}

#[tokio::test]
async fn hot_reload_allows_new_process_recovery() -> anyhow::Result<()> {
    let temp_dir = TempDir::new()?;
    let base_path = temp_dir.path();
    let pid_dir = base_path.join("pids");
    std::fs::create_dir_all(&pid_dir)?;

    let initial_config = make_config(
        base_path,
        vec![make_process("alpha", "/bin/true", vec![], &pid_dir)],
    );
    let shared = Arc::new(RwLock::new(initial_config));
    let (tx, _) = broadcast::channel(8);
    let rx = tx.subscribe();

    let mut healer = ProcessHealer::new(rx, Arc::clone(&shared)).await;

    let marker_path = base_path.join("beta_marker.log");
    let script = format!("echo hot >> {}", marker_path.display());
    let beta_process = make_process("beta", "/bin/sh", vec!["-c".into(), script], &pid_dir);

    {
        let mut guard = shared.write().await;
        *guard = make_config(base_path, vec![beta_process]);
    }

    healer.heal_process(&"alpha".to_string()).await;
    assert!(
        !marker_path.exists(),
        "Marker file should not exist before beta recovery is triggered"
    );

    healer.heal_process(&"beta".to_string()).await;
    sleep(Duration::from_millis(300)).await;

    assert!(
        marker_path.exists(),
        "Expected beta recovery command to run after reload"
    );
    let content = std::fs::read_to_string(&marker_path)?;
    assert!(
        content.contains("hot"),
        "Marker file should contain recovery output"
    );

    Ok(())
}
