use async_trait::async_trait;
pub mod ebpf_monitor;
pub mod network_monitor;
pub mod pid_monitor;
use sysinfo::{RefreshKind, System};
#[async_trait]
pub trait Monitor: Send + Sync {
    // 启动并运行监控任务。
    // 这个方法需要包含一个无限循环，因此它本身不会返回。
    // 它应该被作为一个独立的并发任务来 spawn。
    // 但是目前eBPF不需要run。后续考虑重新抽象Monitor trait (Todo)
    async fn run(self);

    fn name(&self) -> String;
}
