use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

fn cleanup_stray_processes() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let healer_bin = base.join("target/debug/healer");
    let test_helper = base.join("target/debug/test_process");
    let dummy_py = base.join("tests/fixtures/dummy_service.py");

    let patterns = vec![
        healer_bin.to_string_lossy().to_string(),
        test_helper.to_string_lossy().to_string(),
        dummy_py.to_string_lossy().to_string(),
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
                "info,healer::monitor::pid_monitor=debug,healer_action=debug,healer_event=info,dep_coord=debug".to_string()
            }),
        );
    // 测试时可继承 stdio（HEALER_TEST_INHERIT_STDIO=1）
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
    // 先优雅退出（SIGINT），必要时再强杀（SIGKILL）
    let _ = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status();
    for _ in 0..10 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        wait_secs(0); // yield
        thread::sleep(Duration::from_millis(50));
    }
    let _ = Command::new("/bin/kill")
        .args(["-9", &child.id().to_string()])
        .status();
    let _ = child.wait();
}

fn build_temp_config(base: &str) -> String {
    format!(
        r#"
log_level: "info"
log_directory: "/tmp/healer-tests/logs"
pid_file_directory: "/tmp/healer-tests/pids"
working_directory: "/"

processes:
  - name: "counter"
    enabled: true
    command: "{base}/target/debug/test_process"
    args: []
    run_as_root: true
    run_as_user: null
    monitor:
      type: "pid"
      pid_file_path: "/tmp/healer-tests/pids/counter.pid"
      interval_secs: 1
    recovery:
      type: "regular"
      retries: 3
      retry_window_secs: 10
      cooldown_secs: 5

  - name: "dummy_net"
    enabled: true
    command: "/usr/bin/python3"
    args: ["{base}/tests/fixtures/dummy_service.py"]
    run_as_root: true
    run_as_user: null
    monitor:
      type: "network"
      target_url: "http://127.0.0.1:8080/health"
      interval_secs: 1
    recovery:
      type: "regular"
      retries: 2
      retry_window_secs: 5
      cooldown_secs: 4
"#
    )
}

fn ensure_test_binaries() {
    let helper_src = r#"fn main(){
        use std::{fs,thread,time,process,io::{self,Write}};
        let pid = process::id();
        let _ = fs::create_dir_all("/tmp/healer-tests/pids");
        let _ = fs::write("/tmp/healer-tests/pids/counter.pid", pid.to_string());
        let mut n=0u64; loop{ print!("\\r[PID {}] alive {}", pid,n); let _=io::stdout().flush(); thread::sleep(time::Duration::from_secs(1)); n+=1; }
    }"#;
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bin_dir = base.join("target").join("debug");
    let test_bin_src = base.join("tests/fixtures/test_process.rs");
    let _ = fs::create_dir_all(test_bin_src.parent().unwrap());
    write_file(test_bin_src.to_str().unwrap(), helper_src);
    let out_bin = bin_dir.join("test_process");
    let status = Command::new("rustc")
        .args([
            "-O",
            test_bin_src.to_str().unwrap(),
            "-o",
            out_bin.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run rustc for test helper");
    assert!(status.success(), "failed to build test helper bin");
}

#[test]
fn restart_on_pid_exit_and_circuit_breaker() {
    cleanup_stray_processes();
    ensure_test_binaries();
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cfg_text = build_temp_config(base.to_str().unwrap());
    let cfg_path = base.join("target/debug/it_config.yaml");
    write_file(cfg_path.to_str().unwrap(), &cfg_text);

    // 先启动 helper，保证 PID 文件存在
    let helper_bin = base.join("target/debug/test_process");
    let mut initial = Command::new(helper_bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn initial test_process");
    // 等待 PID 文件就绪
    let pid_path = "/tmp/healer-tests/pids/counter.pid";
    for _ in 0..10 {
        if fs::read_to_string(pid_path).is_ok() {
            break;
        }
        wait_secs(1);
    }

    let mut healer = spawn_healer_foreground(cfg_path.to_str().unwrap());
    wait_secs(2);

    // 检查 healer 是否仍在运行
    match healer.try_wait() {
        Ok(Some(status)) => {
            panic!("Healer exited early with status: {:?}", status);
        }
        Ok(None) => {
            println!("Healer is running normally");
        }
        Err(e) => {
            panic!("Error checking healer status: {}", e);
        }
    }

    let first_pid: i32 = fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if first_pid > 0 {
        kill_by_pid(first_pid);
        // 回收初始子进程，避免僵尸进程
        let _ = initial.wait();
    }

    // 等待重启
    let mut new_pid = first_pid;
    println!("Waiting for restart. Original PID: {}", first_pid);
    for i in 0..15 {
        wait_secs(1);
        if let Ok(s) = fs::read_to_string(pid_path) {
            if let Ok(p) = s.trim().parse::<i32>() {
                println!("Iteration {}: PID file contains: {}", i + 1, p);
                if p > 0 && p != first_pid {
                    new_pid = p;
                    println!(
                        "Process restarted with new PID {} after {} seconds",
                        p,
                        i + 1
                    );
                    break;
                }
            }
        } else {
            println!("Iteration {}: Could not read PID file", i + 1);
        }
        if i % 5 == 4 {
            let current_pid = fs::read_to_string(pid_path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            println!(
                "Still waiting for restart... Current PID: {}, Original PID: {}",
                current_pid, first_pid
            );
        }
    }
    assert!(
        new_pid > 0 && new_pid != first_pid,
        "healer did not restart the counter process"
    );

    // 快速杀 3 次触发熔断（与配置一致）
    for _ in 0..3 {
        kill_by_pid(new_pid);
        let last = new_pid;
        for _ in 0..10 {
            wait_secs(1);
            if let Ok(s) = fs::read_to_string(pid_path) {
                if let Ok(p) = s.trim().parse::<i32>() {
                    if p != last {
                        new_pid = p;
                        break;
                    }
                }
            }
        }
    }

    // 再杀一次应被冷却期拦住，~3s 内不应重启
    kill_by_pid(new_pid);
    let old = new_pid;
    wait_secs(3);
    let after: i32 = fs::read_to_string(pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert_eq!(after, old, "circuit breaker did not hold during cooldown");

    // 冷却后应恢复重试
    let mut restarted = false;
    for _ in 0..6 {
        wait_secs(1);
        if let Ok(s) = fs::read_to_string(pid_path) {
            if let Ok(p) = s.trim().parse::<i32>() {
                if p != old {
                    restarted = true;
                    break;
                }
            }
        }
    }
    assert!(restarted, "healer did not attempt restart after cooldown");

    // 清理：结束 healer 与最后的 helper
    kill_child(healer);
    if let Ok(s) = fs::read_to_string(pid_path) {
        if let Ok(p) = s.trim().parse::<i32>() {
            kill_by_pid(p);
        }
    }
    cleanup_stray_processes();
}

#[test]
fn network_monitor_detects_crash_and_recovers() {
    cleanup_stray_processes();
    ensure_test_binaries();
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dummy_py = r#"import http.server, socketserver, sys
socketserver.TCPServer.allow_reuse_address = True
PORT=8080
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/health':
            self.send_response(200); self.end_headers(); self.wfile.write(b'OK')
        elif self.path == '/crash':
            self.send_response(200); self.end_headers(); self.wfile.write(b'DIE'); sys.exit(1)
        else:
            self.send_response(404); self.end_headers()
with socketserver.TCPServer(("", PORT), H) as srv:
    srv.serve_forever()
"#;
    let py_path = base.join("tests/fixtures/dummy_service.py");
    write_file(py_path.to_str().unwrap(), dummy_py);

    let cfg_text = build_temp_config(base.to_str().unwrap());
    let cfg_path = base.join("target/debug/net_config.yaml");
    write_file(cfg_path.to_str().unwrap(), &cfg_text);

    let healer = spawn_healer_foreground(cfg_path.to_str().unwrap());
    wait_secs(2);

    fn http_get(path: &str) -> Option<String> {
        use std::net::TcpStream;
        let mut stream = TcpStream::connect("127.0.0.1:8080").ok()?;
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            path
        );
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }
    fn is_healthy() -> bool {
        http_get("/health")
            .map(|r| r.starts_with("HTTP/1.1 200") || r.contains("OK"))
            .unwrap_or(false)
    }

    for _ in 0..10 {
        if is_healthy() {
            break;
        }
        wait_secs(1);
    }
    let _ = http_get("/crash");
    let mut healthy = false;
    for _ in 0..20 {
        if is_healthy() {
            healthy = true;
            break;
        }
        wait_secs(1);
    }
    assert!(healthy, "network monitor did not recover dummy service");

    let _ = http_get("/crash");
    kill_child(healer);
    cleanup_stray_processes();
}
