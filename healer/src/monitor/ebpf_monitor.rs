use super::Monitor;
use crate::{config::EbpfMonitorConfig, event_bus::ProcessEvent, publisher::Publisher, utils};
use anyhow::anyhow;
use anyhow::Result;
use async_trait::async_trait;
use aya::{maps::PerfEventArray, programs::TracePoint, util::online_cpus, Ebpf};
use bytes::BytesMut;
use healer_common::ProcessExitEvent;
use std::time::Duration;
use std::{
    collections,
    sync::{Arc, Mutex},
};
use std::{
    os::unix::io::AsRawFd,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{io::unix::AsyncFd, sync::broadcast, time::timeout};
use tracing::{debug, error, info, warn};

pub struct EbpfMonitor {
    bpf: Arc<Mutex<Ebpf>>,
    process_name_mapping: Arc<Mutex<collections::HashMap<String, String>>>, // truncated_name -> full_config_name
    task_handles: Vec<tokio::task::JoinHandle<()>>,                         // 保存后台任务句柄
    shutdown_flag: Arc<AtomicBool>,                                         // 关闭标志
    out_tx: broadcast::Sender<ProcessEvent>,                                // 发布通道
}

#[derive(Clone)]
struct TxPublisher {
    tx: broadcast::Sender<ProcessEvent>,
}

impl Publisher for TxPublisher {
    fn publish(
        &self,
        event: ProcessEvent,
    ) -> Result<usize, broadcast::error::SendError<ProcessEvent>> {
        self.tx.send(event)
    }
}

impl EbpfMonitor {
    pub async fn new(event_tx: broadcast::Sender<ProcessEvent>) -> Result<Self> {
        info!("[EbpfMonitor] Initializing and launching the global eBPF monitor...");

        let mut bpf = aya::Ebpf::load(aya::include_bytes_aligned!(concat!(
            env!("OUT_DIR"),
            "/healer"
        )))?;
        let program: &mut TracePoint = bpf
            .program_mut("healer_exit")
            .ok_or_else(|| anyhow!("Program 'healer_exit' not found"))?
            .try_into()?;
        program.load()?;
        program.attach("sched", "sched_process_exit")?;
        info!("[EbpfMonitor] Tracepoint attached successfully.");

        let events_map = bpf
            .take_map("EVENTS")
            .ok_or_else(|| anyhow!("Failed to take ownership of 'EVENTS' map"))?;
        let mut events = PerfEventArray::try_from(events_map)?;
        let mut task_handles = Vec::new();
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        // 创建进程名映射的共享引用
        let process_name_mapping = Arc::new(Mutex::new(collections::HashMap::new()));
        for cpu_id in online_cpus().unwrap() {
            let perf_buf = events.open(cpu_id, None)?;
            let publisher = TxPublisher {
                tx: event_tx.clone(),
            };
            let fd = perf_buf.as_raw_fd();
            let async_fd = AsyncFd::new(fd)?;
            let shutdown_flag_clone = shutdown_flag.clone();
            let mapping_clone = Arc::clone(&process_name_mapping);

            let handle = tokio::spawn(async move {
                info!("[Worker] Listener task for CPU {} started.", cpu_id);
                let mut local_perf_buf = perf_buf;

                while !shutdown_flag_clone.load(Ordering::SeqCst) {
                    let readable_result =
                        timeout(Duration::from_secs(1), async_fd.readable()).await;
                    match readable_result {
                        Ok(Ok(mut guard)) => {
                            // 确保在作用域结束时清理可读状态
                            let mut should_break = false;
                            let mut bufs: [BytesMut; 1] = [BytesMut::with_capacity(1024)];
                            match local_perf_buf.read_events(&mut bufs) {
                                Ok(events_read) => {
                                    if events_read.read > 0 {
                                        for buf in bufs.iter().take(events_read.read) {
                                            let event = unsafe {
                                                (buf.as_ptr() as *const ProcessExitEvent)
                                                    .read_unaligned()
                                            };

                                            let comm_str = std::str::from_utf8(
                                                &event.comm[..event
                                                    .comm
                                                    .iter()
                                                    .position(|&x| x == 0)
                                                    .unwrap_or(16)],
                                            )
                                            .unwrap_or("unknown");
                                            let process_name = {
                                                let mapping = mapping_clone.lock().unwrap();
                                                mapping
                                                    .get(comm_str)
                                                    .cloned()
                                                    .unwrap_or_else(|| comm_str.to_string())
                                            };

                                            info!(
                                                "(CPU {}) Received Event: PID {} (comm: {}) has exited.",
                                                cpu_id, event.pid, comm_str
                                            );
                                            let send_result =
                                                publisher.publish(ProcessEvent::ProcessDown {
                                                    name: process_name.clone(),
                                                    pid: event.pid,
                                                });

                                            match send_result {
                                                Ok(_) => {
                                                    debug!(
                                                        "(CPU {}) Sent ProcessDown event for '{}'",
                                                        cpu_id, process_name
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        "(CPU {}) Failed to send event: {} - continuing",
                                                        cpu_id, e
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug!(
                                        "[Worker] Perf buffer read error on CPU {}: {}, continuing",
                                        cpu_id, e
                                    );
                                    let error_str = e.to_string();
                                    if error_str.contains("broken pipe")
                                        || error_str.contains("connection")
                                    {
                                        warn!(
                                            "[Worker] Perf buffer connection issue on CPU {}, exiting",
                                            cpu_id
                                        );
                                        should_break = true;
                                    }
                                }
                            }
                            // 明确清理可读状态
                            guard.clear_ready();
                            if should_break {
                                break;
                            }
                        }
                        Ok(Err(e)) => {
                            warn!(
                                "[Worker] AsyncFd error on CPU {}: {}, continuing",
                                cpu_id, e
                            );
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(_) => {
                            // 超时是预料之中的，会继续进行shutdown_falg_clone的检查。用于在外界关闭本守护进程可以在1s内响应
                            continue;
                        }
                    }
                }
                info!("[Worker] Listener task for CPU {} shutting down.", cpu_id);
            });
            task_handles.push(handle);
        }

        info!(
            "[EbpfMonitor] All {} worker tasks have been dispatched.",
            task_handles.len()
        );

        Ok(Self {
            bpf: Arc::new(Mutex::new(bpf)),
            process_name_mapping,
            task_handles,
            shutdown_flag,
            out_tx: event_tx,
        })
    }
    pub async fn wait_and_publish(&mut self) {}
    pub async fn watch_config(&mut self, ebpf_config: EbpfMonitorConfig) -> anyhow::Result<()> {
        // 从命令路径中提取可执行文件名
        let executable_name = utils::extract_executable_name(&ebpf_config.command);
        let truncated_name = utils::truncate_process_name(&executable_name);

        info!(
            "[EbpfMonitor] Adding process '{}' (truncated: '{}') to watch list.",
            executable_name, truncated_name
        );

        // 更新进程名映射
        let mut mapping = self.process_name_mapping.lock().unwrap();
        mapping.insert(truncated_name.clone(), ebpf_config.name.clone());
        drop(mapping);

        // 将进程名添加到 eBPF map 中
        let mut process_name_bytes = [0u8; 16];
        let truncated_bytes = truncated_name.as_bytes();
        let copy_len = truncated_bytes.len().min(15); // 保留一个字节作为null终止符
        process_name_bytes[..copy_len].copy_from_slice(&truncated_bytes[..copy_len]);

        let bpf_clone = Arc::clone(&self.bpf);
        let insert_result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut bpf_guard = bpf_clone
                .lock()
                .map_err(|e| anyhow!("Mutex was poisoned: {}", e))?;

            let map_handle = bpf_guard
                .map_mut("PROCESS_NAMES_TO_MONITOR")
                .ok_or_else(|| anyhow!("eBPF map 'PROCESS_NAMES_TO_MONITOR' not found"))?;

            let mut names_map: aya::maps::HashMap<_, [u8; 16], u8> =
                aya::maps::HashMap::try_from(map_handle)
                    .map_err(|e| anyhow!("Failed to create HashMap view from eBPF map: {}", e))?;

            names_map.insert(process_name_bytes, 1, 0).map_err(|e| {
                anyhow!(
                    "Failed to insert process name {:?} into eBPF map: {}",
                    process_name_bytes,
                    e
                )
            })?;

            Ok(())
        })
        .await;

        match insert_result {
            Ok(inner_result) => match inner_result {
                Ok(()) => {
                    info!(
                        "Successfully added process name '{}' to eBPF monitoring.",
                        truncated_name
                    );
                    Ok(())
                }
                Err(e) => {
                    error!(error = ?e, process_name = %truncated_name, "Task to update eBPF map failed.");
                    Err(e)
                }
            },
            Err(e) => {
                error!(error = ?e, "The spawned blocking task itself failed.");
                Err(e.into())
            }
        }
    }

    pub async fn unwatch_config(&mut self, ebpf_config: EbpfMonitorConfig) -> anyhow::Result<()> {
        let executable_name = utils::extract_executable_name(&ebpf_config.command);
        let truncated_name = utils::truncate_process_name(&executable_name);

        info!(
            "[EbpfMonitor] Removing process '{}' (truncated: '{}') from watch list.",
            executable_name, truncated_name
        );

        // 移除内部状态
        let mut mapping = self.process_name_mapping.lock().unwrap();
        mapping.remove(&truncated_name);
        drop(mapping);

        // 从 eBPF map 中移除进程名
        let mut process_name_bytes = [0u8; 16];
        let truncated_bytes = truncated_name.as_bytes();
        let copy_len = truncated_bytes.len().min(15); // 保留一个字节作为null终止符
        process_name_bytes[..copy_len].copy_from_slice(&truncated_bytes[..copy_len]);

        let bpf_clone = Arc::clone(&self.bpf);
        let remove_result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut bpf_guard = bpf_clone
                .lock()
                .map_err(|e| anyhow!("Mutex was poisoned: {}", e))?;

            let map_handle = bpf_guard
                .map_mut("PROCESS_NAMES_TO_MONITOR")
                .ok_or_else(|| anyhow!("eBPF map 'PROCESS_NAMES_TO_MONITOR' not found"))?;

            let mut names_map: aya::maps::HashMap<_, [u8; 16], u8> =
                aya::maps::HashMap::try_from(map_handle)
                    .map_err(|e| anyhow!("Failed to create HashMap view from eBPF map: {}", e))?;

            names_map.remove(&process_name_bytes).map_err(|e| {
                anyhow!(
                    "Failed to remove process name {:?} from eBPF map: {}",
                    process_name_bytes,
                    e
                )
            })?;

            Ok(())
        })
        .await;

        match remove_result {
            Ok(inner_result) => match inner_result {
                Ok(()) => {
                    info!(
                        "Successfully removed process name '{}' from eBPF monitoring.",
                        truncated_name
                    );
                    Ok(())
                }
                Err(e) => {
                    error!(error = ?e, process_name = %truncated_name, "Task to remove from eBPF map failed.");
                    Err(e)
                }
            },
            Err(e) => {
                error!(error = ?e, "The spawned blocking task itself failed.");
                Err(e.into())
            }
        }
    }
    // 关闭 eBPF 监控器
    pub async fn shutdown(&mut self) {
        info!("[EbpfMonitor] Initiating shutdown...");

        // 设置关闭标志，使循环内的定期超时检查可以关闭对应线程
        self.shutdown_flag.store(true, Ordering::SeqCst);

        // 等待所有任务完成，设置超时
        let mut completed_tasks = 0;
        for handle in self.task_handles.drain(..) {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => {
                    completed_tasks += 1;
                }
                Err(_) => {
                    warn!("[EbpfMonitor] Task did not complete within timeout, force stopping.");
                }
            }
        }

        info!(
            "[EbpfMonitor] Shutdown completed. {} tasks stopped gracefully.",
            completed_tasks
        );
    }
}

impl Drop for EbpfMonitor {
    fn drop(&mut self) {
        // 设置关闭标志，即使 shutdown 没有被调用
        self.shutdown_flag.store(true, Ordering::SeqCst);
        info!("[EbpfMonitor] Monitor dropped, shutdown flag set.");
    }
}
#[async_trait]
impl Monitor for EbpfMonitor {
    // eBPF的monitor的run方法是空的，不应该被调用
    // eBPF的执行逻辑应该是在new创建函数里就进行了
    async fn run(mut self) {
        self.wait_and_publish().await;
    }
    // 监控器的名字或标识
    fn name(&self) -> String {
        // self.config.name.clone()
        "EbpfMonitor".to_string()
    }
}

impl Publisher for EbpfMonitor {
    fn publish(
        &self,
        event: ProcessEvent,
    ) -> Result<usize, broadcast::error::SendError<ProcessEvent>> {
        self.out_tx.send(event)
    }
}
