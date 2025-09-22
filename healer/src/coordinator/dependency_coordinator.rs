use crate::{
    config::{AppConfig, DependencyConfig, DependencyKind, OnFailure},
    event_bus::ProcessEvent,
    publisher::Publisher,
    subscriber::Subscriber,
};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

// no dependency graph imports needed

pub struct DependencyCoordinator {
    /// 下游事件总线（发给 Healer 等消费者）
    pub out_tx: broadcast::Sender<ProcessEvent>,
    /// 上游事件接收器（来自各 Monitor）
    pub in_rx: broadcast::Receiver<ProcessEvent>,
    pub app_config: Arc<RwLock<AppConfig>>,
    // 受管目标集合（来自配置 processes.name），用于区分已托管与未知目标
    managed_targets: HashSet<String>,
    deferred: HashMap<String, DeferredState>, // 延迟恢复状态表
    retry_tx: UnboundedSender<InternalMsg>,
    retry_rx: UnboundedReceiver<InternalMsg>,
    /// 处于recovering的目标及其过期时间（用于为简单依赖提供阻塞）
    recovering_until: HashMap<String, Instant>,
}

#[derive(Debug, Clone)]
struct DeferredState {
    original_event: ProcessEvent, // 初次触发保存，用于最终放行
    deferred_count: u32,
    first_deferred_at: Instant,
    last_eval_at: Instant,
    next_retry_at: Instant,
    // 仅跟踪会阻塞的依赖（Requires + hard=true）
    deps: Vec<PerDepState>,
    // 最近一次评估时仍在阻塞的依赖名（仅用于日志展示）
    waiting_on: Vec<String>,
}

#[derive(Debug, Clone)]
struct PerDepState {
    cfg: DependencyConfig,
    status: DepWaitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepWaitStatus {
    Waiting,
    TimedOut,
}

#[derive(Debug, Clone)]
enum InternalMsg {
    Retry(String),
}

impl DependencyCoordinator {
    pub fn new(
        in_rx: broadcast::Receiver<ProcessEvent>,
        out_tx: broadcast::Sender<ProcessEvent>,
        app_config: Arc<RwLock<AppConfig>>,
    ) -> Self {
        let (retry_tx, retry_rx) = unbounded_channel();
        Self {
            out_tx,
            in_rx,
            app_config,
            managed_targets: HashSet::new(),
            deferred: HashMap::new(),
            retry_tx,
            retry_rx,
            recovering_until: HashMap::new(),
        }
    }

    /// 用于阻塞窗口（秒）：把收到 Down/Disconnected 的目标短暂标记为recovering
    const RECOVERING_HOLD_SECS: u64 = 10;

    fn mark_recovering_until(&mut self, name: &str, now: Instant) {
        self.recovering_until.insert(
            name.to_string(),
            now + Duration::from_secs(Self::RECOVERING_HOLD_SECS),
        );
    }

    fn is_recovering(&self, name: &str, now: Instant) -> bool {
        self.recovering_until
            .get(name)
            .map(|&until| now < until)
            .unwrap_or(false)
    }

    fn prune_recovering(&mut self) {
        let now = Instant::now();
        self.recovering_until.retain(|_, &mut until| now < until);
    }

    async fn refresh_snapshot(&mut self) {
        // 读取配置（限制作用域，避免与后续 &mut self 冲突）
        let managed: HashSet<String> = {
            let cfg = self.app_config.read().await;
            cfg.processes.iter().map(|p| p.name.clone()).collect()
        };
        // 同步managed集合：任何未出现在这里的进程名都视为未受管/unknown
        self.managed_targets = managed;
        // 清理过期的recovering标记
        self.prune_recovering();
    }

    async fn decide_and_publish(&mut self, evt: &ProcessEvent) {
        match evt {
            ProcessEvent::ProcessDown { name, .. }
            | ProcessEvent::ProcessDisconnected { name, .. } => {
                // 刷新一次快照
                self.refresh_snapshot().await;

                // 标记该进程进入recovering窗口，用于阻塞其依赖者（不阻塞自身）
                let now = Instant::now();
                self.mark_recovering_until(name, now);

                // 已存在延迟状态则忽略重复原始事件（监控高频触发），仅记录日志
                if self.deferred.contains_key(name) {
                    tracing::debug!(target="dep_coord", process=%name, "event ignored (already deferred)");
                    return;
                }

                let all_manual = self
                    .manual_requires(name)
                    .into_iter()
                    .filter(|d| d.hard)
                    .collect::<Vec<_>>();
                // 分离已受管与未知目标
                let (manual_deps, unknown): (Vec<_>, Vec<_>) = all_manual
                    .into_iter()
                    .partition(|d| self.managed_targets.contains(&d.target));
                if !unknown.is_empty() {
                    let unknown_names: Vec<String> =
                        unknown.into_iter().map(|d| d.target).collect();
                    tracing::warn!(target="dep_coord", process=%name, unknown_deps=?unknown_names, "dependencies refer to unmanaged/unknown targets; they will not block");
                }
                let deps: Vec<String> = manual_deps.iter().map(|d| d.target.clone()).collect();
                if deps.is_empty() {
                    // 没有受管依赖，直接放行
                    tracing::info!(target="dep_coord", process=%name, "no managed dependencies -> forward now");
                    let _ = self.publish(evt.clone());
                    return;
                }

                // 计算阻塞：依赖中是否有目标处于recovering窗口
                let blocking: Vec<String> = deps
                    .iter()
                    .filter(|d| d.as_str() != name)
                    .filter(|d| self.is_recovering(d, now))
                    .cloned()
                    .collect();

                if blocking.is_empty() {
                    // 首次出现依赖但当前无阻塞 -> 放行并提示
                    tracing::info!(target="dep_coord", process=%name, deps=?deps, "dependencies present, none blocking -> forward");
                    let _ = self.publish(evt.clone());
                } else {
                    // 进入延迟（记录每个依赖的 max_wait_secs / on_failure）
                    self.defer_process(name.clone(), evt.clone(), manual_deps, blocking)
                        .await;
                }
            }
            // 其它事件（例如恢复成功/失败）目前直接透传
            _ => {
                let _ = self.publish(evt.clone());
            }
        }
    }

    fn manual_requires(&self, name: &str) -> Vec<DependencyConfig> {
        if let Some(proc_cfg) = self
            .app_config
            .try_read()
            .ok()
            .and_then(|cfg| cfg.processes.iter().find(|p| p.name == name).cloned())
        {
            return proc_cfg
                .resolved_dependencies()
                .into_iter()
                .filter(|d| d.kind == DependencyKind::Requires)
                .collect();
        }
        Vec::new()
    }

    async fn defer_process(
        &mut self,
        name: String,
        original_event: ProcessEvent,
        manual_deps: Vec<DependencyConfig>,
        blocking: Vec<String>,
    ) {
        let now = Instant::now();
        let per_deps: Vec<PerDepState> = manual_deps
            .into_iter()
            .filter(|d| d.hard && d.kind == DependencyKind::Requires)
            .map(|cfg| PerDepState {
                cfg,
                status: DepWaitStatus::Waiting,
            })
            .collect();
        let deps: Vec<String> = per_deps.iter().map(|d| d.cfg.target.clone()).collect();
        let state = DeferredState {
            original_event,
            deferred_count: 1,
            first_deferred_at: now,
            last_eval_at: now,
            next_retry_at: now + Duration::from_secs(5),
            deps: per_deps,
            waiting_on: blocking.clone(),
        };
        self.deferred.insert(name.clone(), state);
        tracing::warn!(target="dep_coord", process=%name, deps=?deps, waiting_on=?blocking, backoff_s=5, "deferred recovery (blocking dependencies)" );
        // 通过内部通道安排一次定时重试（见 run_loop 的 retry 分支）
        self.schedule_retry(name, Duration::from_secs(5));
    }

    fn schedule_retry(&self, name: String, delay: Duration) {
        let tx = self.retry_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // 发送内部重试消息，唤醒协调器评估该进程是否可放行
            let _ = tx.send(InternalMsg::Retry(name));
        });
    }

    fn compute_backoff(prev_attempts: u32) -> Duration {
        match prev_attempts {
            0 | 1 => Duration::from_secs(5),
            2 => Duration::from_secs(10),
            3 => Duration::from_secs(20),
            _ => Duration::from_secs(30),
        }
    }

    async fn handle_retry(&mut self, name: String) {
        let mut remove_and_forward = None;
        let mut drop_due_to_abort = false;
        // 拿到当前状态的克隆关键信息（避免持有可变引用期间再借 self）
        let (first_deferred_at, orig_event_opt, prev_attempts) =
            if let Some(state) = self.deferred.get(&name) {
                (
                    state.first_deferred_at,
                    Some(state.original_event.clone()),
                    state.deferred_count,
                )
            } else {
                (Instant::now(), None, 0)
            };

        if orig_event_opt.is_none() {
            return;
        }

        self.refresh_snapshot().await;
        // 评估当前哪些依赖仍然在阻塞（仍处于 deferred 集合中）
        let mut currently_blocking: HashSet<String> = HashSet::new();
        if let Some(state) = self.deferred.get(&name) {
            // 使用recovering窗口作为阻塞依据
            let now = Instant::now();
            for d in &state.deps {
                if d.status == DepWaitStatus::Waiting
                    && d.cfg.kind == DependencyKind::Requires
                    && d.cfg.hard
                    && d.cfg.target != name
                    && self.managed_targets.contains(&d.cfg.target)
                    && self.is_recovering(&d.cfg.target, now)
                {
                    currently_blocking.insert(d.cfg.target.clone());
                }
            }
        }

        // 应用超时策略：针对仍阻塞的依赖，若超过 max_wait_secs 则根据 on_failure 处理
        if let Some(state) = self.deferred.get_mut(&name) {
            let now = Instant::now();
            for dep in &mut state.deps {
                if dep.status == DepWaitStatus::Waiting
                    && currently_blocking.contains(&dep.cfg.target)
                {
                    let deadline = first_deferred_at + Duration::from_secs(dep.cfg.max_wait_secs);
                    if now >= deadline {
                        match dep.cfg.on_failure {
                            OnFailure::Abort => {
                                drop_due_to_abort = true;
                                tracing::error!(target="dep_coord", process=%name, dependency=%dep.cfg.target, waited_s=%dep.cfg.max_wait_secs, attempts=prev_attempts, "dependency timeout -> abort handling this process event");
                                // 不再等待其他依赖，直接退出循环
                            }
                            OnFailure::Skip | OnFailure::Degrade => {
                                dep.status = DepWaitStatus::TimedOut;
                                let action = match dep.cfg.on_failure {
                                    OnFailure::Skip => "skip",
                                    OnFailure::Degrade => "degrade",
                                    _ => "",
                                };
                                tracing::warn!(target="dep_coord", process=%name, dependency=%dep.cfg.target, waited_s=%dep.cfg.max_wait_secs, policy=%action, "dependency timeout -> will ignore this dependency");
                            }
                        }
                    }
                }
                if drop_due_to_abort {
                    break;
                }
            }
        }

        if drop_due_to_abort {
            // 丢弃事件并移除延迟状态（Abort 策略）
            self.deferred.remove(&name);
            return;
        }

        // 重新计算是否仍有阻塞的依赖（状态仍为 Waiting 且当前在阻塞集内）
        let mut still_blocking: Vec<String> = Vec::new();
        if let Some(state) = self.deferred.get(&name) {
            for d in &state.deps {
                if d.status == DepWaitStatus::Waiting && currently_blocking.contains(&d.cfg.target)
                {
                    still_blocking.push(d.cfg.target.clone());
                }
            }
        }

        if still_blocking.is_empty() {
            if let Some(state) = self.deferred.get(&name) {
                tracing::info!(target="dep_coord", process=%name, deferred_for=?state.first_deferred_at.elapsed(), "release deferred process (no more blocking or timed out per policy)");
            }
            remove_and_forward = orig_event_opt;
        } else {
            if let Some(state) = self.deferred.get_mut(&name) {
                state.deferred_count += 1;
                state.waiting_on = still_blocking.clone();
                state.last_eval_at = Instant::now();
                let backoff = Self::compute_backoff(state.deferred_count);
                state.next_retry_at = Instant::now() + backoff;
                tracing::warn!(target="dep_coord", process=%name, attempts=state.deferred_count, waiting_on=?state.waiting_on, next_retry_s=backoff.as_secs(), "still blocked, reschedule retry");
                self.schedule_retry(name.clone(), backoff);
            }
        }
        if let Some(evt) = remove_and_forward {
            self.deferred.remove(&name);
            let _ = self.publish(evt);
        }
    }

    /// 运行主事件循环：持续从上游接收事件并决策/转发
    pub async fn run_loop(mut self) {
        loop {
            tokio::select! {
                biased;
                maybe_msg = self.retry_rx.recv() => {
                    if let Some(InternalMsg::Retry(name)) = maybe_msg {
                        // 接收内部重试消息，进入一次评估/重试周期
                        self.handle_retry(name).await;
                    } else if maybe_msg.is_none() {
                        tracing::warn!(target="dep_coord", "internal retry channel closed");
                    }
                }
                recv_res = self.in_rx.recv() => {
                    match recv_res {
                        Ok(evt) => self.decide_and_publish(&evt).await,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(target = "dep_coord", missed = n, "lagged, missed events");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::warn!(target = "dep_coord", "upstream channel closed, exiting run_loop");
                            break;
                        }
                    }
                }
            }
        }
    }
}

impl Publisher for DependencyCoordinator {
    fn publish(
        &self,
        event: ProcessEvent,
    ) -> Result<usize, broadcast::error::SendError<ProcessEvent>> {
        self.out_tx.send(event)
    }
}

#[async_trait]
impl Subscriber for DependencyCoordinator {
    async fn handle_event(&mut self, event: ProcessEvent) {
        self.decide_and_publish(&event).await;
    }
}
