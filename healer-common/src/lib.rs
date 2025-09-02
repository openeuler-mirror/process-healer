#![no_std]

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessExitEvent {
    pub pid: u32,
    pub comm: [u8; 16], // 内核进程名，最多16字节（包括null终止符）
}

#[cfg(feature = "user")]
unsafe impl aya::Pod for ProcessExitEvent {}
