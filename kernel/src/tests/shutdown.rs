#![allow(dead_code)]

mod block {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BlockError {
        NotReady,
        DeviceError,
        Timeout,
        TestFailure,
    }

    pub trait BlockDevice {
        fn flush(&self) -> Result<(), BlockError>;
    }
}

mod ata {
    use crate::block::{BlockDevice, BlockError};

    pub fn is_ready() -> bool {
        false
    }

    pub struct PrimaryMaster;

    impl BlockDevice for PrimaryMaster {
        fn flush(&self) -> Result<(), BlockError> {
            Ok(())
        }
    }
}

mod paging {
    pub fn map_hhdm_range(_hhdm_offset: u64, _physical_address: u64, _length: usize) -> bool {
        true
    }
}

fn console_write(_message: &str) {}

mod input {
    pub fn release() {}
}

mod framebuffer {
    pub fn set_active(_active: bool) {}
}

#[path = "../power.rs"]
mod power;
