# process-healer
## 介绍
A high-performance daemon leveraging eBPF for reliable, low-overhead monitoring and automatic recovery of critical processes to ensure service continuity.

## 快速上手指南

下面的流程使用仓库内的示例进程 `simple_test_process`，演示 Healer 如何监控并自动拉起目标进程。

### 前置条件
- Linux x86_64（Fedora/CentOS 等 systemd 发行版均可）
- Rust stable toolchain（用于编译 Healer 与示例进程）

### 步骤

1. 获取代码并编译所需二进制：
  ```bash
  git clone https://gitee.com/你/仓库地址.git
  cd healer-process
  cargo build -p healer -p simple_test_process
  ```

2. 使用仓库自带脚本生成示例所需目录（默认创建在项目根目录的 `healer-demo/` 下）：
  ```bash
  chmod +x simple_test_process/init.sh
  ./simple_test_process/init.sh
  ```

3. 编写快速体验配置 `quickstart-config.yaml`：
  ```bash
  cat <<EOF > quickstart-config.yaml
  log_level: "info"
  log_directory: "$(pwd)/healer-demo/logs"
  pid_file_directory: "$(pwd)/healer-demo/run"
  working_directory: "$(pwd)"
  processes:
    - name: "demo_counter"
      enabled: true
      command: "$(pwd)/target/debug/simple_test_process"
      args: []
      run_as_root: false
      monitor:
        type: "pid"
        pid_file_path: "$(pwd)/healer-demo/run/simple_counter.pid"
        interval_secs: 3
      recovery:
        type: "regular"
        retries: 3
        retry_window_secs: 60
        cooldown_secs: 180
  EOF
  ```

4. 在终端 A 启动示例进程（它会写入 PID 文件并持续输出心跳）：
  ```bash
  target/debug/simple_test_process
  ```

5. 在终端 B 前台启动 Healer 并加载刚才的配置：
  ```bash
  HEALER_NO_DAEMON=1 HEALER_CONFIG=$(pwd)/quickstart-config.yaml RUST_LOG=info target/debug/healer
  ```

6. 体验自动恢复：在终端 A 或新的终端里执行并观察 Healer 日志出现重启信息（PID 会变化）：
  ```bash
  pkill -x simple_test_process
  ```

  Healer 会捕获退出事件并重新拉起示例进程。按 `Ctrl+C` 可结束 Healer，结束后记得清理临时文件/目录。

## 安装与编译

### 从源码编译
```
RUST_LOG=info cargo run --config 'target."cfg(all())".runner="sudo -E"'
```
用非root权限进行cargo build，并以root权限执行二进制

日志的位置可以由用户自己在 `config.yaml`中定义：
```YAML
# 全局配置
log_level: "info" #日志输出等级，可以调整为debug/tracing发现更多信息，不过会被RUST_LOG环境变量覆盖
log_directory: "/var/log/healer" #日志文件地址，本地址需要root权限，用户可以放在自己定义的位置下。
pid_file_directory: "/var/run/healer" # healer 守护进程自己的 PID 文件目录，用户可以放在自己定义的位置下。
working_directory: "/" #工作目录，默认是根目录
```

### RPM 打包与安装
本仓库提供了 RPM 打包脚本与规范文件，帮助你在基于 RPM 的发行版上安装为系统服务：

- 规范文件：`packaging/rpm/healer.spec`
- systemd 单元：`packaging/systemd/healer.service`
- 构建脚本：`scripts/build-rpm.sh`

步骤：
1. 安装依赖（以 Fedora 为例）：
  ```bash
  sudo dnf install -y rpm-build rpmdevtools gcc clang llvm rust cargo make systemd rsync
  ```
  需要 `bpf-linker` 用于 eBPF 对象链接；脚本会尝试用 `cargo install bpf-linker` 自动安装。

2. 安装 Rust nightly toolchain（eBPF 构建所需）：
  ```bash
  rustup toolchain install nightly
  rustup component add rust-src --toolchain nightly
  ```

3. 构建 RPM：
  ```bash
  bash scripts/build-rpm.sh
  ```
  完成后，生成的二进制包位于 `~/rpmbuild/RPMS/<arch>/healer-<version>-1.<dist>.<arch>.rpm`。

4. 安装与管理：
  ```bash
  sudo rpm -Uvh ~/rpmbuild/RPMS/*/healer-*.rpm
  sudo systemctl enable --now healer
  sudo systemctl status healer
  ```

安装后的文件布局：
- 可执行文件：`/usr/bin/healer`
- 配置文件：`/etc/healer/config.yaml`（标记为 `%config(noreplace)`，升级不会覆盖本地修改）
- 日志目录：`/var/log/healer`
- 运行目录：`/var/run/healer`
- systemd unit：`/usr/lib/systemd/system/healer.service`

卸载：
```bash
sudo systemctl disable --now healer
sudo rpm -e healer
```

## 使用说明

### 配置文件
配置文件在根目录下的`config.yaml`
配置文件采用了`serde`库进行解析。支持 PID eBPF Network 三种工作模式，具体的使用实例可以参考yaml中已有的样例。
```YAML
  - name: "simple_counter" #名字 用于日志
    enabled: true #启用开关
    command: "/home/lxq/ospp/simple_test_process/target/debug/simple_test_process" #恢复命令，在无pid文件时作为进程的主键来识别
    args: [] #恢复命令的参数
    run_as_root: false #进程是否已root进行恢复重启
    run_as_user: "lxq" #如果非root，则以某个用户的身份重启
    monitor:
      type: "pid" # 使用 PID 文件进行监控
      pid_file_path: "/var/run/healer/simple_counter.pid" # pid监控模式应该有对应的pid文件
      interval_secs: 3 # 轮询间隔，单位秒
    # 恢复/重启策略配置
    recovery:
      type: "regular" # 恢复策略，目前只有regular，regular默认实现了熔断，后续可以考虑分为两种恢复模式
      retries: 3 # 60秒内最多重试3次
      retry_window_secs: 60
      cooldown_secs: 180 # 如果发生熔断，冷却3分钟（180秒）
```
配置文件支持热加载，可以给守护进程发送信号sigup来实现更新。

### 命令行参数
healer 守护进程支持以下命令行参数：

```bash
healer [OPTIONS]
```

#### 可用选项
- `-c, --config <CONFIG>`：指定配置文件路径（YAML 格式）
  - 如果未提供，程序会按照以下顺序搜索配置文件：
    1. 环境变量 `HEALER_CONFIG` 指定的路径
    2. 当前目录下的 `config.yaml`
    3. `/etc/healer/config.yaml`（系统级配置）
  
- `--foreground`：在前台运行（不进行守护进程化）
  - 等同于设置环境变量 `HEALER_NO_DAEMON=1`
  - 适用于调试、容器环境或由 systemd 等进程管理器管理时
  
- `--print-config-path`：打印当前使用的配置文件路径并退出
  - 用于调试配置文件解析问题
  - 不会启动守护进程，只显示配置路径
  
- `-h, --help`：显示帮助信息
  
- `-V, --version`：显示版本信息

#### 使用示例
```bash
# 使用默认配置启动
healer

# 指定配置文件
healer -c {/PATH}

# 在前台运行（调试模式）
healer --foreground

# 查看当前会使用的配置文件路径
healer --print-config-path

# 通过环境变量指定配置文件
HEALER_CONFIG=/etc/healer/config.yaml healer

# 不进行守护进程化
HEALER_NO_DAEMON=1 healer
```

#### 环境变量
- `HEALER_CONFIG`：指定配置文件路径
- `HEALER_NO_DAEMON=1`：不进行守护进程化，在前台运行
- `RUST_LOG`：设置日志级别（会覆盖配置文件中的 `log_level` 设置）

## 测试
要运行集成测试，请使用以下命令。请注意，某些测试（例如与 eBPF 相关的测试）可能需要以 root 权限运行。
```
# 检查所有的集成测试并将日志输出不重定向示例（ebpf测试因为需要sudo，默认忽略）
HEALER_TEST_INHERIT_STDIO=1 RUST_LOG=info cargo test -p healer --test process_e2e -- --nocapture --color=always
```
```
# ebpf测试检查的命令示例（需要HEALER_EBPF_E2E=1，同时可执行文件要以sudo权限执行）
HEALER_EBPF_E2E=1 HEALER_TEST_INHERIT_STDIO=1 RUST_LOG=info CARGO_TERM_COLOR=always cargo test -p healer --test ebpf_e2e --config 'target."cfg(all())".runner="sudo -E"' -- --ignored --nocapture --color=always
```

### 测试用例

#### 单元测试
1. **defers_then_releases_on_timeout_skip**
   - 用两条进程配置：A 依赖 B（Requires，hard=true，max_wait_secs=1，on_failure=Skip）。
   - 先发出 B 下线事件，标记 B 正在恢复。
   - 再发出 A 下线事件，期望此时 A 被“延后”（短超时内不应被转发）。
   - 等待一段时间（约 5–8 秒窗口），因为策略是 Skip 且 B 没恢复，A 会在超时后被放行转发。
   - 断言：前期未转发 A，后期成功转发 A（验证“延后 + 超时释放”的策略生效）。

#### 集成测试

1. **restart_on_pid_exit_and_circuit_breaker**
   - 步骤
     - 启动被测进程与 Healer（前台模式便于观测）。
     - 第一次 kill 被测进程，验证 Healer 自动拉起（PID 变化、日志出现 “Successfully restarted process”）。
     - 在 retry_window_secs 窗口内连续多次 kill，触发 retries 上限。
     - 观察进入 Open（冷却）期，期间再次 kill 不应触发立即拉起。
     - 冷却结束后再次 kill，Healer 应恢复重试并拉起。
   - 期望
     - 第一次 kill 后成功拉起（PID 变化）。
     - 窗口内多次 kill 触发熔断，冷却期间不再拉起。
     - 冷却结束后恢复拉起。
   - 断言要点
     - 读取 PID 文件比对前后 PID。
     - 日志含 “Circuit breaker is open” 与恢复成功日志各至少一次。

2. **network_monitor_detects_crash_and_recovers**
   - 步骤
     - 正常启动服务与 Healer，健康检查通过。
     - 让 HTTP 服务自杀/退出，使连接失败（例如监听套接字关闭）。
     - NetworkMonitor 发送 ProcessDisconnected 事件，Healer 尝试拉起目标进程（按 command+args）。
   - 期望
     - 服务退出后，收到 ProcessDisconnected 事件。
     - Healer 触发恢复流程并拉起（若此服务即为被监控与被恢复的同一目标）。
   - 断言要点
     - 订阅 coordinator_event_sender，捕获 ProcessDisconnected { name, url }。
     - 拉起后新进程输出被重定向到日志文件。

3. **ebpf_detects_exit_and_recovers**
   - 步骤
     - 检查环境变量 HEALER_EBPF_E2E=1，仅在显式开启时运行（需要 root 权限）。
     - 清理可能残留的测试进程（healer 和 test_process）。
     - 构建 eBPF 监控配置，指定 monitor.type: "ebpf"，recovery 策略为 retries=3, retry_window_secs=10, cooldown_secs=5。
     - 先启动被监控的 test_process 进程，等待其创建 PID 文件。
     - 启动 Healer（前台模式，便于观测），等待 3 秒让 eBPF 初始化和 watch 生效。
     - 记录基线 PID，通过 kill -9 强制终止被监控进程。
     - 等待最多 20 秒，检查 PID 文件变化，验证 Healer 通过 eBPF 事件检测到进程退出并拉起新进程。
   - 期望
     - eBPF tracepoint 成功附加到内核 sched_process_exit 事件。
     - 进程被 kill 后，eBPF 监控器捕获退出事件并发送 ProcessDown 事件。
     - Healer 接收到事件后成功拉起新进程（PID 发生变化）。
     - 整个恢复过程在 20 秒内完成。
   - 断言要点
     - 基线 PID 必须大于 0。
     - 恢复后的新 PID 必须不同于原 PID。
     - 测试结束时正确清理所有测试进程。
   - 测试环境要求
     - 必须设置环境变量 `HEALER_EBPF_E2E=1` 才会执行，需要 root 权限。

4. **hot_reload_allows_new_process_recovery**
   - 步骤
     - 启动 `ProcessHealer` 并加载仅包含进程 `alpha` 的初始配置。
     - 通过热更新替换为只包含进程 `beta` 的新配置，其恢复命令会在临时目录中写入标记文件。
     - 先对移除的 `alpha` 执行一次恢复以确保熔断状态被清理，再对 `beta` 调用恢复。
   - 期望
     - 旧进程的熔断状态被移除，不再阻塞新的恢复请求。
     - `beta` 的恢复命令被执行，标记文件被正确写入。
   - 断言要点
     - 临时目录中不存在旧标记文件。
     - 恢复后标记文件出现且内容包含 `hot` 字样。

5. **reconcile_starts_stops_pid_and_network_monitors**
   - 步骤
     - 构造包含 PID 与 Network 两类启用进程的配置，调用 `MonitorManager::reconcile`，确认对应监控器被创建。
     - 再次调用 `reconcile` 时移除 PID 进程，仅保留 Network 进程。
   - 期望
     - 初次调度后，PID 与网络监控任务均处于运行状态，禁用进程不会被启动。
     - 第二次调度后，移除的 PID 监控任务被停止，仅保留网络监控任务。
   - 断言要点
     - 通过 `running_monitor_names` 检查当前活跃监控器集合的变化。
     - 测试结束调用 `shutdown` 释放后台任务。

## 软件架构
Healer 是一个面向关键进程自愈场景的轻量守护进程，当前已实现的核心要点：

1. 统一事件总线（broadcast）串联 “监控 → 恢复”。
2. 可插拔监控插件（PID 文件 / 网络探测 / eBPF ）。
3. 熔断与退避的恢复控制（ProcessHealer）。
4. 守护进程模式运行，带信号管理（SIGHUP 热加载 / SIGTERM 优雅退出）与僵尸进程回收。


```
+------------------+
main ------>+ daemonize parent |----> parent退出 / 子进程常驻
            +---------+--------+
                      |
                      v
         +-----------------------------+
     | Core Runtime (tokio runtime)|
         +-----------------------------+
    init: load config -> init logger -> create event bus
      | spawn monitors
      | spawn healer subscriber
      | enter signal loop (SIGHUP reload / SIGTERM shutdown)
```


## 模块与职责概览
核心模块按 “监控 → 恢复” 主链路与支撑层次划分（协调层尚未实现，后续加入）。

### 事件主链路
- `monitor/*` (PID / Network / eBPF 退出事件)：采集进程或服务健康信号，发布事件（如 `ProcessDown`, `ProcessDisconnected`）到广播通道。
- `subscriber/process_healer.rs`（ProcessHealer）：执行真正的重启 / 恢复动作；实现熔断控制（`retries` / `retry_window_secs` / `cooldown_secs`；状态 Closed → Open → HalfOpen），并输出日志。

### 配置与运行时
- `config.rs` / `config_manager.rs`：加载、验证、热更新（SIGHUP）配置；定义监控与恢复策略结构体。
- `core_logic.rs`：启动顺序（配置→日志→事件通道→监控→订阅者），托管 tokio runtime 主循环。
- `service_manager.rs`：统一拉起 Healer 等长期任务（后续可扩展其他订阅者）。
- `monitor_manager.rs`：按配置集管理 / 重建各监控实例。
- `daemon_handler.rs`：守护进程化（fork + 父进程退出）。
- `signal_handler.rs`：处理 `SIGHUP`（重载）、`SIGTERM` / `SIGINT`（优雅退出）、回收僵尸进程。
- `logger.rs`：初始化 tracing/log 目录与等级（支持配置与 `RUST_LOG` 覆盖）。
- `event_bus.rs`：定义 `ProcessEvent` 枚举与 broadcast 通道（monitors → healer）。

### 监控插件 (Monitors)
- `pid_monitor.rs`：根据 PID 文件轮询存活状态。
- `network_monitor.rs`：网络连通 / 端口可达检测（输出断连事件）。
- `ebpf_monitor.rs`：已实现的 eBPF 监控：加载内置 BPF 对象，附加 tracepoint `sched:sched_process_exit`，使用 perf ring buffer 读取 `ProcessExitEvent`，并通过 `PROCESS_NAMES_TO_MONITOR` 映射（截断进程名 -> 配置名）筛选关注的进程，向事件总线发送 `ProcessDown { pid, name }`。


### 工具与辅助
- `utils.rs`：通用帮助函数（路径、时间、权限等后续可放入）。
- `tests/integration`：端到端场景验证（计划：依赖阻塞 → 延迟 → 释放；熔断路径；配置热加载）。


### 事件流简述
Monitors → broadcast → ProcessHealer（执行恢复 / 熔断）。

（依赖图 / 延迟恢复等“协调”能力尚未实现，未来若加入，将插入在 Monitors 与 Healer 之间。）

---
以上列表可随实现推进持续更新，保持 “新增模块 = 更新职责表” 的维护约定。

## 未来计划（未实现）
以下能力仍处于设计阶段，当前仓库不含对应代码，仅作为路线参考：
- 依赖关系自动发现与建模（系统d unit / 命令行启发式 / 配置融合）。
- 延迟 / 条件性恢复（等待关键依赖 ready 后再放行重启）。
- eBPF 深度扩展（在现有退出事件基础上继续增加退出原因、异常 syscall / 资源异常 统计等更细粒度信号）。
