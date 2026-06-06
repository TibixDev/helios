//! VidPN (Video Present Network) mode-management for the DOD.
//!
//! The display-config DDIs dxgkrnl drives to negotiate + commit a display mode:
//! `IsSupportedVidPn`, `EnumVidPnCofuncModality`, `CommitVidPn`,
//! `RecommendMonitorModes`. Ported from the Microsoft KMDOD sample
//! (`video/KMDOD/bdd_dmm.cxx`) — the canonical reference for the VidPN calls a
//! display-only driver must make so a cofunctional mode commits and
//! `DxgkDdiPresentDisplayOnly` starts being called. Without these (the prior
//! SUCCESS-returning stubs) no mode commits and the desktop never paints.
//!
//! The dod.rs DDI thunks are thin wrappers over the `*_impl` fns here. First
//! light offers a single 1024x768 BGRA mode (a guaranteed-pairable
//! source/target/monitor triple); expand `MODE_TABLE` once the desktop appears.

use core::ffi::c_void;

use crate::adapter::AdapterContext;
use crate::dxgk::*;

/// Modes offered (width, height), all 32bpp BGRA (`D3DDDIFMT_A8R8G8B8`). One mode
/// for first light — same set on source/target/monitor so dxgkrnl can always pair
/// them. The first entry is preferred.
const MODE_TABLE: &[(u32, u32)] = &[(1024, 768)];

// ── Graphics status codes bindgen dropped (C #defines). NTSTATUS = i32. ──────
const STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET: NTSTATUS = 0x401E_034Cu32 as NTSTATUS;
const STATUS_GRAPHICS_MODE_ALREADY_IN_MODESET: NTSTATUS = 0xC01E_0314u32 as NTSTATUS;

#[inline]
fn ok(st: NTSTATUS) -> bool {
    st >= 0
}

/// Fetch the `DXGK_VIDPN_INTERFACE` for a VidPN handle via the saved Dxgkrnl
/// callback.
unsafe fn query_vidpn_interface(
    adapter: &AdapterContext,
    h_vidpn: D3DKMDT_HVIDPN,
) -> Result<*const DXGK_VIDPN_INTERFACE, NTSTATUS> {
    let dxgkrnl = adapter.dxgkrnl().map_err(|e| e.into_ntstatus())?;
    let cb = dxgkrnl.DxgkCbQueryVidPnInterface.ok_or(STATUS_INVALID_PARAMETER)?;
    let mut iface: *const DXGK_VIDPN_INTERFACE = core::ptr::null();
    // SAFETY: callback valid for the device lifetime; h_vidpn is dxgkrnl's; iface
    // is a valid out-pointer.
    let st = unsafe {
        cb(
            h_vidpn,
            _DXGK_VIDPN_INTERFACE_VERSION::DXGK_VIDPN_INTERFACE_VERSION_V1,
            &mut iface,
        )
    };
    if !ok(st) {
        return Err(st);
    }
    if iface.is_null() {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(iface)
}

/// Build a `D3DKMDT_VIDEO_SIGNAL_INFO` for a `w`x`h` progressive mode with
/// unspecified timings (the host owns timing). Zeroed → VSync/HSync/PixelRate =
/// `D3DKMDT_FREQUENCY_NOTSPECIFIED` (the zero rational).
unsafe fn video_signal_info(w: u32, h: u32) -> D3DKMDT_VIDEO_SIGNAL_INFO {
    let mut sig: D3DKMDT_VIDEO_SIGNAL_INFO = unsafe { core::mem::zeroed() };
    sig.VideoStandard = _D3DKMDT_VIDEO_SIGNAL_STANDARD::D3DKMDT_VSS_OTHER;
    sig.TotalSize.cx = w;
    sig.TotalSize.cy = h;
    sig.ActiveSize = sig.TotalSize;
    // VSyncFreq/HSyncFreq/PixelRate stay 0 = NOTSPECIFIED.
    sig.__bindgen_anon_1.ScanLineOrdering =
        _D3DDDI_VIDEO_SIGNAL_SCANLINE_ORDERING::D3DDDI_VSSLO_PROGRESSIVE;
    sig
}

// ── IsSupportedVidPn ────────────────────────────────────────────────────────

/// Accept all VidPNs (single source/target). Rejection is by setting the bool
/// FALSE + returning SUCCESS, never a failure status.
pub unsafe fn is_supported_vidpn(arg: *mut _DXGKARG_ISSUPPORTEDVIDPN) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    unsafe { (*arg).IsVidPnSupported = 1 };
    STATUS_SUCCESS
}

// ── EnumVidPnCofuncModality ─────────────────────────────────────────────────

/// Add our source mode(s) to a (newly created) source mode set.
unsafe fn add_single_source_mode(
    iface: &DXGK_VIDPNSOURCEMODESET_INTERFACE,
    h_set: D3DKMDT_HVIDPNSOURCEMODESET,
) {
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return;
    };
    for &(w, h) in MODE_TABLE {
        let mut p_mode: *mut _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null_mut();
        // SAFETY: dxgkrnl allocates the mode; out-pointer valid.
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        // SAFETY: p_mode points to a fresh source-mode (Id pre-set by dxgkrnl).
        let m = unsafe { &mut *p_mode };
        m.Type = _D3DKMDT_VIDPN_SOURCE_MODE_TYPE::D3DKMDT_RMT_GRAPHICS;
        let g = unsafe { &mut m.Format.Graphics };
        g.PrimSurfSize.cx = w;
        g.PrimSurfSize.cy = h;
        g.VisibleRegionSize = g.PrimSurfSize;
        g.Stride = w * 4;
        g.PixelFormat = _D3DDDIFORMAT::D3DDDIFMT_A8R8G8B8;
        g.ColorBasis = _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SCRGB;
        g.PixelValueAccessMode = _D3DKMDT_PIXEL_VALUE_ACCESS_MODE::D3DKMDT_PVAM_DIRECT;
        // SAFETY: add takes ownership-by-const-ptr of the populated mode.
        let st = unsafe { add(h_set, p_mode) };
        if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
        }
    }
}

/// Add our target mode(s) to a (newly created) target mode set.
unsafe fn add_single_target_mode(
    iface: &DXGK_VIDPNTARGETMODESET_INTERFACE,
    h_set: D3DKMDT_HVIDPNTARGETMODESET,
) {
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return;
    };
    for &(w, h) in MODE_TABLE {
        let mut p_mode: *mut _D3DKMDT_VIDPN_TARGET_MODE = core::ptr::null_mut();
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        let m = unsafe { &mut *p_mode };
        m.VideoSignalInfo = unsafe { video_signal_info(w, h) };
        let st = unsafe { add(h_set, p_mode) };
        if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
        }
    }
}

/// `DxgkDdiEnumVidPnCofuncModality` — for every present path with an unpinned
/// source/target mode set, create + assign our mode set, and advertise
/// identity/centered scaling + identity rotation. Honors the source/target pivot.
pub unsafe fn enum_vidpn_cofunc_modality(
    adapter: &AdapterContext,
    arg: *const _DXGKARG_ENUMVIDPNCOFUNCMODALITY,
) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    let h_vidpn = arg.hConstrainingVidPn;
    let iface = match unsafe { query_vidpn_interface(adapter, h_vidpn) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let iface = unsafe { &*iface };
    let Some(get_topology) = iface.pfnGetTopology else {
        return STATUS_INVALID_PARAMETER;
    };
    let mut h_topo: D3DKMDT_HVIDPNTOPOLOGY = unsafe { core::mem::zeroed() };
    let mut p_topo: *const DXGK_VIDPNTOPOLOGY_INTERFACE = core::ptr::null();
    if !ok(unsafe { get_topology(h_vidpn, &mut h_topo, &mut p_topo) }) || p_topo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let topo = unsafe { &*p_topo };
    let (Some(first), Some(next), Some(release_path)) =
        (topo.pfnAcquireFirstPathInfo, topo.pfnAcquireNextPathInfo, topo.pfnReleasePathInfo)
    else {
        return STATUS_INVALID_PARAMETER;
    };

    // Diagnostics: count paths iterated + whether we assigned a source/target
    // mode set, reported at the end via HeliosStep low bits
    // (0x0800_00<flags><paths>): paths in bits[0..8], src-assigned=0x100,
    // tgt-assigned=0x200. If paths==0 the constraining topology was empty (bug
    // upstream); if assigns are 0 the mode-set create/assign failed.
    let mut paths: u32 = 0;
    let mut diag_flags: u32 = 0;
    let mut p_path: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
    let mut st = unsafe { first(h_topo, &mut p_path) };
    while ok(st) && st != STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET && !p_path.is_null() {
        paths = paths.saturating_add(1);
        let path = unsafe { &*p_path };
        let source_id = path.VidPnSourceId;
        let target_id = path.VidPnTargetId;

        // ── Source side (skip if pivoting on this source) ───────────────────
        let src_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_VIDPNSOURCE
            && arg.EnumPivot.VidPnSourceId == source_id;
        if !src_pivot {
            if let (Some(acq), Some(create_set), Some(release_set), Some(assign)) = (
                iface.pfnAcquireSourceModeSet,
                iface.pfnCreateNewSourceModeSet,
                iface.pfnReleaseSourceModeSet,
                iface.pfnAssignSourceModeSet,
            ) {
                let mut h_set: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
                let mut p_iface: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
                if ok(unsafe { acq(h_vidpn, source_id, &mut h_set, &mut p_iface) })
                    && !p_iface.is_null()
                {
                    let set_iface = unsafe { &*p_iface };
                    let mut pinned: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
                    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
                        unsafe { acq_pin(h_set, &mut pinned) };
                    }
                    if pinned.is_null() {
                        // No pinned mode → replace with our mode set.
                        unsafe { release_set(h_vidpn, h_set) };
                        let mut h_new: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
                        let mut p_new: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
                        if ok(unsafe { create_set(h_vidpn, source_id, &mut h_new, &mut p_new) })
                            && !p_new.is_null()
                        {
                            unsafe { add_single_source_mode(&*p_new, h_new) };
                            if ok(unsafe { assign(h_vidpn, source_id, h_new) }) {
                                diag_flags |= 0x100;
                            }
                        }
                    } else {
                        if let Some(rel) = set_iface.pfnReleaseModeInfo {
                            unsafe { rel(h_set, pinned) };
                        }
                        unsafe { release_set(h_vidpn, h_set) };
                    }
                }
            }
        }

        // ── Target side (skip if pivoting on this target) ───────────────────
        let tgt_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_VIDPNTARGET
            && arg.EnumPivot.VidPnTargetId == target_id;
        if !tgt_pivot {
            if let (Some(acq), Some(create_set), Some(release_set), Some(assign)) = (
                iface.pfnAcquireTargetModeSet,
                iface.pfnCreateNewTargetModeSet,
                iface.pfnReleaseTargetModeSet,
                iface.pfnAssignTargetModeSet,
            ) {
                let mut h_set: D3DKMDT_HVIDPNTARGETMODESET = unsafe { core::mem::zeroed() };
                let mut p_iface: *const DXGK_VIDPNTARGETMODESET_INTERFACE = core::ptr::null();
                if ok(unsafe { acq(h_vidpn, target_id, &mut h_set, &mut p_iface) })
                    && !p_iface.is_null()
                {
                    let set_iface = unsafe { &*p_iface };
                    let mut pinned: *const _D3DKMDT_VIDPN_TARGET_MODE = core::ptr::null();
                    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
                        unsafe { acq_pin(h_set, &mut pinned) };
                    }
                    if pinned.is_null() {
                        unsafe { release_set(h_vidpn, h_set) };
                        let mut h_new: D3DKMDT_HVIDPNTARGETMODESET = unsafe { core::mem::zeroed() };
                        let mut p_new: *const DXGK_VIDPNTARGETMODESET_INTERFACE = core::ptr::null();
                        if ok(unsafe { create_set(h_vidpn, target_id, &mut h_new, &mut p_new) })
                            && !p_new.is_null()
                        {
                            unsafe { add_single_target_mode(&*p_new, h_new) };
                            if ok(unsafe { assign(h_vidpn, target_id, h_new) }) {
                                diag_flags |= 0x200;
                            }
                        }
                    } else {
                        if let Some(rel) = set_iface.pfnReleaseModeInfo {
                            unsafe { rel(h_set, pinned) };
                        }
                        unsafe { release_set(h_vidpn, h_set) };
                    }
                }
            }
        }

        // ── Path scaling/rotation support ───────────────────────────────────
        // Owned copy (the struct may not be Copy due to unions/arrays).
        let mut local: _D3DKMDT_VIDPN_PRESENT_PATH = unsafe { core::ptr::read(p_path) };
        let mut modified = false;
        if local.ContentTransformation.Scaling
            == _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_UNPINNED
        {
            local.ContentTransformation.ScalingSupport.set_Identity(1);
            local.ContentTransformation.ScalingSupport.set_Centered(1);
            modified = true;
        }
        if local.ContentTransformation.Rotation
            == _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_UNPINNED
        {
            local.ContentTransformation.RotationSupport.set_Identity(1);
            local.ContentTransformation.RotationSupport.set_Offset0(1);
            modified = true;
        }
        if modified {
            if let Some(update) = topo.pfnUpdatePathSupportInfo {
                unsafe { update(h_topo, &local) };
            }
        }

        // ── Advance ─────────────────────────────────────────────────────────
        let mut p_next: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
        st = unsafe { next(h_topo, p_path, &mut p_next) };
        unsafe { release_path(h_topo, p_path) };
        p_path = p_next;
    }
    // Report what the enumeration did (see the `paths`/`diag_flags` note above).
    crate::diag::record(0x0800_0000 | (paths & 0xFF) | diag_flags);
    STATUS_SUCCESS
}

// ── CommitVidPn ─────────────────────────────────────────────────────────────

/// `DxgkDdiCommitVidPn` — read the pinned source mode and program the scanout to
/// that resolution (`set_desktop_mode`). This is what makes the committed mode
/// take effect on the device.
pub unsafe fn commit_vidpn(
    adapter: &AdapterContext,
    arg: *const _DXGKARG_COMMITVIDPN,
) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    let h_vidpn = arg.hFunctionalVidPn;
    let iface = match unsafe { query_vidpn_interface(adapter, h_vidpn) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let iface = unsafe { &*iface };
    let (Some(get_topology), Some(acq_src), Some(release_src)) = (
        iface.pfnGetTopology,
        iface.pfnAcquireSourceModeSet,
        iface.pfnReleaseSourceModeSet,
    ) else {
        return STATUS_INVALID_PARAMETER;
    };
    let mut h_topo: D3DKMDT_HVIDPNTOPOLOGY = unsafe { core::mem::zeroed() };
    let mut p_topo: *const DXGK_VIDPNTOPOLOGY_INTERFACE = core::ptr::null();
    if !ok(unsafe { get_topology(h_vidpn, &mut h_topo, &mut p_topo) }) || p_topo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let topo = unsafe { &*p_topo };
    let mut num_paths: SIZE_T = 0;
    if let Some(get_num) = topo.pfnGetNumPaths {
        unsafe { get_num(h_topo, &mut num_paths) };
    }
    if num_paths == 0 {
        return STATUS_SUCCESS; // nothing pinned
    }

    let mut h_set: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
    let mut p_iface: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
    if !ok(unsafe { acq_src(h_vidpn, arg.AffectedVidPnSourceId, &mut h_set, &mut p_iface) })
        || p_iface.is_null()
    {
        return STATUS_SUCCESS;
    }
    let set_iface = unsafe { &*p_iface };
    let mut pinned: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
        unsafe { acq_pin(h_set, &mut pinned) };
    }
    let mut dims: Option<(u32, u32)> = None;
    if !pinned.is_null() {
        let g = unsafe { &(*pinned).Format.Graphics };
        dims = Some((g.PrimSurfSize.cx, g.PrimSurfSize.cy));
        if let Some(rel) = set_iface.pfnReleaseModeInfo {
            unsafe { rel(h_set, pinned) };
        }
    }
    unsafe { release_src(h_vidpn, h_set) };

    if let Some((w, h)) = dims {
        // Program the scanout to the committed resolution (PASSIVE_LEVEL).
        let _ = crate::dod::set_desktop_mode(adapter, w, h);
    }
    STATUS_SUCCESS
}

// ── RecommendMonitorModes ───────────────────────────────────────────────────

/// `DxgkDdiRecommendMonitorModes` — publish the monitor's supported modes (the
/// first marked preferred).
pub unsafe fn recommend_monitor_modes(arg: *const _DXGKARG_RECOMMENDMONITORMODES) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    if arg.pMonitorSourceModeSetInterface.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let iface = unsafe { &*arg.pMonitorSourceModeSetInterface };
    let h_set = arg.hMonitorSourceModeSet;
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return STATUS_INVALID_PARAMETER;
    };
    for &(w, h) in MODE_TABLE {
        let mut p_mode: *mut _D3DKMDT_MONITOR_SOURCE_MODE = core::ptr::null_mut();
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        let m = unsafe { &mut *p_mode };
        m.VideoSignalInfo = unsafe { video_signal_info(w, h) };
        m.Origin = _D3DKMDT_MONITOR_CAPABILITIES_ORIGIN::D3DKMDT_MCO_DRIVER;
        m.Preference = _D3DKMDT_MODE_PREFERENCE::D3DKMDT_MP_PREFERRED;
        m.ColorBasis = _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SRGB;
        // ColorCoeffDynamicRanges left zeroed (8-bit channels are the common
        // default the OS fills; KMDOD sets FirstChannel=...=8 but zero is accepted).
        let st = unsafe { add(h_set, p_mode) };
        if st == STATUS_GRAPHICS_MODE_ALREADY_IN_MODESET {
            unsafe { release(h_set, p_mode as *const _) };
        } else if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
            return st;
        }
    }
    STATUS_SUCCESS
}

// Keep `c_void` referenced (used via raw-pointer casts in the DDI thunks).
const _: () = {
    let _ = core::mem::size_of::<*const c_void>();
};
