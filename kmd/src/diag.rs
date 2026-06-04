//! Boot-time breadcrumb for headless diagnosis (no kernel debugger / DebugView).
//!
//! Each device-start milestone records a `stage` number and the last `NTSTATUS`
//! into the device's hardware registry key (`PLUGPLAY_REGKEY_DEVICE` →
//! `HKLM\SYSTEM\CurrentControlSet\Enum\PCI\...\Device Parameters`), readable from
//! user mode. After a failed start, the highest stage written tells us exactly
//! which callback the start reached. Best-effort: any registry error is ignored.

use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, ULONG, UNICODE_STRING, WDFDEVICE, WDFKEY,
    WDF_NO_OBJECT_ATTRIBUTES, NT_SUCCESS,
};

/// `PLUGPLAY_REGKEY_DEVICE` — the device's hardware key (Enum\...\Device
/// Parameters), which always exists for an enumerated device.
const PLUGPLAY_REGKEY_DEVICE: ULONG = 0x0000_0001;
/// `KEY_WRITE` access mask.
const KEY_WRITE: ULONG = 0x0002_0006;

// Stage codes (the start path writes these in order).
pub const STAGE_DEVICE_ADD_OK: u32 = 1;
pub const STAGE_PREPARE_HW_ENTER: u32 = 2;
pub const STAGE_BUS_INTERFACE_OK: u32 = 3;
pub const STAGE_VIRTIO_INIT_FAILED: u32 = 4;
pub const STAGE_PREPARE_HW_OK: u32 = 5;
pub const STAGE_D0_ENTRY: u32 = 6;

/// Compile-time UTF-16 (ASCII subset) for registry value names.
const fn utf16<const N: usize>(b: &[u8; N]) -> [u16; N] {
    let mut out = [0u16; N];
    let mut i = 0;
    while i < N {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

static STAGE_NAME: [u16; 12] = utf16(b"HeliosStage\0");
static STATUS_NAME: [u16; 13] = utf16(b"HeliosStatus\0");

/// Build a `UNICODE_STRING` over a NUL-terminated static UTF-16 array (Length
/// excludes the NUL; MaximumLength includes it).
fn unicode(s: &'static [u16]) -> UNICODE_STRING {
    UNICODE_STRING {
        Length: ((s.len() - 1) * 2) as u16,
        MaximumLength: (s.len() * 2) as u16,
        Buffer: s.as_ptr() as *mut u16,
    }
}

/// Record `(stage, status)` into the device's hardware registry key.
///
/// # Safety
/// `device` must be a valid WDFDEVICE. Best-effort; runs at PASSIVE_LEVEL (the
/// PnP/power callbacks that call it are all PASSIVE).
pub unsafe fn record(device: WDFDEVICE, stage: u32, status: NTSTATUS) {
    let mut key: WDFKEY = core::ptr::null_mut();
    let st = call_unsafe_wdf_function_binding!(
        WdfDeviceOpenRegistryKey,
        device,
        PLUGPLAY_REGKEY_DEVICE,
        KEY_WRITE,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut key
    );
    if !NT_SUCCESS(st) {
        return;
    }
    let sname = unicode(&STAGE_NAME);
    let _ = call_unsafe_wdf_function_binding!(WdfRegistryAssignULong, key, &sname, stage);
    let stname = unicode(&STATUS_NAME);
    let _ = call_unsafe_wdf_function_binding!(WdfRegistryAssignULong, key, &stname, status as ULONG);
    call_unsafe_wdf_function_binding!(WdfRegistryClose, key);
}
