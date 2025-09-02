use std::collections::HashMap;
use std::default::Default;
use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
use tracing::debug;

pub fn find_pid_by_exe_path(path: &str) -> Option<u32> {
    let process_kind = ProcessRefreshKind::default().with_exe(UpdateKind::Always);
    let rk = RefreshKind::nothing().with_processes(process_kind);
    let sys = System::new_with_specifics(rk);
    for process in sys.processes().values() {
        debug!("the exe for pid {} is {:?}", process.pid(), process.exe());
        if let Some(exe_path) = process.exe() {
            if exe_path.to_str() == Some(path) {
                let pid_as_usize = usize::from(process.pid());
                return Some(pid_as_usize as u32);
            }
        }
    }
    None
}

/// 截断进程名到内核限制的16个字符（包括null终止符，所以实际是15个字符）
pub fn truncate_process_name(name: &str) -> String {
    const MAX_COMM_LEN: usize = 15; // 内核中TASK_COMM_LEN - 1
    if name.len() <= MAX_COMM_LEN {
        name.to_string()
    } else {
        name.chars().take(MAX_COMM_LEN).collect()
    }
}

/// 根据截断的进程名查找完整的进程配置名
/// 返回可能匹配的进程配置名列表
pub fn find_process_configs_by_truncated_name(
    truncated_name: &str,
    process_names: &[String],
) -> Vec<String> {
    let mut matches = Vec::new();

    for process_name in process_names {
        let truncated_config_name = truncate_process_name(process_name);
        if truncated_config_name == truncated_name {
            matches.push(process_name.clone());
        }
    }

    matches
}

/// 构建进程名到配置名的映射表
/// 用于快速查找截断名对应的完整配置
pub fn build_process_name_mapping(process_names: &[String]) -> HashMap<String, Vec<String>> {
    let mut mapping = HashMap::new();

    for process_name in process_names {
        let truncated = truncate_process_name(process_name);
        mapping
            .entry(truncated)
            .or_insert_with(Vec::new)
            .push(process_name.clone());
    }

    mapping
}

/// 从进程名获取可执行文件名部分
/// 例如："/usr/bin/nginx" -> "nginx"
pub fn extract_executable_name(command: &str) -> String {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_string()
}

/// 匹配进程名：处理截断名可能对应多个配置的情况
/// 优先返回精确匹配，如果有多个匹配则返回第一个
pub fn smart_match_process_name(
    truncated_name: &str,
    name_mapping: &HashMap<String, Vec<String>>,
) -> Option<String> {
    if let Some(matches) = name_mapping.get(truncated_name) {
        if matches.len() == 1 {
            Some(matches[0].clone())
        } else if matches.len() > 1 {
            tracing::warn!(
                "Multiple process configs match truncated name '{}': {:?}. Using first match: '{}'",
                truncated_name,
                matches,
                matches[0]
            );
            Some(matches[0].clone())
        } else {
            None
        }
    } else {
        None
    }
}
