# Helios vGPU

Hardware-accelerated Vulkan for Windows guests on Linux/KVM hosts — without GPU
passthrough. Helios is a paravirtualized GPU driver stack: a **System-class KMDF
kernel driver** (Rust) for the QEMU `virtio-gpu` device plus a **Mesa Venus
Vulkan ICD**, tunneling Vulkan commands to the host GPU through QEMU and
virglrenderer's Venus renderer. The guest sees the host GPU as a Vulkan 1.4
physical device; rendering executes on real host silicon (RADV/ANV/NVIDIA).

Verified working end-to-end: `vulkaninfo`, `vkcube`, and DOOM (2016) rendering
through Venus on AMD and Intel hosts, on stock Fedora QEMU/virglrenderer
packages.

## How it fits together

```
+--------------------------------------------------------------+
|  Windows 11 Guest (KVM VM)                                   |
|                                                              |
|   D3D11/D3D12 app          Vulkan app                        |
|        | (DXVK / VKD3D-Proton - planned)                     |
|        v                        v                            |
|   +----------------------------------------------------+    |
|   |  Mesa Venus Vulkan ICD (vulkan_virtio.dll)         |    |
|   |  Venus command encoder                             |    |
|   +------------------------+---------------------------+    |
|                            | DeviceIoControl                 |
|                            | (GUID_DEVINTERFACE_HELIOS)      |
|   +------------------------v---------------------------+    |
|   |  Helios KMD (helios_kmd.sys)                       |    |
|   |  System-class KMDF function driver                 |    |
|   +------------------------+---------------------------+    |
|                            | virtio queues (VEN_1AF4:1050)   |
+----------------------------+---------------------------------+
                             |
+----------------------------v---------------------------------+
|  Linux host                                                  |
|  QEMU virtio-gpu-gl (venus=true, blob=true, hostmem=...)     |
|      -> virglrenderer (Venus decoder / render server)        |
|          -> host Vulkan driver (RADV / ANV / ...)            |
|              -> physical GPU                                 |
+---------------------------------------------------------------+
```

There is deliberately **no WDDM/display driver**: the Helios device is a render
device, not a Windows display adapter. Presentation goes through GDI into
whatever desktop the VM has (std VGA console, RDP session, or a Looking Glass
IDD — see [docs/LOOKING_GLASS.md](docs/LOOKING_GLASS.md)).

## Quickstart

**Host** (Linux, kernel 6.13+): QEMU 9.2+ and a Venus-enabled virglrenderer
(Fedora 41+ stock packages work). Attach to the VM:

```
-device virtio-gpu-gl-pci,venus=true,blob=true,hostmem=8G,max_hostmem=8G
-object memory-backend-memfd,id=mem0,size=<ram>M,share=on -machine memory-backend=mem0
```

Keep the guest's *desktop* display on a non-virtio adapter (std VGA / QXL):
`virtio-vga` carries the same PCI ID the Helios driver binds.

**Guest** (Windows 11): enable test signing (`bcdedit /set testsigning on`,
Secure Boot off), then install a driver package — built by CI
(`.github/workflows/package.yml`) or locally by
[tools/package/make-package.sh](tools/package/make-package.sh) — and reboot.
See [tools/package/README.md](tools/package/README.md) for details and
troubleshooting.

**Verify**: `vulkaninfo --summary` in the guest should list
`Virtio-GPU Venus (<your host GPU>)`.

## Repository map

| Path | What |
|---|---|
| `kmd/` | The kernel driver (Rust, KMDF via windows-drivers-rs) |
| `icd/mesa` | Mesa fork (submodule) — the Venus ICD with the Helios IOCTL backend |
| `icd/win-build/` | Out-of-tree scaffolding to build Mesa's venus on Windows (mingw) |
| `protocol/` | Shared `no_std` wire definitions (IOCTLs, virtio-gpu structs) — single source of truth |
| `probe/` | IOCTL smoke test against the live driver |
| `host/` | Host-side QMP diagnostics |
| `tools/package/` | Driver package build + install/uninstall scripts |
| `tools/win-mcp/` | MCP server for driving the Windows dev VM over SSH |
| `LookingGlass` | Looking Glass (submodule) — optional low-latency display path |
| `docs/` | Active documentation (see below) |
| `docs/archive/` | Historical records: the abandoned WDDM/DOD display pivot, phase handovers |

Submodules: `icd/mesa` is required for ICD builds (`git submodule update --init
--depth 1 icd/mesa`); `LookingGlass` only for the Looking Glass display path.

## Driving the Windows dev VM

Builds and tests run inside a Windows guest, driven from Linux over SSH by the
`tools/win-mcp` MCP server (build it with `CARGO_TARGET_DIR=target/linux cargo
build --release` in that directory). Register it per machine — the binary path
and Windows account are machine-specific, so `.mcp.json` is gitignored. Either
copy [.mcp.json.example](.mcp.json.example) to `.mcp.json` and edit
`HELIOS_WIN_USER`, or run:

```
claude mcp add win -e HELIOS_WIN_USER=<WindowsUser> -- <repo>/tools/win-mcp/target/linux/release/win-mcp
```

VM path/host overrides (`HELIOS_WIN_USER`, `HELIOS_WIN_SSH_HOST`,
`HELIOS_WIN_MIRROR_ROOT`, `HELIOS_WIN_MESA_BUILD`, `HELIOS_WIN_MINGW_BIN`,
`HELIOS_WIN_LG_BUILD`) are documented in [docs/TOOLCHAIN.md](docs/TOOLCHAIN.md).

## Documentation

Start with [ARCH.md](ARCH.md) (canonical architecture), then per subsystem:
[docs/KMD.md](docs/KMD.md), [docs/ICD.md](docs/ICD.md),
[docs/TRANSPORT.md](docs/TRANSPORT.md), [docs/HOST.md](docs/HOST.md),
[docs/TOOLCHAIN.md](docs/TOOLCHAIN.md),
[docs/LOOKING_GLASS.md](docs/LOOKING_GLASS.md). The decision record for the
current architecture is
[docs/decisions/SYSTEM_CLASS_REFOCUS_2026_06_07.md](docs/decisions/SYSTEM_CLASS_REFOCUS_2026_06_07.md).

## Status & caveats

- The renderer path (KMD + ICD + Venus) is functional; presentation/display
  integration and DXVK/VKD3D validation are active work.
- The KMD is fence-**polling** by design (the interrupt path is parked; see
  docs/archive/PHASE4E_ASYNC_HANDOVER.md section 4 for why).
- Drivers are test-signed: guests must run with test signing enabled. Production
  signing is a future concern.
- Known-open hardening items: non-elevated device-interface access (SDDL),
  host-context cleanup when a client crashes, and a rare boot-time 0x7F
  bugcheck under investigation (avoid live driver rebinds; bind at boot).
