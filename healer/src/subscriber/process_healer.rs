use super::Subscriber;
use crate::config::{self, AppConfig, ProcessConfig, RecoveryConfig, RegularHealerFields};
use crate::event_bus::ProcessEvent;
use async_trait::async_trait;
use serde::de::value::StrDeserializer;
use std::collections::{HashMap, VecDeque};
use std::iter::StepBy;
use std::os::unix::process::{self, CommandExt};
use std::process::{Command, Stdio};
use std::{default, fs};
use std::{sync::Arc, time::Instant};
use tokio::sync::RwLock;
use tokio::sync::{Mutex, broadcast};
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
        let config_guard =
            match tokio::time::timeout(std::time::Duration::from_secs(5), self.app_config.read())
                .await
            {
                Ok(guard) => guard,
                Err(_) => {
                    warn!(
                        target = "healer_action",
                        process_name = %name,
                        "Failed to acquire config lock within timeout, skipping recovery."
                    );
                    return;
                }
            };

        if let Some(process_config) = config_guard.get_process_config_for(&name) {
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
        } else {
            warn!(
                target = "healer_action",
                process_name = %name,
                "No configuration found for process."
            );
        }
    }
    async fn check_circuit_breaker(&mut self, name: &String) -> bool {
        match self.process_recovery_windows.lock().await.get_mut(name) {
            Some(stats) => {
                debug!("[{}] Checking circuit breaker state.", name);
                match stats.recovery_state {
                    State::Closed => {
                        let config_guard = self.app_config.read().await;
                        if let Some(process_config) = config_guard.get_process_config_for(name) {
                            if let RecoveryConfig::Regular(regular_healer_fields) =
                                &process_config.recovery
                            {
                                //遍历一遍，把时间超过窗口期的恢复次数删除掉
                                //Rust不用for和迭代器遍历，retain 来判断窗口期时间
                                stats.recovery_session_starts.retain(|start_time| {
                                    start_time.elapsed().as_secs()
                                        < regular_healer_fields.retry_window_secs
                                });

                                if stats.recovery_session_starts.len()
                                    == regular_healer_fields.retries as usize
                                {
                                    //熔断了，切换为开路，设置好冷却时间，同时重置历史累计的回复次数
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
                                warn!("Shouldn'be here, NotRegular is not implemented yet");
                                return false; // 开还是关的返回值还未确定，健壮性有待商榷 (Todo)
                            } else {
                                warn!("No recovery configuration found for process {}", name);
                                return false; // 开还是关的返回值还未确定，健壮性有待商榷
                            }
                        } else {
                            warn!("No configuration found for process {}", name);
                            return false;
                        }
                    }
                    State::Open => {
                        if let Some(cooldown_until) = stats.in_cooldown_until {
                            if Instant::now() < cooldown_until {
                                return true; // 熔断器仍然处于开启状态
                            } else {
                                // 如果半开重试标志为true，说明可以尝试恢复
                                stats.recovery_state = State::HalfOpen;
                                stats.recovery_session_starts.clear();
                                stats.in_cooldown_until = None; // 清除冷却时间
                                stats.half_open_safe_until =
                                    Some(Instant::now() + std::time::Duration::from_secs(2)); //Todo: 这里的2秒是个写死的，实际应该根据配置来设置或者给出默认值
                                return false;
                            }
                        } else {
                            // in_cool_down_until为None也处理为可以半开路, 但是不应该走到这里
                            warn!(
                                "Shouldn't be here, in_cooldown_until is None for process {}",
                                name
                            );
                            warn!(
                                "No cooldown time set for process {}, assuming it can be recovered.",
                                name
                            );
                            // 如果半开重试标志为true，说明
                            stats.recovery_state = State::HalfOpen;
                            stats.recovery_session_starts.clear();
                            stats.in_cooldown_until = None; // 清除冷却时间
                            return false;
                        }
                    }
                    State::HalfOpen => {
                        // 半开状态，尝试恢复
                        if let Some(safe_until) = stats.half_open_safe_until {
                            if Instant::now() < safe_until {
                                //切换成Open
                                //可以考虑在后续实现为指数回避 (Todo)
                                warn!(
                                    "Process {} is in half-open state, but safe time has not passed yet.",
                                    name
                                );
                                stats.recovery_state = State::Open;
                                stats.half_open_safe_until = None; // 清除半开安全时间
                                stats.recovery_session_starts.clear();
                                return true; // 半开状态，不能恢复
                            } else {
                                // 半开状态结束，重置为关闭状态
                                stats.recovery_state = State::Closed;
                                stats.half_open_safe_until = None;
                                stats.recovery_session_starts.clear();
                                return false; // 可以恢复
                            }
                        } else {
                            warn!("Half-open state without safe time set for process {}", name);
                            stats.recovery_state = State::Closed;
                            stats.half_open_safe_until = None;
                            stats.recovery_session_starts.clear();
                            return false; // 可以恢复
                        }
                    }
                }
            }
            None => true,
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
