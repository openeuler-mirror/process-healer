use std::path::PathBuf;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 128;

#[derive(Clone, Debug)]
pub enum ProcessEvent {
    ProcessDown {
        name: String,
        pid: u32,
    },
    ProcessDisconnected {
        name: String,
        url: String,
    },
    #[allow(dead_code)]
    ProcessDependencyDetected {
        name: String,
        dependencies: Vec<String>,
    },
    #[allow(dead_code)]
    ProcessRestartSuccess {},
    #[allow(dead_code)]
    ProcessRestartFailed {},
}
#[allow(dead_code)]
pub struct RestartProcessConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub workding_dir: Option<PathBuf>,
}
pub fn create_event_sender() -> broadcast::Sender<ProcessEvent> {
    let (tx, _rx_initial) = broadcast::channel(CHANNEL_CAPACITY);
    tx
}
