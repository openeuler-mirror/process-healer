use healer::config::{
    MonitorConfig, NetworkMonitorFields, PidMonitorFields, ProcessConfig, RecoveryConfig,
    RegularHealerFields,
};
use healer::event_bus::create_event_sender;
use healer::monitor_manager::MonitorManager;

fn pid_process(name: &str, pid_path: &str) -> ProcessConfig {
    ProcessConfig {
        name: name.into(),
        enabled: true,
        command: "/bin/true".into(),
        args: vec![],
        run_as_user: None,
        run_as_root: true,
        working_dir: None,
        monitor: MonitorConfig::Pid(PidMonitorFields {
            pid_file_path: pid_path.into(),
            interval_secs: 1,
        }),
        recovery: RecoveryConfig::Regular(RegularHealerFields {
            retries: 3,
            retry_window_secs: 30,
            cooldown_secs: 10,
        }),
        dependencies: vec![],
    }
}

fn network_process(name: &str, url: &str) -> ProcessConfig {
    ProcessConfig {
        name: name.into(),
        enabled: true,
        command: "/bin/true".into(),
        args: vec![],
        run_as_user: None,
        run_as_root: true,
        working_dir: None,
        monitor: MonitorConfig::Network(NetworkMonitorFields {
            target_url: url.into(),
            interval_secs: 1,
        }),
        recovery: RecoveryConfig::Regular(RegularHealerFields {
            retries: 3,
            retry_window_secs: 30,
            cooldown_secs: 10,
        }),
        dependencies: vec![],
    }
}

fn disabled_process(name: &str) -> ProcessConfig {
    ProcessConfig {
        name: name.into(),
        enabled: false,
        command: "/bin/true".into(),
        args: vec![],
        run_as_user: None,
        run_as_root: true,
        working_dir: None,
        monitor: MonitorConfig::Pid(PidMonitorFields {
            pid_file_path: "/tmp/ignore.pid".into(),
            interval_secs: 1,
        }),
        recovery: RecoveryConfig::Regular(RegularHealerFields {
            retries: 3,
            retry_window_secs: 30,
            cooldown_secs: 10,
        }),
        dependencies: vec![],
    }
}

#[tokio::test]
async fn reconcile_starts_stops_pid_and_network_monitors() {
    let event_tx = create_event_sender();
    let mut manager = MonitorManager::new_without_ebpf(event_tx);

    let initial = vec![
        pid_process("pid_a", "/tmp/pid_a.pid"),
        network_process("net_a", "http://localhost:1234/health"),
        disabled_process("pid_disabled"),
    ];

    manager
        .reconcile(&initial)
        .await
        .expect("initial reconcile should succeed");

    let mut running = manager.running_monitor_names();
    running.sort();
    assert_eq!(
        running,
        vec!["net_a".to_string(), "pid_a".to_string()],
        "PID and network monitors should start for enabled processes"
    );

    let updated = vec![network_process("net_a", "http://localhost:1234/health")];
    manager
        .reconcile(&updated)
        .await
        .expect("second reconcile should succeed");

    let running_after = manager.running_monitor_names();
    assert_eq!(
        running_after,
        vec!["net_a".to_string()],
        "PID monitor should stop when the process is removed"
    );

    manager.shutdown().await;
}
