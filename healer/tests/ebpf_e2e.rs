use std::fs;
// no extra std::io imports needed
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR 指向 healer 子 crate；集成测试期望使用工作区根目录（其父目录）
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(crate_dir)
}
fn cleanup_stray_processes() {
    let base = workspace_root();
    let healer_bin = base.join("target/debug/healer");
    let test_helper = base.join("target/debug/test_process");

    let patterns = vec![
        healer_bin.to_string_lossy().to_string(),
        test_helper.to_string_lossy().to_string(),
    ];

    for pat in patterns {
        let _ = Command::new("pkill").args(["-9", "-f", &pat]).status();
    }
}

fn write_file(path: &str, content: &str) {
    let p = PathBuf::from(path);
    if let Some(parent) = p.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(p, content).expect("write file failed");
}

fn spawn_healer_foreground(config_path: &str) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_healer"));
    cmd.env("HEALER_CONFIG", config_path)
        .env("HEALER_NO_DAEMON", "1")
        .env(
            "RUST_LOG",
            std::env::var("RUST_LOG").unwrap_or_else(|_| {
                "info,healer::monitor::ebpf_monitor=debug,healer_action=debug,healer_event=info,dep_coord=debug".to_string()
            }),
        );
    let inherit_stdio = std::env::var("HEALER_TEST_INHERIT_STDIO")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if inherit_stdio {
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    cmd.spawn().expect("failed to spawn healer")
}

fn wait_secs(s: u64) {
    thread::sleep(Duration::from_secs(s));
}

fn kill_by_pid(pid: i32) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

fn kill_child(mut child: Child) {
    let _ = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status();
    for _ in 0..10 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("/bin/kill")
        .args(["-9", &child.id().to_string()])
        .status();
    let _ = child.wait();
}

fn build_ebpf_config(base: &str) -> String {
    format!(
        r#"
log_level: "info"
log_directory: "{base}/target/debug/healer-tests/logs"
pid_file_directory: "{base}/target/debug/healer-tests/pids"
working_directory: "/"

processes:
  - name: "counter_ebpf"
    enabled: true
    command: "{base}/target/debug/test_process_ebpf"
    args: []
    run_as_root: true
    run_as_user: null
    monitor:
      type: "ebpf"
    recovery:
      type: "regular"
      retries: 3
      retry_window_secs: 10
      cooldown_secs: 5
"#
    )
}

fn ensure_test_binaries() {
    // 创建共享的测试二进制，使用统一的逻辑
    let base = workspace_root();
    let pids_dir = base.join("target/debug/healer-tests/pids");
    let helper_src_dir = base.join("target/debug/healer-tests");
    if let Err(err) = fs::create_dir_all(&helper_src_dir) {
        panic!(
            "failed to create helper source directory {}: {}",
            helper_src_dir.display(),
            err
        );
    }

    // 使用与 process_e2e.rs 相同的测试二进制代码
    let helper_src = format!(
        r#"fn main(){{
        use std::{{fs,thread,time,process,io::{{self,Write}}}};
        let pid = process::id();
        fs::create_dir_all("{}").expect("failed to create pid directory");
        fs::write("{}/counter.pid", pid.to_string()).expect("failed to write pid file");
        let mut n=0u64; loop{{ print!("\r[PID {{}}] alive {{}}", pid,n); io::stdout().flush().expect("flush failed"); thread::sleep(time::Duration::from_secs(1)); n+=1; }}
    }}"#,
        pids_dir.display(),
        pids_dir.display()
    );

    let bin_dir = base.join("target").join("debug");
    let test_bin_src = helper_src_dir.join("test_process_ebpf.rs");
    write_file(test_bin_src.to_str().unwrap(), &helper_src);

    // 为 eBPF 测试使用独立的二进制名称
    let out_bin = bin_dir.join("test_process_ebpf");

    // 如果已存在可执行文件，跳过编译，避免在 sudo 环境下找不到 rustc
    if out_bin.exists() {
        return;
    }

    // 尝试编译，如果失败则提供有用的错误信息
    let result = try_compile_test_binary(&test_bin_src, &out_bin);
    match result {
        Ok(()) => println!("Successfully compiled eBPF test binary"),
        Err(e) => {
            eprintln!("Warning: Failed to compile eBPF test binary: {}", e);
            eprintln!("Hint: If running with sudo, try pre-compiling the binary:");
            eprintln!(
                "  rustc -O {} -o {}",
                test_bin_src.display(),
                out_bin.display()
            );
            eprintln!("Or set RUSTC environment variable to point to rustc executable.");
            panic!("Cannot proceed without test binary");
        }
    }
}

fn try_compile_test_binary(src: &std::path::Path, out: &std::path::Path) -> Result<(), String> {
    // 尝试多种方式找到 rustc
    let rustc_candidates = [
        // 首先尝试环境变量
        std::env::var("RUSTC").ok(),
        // 尝试使用 which 命令
        Command::new("which")
            .arg("rustc")
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|s| !s.is_empty()),
        // 常见路径
        Some("/usr/bin/rustc".to_string()),
        Some("/usr/local/bin/rustc".to_string()),
        Some("/home/lxq/.cargo/bin/rustc".to_string()),
        Some("/root/.cargo/bin/rustc".to_string()),
        // 最后尝试直接调用
        Some("rustc".to_string()),
    ];

    for rustc_opt in rustc_candidates.into_iter().flatten() {
        if let Ok(status) = Command::new(&rustc_opt)
            .args(["-O", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
            .status()
        {
            if status.success() {
                return Ok(());
            } else {
                return Err(format!("rustc compilation failed with status: {}", status));
            }
        }
    }

    Err("Could not find rustc executable".to_string())
}

#[test]
#[ignore]
fn ebpf_detects_exit_and_recovers() {
    // 仅在显式开启时运行（需要 root/capabilities）
    if std::env::var("HEALER_EBPF_E2E")
        .ok()
        .map(|v| v == "1")
        .unwrap_or(false)
        == false
    {
        eprintln!("skipped: set HEALER_EBPF_E2E=1 to run");
        return;
    }

    cleanup_stray_processes();
    ensure_test_binaries();
    let base = workspace_root();
    let cfg_text = build_ebpf_config(base.to_str().unwrap());
    let cfg_path = base.join("target/debug/ebpf_config.yaml");
    write_file(cfg_path.to_str().unwrap(), &cfg_text);

    // 先启动被监控进程，便于观察 eBPF 事件
    let helper_bin = base.join("target/debug/test_process_ebpf");
    let mut child = Command::new(&helper_bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn test_process_ebpf");

    let pid_path = base.join("target/debug/healer-tests/pids/counter.pid");
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent).expect("failed to ensure pid directory exists");
    }

    let helper_pid = child.id() as i32;
    fs::write(&pid_path, helper_pid.to_string()).expect("failed to prime helper pid file");
    let recorded_pid: i32 = fs::read_to_string(&pid_path)
        .expect("failed to read primed pid file")
        .trim()
        .parse()
        .expect("primed pid file contains invalid pid");
    assert_eq!(
        recorded_pid, helper_pid,
        "PID file content mismatch: expected {}, got {}",
        helper_pid, recorded_pid
    );

    let healer = spawn_healer_foreground(cfg_path.to_str().unwrap());
    // 等待 eBPF 初始化与 watch 生效
    wait_secs(3);

    // 基线 PID
    let first_pid: i32 = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert!(first_pid > 0, "invalid baseline pid");

    // 触发退出
    kill_by_pid(first_pid);
    let _ = child.wait();

    // 期望 healer 通过 eBPF 事件拉起新进程（PID 变化）
    let mut new_pid = first_pid;
    for _ in 0..20 {
        wait_secs(1);
        if let Ok(s) = fs::read_to_string(&pid_path) {
            if let Ok(p) = s.trim().parse::<i32>() {
                if p > 0 && p != first_pid {
                    new_pid = p;
                    break;
                }
            }
        }
    }
    assert!(
        new_pid > 0 && new_pid != first_pid,
        "ebpf did not trigger restart"
    );

    // 清理
    kill_child(healer);
    if let Ok(s) = fs::read_to_string(&pid_path) {
        if let Ok(p) = s.trim().parse::<i32>() {
            kill_by_pid(p);
        }
    }
    cleanup_stray_processes();
}
