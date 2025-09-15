fn main(){
        use std::{fs,thread,time,process,io::{self,Write}};
        let pid = process::id();
        let _ = fs::create_dir_all("/tmp/healer-tests/pids");
        let _ = fs::write("/tmp/healer-tests/pids/counter.pid", pid.to_string());
        let mut n=0u64; loop{ print!("\r[PID {}] alive {}", pid,n); let _=io::stdout().flush(); thread::sleep(time::Duration::from_secs(1)); n+=1; }
    }