#![cfg_attr(feature = "build-ebpf", no_std)]
#![cfg_attr(feature = "build-ebpf", no_main)]

// 将 eBPF 相关代码放到一个 cfg 模块里，避免到处写 #[cfg(...)]
#[cfg(feature = "build-ebpf")]
mod ebpf {
    use aya_ebpf::{
        EbpfContext,
        macros::{map, tracepoint},
        maps::{HashMap, PerfEventArray},
        programs::TracePointContext,
    };
    use aya_log_ebpf::info;
    use healer_common::ProcessExitEvent;

    // 存储要监控的进程名（截断到15个字符）
    #[map]
    static PROCESS_NAMES_TO_MONITOR: HashMap<[u8; 16], u8> = HashMap::with_max_entries(1024, 0);

    #[map]
    static EVENTS: PerfEventArray<ProcessExitEvent> = PerfEventArray::new(0);

    #[tracepoint]
    pub fn healer_exit(ctx: TracePointContext) -> u32 {
        match try_healer_exit(ctx) {
            Ok(ret) => ret,
            Err(ret) => ret,
        }
    }

    fn try_healer_exit(ctx: TracePointContext) -> Result<u32, u32> {
        let pid = ctx.pid();
        let tgid = ctx.tgid();

        if pid != tgid {
            return Ok(0);
        }

        let comm = match aya_ebpf::helpers::bpf_get_current_comm() {
            Ok(comm_array) => comm_array,
            Err(_) => return Ok(0),
        };

        // 检查这个进程名是否在监控列表中
        if unsafe { PROCESS_NAMES_TO_MONITOR.get(&comm) }.is_some() {
            info!(&ctx, "Monitored process detected");

            // 发送包含进程名的事件
            let event = ProcessExitEvent { pid, comm };
            EVENTS.output(&ctx, &event, 0);
        }

        Ok(0)
    }

    // 提供 panic 处理，仅在 eBPF 特性开启时使用（no_std 环境）
    #[cfg(not(test))]
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        loop {}
    }
}

// 未启用 eBPF 时，提供一个空的 std main，避免宿主构建报错。
#[cfg(not(feature = "build-ebpf"))]
fn main() {
    eprintln!("healer-ebpf built without 'build-ebpf' feature; skipping eBPF program");
}
