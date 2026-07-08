# Helios vGPU - guest driver package

A ready-to-install bundle of the Helios virtual-GPU guest driver: a System-class
KMDF kernel driver for the QEMU `virtio-gpu` device plus a Mesa **Venus** Vulkan
ICD. Together they give a Windows guest hardware-accelerated Vulkan rendered on
the *host* GPU (via QEMU + virglrenderer's Venus backend).

## Package contents

```
helios-vgpu-<version>/
├── install.ps1          # installer (run elevated)
├── uninstall.ps1        # remover (run elevated)
├── versions.json        # version + source commit + build metadata
├── driver/              # KMD: helios_kmd.sys/.inf/.cat + WDRLocalTestCert.cer
└── icd/                 # Mesa Venus ICD: vulkan_virtio-<hash>.dll + Khronos manifest
```

## Host requirements (the VM must be launched with)

- QEMU **9.2+** with a Venus-capable **virglrenderer 1.x** (Fedora 44's stock
  `qemu` + `virglrenderer` packages are sufficient - no source build needed).
- A `virtio-gpu-gl-pci` device with `venus=true,blob=true,hostmem=<N>` and a
  shared memory backend (`memory-backend-memfd,share=on`).
- The guest's **desktop** display should be a *non-virtio* adapter (std VGA /
  QXL). `virtio-vga` shares this driver's PCI ID (`VEN_1AF4&DEV_1050`) and would
  be claimed by the Helios KMD, taking it away from desktop duty.

## Guest requirements

- **Test signing ON** (`bcdedit /set testsigning on`, needs Secure Boot **off**,
  then reboot). The KMD is signed with a local test certificate.
- A Vulkan **loader** (`vulkan-1.dll`) - e.g. install the Vulkan SDK/Runtime - to
  actually use the ICD. `vulkaninfo`/`vkcube` come with the SDK.

## Install

From an **elevated** PowerShell, in the extracted folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

Then **reboot** (important - see below), and verify:

```powershell
vulkaninfo --summary    # expect a "Virtio-GPU Venus (<host GPU>)" device
```

## Why the installer stages, then asks you to reboot

The installer deliberately does **not** live-attach the driver to the running
device. Two hard-won reasons:

1. **Driver ranking.** If the `virtio-win` guest tools are installed, their
   `viogpudo.inf` (Red Hat "VirtIO GPU DOD controller") matches the same PCI ID
   and *out-ranks* a test-signed package. The installer removes it so the Helios
   KMD can win the bind. That takes effect on the next enumeration (boot).
2. **Stability.** Swapping the driver on a live device node was observed to
   produce an `ACCESS_DENIED` on the device interface and, on a later attempt, a
   `0x7F UNEXPECTED_KERNEL_MODE_TRAP` bugcheck. Binding cleanly at boot avoids
   this entirely.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `pnputil` - *"certificate is not within its validity period"* | Guest clock (incl. **timezone**) is wrong; the signing timestamp predates the cert. Fix the clock and reinstall. |
| `vulkaninfo` shows no Venus GPU | Test signing off, or the device isn't attached, or `viogpudo` still bound - reboot after install; confirm the `virtio-gpu-gl-pci` device is present. |
| Device shows a yellow bang in Device Manager | Check `testsigning`; check the KMD's problem code with `Get-PnpDevice`. |
| App picks the wrong GPU | The host's llvmpipe is also exposed through Venus (as a CPU-type device). Select the discrete GPU explicitly. |

See `versions.json` for the exact source commit this package was built from.
