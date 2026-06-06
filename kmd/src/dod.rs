//! WDDM Display-Only Driver (DOD) DDIs.
//!
//! `lib.rs` wires these into the `KMDDOD_INITIALIZATION_DATA` table and registers
//! it with dxgkrnl via `DxgkInitializeDisplayOnlyDriver`. dxgkrnl then drives the
//! device lifecycle (AddDevice → StartDevice → the VidPN/present DDIs) through
//! these callbacks. Reference: the inbox `VioGpuDod` + `qxldod` (DISPLAY.md §3,
//! §5; the per-DDI blueprint is in the Phase-7.1 research handover).
//!
//! Phase split:
//!  - 7.1a (this cut): the device loads as a Display adapter (Code 0) — full
//!    lifecycle (AddDevice/StartDevice/Stop/Remove), QueryAdapterInfo(DRIVERCAPS),
//!    a single monitor child, the 2D desktop scanout brought up at StartDevice,
//!    and `DxgkDdiPresentDisplayOnly` painting the primary. The VidPN
//!    mode-negotiation DDIs are conservative stubs (marked `// STUB (7.1b)`).
//!  - 7.1b: flesh out EnumVidPnCofuncModality / CommitVidPn / RecommendMonitorModes
//!    against the generated VidPN interface bindings so the desktop mode commits.
//!  - 7.2: `DxgkDdiEscape` carries the venus ops (body = today's ioctl.rs).

use core::ffi::c_void;
use core::mem::size_of;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::adapter::AdapterContext;
use crate::dxgk::*;
use crate::virtio::{DxgkConfigAccess, VirtioGpu};

/// Single-output DOD: one video-present source, one monitor child.
const MAX_VIEWS: u32 = 1;
const MAX_CHILDREN: u32 = 1;
/// The monitor child id == the VidPN target id == 0 (must match everywhere).
const CHILD_UID: u32 = 0;
/// Default desktop mode when no POST framebuffer geometry is available.
const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;
const BYTES_PER_PIXEL: u32 = 4;

/// Recover the `&AdapterContext` from the opaque miniport device context dxgkrnl
/// hands back on every DDI.
///
/// # Safety
/// `ctx` must be the pointer AddDevice returned (a `Box<AdapterContext>` leaked
/// to a raw pointer), still live (freed only in RemoveDevice).
unsafe fn adapter<'a>(ctx: *mut c_void) -> Option<&'a AdapterContext> {
    if ctx.is_null() {
        None
    } else {
        Some(unsafe { &*(ctx as *const AdapterContext) })
    }
}

// ── Lifecycle ───────────────────────────────────────────────────────────────

/// `DxgkDdiAddDevice` — allocate the adapter context for a discovered device.
pub unsafe extern "C" fn add_device(
    physical_device_object: PDEVICE_OBJECT,
    miniport_device_context: *mut *mut c_void,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: AddDevice\n");
    crate::diag::record(0x0100_0000);
    if miniport_device_context.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let ctx = match AdapterContext::new(physical_device_object) {
        Ok(c) => c,
        Err(e) => return e.into_ntstatus(),
    };
    // Leak to a raw pointer; dxgkrnl hands it back on every DDI, reclaimed in
    // RemoveDevice.
    let raw = Box::into_raw(Box::new(ctx)) as *mut c_void;
    // SAFETY: valid out-pointer per the DDI contract.
    unsafe { *miniport_device_context = raw };
    STATUS_SUCCESS
}

/// `DxgkDdiStartDevice` — save the dxgkrnl interface, bring up the virtio-gpu
/// transport, report one source + one child, and install the default desktop
/// scanout so the host shows the (black) primary immediately.
pub unsafe extern "C" fn start_device(
    miniport_device_context: *mut c_void,
    _dxgk_start_info: *mut DXGK_START_INFO,
    dxgkrnl_interface: *mut DXGKRNL_INTERFACE,
    number_of_video_present_sources: *mut u32,
    number_of_children: *mut u32,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StartDevice\n");
    crate::diag::record(0x0200_0000);
    if miniport_device_context.is_null()
        || dxgkrnl_interface.is_null()
        || number_of_video_present_sources.is_null()
        || number_of_children.is_null()
    {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl passes our adapter context + valid out-pointers. StartDevice
    // is serialized w.r.t. other DDIs, so the &mut to write `dxgkrnl` is sound.
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    // Save the callback interface for the driver's lifetime (Copy struct).
    adapter.dxgkrnl = Some(unsafe { *dxgkrnl_interface });

    // Bring up the virtio transport (PCI cap scan + BAR map + virtqueue) over the
    // Dxgkrnl config-space callbacks. Hard-fail StartDevice on error so a bring-up
    // failure surfaces distinctly in the device's problem code.
    adapter.set_virtio(None);
    let access = DxgkConfigAccess::new(unsafe { &*dxgkrnl_interface });
    match VirtioGpu::init(&access) {
        Ok(gpu) => {
            crate::kmsg(c"Helios DOD: virtio-gpu transport up\n");
            adapter.set_virtio(Some(gpu));
        }
        Err(e) => {
            crate::kmsg(c"Helios DOD: virtio-gpu init FAILED\n");
            return e.into();
        }
    }
    crate::diag::record(0x0200_0001);

    // Single monitor on one source/target.
    // SAFETY: out-pointers validated non-null above.
    unsafe {
        *number_of_video_present_sources = MAX_VIEWS;
        *number_of_children = MAX_CHILDREN;
    }

    // Install the default desktop scanout (best-effort): proves the 2D scanout
    // path end-to-end (the host shows a black primary) even before the VidPN
    // mode-commit flow runs. A failure here does NOT fail StartDevice — the mode
    // is (re)set later by CommitVidPn.
    let _ = set_desktop_mode(adapter, DEFAULT_WIDTH, DEFAULT_HEIGHT);
    crate::diag::record(0x0200_00FF);

    STATUS_SUCCESS
}

/// `DxgkDdiStopDevice` — quiesce: tear down the virtio transport (drops the
/// scanout fb + resets the device).
pub unsafe extern "C" fn stop_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StopDevice\n");
    if let Some(adapter) = unsafe { adapter(miniport_device_context) } {
        adapter.set_virtio(None);
    }
    STATUS_SUCCESS
}

/// `DxgkDdiRemoveDevice` — free the adapter context allocated in AddDevice.
pub unsafe extern "C" fn remove_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: RemoveDevice\n");
    if !miniport_device_context.is_null() {
        // SAFETY: came from Box::into_raw in AddDevice; freed exactly once.
        drop(unsafe { Box::from_raw(miniport_device_context as *mut AdapterContext) });
    }
    STATUS_SUCCESS
}

/// `DxgkDdiStopDeviceAndReleasePostDisplayOwnership` — hand the framebuffer back
/// to the OS for the post-display path. We have no cached POST geometry yet
/// (7.1b), so report the current mode and stop.
pub unsafe extern "C" fn stop_device_and_release_post_display_ownership(
    miniport_device_context: *mut c_void,
    _target_id: u32,
    display_info: *mut DXGK_DISPLAY_INFORMATION,
) -> NTSTATUS {
    crate::kmsg(c"Helios DOD: StopDeviceAndReleasePostDisplayOwnership\n");
    if !display_info.is_null() {
        // SAFETY: dxgkrnl provides a valid out struct; zero it (no POST handoff).
        unsafe { core::ptr::write_bytes(display_info as *mut u8, 0, size_of::<DXGK_DISPLAY_INFORMATION>()) };
    }
    if let Some(adapter) = unsafe { adapter(miniport_device_context) } {
        adapter.set_virtio(None);
    }
    STATUS_SUCCESS
}

/// `DxgkDdiUnload` — driver-wide unload; release the cached BAR MMIO mappings.
pub unsafe extern "C" fn unload() {
    crate::kmsg(c"Helios DOD: Unload\n");
    crate::virtio::hal::WdkHal::unmap_all();
}

// ── Adapter info / children ─────────────────────────────────────────────────

/// `DxgkDdiQueryAdapterInfo` — a DOD answers only `DXGKQAITYPE_DRIVERCAPS`, and
/// sets a minimal cap set (NO segment / GPU-engine / flip caps — leaving those
/// zero is what keeps dxgkrnl treating us as display-only, not a render adapter).
pub unsafe extern "C" fn query_adapter_info(
    _h_adapter: *mut c_void,
    p_query_adapter_info: *const DXGKARG_QUERYADAPTERINFO,
) -> NTSTATUS {
    if p_query_adapter_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract; we only read the args.
    let args = unsafe { &*p_query_adapter_info };
    crate::diag::record(0x0300_0000 | (args.Type as u32 & 0xFFFF));
    if args.Type != _DXGK_QUERYADAPTERINFOTYPE::DXGKQAITYPE_DRIVERCAPS {
        return STATUS_NOT_SUPPORTED;
    }
    if (args.OutputDataSize as usize) < size_of::<DXGK_DRIVERCAPS>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: pOutputData points to a DXGK_DRIVERCAPS of sufficient size.
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_DRIVERCAPS) };
    unsafe { core::ptr::write_bytes(caps as *mut _ as *mut u8, 0, size_of::<DXGK_DRIVERCAPS>()) };
    // 64-bit addressable; not a legacy VGA device; allow smooth rotation. No HW
    // cursor advertised (PointerCaps stays zero) → dxgkrnl composites a software
    // cursor into the present source, so we need no virtio cursor queue yet.
    caps.HighestAcceptableAddress.QuadPart = -1;
    caps.SupportNonVGA = 1;
    caps.SupportSmoothRotation = 1;
    // A DOD reports its WDDM version in DRIVERCAPS. The Microsoft KMDOD sample
    // sets DXGKDDI_WDDMv1_2 here (even though it compiles at the native interface
    // version), so mirror that exactly.
    caps.WDDMVersion = _DXGK_WDDMVERSION::DXGKDDI_WDDMv1_2;
    STATUS_SUCCESS
}

/// `DxgkDdiQueryChildRelations` — report the single monitor child (video output
/// on target 0). The array dxgkrnl passes has room for `size/sizeof - 1` entries
/// plus a NULL terminator; we fill index 0.
pub unsafe extern "C" fn query_child_relations(
    _miniport_device_context: *mut c_void,
    child_relations: *mut DXGK_CHILD_DESCRIPTOR,
    child_relations_size: u32,
) -> NTSTATUS {
    crate::diag::record(0x0400_0000);
    if child_relations.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let count = child_relations_size as usize / size_of::<DXGK_CHILD_DESCRIPTOR>();
    if count == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl provides a zeroed array of `count` descriptors.
    let desc = unsafe { &mut *child_relations };
    desc.ChildDeviceType = _DXGK_CHILD_DEVICE_TYPE::TypeVideoOutput;
    desc.ChildCapabilities.HpdAwareness = _DXGK_CHILD_DEVICE_HPD_AWARENESS::HpdAwarenessInterruptible;
    desc.ChildCapabilities
        .Type
        .VideoOutput
        .InterfaceTechnology = _D3DKMDT_VIDEO_OUTPUT_TECHNOLOGY::D3DKMDT_VOT_HD15;
    desc.ChildCapabilities
        .Type
        .VideoOutput
        .MonitorOrientationAwareness = _D3DKMDT_MONITOR_ORIENTATION_AWARENESS::D3DKMDT_MOA_NONE;
    desc.ChildCapabilities.Type.VideoOutput.SupportsSdtvModes = 0;
    desc.AcpiUid = 0;
    desc.ChildUid = CHILD_UID;
    STATUS_SUCCESS
}

/// `DxgkDdiQueryChildStatus` — the monitor is always connected while we are
/// started.
pub unsafe extern "C" fn query_child_status(
    _miniport_device_context: *mut c_void,
    child_status: *mut DXGK_CHILD_STATUS,
    _non_destructive_only: BOOLEAN,
) -> NTSTATUS {
    if child_status.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract.
    let status = unsafe { &mut *child_status };
    crate::diag::record(0x0500_0000 | (status.Type as u32 & 0xFFFF));
    match status.Type {
        _DXGK_CHILD_STATUS_TYPE::StatusConnection => {
            // The HotPlug arm of the union is valid for StatusConnection. (A union
            // field *write* needs no `unsafe`; only reads do.)
            status.__bindgen_anon_1.HotPlug.Connected = 1;
            STATUS_SUCCESS
        }
        _ => STATUS_NOT_SUPPORTED,
    }
}

/// `DxgkDdiQueryDeviceDescriptor` — no EDID; dxgkrnl uses our RecommendMonitorModes
/// instead.
pub unsafe extern "C" fn query_device_descriptor(
    _miniport_device_context: *mut c_void,
    _child_uid: u32,
    _device_descriptor: *mut DXGK_DEVICE_DESCRIPTOR,
) -> NTSTATUS {
    crate::diag::record(0x0600_0000);
    STATUS_MONITOR_NO_MORE_DESCRIPTOR_DATA
}

// ── Power / IO / interrupts (minimal) ───────────────────────────────────────

/// `DxgkDdiSetPowerState` — accept all transitions (no device-specific work yet).
pub unsafe extern "C" fn set_power_state(
    _miniport_device_context: *mut c_void,
    _device_uid: u32,
    _device_power_state: DEVICE_POWER_STATE,
    _action_type: POWER_ACTION::Type,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiDispatchIoRequest` — DODs receive no legacy VRP IO.
pub unsafe extern "C" fn dispatch_io_request(
    _miniport_device_context: *mut c_void,
    _vidpn_source_id: u32,
    _video_request_packet: PVIDEO_REQUEST_PACKET,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiInterruptRoutine` — the transport polls (device notifications
/// suppressed), so never claim the interrupt.
pub unsafe extern "C" fn interrupt_routine(
    _miniport_device_context: *mut c_void,
    _message_number: u32,
) -> BOOLEAN {
    0
}

/// `DxgkDdiDpcRoutine` — nothing to drain while polling.
pub unsafe extern "C" fn dpc_routine(_miniport_device_context: *mut c_void) {}

/// `DxgkDdiQueryInterface` — we export no driver-defined interface.
pub unsafe extern "C" fn query_interface(
    _miniport_device_context: *mut c_void,
    _query_interface: *mut _QUERY_INTERFACE,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

// ── VidPN (mode negotiation) ────────────────────────────────────────────────
// 7.1a: conservative stubs that let the adapter load. The full cofunctional
// modality enumeration + mode commit (what actually makes the desktop appear)
// lands in 7.1b against the generated VidPN interface bindings.

/// `DxgkDdiIsSupportedVidPn` — a VidPN is rejected by setting the out bool FALSE,
/// never by returning a failure status. We accept all (single source/target).
pub unsafe extern "C" fn is_supported_vidpn(
    _h_adapter: *mut c_void,
    p_is_supported_vidpn: *mut DXGKARG_ISSUPPORTEDVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0700_0000);
    if p_is_supported_vidpn.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid INOUT struct per the DDI contract.
    unsafe { (*p_is_supported_vidpn).IsVidPnSupported = 1 };
    STATUS_SUCCESS
}

/// `DxgkDdiRecommendFunctionalVidPn` — we recommend none (dxgkrnl builds one).
pub unsafe extern "C" fn recommend_functional_vidpn(
    _h_adapter: *mut c_void,
    _p_recommend_functional_vidpn: *const _DXGKARG_RECOMMENDFUNCTIONALVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0F00_0000);
    STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN
}

/// `DxgkDdiEnumVidPnCofuncModality`.
// STUB (7.1b): walk the topology and pin/assign the source+target mode sets.
pub unsafe extern "C" fn enum_vidpn_cofunc_modality(
    _h_adapter: *mut c_void,
    _p_enum_cofunc_modality: *const _DXGKARG_ENUMVIDPNCOFUNCMODALITY,
) -> NTSTATUS {
    crate::diag::record(0x0800_0000);
    STATUS_SUCCESS
}

/// `DxgkDdiSetVidPnSourceVisibility` — track visibility; blackout handled at the
/// scanout when hidden (7.1b).
pub unsafe extern "C" fn set_vidpn_source_visibility(
    _h_adapter: *mut c_void,
    _p_set_vidpn_source_visibility: *const _DXGKARG_SETVIDPNSOURCEVISIBILITY,
) -> NTSTATUS {
    crate::diag::record(0x0C00_0000);
    STATUS_SUCCESS
}

/// `DxgkDdiCommitVidPn`.
// STUB (7.1b): read the pinned source mode and (re)program the scanout via
// set_desktop_mode at the committed resolution.
pub unsafe extern "C" fn commit_vidpn(
    _h_adapter: *mut c_void,
    _p_commit_vidpn: *const _DXGKARG_COMMITVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x0900_0000);
    STATUS_SUCCESS
}

/// `DxgkDdiUpdateActiveVidPnPresentPath`.
pub unsafe extern "C" fn update_active_vidpn_present_path(
    _h_adapter: *mut c_void,
    _p_update_active_vidpn_present_path: *const _DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH,
) -> NTSTATUS {
    crate::diag::record(0x0E00_0000);
    STATUS_SUCCESS
}

/// `DxgkDdiRecommendMonitorModes`.
// STUB (7.1b): add the supported monitor modes (current = preferred).
pub unsafe extern "C" fn recommend_monitor_modes(
    _h_adapter: *mut c_void,
    _p_recommend_monitor_modes: *const _DXGKARG_RECOMMENDMONITORMODES,
) -> NTSTATUS {
    crate::diag::record(0x0A00_0000);
    STATUS_SUCCESS
}

/// `DxgkDdiQueryVidPnHWCapability` — no HW transform offloads; zeroed caps
/// (everything driver-handled by dxgkrnl) is correct for a DOD.
pub unsafe extern "C" fn query_vidpn_hw_capability(
    _h_adapter: *mut c_void,
    _io_p_vidpn_hw_caps: *mut _DXGKARG_QUERYVIDPNHWCAPABILITY,
) -> NTSTATUS {
    crate::diag::record(0x0B00_0000);
    STATUS_SUCCESS
}

// ── Present ─────────────────────────────────────────────────────────────────

/// `DxgkDdiPresentDisplayOnly` — paint the desktop primary onto the scanout.
/// Blts the system-memory source into the desktop framebuffer (full frame for
/// first light) then pushes it to the host (`TRANSFER_TO_HOST_2D` +
/// `RESOURCE_FLUSH`). Runs at PASSIVE/APC; the source is valid for this call only.
pub unsafe extern "C" fn present_display_only(
    _h_adapter: *mut c_void,
    p_present_display_only: *const DXGKARG_PRESENT_DISPLAYONLY,
) -> NTSTATUS {
    if p_present_display_only.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // STUB (7.1b): paint the source into the desktop framebuffer and push it
    // (`present_desktop` → blt + TRANSFER_TO_HOST_2D + RESOURCE_FLUSH). Deferred
    // until the real CommitVidPn (below) pins the source mode to our scanout
    // geometry — only then is the source surface's size (Pitch × committed
    // height) known to match, so the per-row blt cannot over-read `pSource`. For
    // the 7.1a load gate we accept-and-drop the frame (no memory access), which
    // cannot bugcheck. `present_desktop`/`set_desktop_mode` in gpu.rs are the
    // wired-up path this enables.
    STATUS_SUCCESS
}

// ── Pointer (software cursor — accept-and-ignore) ───────────────────────────
// We advertise no HW cursor (QueryAdapterInfo PointerCaps == 0), so dxgkrnl
// composites the cursor into the present source and these are not load-bearing.

/// `DxgkDdiSetPointerPosition`.
pub unsafe extern "C" fn set_pointer_position(
    _h_adapter: *mut c_void,
    _p_set_pointer_position: *const _DXGKARG_SETPOINTERPOSITION,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiSetPointerShape`.
pub unsafe extern "C" fn set_pointer_shape(
    _h_adapter: *mut c_void,
    _p_set_pointer_shape: *const _DXGKARG_SETPOINTERSHAPE,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

// ── System display (bugcheck screen) — not supported (virtio is not VGA) ─────

/// `DxgkDdiSystemDisplayEnable`.
pub unsafe extern "C" fn system_display_enable(
    _miniport_device_context: *mut c_void,
    _target_id: u32,
    _flags: *mut _DXGKARG_SYSTEM_DISPLAY_ENABLE_FLAGS,
    _width: *mut u32,
    _height: *mut u32,
    _color_format: *mut D3DDDIFORMAT,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiSystemDisplayWrite` — never called once Enable returns failure.
pub unsafe extern "C" fn system_display_write(
    _miniport_device_context: *mut c_void,
    _source: *mut c_void,
    _source_width: u32,
    _source_height: u32,
    _source_stride: u32,
    _position_x: u32,
    _position_y: u32,
) {
}

// ── Mandatory base + misc DDIs (trivial) ───────────────────────────────────
// dxgkrnl's DOD init path rejects the table (STATUS_FAILED_DRIVER_ENTRY → device
// Code 37) when the base-block DDIs ResetDevice / NotifyAcpiEvent /
// ControlEtwLogging are NULL — even though they do nothing useful here (the
// render-miniport bring-up established this; same gate for a DOD). We register
// the full set as trivial stubs. (Best-guess signatures; the compiler reports
// the exact bindgen typedefs to match.)

/// `DxgkDdiResetDevice` — reset to a known state (e.g. pre-bugcheck). No-op.
pub unsafe extern "C" fn reset_device(_miniport_device_context: *mut c_void) {}

/// `DxgkDdiNotifyAcpiEvent` — we service no ACPI events.
pub unsafe extern "C" fn notify_acpi_event(
    _miniport_device_context: *mut c_void,
    _event_type: DXGK_EVENT_TYPE,
    _event: u32,
    _argument: *mut c_void,
    _acpi_flags: *mut u32,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiControlEtwLogging` — we emit no ETW; no-op.
pub unsafe extern "C" fn control_etw_logging(_enable: BOOLEAN, _flags: u32, _level: u8) {}

/// `DxgkDdiSetPalette` — no palette (32bpp direct). Trivial success.
pub unsafe extern "C" fn set_palette(
    _h_adapter: *mut c_void,
    _p_set_palette: *const DXGKARG_SETPALETTE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiCollectDbgInfo` — nothing to collect.
pub unsafe extern "C" fn collect_dbg_info(
    _h_adapter: *mut c_void,
    _p_collect_dbg_info: *const DXGKARG_COLLECTDBGINFO,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiGetScanLine` — no scanline reporting.
pub unsafe extern "C" fn get_scan_line(
    _h_adapter: *mut c_void,
    _p_get_scan_line: *mut DXGKARG_GETSCANLINE,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiControlInterrupt` — the transport polls; service no interrupt classes.
pub unsafe extern "C" fn control_interrupt(
    _h_adapter: *mut c_void,
    _interrupt_type: DXGK_INTERRUPT_TYPE,
    _enable_interrupt: BOOLEAN,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiGetChildContainerId` — no container id.
pub unsafe extern "C" fn get_child_container_id(
    _miniport_device_context: *mut c_void,
    _child_uid: u32,
    _container_id: *mut DXGK_CHILD_CONTAINER_ID,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiNotifySurpriseRemoval` — accept.
pub unsafe extern "C" fn notify_surprise_removal(
    _miniport_device_context: *mut c_void,
    _removal_type: DXGK_SURPRISE_REMOVAL_TYPE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiEscape` — the venus carrier. STUB (7.2): port today's ioctl.rs body.
pub unsafe extern "C" fn escape(
    _h_adapter: *mut c_void,
    _p_escape: *const DXGKARG_ESCAPE,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// (Re)program the desktop scanout to `width`×`height`. Allocates the persistent
/// contiguous framebuffer at PASSIVE_LEVEL (outside the virtio spinlock), installs
/// it under the lock via [`VirtioGpu::set_desktop_mode`], and drops the old
/// framebuffer (if any) back at PASSIVE. Returns the command result.
fn set_desktop_mode(adapter: &AdapterContext, width: u32, height: u32) -> Result<(), crate::error::DriverError> {
    let bytes = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(BYTES_PER_PIXEL as usize);
    let fb = match crate::virtio::hal::DmaBuffer::new(bytes) {
        Some(fb) => fb,
        None => return Err(crate::error::DriverError::InsufficientResources),
    };
    let mut retired: Vec<crate::virtio::gpu::InFlight> = Vec::new();
    // SAFETY/IRQL: with_virtio runs the closure at DISPATCH; the closure performs
    // no allocation/free (the fb came in pre-allocated, the old fb comes back out
    // to be dropped here at PASSIVE).
    let (old_fb, result) =
        adapter.with_virtio(|v| v.set_desktop_mode(fb, width, height, &mut retired))?;
    drop(old_fb); // PASSIVE_LEVEL — frees the prior contiguous framebuffer
    drop(retired);
    result.map_err(|_| crate::error::DriverError::IoError)
}
