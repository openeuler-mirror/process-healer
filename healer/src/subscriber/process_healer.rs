use super::Subscriber;
use crate::config::{AppConfig, RecoveryConfig};
use crate::event_bus::ProcessEvent;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::{fs, sync::Arc, time::Instant};
use tokio::sync::RwLock;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};
use users::get_user_by_name;

#[derive(PartialEq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}
struct ProcessRecoveryStats {
    recovery_session_starts: VecDeque<Instant>,
    recovery_state: State,
    in_cooldown_until: Option<Instant>,
    half_open_safe_until: Option<Instant>,
    // half_open_retry_flag: Option<bool>,
}
impl Default for ProcessRecoveryStats {
    fn default() -> Self {
        Self {
            recovery_session_starts: VecDeque::new(),
            recovery_state: State::Closed,
            in_cooldown_until: None,
            half_open_safe_until: None,
        }
    }
}
pub struct ProcessHealer {
    pub event_rx: broadcast::Receiver<ProcessEvent>,
    pub app_config: Arc<RwLock<AppConfig>>,
    process_recovery_windows: Mutex<HashMap<String, ProcessRecoveryStats>>,
}

impl ProcessHealer {
    pub async fn new(
        rx: broadcast::Receiver<ProcessEvent>,
        config: Arc<RwLock<AppConfig>>,
    ) -> Self {
        let recover_map = {
            let config_guard = config.read().await;
            config_guard
                .processes
                .iter()
                .map(|p| (p.name.clone(), ProcessRecoveryStats::default()))
                .collect::<HashMap<String, ProcessRecoveryStats>>()
        }; // 读锁在这个作用域结束时自动释放

        Self {
            event_rx: rx,
            app_config: config,
            process_recovery_windows: Mutex::new(recover_map),
        }
    }

    pub async fn heal_process(&mut self, name: &String) {
        // 使用超时机制获取配置锁，避免无限期阻塞
        //breaker 返回true，说明仍在熔断；返回false说明可以执行
        if self.check_circuit_breaker(&name).await {
            warn!(
                target = "healer_action",
                process_name = %name,
                "Circuit breaker is open, skipping recovery for process {}.",
                name
            );
            return;
        }
        // 限定 read 锁作用域：只在获取并克隆需要的配置期间持有，避免后续阻塞操作（文件IO、spawn）长期占用读锁
        let process_config_opt = {
            match tokio::time::timeout(std::time::Duration::from_secs(5), self.app_config.read())
                .await
            {
                Ok(guard) => guard.get_process_config_for(&name).cloned(),
                Err(_) => None,
            }
        }; // 读锁在这里释放

        if process_config_opt.is_none() {
            warn!(
                target = "healer_action",
                process_name = %name,
                "No configuration found for process."
            );
            return;
        }
        let process_config = process_config_opt.unwrap();

        let command_to_run = &process_config.command;
        let args = &process_config.args;
        info!(target = "healer_event", process_name = %name, "Parsed the restart command. Conducting recovery.");

        // 创建日志目录（如果不存在）
        if let Err(e) = std::fs::create_dir_all("/var/log/healer") {
            warn!(target = "healer_action", process_name = %name, error = %e, "Failed to create log directory, using /tmp");
        }

        let child_log_path = format!("/var/log/healer/{}.restarted.log", name);
        let child_output_file = match fs::File::create(&child_log_path) {
            Ok(file) => file,
            Err(e) => {
                warn!(
                    target = "healer_action",
                    process_name = %name,
                    error = %e,
                    "Failed to create log file, trying /tmp"
                );
                // 尝试在/tmp创建日志文件
                let fallback_path = format!("/tmp/healer_{}.restarted.log", name);
                match fs::File::create(&fallback_path) {
                    Ok(file) => file,
                    Err(e2) => {
                        tracing::error!(
                            target = "healer_action",
                            process_name = %name,
                            error = %e2,
                            "Failed to create fallback log file, aborting recovery."
                        );
                        return;
                    }
                }
            }
        };

        let mut command = Command::new(command_to_run);
        command.args(args);

        // 改进的权限处理
        if !process_config.run_as_root {
            if let Some(username) = &process_config.run_as_user {
                match get_user_by_name(username) {
                    Some(user) => {
                        command.uid(user.uid());
                        command.gid(user.primary_group_id());
                        info!(target: "healer_action", process_name = %name, user = %username, uid = %user.uid(), "Dropping privileges to run as specified user.");
                    }
                    None => {
                        warn!(target: "healer_action", process_name = %name, user = %username, "Specified user not found. Process will run as root. This is a security risk.");
                    }
                }
            } else {
                warn!(target: "healer_action", process_name = %name, "run_as_root is false but no run_as_user specified. Process will run as root.");
            }
        }

        // 被恢复的进程重定向io
        command.stdout(Stdio::from(child_output_file.try_clone().unwrap()));
        command.stderr(Stdio::from(child_output_file));

        match command.spawn() {
            Ok(child) => {
                info!(target = "healer_event", process_name = %name, process_pid = %child.id(), "Successfully restarted process.");
            }
            Err(e) => {
                tracing::error!(target = "healer_action",
                    process_name = %name,
                    error = %e,
                    "Failed to restart process. This might be due to permission issues or invalid command path.");
            }
        }
    }
    async fn check_circuit_breaker(&mut self, name: &String) -> bool {
        let process_config = {
            let cfg = self.app_config.read().await;
            cfg.get_process_config_for(name).cloned()
        };

        let Some(process_config) = process_config else {
            warn!("No configuration found for process {}", name);
            self.process_recovery_windows.lock().await.remove(name);
            return false;
        };

        let mut windows = self.process_recovery_windows.lock().await;
        let stats = windows
            .entry(name.clone())
            .or_insert_with(ProcessRecoveryStats::default);

        debug!("[{}] Checking circuit breaker state.", name);

        match stats.recovery_state {
            State::Closed => {
                if let RecoveryConfig::Regular(regular_healer_fields) = &process_config.recovery {
                    stats.recovery_session_starts.retain(|start_time| {
                        start_time.elapsed().as_secs() < regular_healer_fields.retry_window_secs
                    });

                    if stats.recovery_session_starts.len() == regular_healer_fields.retries as usize
                    {
                        stats.recovery_state = State::Open;
                        stats.in_cooldown_until = Some(
                            Instant::now()
                                + std::time::Duration::from_secs(
                                    regular_healer_fields.cooldown_secs,
                                ),
                        );
                        stats.recovery_session_starts.clear();
                        return true;
                    } else {
                        stats.recovery_session_starts.push_back(Instant::now());
                        debug!(
                            "Process {} has tried {} times",
                            &name,
                            stats.recovery_session_starts.len()
                        );
                        return false;
                    }
                } else if let RecoveryConfig::NotRegular(_) = &process_config.recovery {
                    warn!("Shouldn't be here, NotRegular is not implemented yet");
                    return false;
                } else {
                    warn!("No recovery configuration found for process {}", name);
                    return false;
                }
            }
            State::Open => {
                let now = Instant::now();
                let cooldown_secs = match &process_config.recovery {
                    RecoveryConfig::Regular(fields) => fields.cooldown_secs,
                    _ => {
                        warn!("Shouldn't be here, NotRegular is not implemented yet");
                        5
                    }
                };

                if let Some(cooldown_until) = stats.in_cooldown_until {
                    if now < cooldown_until {
                        return true;
                    }
                } else {
                    warn!(
                        "Open state without cooldown for process {}. Reinstating cooldown.",
                        name
                    );
                    stats.in_cooldown_until =
                        Some(now + std::time::Duration::from_secs(cooldown_secs));
                    return true;
                }

                stats.recovery_state = State::HalfOpen;
                stats.recovery_session_starts.clear();
                stats.half_open_safe_until = Some(now + std::time::Duration::from_secs(2));
                false
            }
            State::HalfOpen => {
                if let Some(safe_until) = stats.half_open_safe_until {
                    let now = Instant::now();
                    if now < safe_until {
                        let cooldown_secs = match &process_config.recovery {
                            RecoveryConfig::Regular(fields) => fields.cooldown_secs,
                            _ => {
                                warn!("Shouldn't be here, NotRegular is not implemented yet");
                                5
                            }
                        };
                        warn!(
                            "Process {} is in half-open; attempt failed within safe window. Back to open (cooldown).",
                            name
                        );
                        stats.recovery_state = State::Open;
                        stats.in_cooldown_until =
                            Some(now + std::time::Duration::from_secs(cooldown_secs));
                        stats.half_open_safe_until = None;
                        stats.recovery_session_starts.clear();
                        return true;
                    } else {
                        stats.recovery_state = State::Closed;
                        stats.half_open_safe_until = None;
                        stats.recovery_session_starts.clear();
                        return false;
                    }
                } else {
                    warn!("Half-open state without safe time set for process {}", name);
                    stats.recovery_state = State::Closed;
                    stats.half_open_safe_until = None;
                    stats.recovery_session_starts.clear();
                    return false;
                }
            }
        }
    }
}

#[async_trait]
impl Subscriber for ProcessHealer {
    async fn handle_event(&mut self, event: ProcessEvent) {
        //heal_process
        if let ProcessEvent::ProcessDown { name, pid } = &event {
            info!(target = "healer_event", process_name = %name, process_pid = %pid, "Received ProcessDown event. Initiating recovery process.");
            self.heal_process(name).await
        } else if let ProcessEvent::ProcessDisconnected { name, url } = &event {
            info!(target = "healer_event", process_name = %name, url = %url, "Received ProcessDisconnected event. Initiating recovery process.");
            self.heal_process(name).await;
        }
    }
}
