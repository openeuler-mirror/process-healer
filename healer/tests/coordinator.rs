use healer::config::{
    AppConfig, DependencyConfig, DependencyKind, MonitorConfig, NetworkMonitorFields, OnFailure,
    ProcessConfig, RawDependency, RecoveryConfig, RegularHealerFields,
};
use healer::coordinator::dependency_coordinator::DependencyCoordinator;
use healer::event_bus::{create_event_sender, ProcessEvent};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

fn mk_process(name: &str, deps: Vec<RawDependency>) -> ProcessConfig {
    ProcessConfig {
        name: name.to_string(),
        enabled: true,
        command: "/bin/true".to_string(),
        args: vec![],
        run_as_user: None,
        run_as_root: true,
        working_dir: None,
        monitor: MonitorConfig::Network(NetworkMonitorFields {
            target_url: "http://127.0.0.1:1/health".to_string(),
            interval_secs: 60,
        }),
        recovery: RecoveryConfig::Regular(RegularHealerFields {
            retries: 1,
            retry_window_secs: 5,
            cooldown_secs: 5,
        }),
        dependencies: deps,
    }
}

#[tokio::test]
async fn defers_then_releases_on_timeout_skip() {
    let dep = RawDependency::Detailed(DependencyConfig {
        target: "B".to_string(),
        kind: DependencyKind::Requires,
        hard: true,
        max_wait_secs: 1,
        on_failure: OnFailure::Skip,
    });
    let cfg = AppConfig {
        log_level: None,
        log_directory: None,
        pid_file_directory: None,
        working_directory: Some(PathBuf::from("/")),
        processes: vec![mk_process("A", vec![dep]), mk_process("B", vec![])],
    };
    let shared = Arc::new(RwLock::new(cfg));

    let in_tx = create_event_sender();
    let in_rx = in_tx.subscribe();
    let out_tx = create_event_sender();
    let mut out_rx = out_tx.subscribe();

    // 启动协调器循环
    let coordinator = DependencyCoordinator::new(in_rx, out_tx.clone(), Arc::clone(&shared));
    tokio::spawn(async move {
        coordinator.run_loop().await;
    });

    // 1) B 先下线，标记为恢复中
    let _ = in_tx.send(ProcessEvent::ProcessDown {
        name: "B".to_string(),
        pid: 123,
    });
    // 留出处理时间
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // 2) A 下线，应被延后（不应立刻转发）
    let _ = in_tx.send(ProcessEvent::ProcessDown {
        name: "A".to_string(),
        pid: 456,
    });

    // 短超时内不应收到 A 的转发
    let mut got_immediate = false;
    if let Ok(evt) =
        tokio::time::timeout(std::time::Duration::from_millis(200), out_rx.recv()).await
    {
        // 若收到 A，则延后逻辑失败
        if let Ok(ProcessEvent::ProcessDown { name, .. }) = evt {
            if name == "A" {
                got_immediate = true;
            }
        }
    }
    assert!(
        !got_immediate,
        "A should have been deferred due to B recovering"
    );

    // 约 5s 后重试，最多等 ~8s
    let mut forwarded = false;
    for _ in 0..16 {
        if let Ok(Ok(ProcessEvent::ProcessDown { name, .. })) =
            tokio::time::timeout(std::time::Duration::from_millis(500), out_rx.recv()).await
        {
            if name == "A" {
                forwarded = true;
                break;
            }
        }
    }
    assert!(
        forwarded,
        "A should be forwarded after dependency timeout with Skip policy"
    );
}
