// src/main.rs

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;
use std::thread;
use std::time::Duration;

fn resolve_pid_file() -> PathBuf {
    let base = env::var("HEALER_DEMO_BASE").ok().map(PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.join("healer-demo"))
            .expect("无法获取当前目录")
    });
    base.join("run/simple_counter.pid")
}

fn main() {
    // 1. 获取并打印自己的进程ID (PID)
    //    这是最重要的信息，你的 healer 程序需要监控这个 PID
    let pid_file_path = resolve_pid_file();
    let my_pid = process::id();
    println!("测试进程已启动！PID: {}", my_pid);
    if let Some(parent_dir) = pid_file_path.parent() {
        // create_dir_all 会创建所有不存在的父目录，非常方便。
        if let Err(e) = fs::create_dir_all(parent_dir) {
            eprintln!(
                "错误：无法创建 PID 文件所在的目录 {}: {}",
                parent_dir.display(),
                e
            );
            process::exit(1); // 如果目录都创建不了，直接退出
        }
    }
    println!("你可以随时在另一个终端中使用 'kill {}' 或 'kill -9 {}' 来终止我。", my_pid, my_pid);
    match fs::write(&pid_file_path, my_pid.to_string()) {
        Ok(_) => println!("成功写入 PID 文件到: {}", pid_file_path.display()),
        Err(e) => {
            eprintln!(
                "错误：无法写入 PID 文件 {}: {}",
                pid_file_path.display(),
                e
            );
            process::exit(1);
        }
    }
    // 2. 启动一个无限循环，模拟一个正在持续工作的进程
    let mut counter = 0;
    loop {
        // 3. 周期性地打印存活信息，让我们知道它还在运行
        //    我们使用 `\r` 让光标回到行首，可以实现单行刷新的效果，避免刷屏
        print!("\r[PID: {}] 我还活着... 计数器: {}", my_pid, counter);
        
        // 刷新标准输出，确保信息能立刻显示在终端上
        io::stdout().flush().unwrap();

        // 4. 让当前线程休眠2秒钟
        //    这可以防止这个循环把你的CPU吃到100%
        thread::sleep(Duration::from_secs(2));

        // 5. 更新计数器
        counter += 1;
    }
}