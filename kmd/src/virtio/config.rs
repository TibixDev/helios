//! `virtio_drivers::transport::pci::bus::ConfigurationAccess` backed by the
//! Dxgkrnl `DxgkCbReadDeviceSpace`/`DxgkCbWriteDeviceSpace` callbacks.
//!
//! A WDDM miniport does not own the PCI bus, so it cannot poke CAM/ECAM
//! directly; instead Dxgkrnl exposes our device's config space through these
//! callbacks (already scoped to our device by the `DeviceHandle`). We therefore
//! ignore the `DeviceFunction` argument and always access our own device. These
//! callbacks are callable up to DISPATCH_LEVEL.
//!
//! Phase-7 display pivot (DISPLAY.md §3.1): this reverts the System-class
//! `KmdfConfigAccess` (over `BUS_INTERFACE_STANDARD`) back to `DxgkConfigAccess`
//! — recovered from git `658168f:kmd/src/virtio/config.rs`, plus a `read32`
//! helper the host-visible capability walk needs (a 16-bit config offset the
//! `ConfigurationAccess::read_word` u8 offset cannot express).

use core::ffi::c_void;

use virtio_drivers::transport::pci::bus::{ConfigurationAccess, DeviceFunction};

use crate::dxgk::*;

/// PCI config-space accessor over Dxgkrnl. `Copy` (handle + two callback
/// pointers) so `unsafe_clone` and `PciRoot`'s cloning needs are trivial.
#[derive(Clone, Copy)]
pub struct DxgkConfigAccess {
    handle: HANDLE,
    read: DXGKCB_READ_DEVICE_SPACE,
    write: DXGKCB_WRITE_DEVICE_SPACE,
}

// SAFETY: the handle + callback pointers are valid for as long as the Dxgkrnl
// interface is valid (saved in StartDevice, used until StopDevice). The accessor
// is only used under the AdapterContext virtio lock during transport bring-up.
unsafe impl Send for DxgkConfigAccess {}
unsafe impl Sync for DxgkConfigAccess {}

impl DxgkConfigAccess {
    /// Capture the device handle + config-space callbacks saved in StartDevice.
    pub fn new(dxgkrnl: &DXGKRNL_INTERFACE) -> Self {
        Self {
            handle: dxgkrnl.DeviceHandle,
            read: dxgkrnl.DxgkCbReadDeviceSpace,
            write: dxgkrnl.DxgkCbWriteDeviceSpace,
        }
    }

    /// Read a 32-bit dword from our device's PCI config space at byte `offset`.
    /// `offset` is a `u16` (not `u8`) so the host-visible cap walk can index the
    /// full 256-byte config window without `u8` add-overflow on `cap + 20`.
    pub fn read32(&self, offset: u16) -> u32 {
        let mut val: u32 = 0;
        let mut bytes_read: ULONG = 0;
        if let Some(read) = self.read {
            // SAFETY: reads 4 bytes of our device's PCI config space at `offset`;
            // `val` is a valid 4-byte buffer. A short read leaves the remaining
            // bytes 0, which the cap walk treats as "no capability".
            unsafe {
                read(
                    self.handle,
                    DXGK_WHICHSPACE_CONFIG,
                    (&mut val as *mut u32).cast::<c_void>(),
                    offset as ULONG,
                    4,
                    &mut bytes_read,
                );
            }
        }
        val
    }
}

impl ConfigurationAccess for DxgkConfigAccess {
    fn read_word(&self, _device_function: DeviceFunction, register_offset: u8) -> u32 {
        self.read32(register_offset as u16)
    }

    fn write_word(&mut self, _device_function: DeviceFunction, register_offset: u8, data: u32) {
        let mut data = data;
        let mut bytes_written: ULONG = 0;
        if let Some(write) = self.write {
            // SAFETY: writes 4 bytes to our device's PCI config space at `offset`.
            unsafe {
                write(
                    self.handle,
                    DXGK_WHICHSPACE_CONFIG,
                    (&mut data as *mut u32).cast::<c_void>(),
                    register_offset as ULONG,
                    4,
                    &mut bytes_written,
                );
            }
        }
    }

    unsafe fn unsafe_clone(&self) -> Self {
        *self
    }
}
