//! Driver error type mapped to NTSTATUS codes.

use wdk_sys::{
    NTSTATUS, STATUS_DEVICE_DOES_NOT_EXIST, STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_PARAMETER,
    STATUS_IO_DEVICE_ERROR, STATUS_NOT_IMPLEMENTED,
};

#[derive(Debug, Clone, Copy)]
pub enum DriverError {
    InsufficientResources,
    InvalidParameter,
    DeviceNotFound,
    IoError,
    NotImplemented,
}

impl DriverError {
    pub fn into_ntstatus(self) -> NTSTATUS {
        match self {
            Self::InsufficientResources => STATUS_INSUFFICIENT_RESOURCES,
            Self::InvalidParameter => STATUS_INVALID_PARAMETER,
            Self::DeviceNotFound => STATUS_DEVICE_DOES_NOT_EXIST,
            Self::IoError => STATUS_IO_DEVICE_ERROR,
            Self::NotImplemented => STATUS_NOT_IMPLEMENTED,
        }
    }
}

impl From<DriverError> for NTSTATUS {
    fn from(e: DriverError) -> Self {
        e.into_ntstatus()
    }
}
