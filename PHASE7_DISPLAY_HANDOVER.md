# Phase 7 Handover — Helios Display Engine (DOD + Venus + zero-copy `SET_SCANOUT_BLOB`)

**Read `DISPLAY.md` first (canonical spec).** This is the actionable handover for the display pivot decided
2026-06-06. It replaces the Phase-6 WSI plan (`icd/PHASE6_WSI_HANDOVER.md`), which is now **abandoned**: the
Mesa `wsi_win32` software present is architecturally incapable of using the host GL display (it issues no
scanout flush; <1 fps is a host-visibility lag the backend change alone cannot fix — verified against QEMU
source). Do **not** revive `VN_PERF=no_fence_feedback` (fast but corrupt; rejected twice) and do **not** revive
the WDDM **render** miniport (confirmed dead end: needs a multi-man-year native D3D-to-venus UMD — LB3/LB4).

---

## 0. Prompt for the next agent

Continue Helios. The project pivots its DISPLAY path to a **WDDM Display-Only Driver (DOD)** that owns the
virtio-gpu scanout 0 **and** carries the venus ops over `DxgkDdiEscape`, so venus-rendered content reaches the
host **directly** via `VIRTIO_GPU_CMD_SET_SCANOUT_BLOB` (zero-copy dmabuf) displayed under **`-spice gl=on`** —
instead of the dead software-blit WSI path. The venus rendering stack (Mesa venus ICD, virtio transport, blob
mapping) is **reused**; the driver MODEL flips System-class KMDF → DOD, and the venus carrier flips IOCTL →
`D3DKMTEscape` (byte layouts unchanged; both shapes exist in git `658168f`). **The win = fullscreen vkcube /
DXVK / VKD3D displayed fast and zero-copy; plus a real Windows desktop on SPICE (DOD 2D path, GL-displayed).**

**DO STEP 1 FIRST — the go/no-go gate.** Before any DOD rewrite, prove a venus-rendered blob can be
`SET_SCANOUT_BLOB`'d and displayed zero-copy under gl=on, on the *current* System-class driver (it already owns
the FDO). This de-risks the single load-bearing unknown (does the venus swapchain image export a real
`dmabuf_fd` — Intel ANV should, but it's unverified). See §2.

Build/run via the `win` MCP (`win_cargo`/`win_meson`/`win_exec`). GUI apps need the interactive console
(`schtasks /it`, not session-0 SSH). The user must visually confirm the spice display; you check host logs +
the process staying alive. After repeated venus crashes, `devcon restart "PCI\VEN_1AF4&DEV_1050"` to clear
leaked host contexts.

---

## 1. Why this shape (the verified conclusions — don't re-litigate)

- **DWM can't be GPU-accelerated here** (no WDDM render adapter; a Vulkan ICD/DXVK is invisible to DWM → WARP
  composites the desktop). A full WDDM render miniport would still need a from-scratch native D3D-to-venus UMD.
  **Rejected.** (LB3/LB4, both *confirmed*.)
- **The software WSI present is a dead end** — it issues no virtio-gpu scanout flush, so the host GL backend
  never presents it and it eats a ~3.5 s/frame host-visibility lag. **Replace it with a real scanout.** (LB2,
  mechanism *refuted* against QEMU `ui/gtk-egl.c` / `virtio-gpu.c`.)
- **One driver per FDO** → Helios must BE the display driver to own a scanout. **DOD** (Display-only) loads
  cleanly — it has none of the render AddAdapter cap contract that caused Code 43. (LB5 *confirmed*.)
- **The fast path = `SET_SCANOUT_BLOB` of an exportable venus blob** (`res->base.dmabuf_fd >= 0` from
  `virgl_renderer_resource_get_info`; venus always runs in the render-server which exports it; ANV exports
  dmabuf), displayed zero-copy by spice gl=on. (LB6 *partially-correct* — the only correction was that venus is
  never in-process, which doesn't change the plan.)

---

## 2. STEP 1 — the go/no-go gate (current driver, no DOD yet)

> **✅ GATE RESULT (2026-06-06): GO on the load-bearing criterion; proceed to the DOD.**
> A venus blob **exports a real, scannable `DRM_FORMAT_MODIFIER(LINEAR)` dma-buf** and the host
> **accepts `SET_SCANOUT_BLOB`** (`dmabuf_fd >= 0`, no `RESP_ERR_UNSPEC`). The whole scanout
> infrastructure (protocol + KMD `set_scanout_blob`/`resource_flush` + present IOCTL + ICD) works.
> **Required fix found + applied:** venus rejected DMA_BUF scanout images that weren't
> `TILING_DRM_FORMAT_MODIFIER` (`-11`); **removed the `#if !DETECT_OS_WINDOWS` gate on
> `EXT_image_drm_format_modifier`** in `icd/mesa/.../vn_physical_device.c` (~L1426) and the gate
> test now creates a `DRM_FORMAT_MOD_LINEAR` DMA_BUF image → `imgfmt2` now `OK+EXPORTABLE`.
> **DO NOT also remove the `EXT_external_memory_dma_buf`/`KHR_external_memory_fd` gate (~L1211) —
> that breaks `vkEnumeratePhysicalDevices` (-3) on Windows; the test doesn't need it advertised.**
>
> **On-screen pixels were NOT visually confirmed** — every backend hit a *host* display bug (not
> Helios): **gtk gl=on** → `eglMakeCurrent failed` (qemu-gtk GL on this multi-GPU Wayland host;
> irrelevant to the DOD's spice path); **spice gl=on** → GL binds to the QXL-*primary* console, so
> a *secondary* venus head is dropped (auto-fixed once Helios IS the primary display = the DOD).
> Owner-directed: stop debugging the host display, proceed to 7.1; the DOD's own 7.1 gate
> (desktop-on-spice) re-confirms the visual early. Debug tool: `tools/launch-helios-gtk.sh`
> (standalone qemu, gtk gl=on, native Wayland). See the `phase7-gate-status` memory for full detail.



1. **Host:** edit the win11 VM display `egl-headless` → **`gtk,gl=on`** (local, easiest to *see*) or
   **`spice gl=on`** (production). Restart the VM; `devcon update …\helios_kmd.inf "PCI\VEN_1AF4&DEV_1050"` to
   rebind Helios. Keep `virtio-gpu-gl venus=on,blob=on,hostmem=…`, render-server + udmabuf on.
2. **Guest:** add a **throwaway** `SET_SCANOUT_BLOB` path to the *current* System-class driver (a temp IOCTL or
   escape verb). Reuse `ALLOC_BLOB(blob_id = venus mem id)` + a venus render (the existing `helios_vk_exec`
   machinery produces a HOST3D blob); then call new `gpu.rs` helpers `set_scanout_blob(scanout=0, res_id, fmt,
   w, h)` + `resource_flush`.
3. **Observe.** If the venus frame appears on the gtk/spice window → **zero-copy venus scanout works, proceed to
   the DOD (§3).** On the host, confirm `virgl_cmd_set_scanout_blob` did **not** return `RESP_ERR_UNSPEC`
   ("resource not backed by dmabuf"), i.e. `res->base.dmabuf_fd >= 0`.
4. **If it fails** (`dmabuf_fd < 0`): the venus WSI/image memory isn't exportable, or ANV gave `OPAQUE_FD` only.
   Make the venus allocation chain `VkExportMemoryAllocateInfo` (dma-buf handle type; virglrenderer MR !1458
   context), re-confirm render-server + udmabuf, re-test. **Fix this before the DOD rewrite.**

---

## 3. STEP 2+ — the DOD (after the gate passes)

Per `DISPLAY.md` §3–§5 and §9. In order:

- **7.1 DOD skeleton.** INF Class System→**Display** {4d36e968-…}; `DriverEntry` →
  `DxgkInitializeDisplayOnlyDriver` + `DXGKDDI_DISPLAY_ONLY_FUNCTIONS`; `DxgkDdiStartDevice` maps BARs + inits
  the **reused** virtio transport; `DxgkConfigAccess` (revert from `BUS_INTERFACE_STANDARD`). Implement
  `DxgkDdiPresentDisplayOnly` + the VidPN/pointer DDIs (reference **VioGpuDod** + **qxldod**). **Recover the
  shapes from git** `658168f` (`lib.rs`, `dxgk.rs`, `build.rs`, `virtio/config.rs`, `ddi/escape.rs`,
  `helios_kmd.inx`) — take the display-only/escape/config parts; **leave** every render/AddAdapter/UMD piece.
  Gate: loads as a Display adapter (Code 0); the Windows **desktop appears on spice** (DOD 2D path).
- **7.2 Venus over `DxgkDdiEscape`.** Port today's `ioctl.rs` body to a `DxgkDdiEscape` handler (header re-used
  as the verb). Switch Mesa `vn_renderer_helios.c` transport `DeviceIoControl` → `D3DKMTOpenAdapterFromLuid` +
  `D3DKMTEscape` (now works — Helios is a real WDDM/LUID adapter). Gate: `helios_vk_exec` `vkQueueWaitIdle => 0`
  + `vkCmdFillBuffer` round-trip `0xDEADBEEF`, through the DOD.
- **7.3 Fullscreen present.** Add escape op `HELIOS_PRESENT_BLOB(res_id, w, h, fmt, fence)`; KMD scanout-0
  arbiter switches desktop-primary ⇄ venus blob via `SET_SCANOUT_BLOB`; double-buffer with the fence table.
  Venus WSI fullscreen path emits `HELIOS_PRESENT_BLOB` instead of the GDI blit. **Gate: fullscreen vkcube on
  spice, fast AND visually correct, zero-copy.**
- **7.4 Harden + DXVK/VKD3D.** CTX_DESTROY/blob-free on escape close / process teardown (the leak that makes
  `vkCreateInstance` fail -1 after crashes); mode-set/hotplug; then fullscreen DXVK/VKD3D titles.

---

## 4. Gotchas / non-obvious facts (carried forward)

- **No scanout-arbitration fight** (unlike the old `wsi-present-plan` worry): Helios IS the DOD, so it owns
  scanout 0 outright and just mode-switches it. `max_outputs=1` is fine.
- **`D3DKMTOpenAdapterFromLuid` now works** — a DOD is a real WDDM adapter with a LUID (resolves ARCH.md §12).
- **Desktop is WARP-composited + uploaded** per dirty rect — that's inherent and fine for 2D; the zero-copy win
  is only for venus content. Windowed 3D stays limited (round-trips to WARP). State this; don't promise it.
- **Don't bring back** `displib.lib`, `DxgkInitialize`, `d3dkmddi.h` render DDIs, `query_adapter_info.rs`, the
  cap-burst handlers, or the stub UMD — those are the rejected render path (`addadapter-umd-blocker`).
- **Keep** the Phase-4e async submit + the `vn_queue.c` fence fixes; don't regress the `helios_vk_exec` gate.
- **Build/test discipline:** KMD `infverif VALID` + `devcon` rebind; ICD via `win_meson`. GUI present only via
  `schtasks /it` into the interactive session (session-0 SSH can't present). Commit ICD (submodule) + the parent
  gitlink as scoped commits.

---

## 5. Reading order for the new session

1. `DISPLAY.md` (canonical spec — the whole design + the §8 gate + §10 repo deltas).
2. This file (the ordered steps).
3. Memories: `display-pivot` (start here), `systemclass-pivot` + `addadapter-umd-blocker` (what's rejected and
   why — so you don't redo it), `phase5-backend-status` + `mesa-venus-icd-port` (the venus ICD you're keeping),
   `fence-feedback-hack` + `wsi-bringup-status` (the dead software path + the host-lag root cause).
4. Git `658168f` for the reusable old WDDM display/escape/config code.
