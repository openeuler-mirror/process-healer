# process-healer
## 介绍
A high-performance daemon leveraging eBPF for reliable, low-overhead monitoring and automatic recovery of critical processes to ensure service continuity.


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



## 配置文件使用指南
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


## 编译使用
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
