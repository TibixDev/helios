//! WDF interrupt object: `WdfInterruptCreate` + `EvtInterruptIsr`/`EvtInterruptDpc`.
//!
//! Phase 1 creates a well-formed interrupt object but runs the device with its
//! used-ring interrupts SUPPRESSED (`VirtQueue::set_dev_notify(false)` in
//! `gpu.rs::init`): the Phase 1–3 control path is purely synchronous — every
//! command rides `add_notify_wait_pop`, which POLLS the used ring — so the
//! driver needs no interrupt and never reads the virtio ISR-status register.
//!
//! Because our device therefore never asserts its interrupt, the ISR must
//! return FALSE (not-ours): virtio-drivers does NOT program MSI-X, so the
//! assigned interrupt is a LEVEL-triggered, possibly-SHARED INTx line.
//! Returning TRUE there would (a) steal a co-owner's interrupt and (b) — since
//! the ISR can't read+ack the ISR-status register (that lives behind the
//! DISPATCH-level `virtio_lock`, unreachable at DIRQL) — leave a level line
//! asserted, storming the CPU. Returning FALSE lets the real owner service it.
//!
//! Phase 4 (async submission) re-enables device interrupts (`set_dev_notify(true)`)
//! and gives the ISR a transport-independent ISR-status read (so it can ack +
//! claim only its own), making the DPC the sole used-ring consumer: it will
//! acquire `virtio_lock`, pop the used ring, and `FenceTable::signal` each fence.

use wdk_sys::{
    call_unsafe_wdf_function_binding, BOOLEAN, NTSTATUS, ULONG, WDFDEVICE, WDFINTERRUPT, WDFOBJECT,
    WDF_NO_OBJECT_ATTRIBUTES,
};

use crate::wdf;

/// Create the device's WDF interrupt object. Called from `evt_device_add`.
///
/// # Safety
/// `device` must be a valid WDFDEVICE.
pub unsafe fn create_interrupt(device: WDFDEVICE) -> NTSTATUS {
    let mut cfg = wdf::interrupt_config(Some(evt_interrupt_isr), Some(evt_interrupt_dpc));
    let mut interrupt: WDFINTERRUPT = core::ptr::null_mut();
    // SAFETY: `device`, `&mut cfg`, `&mut interrupt` are valid; attributes null.
    call_unsafe_wdf_function_binding!(
        WdfInterruptCreate,
        device,
        &mut cfg,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut interrupt
    )
}

/// `EvtInterruptIsr` — runs at DIRQL. Our device's interrupts are suppressed in
/// Phase 1–3 (polling model), so any invocation is a shared-line interrupt that
/// is NOT ours: return FALSE so the framework passes it to the real owner. We do
/// not (and at DIRQL cannot) touch the transport. See module docs.
unsafe extern "C" fn evt_interrupt_isr(_interrupt: WDFINTERRUPT, _message_id: ULONG) -> BOOLEAN {
    0 // FALSE — not ours (device interrupts suppressed; nothing to service).
}

/// `EvtInterruptDpc` — runs at DISPATCH_LEVEL. Never queued in Phase 1–3 (the
/// ISR claims nothing); the synchronous submit path is the used-ring consumer.
/// Becomes the sole consumer in Phase 4 (see module docs).
unsafe extern "C" fn evt_interrupt_dpc(_interrupt: WDFINTERRUPT, _associated_object: WDFOBJECT) {}
