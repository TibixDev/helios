//! TEMPORARY registry-breadcrumb tracer for DOD bring-up.
//!
//! No kernel debugger is available on this host, so each instrumented DDI writes
//! a step code to `HKLM\SYSTEM\CurrentControlSet\Services\helios_kmd\HeliosStep`
//! (REG_DWORD). Read it after install to see the furthest DDI reached / the
//! failing step — dxgkrnl stops calling DDIs after one returns an error, so the
//! last code written is the suspect. The high byte identifies the DDI; the low
//! bits carry a sub-step or argument. Remove once the DOD loads cleanly (Code 0).

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use wdk_sys::ntddk::RtlWriteRegistryValue;

// `RelativeTo` = RTL_REGISTRY_SERVICES → path relative to ...\CurrentControlSet\
// Services. REG_DWORD = 4. (Stable Windows values; declared locally to avoid a
// dependency on whether wdk-sys exports them.)
const RTL_REGISTRY_SERVICES: u32 = 1;
const REG_DWORD_TYPE: u32 = 4;

// L"helios_kmd\0" and L"HeliosStep\0" as NUL-terminated UTF-16.
static SVC_PATH: [u16; 11] = [
    b'h' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'_' as u16,
    b'k' as u16, b'm' as u16, b'd' as u16, 0,
];
static VAL_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'S' as u16,
    b't' as u16, b'e' as u16, b'p' as u16, 0,
];
// "HeliosMask\0"
static MASK_NAME: [u16; 11] = [
    b'H' as u16, b'e' as u16, b'l' as u16, b'i' as u16, b'o' as u16, b's' as u16, b'M' as u16,
    b'a' as u16, b's' as u16, b'k' as u16, 0,
];

/// Sticky OR of `1 << (code >> 24)` for every code ever recorded — so the mask
/// shows EVERY DDI that ran, not just the last (which `HeliosStep` gives). Lets
/// us tell whether CommitVidPn (bit 9) / Present (bit 0xD) ever fired even when
/// EnumVidPnCofuncModality (bit 8) is the last call before dxgkrnl gives up.
static SEEN: AtomicU32 = AtomicU32::new(0);

/// Write `code` to the breadcrumb value. Best-effort (ignores failure).
/// PASSIVE_LEVEL only (RtlWriteRegistryValue is pageable) — call from lifecycle
/// / VidPN / present DDIs, never from the ISR/DPC path.
pub fn record(code: u32) {
    let mut v = code;
    let bit = (code >> 24) & 0x1f;
    let mut mask = SEEN.fetch_or(1u32 << bit, Ordering::Relaxed) | (1u32 << bit);
    // SAFETY: SVC_PATH/VAL_NAME/MASK_NAME are NUL-terminated UTF-16 valid for the
    // program's lifetime; v/mask are 4-byte DWORDs. RtlWriteRegistryValue copies.
    unsafe {
        let _ = RtlWriteRegistryValue(
            RTL_REGISTRY_SERVICES,
            SVC_PATH.as_ptr() as *mut u16,
            VAL_NAME.as_ptr() as *mut u16,
            REG_DWORD_TYPE,
            &mut v as *mut u32 as *mut c_void,
            4,
        );
        let _ = RtlWriteRegistryValue(
            RTL_REGISTRY_SERVICES,
            SVC_PATH.as_ptr() as *mut u16,
            MASK_NAME.as_ptr() as *mut u16,
            REG_DWORD_TYPE,
            &mut mask as *mut u32 as *mut c_void,
            4,
        );
    }
}
