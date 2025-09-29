use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

struct TestContext {
    temp_dir: TempDir,
    port: u16,
    children: Vec<Child>,
}

impl TestContext {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");

        // 动态分配端口
        let port = find_free_port();

        Self {
            temp_dir,
            port,
            children: Vec::new(),
        }
    }

    fn temp_path(&self) -> &std::path::Path {
        self.temp_dir.path()
    }

    fn logs_dir(&self) -> PathBuf {
        let logs = self.temp_path().join("logs");
        fs::create_dir_all(&logs).expect("Failed to create logs directory");
        logs
    }

    fn pids_dir(&self) -> PathBuf {
        let pids = self.temp_path().join("pids");
        fs::create_dir_all(&pids).expect("Failed to create pids directory");
        pids
    }

    fn add_child(&mut self, child: Child) {
        self.children.push(child);
    }

    fn cleanup(&mut self) {
        // 清理所有子进程
        for mut child in self.children.drain(..) {
            kill_child(&mut child);
        }
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn find_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("Failed to bind to a random port")
        .local_addr()
        .expect("Failed to get local address")
        .port()
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR 指向 healer 子 crate；集成测试期望使用工作区根目录（其父目录）
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(crate_dir)
}

fn write_file(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, content).expect("write file failed");
}

fn spawn_healer_foreground(config_path: &PathBuf) -> Child {
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

fn kill_child(child: &mut Child) {
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

fn build_pid_only_config(ctx: &TestContext) -> String {
    let base = workspace_root();
    let logs_dir = ctx.logs_dir();
    let pids_dir = ctx.pids_dir();
    let test_id = ctx.temp_path().file_name().unwrap().to_string_lossy();

    format!(
        r#"
log_level: "info"
log_directory: "{}"
pid_file_directory: "{}"
working_directory: "/"

processes:
  - name: "counter"
    enabled: true
    command: "{}/target/debug/test_process_{}"
    args: []
    run_as_root: true
    run_as_user: null
    monitor:
      type: "pid"
      pid_file_path: "{}/counter.pid"
      interval_secs: 1
    recovery:
      type: "regular"
      retries: 3
      retry_window_secs: 10
      cooldown_secs: 5
"#,
        logs_dir.display(),
        pids_dir.display(),
        base.display(),
        test_id,
        pids_dir.display()
    )
}

fn build_network_only_config(ctx: &TestContext) -> String {
    let logs_dir = ctx.logs_dir();
    let pids_dir = ctx.pids_dir();

    format!(
        r#"
log_level: "info"
log_directory: "{}"
pid_file_directory: "{}"
working_directory: "/"

processes:
  - name: "dummy_net"
    enabled: true
    command: "/usr/bin/python3"
    args: ["{}/dummy_service.py"]
    run_as_root: true
    run_as_user: null
    monitor:
      type: "network"
      target_url: "http://127.0.0.1:{}/health"
      interval_secs: 1
    recovery:
      type: "regular"
      retries: 2
      retry_window_secs: 5
      cooldown_secs: 4
"#,
        logs_dir.display(),
        pids_dir.display(),
        ctx.temp_path().display(),
        ctx.port
    )
}

fn ensure_test_binaries(ctx: &TestContext) {
    let pids_dir = ctx.pids_dir();
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

    let base = workspace_root();
    let bin_dir = base.join("target").join("debug");
    let test_bin_src = ctx.temp_path().join("test_process.rs");
    write_file(&test_bin_src, &helper_src);

    // 为每个测试创建唯一的二进制文件名
    let test_id = ctx.temp_path().file_name().unwrap().to_string_lossy();
    let out_bin = bin_dir.join(format!("test_process_{}", test_id));
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
    let mut ctx = TestContext::new();
    ensure_test_binaries(&ctx);

    let cfg_text = build_pid_only_config(&ctx);
    let cfg_path = ctx.temp_path().join("it_config.yaml");
    write_file(&cfg_path, &cfg_text);

    // 先启动 helper，保证 PID 文件存在
    let base = workspace_root();
    let test_id = ctx.temp_path().file_name().unwrap().to_string_lossy();
    let helper_bin = base
        .join("target")
        .join("debug")
        .join(format!("test_process_{}", test_id));
    let mut initial = Command::new(helper_bin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn initial test_process");

    // 直接写入 PID 文件，消除 helper 自行写入的竞态
    let pid_path = ctx.pids_dir().join("counter.pid");
    let initial_pid = initial.id() as i32;
    fs::write(&pid_path, initial_pid.to_string())
        .expect("failed to prime PID file for initial helper");

    let recorded_pid: i32 = fs::read_to_string(&pid_path)
        .expect("failed to read primed PID file")
        .trim()
        .parse()
        .expect("primed PID file contains invalid pid");
    assert_eq!(
        recorded_pid, initial_pid,
        "PID file content mismatch: expected {}, got {}",
        initial_pid, recorded_pid
    );

    println!(
        "Initial process started with PID: {} (primed at {})",
        initial_pid,
        pid_path.display()
    );

    let healer = spawn_healer_foreground(&cfg_path);
    ctx.add_child(healer);
    wait_secs(3); // 增加等待时间以确保 healer 完全启动

    // 检查 healer 是否仍在运行
    if let Some(healer) = ctx.children.last_mut() {
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
    }

    let first_pid: i32 = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // 验证我们有有效的初始 PID
    assert!(first_pid > 0, "No valid initial PID found: {}", first_pid);

    if first_pid > 0 {
        kill_by_pid(first_pid);
        // 等待初始进程退出
        let _ = initial.wait();
    }

    // 等待重启
    let mut new_pid = first_pid;
    println!("Waiting for restart. Original PID: {}", first_pid);
    for i in 0..15 {
        wait_secs(1);
        if let Ok(s) = fs::read_to_string(&pid_path) {
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
            let current_pid = fs::read_to_string(&pid_path)
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
            if let Ok(s) = fs::read_to_string(&pid_path) {
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
    let after: i32 = fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert_eq!(after, old, "circuit breaker did not hold during cooldown");

    // 冷却后应恢复重试
    let mut restarted = false;
    for _ in 0..6 {
        wait_secs(1);
        if let Ok(s) = fs::read_to_string(&pid_path) {
            if let Ok(p) = s.trim().parse::<i32>() {
                if p != old {
                    restarted = true;
                    break;
                }
            }
        }
    }
    assert!(restarted, "healer did not attempt restart after cooldown");

    // 清理最后的 helper
    if let Ok(s) = fs::read_to_string(&pid_path) {
        if let Ok(p) = s.trim().parse::<i32>() {
            kill_by_pid(p);
        }
    }
    // TestContext 的 drop 会自动清理所有子进程
}

#[test]
fn network_monitor_detects_crash_and_recovers() {
    let mut ctx = TestContext::new();
    // ensure_test_binaries(&ctx);

    let dummy_py = format!(
        r#"import http.server, socketserver, sys
socketserver.TCPServer.allow_reuse_address = True
PORT={}
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
"#,
        ctx.port
    );

    let py_path = ctx.temp_path().join("dummy_service.py");
    write_file(&py_path, &dummy_py);

    let cfg_text = build_network_only_config(&ctx);
    let cfg_path = ctx.temp_path().join("net_config.yaml");
    write_file(&cfg_path, &cfg_text);

    let healer = spawn_healer_foreground(&cfg_path);
    ctx.add_child(healer);
    wait_secs(2);

    fn http_get(port: u16, path: &str) -> Option<String> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).ok()?;
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            path
        );
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }

    let is_healthy = |port: u16| -> bool {
        http_get(port, "/health")
            .map(|r| r.starts_with("HTTP/1.1 200") || r.contains("OK"))
            .unwrap_or(false)
    };

    for _ in 0..10 {
        if is_healthy(ctx.port) {
            break;
        }
        wait_secs(1);
    }

    let _ = http_get(ctx.port, "/crash");
    let mut healthy = false;
    for _ in 0..20 {
        if is_healthy(ctx.port) {
            healthy = true;
            break;
        }
        wait_secs(1);
    }
    assert!(healthy, "network monitor did not recover dummy service");

    let _ = http_get(ctx.port, "/crash");
    // TestContext 的 drop 会自动清理所有子进程
}
