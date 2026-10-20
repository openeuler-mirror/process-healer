# process-healer

## Description

A high-performance daemon leveraging eBPF for reliable, low-overhead monitoring and automatic recovery of critical processes to ensure service continuity.

## Quick Start Guide

The following walkthrough uses the sample process `simple_test_process` from this repository to demonstrate how Healer monitors and automatically restarts a target process.

### Prerequisites
- Linux x86_64 (any systemd-based distribution such as Fedora/CentOS)
- Rust stable toolchain (to build Healer and the sample process)

### Steps

1. Clone the code and build the required binaries:
  ```bash
  git clone https://gitee.com/you/repo-url.git
  cd healer-process
  cargo build -p healer -p simple_test_process
  ```

2. Use the repository's built-in script to generate the directories needed for the demo (by default they are created under `healer-demo/` in the project root):
  ```bash
  chmod +x simple_test_process/init.sh
  ./simple_test_process/init.sh
  ```

3. Write the quick-start configuration `quickstart-config.yaml`:
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

4. In terminal A, start the sample process (it writes a PID file and keeps printing heartbeats):
  ```bash
  target/debug/simple_test_process
  ```

5. In terminal B, start Healer in the foreground and load the configuration you just created:
  ```bash
  HEALER_NO_DAEMON=1 HEALER_CONFIG=$(pwd)/quickstart-config.yaml RUST_LOG=info target/debug/healer
  ```

6. Try the automatic recovery: run the following command in terminal A or a new terminal, and watch Healer's log for restart messages (the PID will change):
  ```bash
  pkill -x simple_test_process
  ```

  Healer captures the exit event and restarts the sample process. Press `Ctrl+C` to stop Healer, and remember to clean up the temporary files/directories afterwards.

## Build & Installation

### Building from Source
```
RUST_LOG=info cargo run --config 'target."cfg(all())".runner="sudo -E"'
```
Build with `cargo build` as a non-root user, and run the binary with root privileges.

The log location can be customized by the user in `config.yaml`:
```YAML
# Global configuration
log_level: "info" # Log level; can be set to debug/tracing to reveal more information, but is overridden by the RUST_LOG environment variable
log_directory: "/var/log/healer" # Log file directory; this path requires root privileges, but users may point it to their own location.
pid_file_directory: "/var/run/healer" # Directory for the healer daemon's own PID file; users may point it to their own location.
working_directory: "/" # Working directory, defaults to the root directory
```

### RPM Packaging & Installation
This repository provides RPM packaging scripts and a spec file to help you install Healer as a system service on RPM-based distributions:

- Spec file: `packaging/rpm/healer.spec`
- systemd unit: `packaging/systemd/healer.service`
- Build script: `scripts/build-rpm.sh`

Steps:
1. Install the dependencies (Fedora as an example):
  ```bash
  sudo dnf install -y rpm-build rpmdevtools gcc clang llvm rust cargo make systemd rsync
  ```

  `bpf-linker` is required for linking the eBPF object; the script tries to install it automatically via `cargo install bpf-linker`.

2. Install the Rust nightly toolchain (required for the eBPF build):
  ```bash
  rustup toolchain install nightly
  rustup component add rust-src --toolchain nightly
  ```

3. Build the RPM:
  ```bash
  bash scripts/build-rpm.sh
  ```
  When finished, the built binary package is located at `~/rpmbuild/RPMS/<arch>/healer-<version>-1.<dist>.<arch>.rpm`.

4. Install and manage:
  ```bash
  sudo rpm -Uvh ~/rpmbuild/RPMS/*/healer-*.rpm
  sudo systemctl enable --now healer
  sudo systemctl status healer
  ```

File layout after installation:
- Executable: `/usr/bin/healer`
- Configuration: `/etc/healer/config.yaml` (marked as `%config(noreplace)`, so upgrades will not overwrite local modifications)
- Log directory: `/var/log/healer`
- Run directory: `/var/run/healer`
- systemd unit: `/usr/lib/systemd/system/healer.service`

Uninstall:
```bash
sudo systemctl disable --now healer
sudo rpm -e healer
```

## Usage

### Configuration File
The configuration file is `config.yaml` in the repository root.
It is parsed with the `serde` library. Three working modes are supported — PID, eBPF, and Network; see the existing examples in the YAML file for concrete usage.
```YAML
  - name: "simple_counter" # Name, used in logs
    enabled: true # Enable switch
    command: "/home/lxq/ospp/simple_test_process/target/debug/simple_test_process" # Recovery command; also used as the identity (primary key) of the process when no PID file exists
    args: [] # Arguments of the recovery command
    run_as_root: false # Whether the process should be recovered/restarted as root
    run_as_user: "lxq" # If not root, the user identity used for the restart
    monitor:
      type: "pid" # Monitor via a PID file
      pid_file_path: "/var/run/healer/simple_counter.pid" # PID monitoring mode requires a corresponding PID file
      interval_secs: 3 # Polling interval, in seconds
    # Recovery/restart policy configuration
    recovery:
      type: "regular" # Recovery policy; only "regular" for now, which has circuit breaking built in. Two separate recovery modes may be introduced later.
      retries: 3 # At most 3 retries within 60 seconds
      retry_window_secs: 60
      cooldown_secs: 180 # If the circuit breaker trips, cool down for 3 minutes (180 seconds)
```
The configuration file supports hot reloading: send the SIGHUP signal to the daemon to apply updates.

### Command-Line Options
The healer daemon supports the following command-line options:

```bash
healer [OPTIONS]
```

#### Available Options
- `-c, --config <CONFIG>`: Path to the configuration file (YAML format)
  - If not provided, the program searches for a configuration file in the following order:
    1. The path specified by the `HEALER_CONFIG` environment variable
    2. `config.yaml` in the current directory
    3. `/etc/healer/config.yaml` (system-wide configuration)

- `--foreground`: Run in the foreground (skip daemonization)
  - Equivalent to setting the environment variable `HEALER_NO_DAEMON=1`
  - Useful for debugging, container environments, or when managed by a process manager such as systemd

- `--print-config-path`: Print the configuration file path that would be used and exit
  - Helpful for debugging configuration file resolution
  - Does not start the daemon; only prints the config path

- `-h, --help`: Show help information

- `-V, --version`: Show version information

#### Examples
```bash
# Start with the default configuration
healer

# Specify a configuration file
healer -c {/PATH}

# Run in the foreground (debug mode)
healer --foreground

# Print the configuration file path that would be used
healer --print-config-path

# Specify the configuration file via an environment variable
HEALER_CONFIG=/etc/healer/config.yaml healer

# Skip daemonization
HEALER_NO_DAEMON=1 healer
```

#### Environment Variables
- `HEALER_CONFIG`: Path to the configuration file
- `HEALER_NO_DAEMON=1`: Skip daemonization and run in the foreground
- `RUST_LOG`: Set the log level (overrides the `log_level` setting in the configuration file)

## Testing
To run the integration tests, use the commands below. Note that some tests (e.g., the eBPF-related ones) may require root privileges.
```
# Run all integration tests with log output not redirected (eBPF tests are skipped by default because they need sudo)
HEALER_TEST_INHERIT_STDIO=1 RUST_LOG=info cargo test -p healer --test process_e2e -- --nocapture --color=always
```
```
# Example command for the eBPF tests (requires HEALER_EBPF_E2E=1, and the test executable must run under sudo)
HEALER_EBPF_E2E=1 HEALER_TEST_INHERIT_STDIO=1 RUST_LOG=info CARGO_TERM_COLOR=always cargo test -p healer --test ebpf_e2e --config 'target."cfg(all())".runner="sudo -E"' -- --ignored --nocapture --color=always
```

### Test Cases

#### Unit Tests
1. **defers_then_releases_on_timeout_skip**
   - Two process configurations: A depends on B (Requires, hard=true, max_wait_secs=1, on_failure=Skip).
   - First emit a "B is down" event, marking B as recovering.
   - Then emit an "A is down" event; at this point A is expected to be "deferred" (not forwarded within a short timeout).
   - Wait for a while (roughly a 5–8 second window); since the policy is Skip and B never recovers, A is released and forwarded after the timeout.
   - Assertions: A was not forwarded early, and was successfully forwarded later (verifying the "defer + release-on-timeout" policy works).

#### Integration Tests

1. **restart_on_pid_exit_and_circuit_breaker**
   - Steps
     - Start the process under test and Healer (foreground mode for easier observation).
     - Kill the process under test once and verify Healer restarts it automatically (PID changes, log shows "Successfully restarted process").
     - Kill it repeatedly within the retry_window_secs window to hit the retries limit.
     - Observe entering the Open (cooldown) period; further kills during this period should not trigger an immediate restart.
     - After the cooldown ends, kill it again — Healer should resume retrying and restart the process.
   - Expected
     - After the first kill, the process is restarted successfully (PID changes).
     - Repeated kills within the window trip the circuit breaker; no restart happens during the cooldown.
     - Restarting resumes after the cooldown ends.
   - Key assertions
     - Read the PID file and compare PIDs before and after.
     - The log contains "Circuit breaker is open" and the successful-restart log at least once each.

2. **network_monitor_detects_crash_and_recovers**
   - Steps
     - Start the service and Healer normally; the health check passes.
     - Make the HTTP service exit/kill itself so connections fail (e.g., the listening socket closes).
     - NetworkMonitor sends a ProcessDisconnected event, and Healer tries to restart the target process (by command+args).
   - Expected
     - After the service exits, a ProcessDisconnected event is received.
     - Healer triggers the recovery flow and restarts it (when the monitored and recovered target is the same service).
   - Key assertions
     - Subscribe to coordinator_event_sender and capture ProcessDisconnected { name, url }.
     - After the restart, the new process's output is redirected to a log file.

3. **ebpf_detects_exit_and_recovers**
   - Steps
     - Check the environment variable HEALER_EBPF_E2E=1; run only when explicitly enabled (requires root privileges).
     - Clean up any leftover test processes (healer and test_process).
     - Build an eBPF monitoring configuration with monitor.type: "ebpf" and a recovery policy of retries=3, retry_window_secs=10, cooldown_secs=5.
     - Start the monitored test_process first and wait for it to create its PID file.
     - Start Healer (foreground mode for easier observation) and wait 3 seconds for eBPF initialization and the watch to take effect.
     - Record the baseline PID, then force-kill the monitored process with kill -9.
     - Wait up to 20 seconds, check the PID file for changes, and verify that Healer detected the exit via the eBPF event and started a new process.
   - Expected
     - The eBPF tracepoint attaches successfully to the kernel sched_process_exit event.
     - After the process is killed, the eBPF monitor captures the exit event and sends a ProcessDown event.
     - Healer receives the event and successfully starts a new process (the PID changes).
     - The whole recovery completes within 20 seconds.
   - Key assertions
     - The baseline PID must be greater than 0.
     - The new PID after recovery must differ from the original PID.
     - All test processes are properly cleaned up at the end of the test.
   - Test environment requirements
     - The environment variable `HEALER_EBPF_E2E=1` must be set for the test to run; root privileges are required.

4. **hot_reload_allows_new_process_recovery**
   - Steps
     - Start `ProcessHealer` and load an initial configuration containing only the process `alpha`.
     - Hot-reload a new configuration containing only the process `beta`, whose recovery command writes a marker file into a temporary directory.
     - Trigger a recovery for the removed `alpha` once first, to make sure its circuit-breaker state is cleaned up, then trigger a recovery for `beta`.
   - Expected
     - The removed process's circuit-breaker state is cleared and no longer blocks new recovery requests.
     - `beta`'s recovery command is executed and the marker file is written correctly.
   - Key assertions
     - No old marker file exists in the temporary directory.
     - After recovery, the marker file appears and its content contains the string `hot`.

5. **reconcile_starts_stops_pid_and_network_monitors**
   - Steps
     - Build a configuration containing both a PID-type and a Network-type enabled process, call `MonitorManager::reconcile`, and confirm the corresponding monitors are created.
     - Call `reconcile` again with the PID process removed and only the Network process kept.
   - Expected
     - After the first reconcile, both the PID and network monitoring tasks are running; disabled processes are not started.
     - After the second reconcile, the removed PID monitoring task is stopped and only the network monitoring task remains.
   - Key assertions
     - Check the change in the set of active monitors via `running_monitor_names`.
     - Call `shutdown` at the end of the test to release the background tasks.

## Software Architecture
Healer is a lightweight daemon designed for critical-process self-healing. The currently implemented core points:

1. A unified event bus (broadcast) connecting "monitoring → recovery".
2. Pluggable monitoring plugins (PID file / network probing / eBPF).
3. Circuit-breaking and backoff-based recovery control (ProcessHealer).
4. Daemon-mode operation with signal management (SIGHUP hot reload / SIGTERM graceful shutdown) and zombie process reaping.


```
+------------------+
main ------>+ daemonize parent |----> parent exits / child stays resident
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


## Modules and Responsibilities Overview
The core modules are organized along the main "monitoring → recovery" chain and its supporting layers (the coordination layer is not implemented yet and will be added later).

### Main Event Chain
- `monitor/*` (PID / Network / eBPF exit events): collect process or service health signals and publish events (e.g., `ProcessDown`, `ProcessDisconnected`) to the broadcast channel.
- `subscriber/process_healer.rs` (ProcessHealer): performs the actual restart / recovery actions; implements circuit-breaker control (`retries` / `retry_window_secs` / `cooldown_secs`; states Closed → Open → HalfOpen) and emits logs.

### Configuration & Runtime
- `config.rs` / `config_manager.rs`: load, validate, and hot-reload (SIGHUP) the configuration; define the monitoring and recovery policy structs.
- `core_logic.rs`: startup sequence (config → logger → event channel → monitors → subscriber); hosts the main loop of the tokio runtime.
- `service_manager.rs`: uniformly launches long-running tasks such as Healer (extensible to other subscribers later).
- `monitor_manager.rs`: manages / rebuilds monitor instances according to the configuration.
- `daemon_handler.rs`: daemonization (fork + parent exits).
- `signal_handler.rs`: handles `SIGHUP` (reload), `SIGTERM` / `SIGINT` (graceful shutdown), and reaps zombie processes.
- `logger.rs`: initializes the tracing/log directory and level (supports configuration and `RUST_LOG` override).
- `event_bus.rs`: defines the `ProcessEvent` enum and the broadcast channel (monitors → healer).

### Monitoring Plugins (Monitors)
- `pid_monitor.rs`: polls liveness based on a PID file.
- `network_monitor.rs`: network connectivity / port reachability detection (emits disconnect events).
- `ebpf_monitor.rs`: the implemented eBPF monitor: loads the built-in BPF object, attaches the tracepoint `sched:sched_process_exit`, reads `ProcessExitEvent` from a perf ring buffer, filters processes of interest via the `PROCESS_NAMES_TO_MONITOR` mapping (truncated process name -> configured name), and sends `ProcessDown { pid, name }` to the event bus.


### Utilities & Helpers
- `utils.rs`: general helper functions (paths, time, permissions, etc. can be added here later).
- `tests/integration`: end-to-end scenario verification (planned: dependency blocking → deferral → release; circuit-breaker paths; configuration hot reload).


### Event Flow in Brief
Monitors → broadcast → ProcessHealer (executes recovery / circuit breaking).

(Dependency-graph / deferred-recovery "coordination" capabilities are not implemented yet; if added in the future, they will be inserted between the Monitors and the Healer.)

---
The list above should be kept up to date as the implementation progresses, following the maintenance convention of "new module = update the responsibility table".

## Future Plans (Not Implemented)
The following capabilities are still in the design stage; the current repository contains no corresponding code and they serve only as a roadmap:
- Automatic discovery and modeling of dependency relationships (systemd units / command-line heuristics / configuration fusion).
- Deferred / conditional recovery (hold back a restart until critical dependencies are ready).
- Deeper eBPF extensions (building on the existing exit events with finer-grained signals such as exit reasons and abnormal syscall / resource anomaly statistics).

## Contribution

1. Fork the repository
2. Create a Feat_xxx branch
3. Commit your code
4. Create a Pull Request
