//! `fence_id → KEVENT` table for the async `IOCTL_HELIOS_WAIT_FENCE` path.
//!
//! Each in-flight fenced submission registers a slot here; the used-ring DPC
//! signals the matching KEVENT on completion, and a `WAIT_FENCE` IOCTL blocks on
//! it (at PASSIVE_LEVEL) until signaled or the timeout elapses.
//!
//! STATUS: present and correct, but **not yet wired** in Phase 1–3 — today's
//! `submit_venus` is synchronous (it polls the used ring and returns only after
//! the device acknowledges the fence), so a fence is already complete by the
//! time `WAIT_FENCE` runs. Phase 4 switches submission to async and calls
//! [`FenceTable::register`] at submit time + [`FenceTable::signal`] from the DPC.
//! Until then these methods are unused; `#![allow(dead_code)]` documents that.

#![allow(dead_code)]

use core::cell::UnsafeCell;

use wdk_sys::ntddk::{
    KeAcquireSpinLockRaiseToDpc, KeInitializeEvent, KeReleaseSpinLock, KeSetEvent,
    KeWaitForSingleObject,
};
use wdk_sys::{EVENT_TYPE, KEVENT, KSPIN_LOCK, KWAIT_REASON, LARGE_INTEGER, PVOID, STATUS_SUCCESS};

use crate::wdf::KERNEL_MODE;

/// `NotificationEvent` (`EVENT_TYPE`): once signaled it stays signaled until
/// explicitly cleared — the right semantics for a one-shot fence.
const NOTIFICATION_EVENT: EVENT_TYPE = 0;
/// `Executive` (`KWAIT_REASON`).
const EXECUTIVE: KWAIT_REASON = 0;
/// `IO_NO_INCREMENT` priority boost for `KeSetEvent`.
const IO_NO_INCREMENT: i8 = 0;

/// Maximum simultaneously-outstanding fences. Conservative; the synchronous
/// model never exceeds 1, and the async model retires fences promptly.
const MAX_FENCES: usize = 64;

struct FenceSlot {
    /// The fence id this slot tracks; meaningful only when `in_use`.
    id: u64,
    /// Signaled by the DPC when the fence completes.
    event: KEVENT,
    in_use: bool,
}

/// A small fixed-capacity fence table guarded by its own spinlock.
pub struct FenceTable {
    /// `0` is the initialized + unlocked state of a `KSPIN_LOCK`.
    lock: UnsafeCell<KSPIN_LOCK>,
    slots: UnsafeCell<[FenceSlot; MAX_FENCES]>,
}

// SAFETY: every access to `slots` is serialized by `lock` (a kernel spinlock).
// The KEVENTs live at stable addresses inside the heap-`Box`ed AdapterContext, so
// a pointer handed to `KeWaitForSingleObject` outside the lock stays valid.
unsafe impl Send for FenceTable {}
unsafe impl Sync for FenceTable {}

impl FenceTable {
    pub fn new() -> Self {
        Self {
            lock: UnsafeCell::new(0),
            // SAFETY: a zeroed slot is `{ id: 0, event: <uninit dispatcher
            // header>, in_use: false }`. `in_use == false` means no slot is live,
            // and every slot's `event` is (re-)initialized by `KeInitializeEvent`
            // in `register` before any wait/signal touches it.
            slots: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        }
    }

    /// Register `fence_id` as outstanding. Returns false if the table is full.
    /// Safe to call at <= DISPATCH_LEVEL.
    pub fn register(&self, fence_id: u64) -> bool {
        // SAFETY: spinlock-guarded exclusive access to the slot array.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.lock.get()) };
        let slots = unsafe { &mut *self.slots.get() };
        let mut ok = false;
        for slot in slots.iter_mut() {
            if !slot.in_use {
                slot.id = fence_id;
                slot.in_use = true;
                // SAFETY: `&mut slot.event` is a valid, stable KEVENT location;
                // initialized unsignaled before anyone can wait on it.
                unsafe { KeInitializeEvent(&mut slot.event, NOTIFICATION_EVENT, 0) };
                ok = true;
                break;
            }
        }
        unsafe { KeReleaseSpinLock(self.lock.get(), irql) };
        ok
    }

    /// Signal the fence with `fence_id` (called from the used-ring DPC at
    /// DISPATCH_LEVEL). No-op if no slot matches.
    pub fn signal(&self, fence_id: u64) {
        // SAFETY: spinlock-guarded exclusive access to the slot array. KeSetEvent
        // is callable at <= DISPATCH_LEVEL and does not block, so we signal WHILE
        // HOLDING the lock. Signaling under the lock is required for correctness:
        // if we instead captured the event pointer, released the lock, then set
        // it, a waiter could time out and `register` could reuse + re-init that
        // slot's KEVENT for a different fence in the gap — and our set would then
        // spuriously complete the wrong (unfinished) fence.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.lock.get()) };
        let slots = unsafe { &mut *self.slots.get() };
        for slot in slots.iter_mut() {
            if slot.in_use && slot.id == fence_id {
                // SAFETY: `&mut slot.event` is a live, initialized KEVENT.
                unsafe { KeSetEvent(&mut slot.event, IO_NO_INCREMENT as i32, 0) };
                break;
            }
        }
        unsafe { KeReleaseSpinLock(self.lock.get(), irql) };
    }

    /// Wait (at PASSIVE_LEVEL) until `fence_id` is signaled or `timeout_ns`
    /// elapses (0 = wait forever). Frees the slot afterwards. Returns the
    /// `KeWaitForSingleObject` NTSTATUS, or `STATUS_SUCCESS` if the fence was not
    /// registered (already complete under the synchronous model).
    pub fn wait_and_remove(&self, fence_id: u64, timeout_ns: u64) -> i32 {
        // Locate the slot's event under the lock; do NOT block while holding it.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.lock.get()) };
        let slots = unsafe { &mut *self.slots.get() };
        let mut ev: *mut KEVENT = core::ptr::null_mut();
        let mut idx = usize::MAX;
        for (i, slot) in slots.iter_mut().enumerate() {
            if slot.in_use && slot.id == fence_id {
                ev = &mut slot.event;
                idx = i;
                break;
            }
        }
        unsafe { KeReleaseSpinLock(self.lock.get(), irql) };
        if ev.is_null() {
            return STATUS_SUCCESS;
        }

        // Relative timeout in 100ns units is negative; 0 means infinite (NULL).
        let mut timeout: LARGE_INTEGER = unsafe { core::mem::zeroed() };
        let timeout_ptr = if timeout_ns == 0 {
            core::ptr::null_mut()
        } else {
            timeout.QuadPart = -((timeout_ns / 100) as i64);
            &mut timeout
        };
        // SAFETY: `ev` is a live KEVENT at a stable address; PASSIVE_LEVEL wait.
        let status = unsafe {
            KeWaitForSingleObject(
                ev as PVOID,
                EXECUTIVE,
                KERNEL_MODE,
                0,
                timeout_ptr,
            )
        };

        // Free the slot.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.lock.get()) };
        let slots = unsafe { &mut *self.slots.get() };
        if idx < slots.len() && slots[idx].in_use && slots[idx].id == fence_id {
            slots[idx].in_use = false;
        }
        unsafe { KeReleaseSpinLock(self.lock.get(), irql) };
        status
    }
}
