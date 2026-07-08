#!/usr/bin/env bash
# make-package.sh - build the Helios vGPU guest driver package end-to-end on the
# local win11 dev VM (over `ssh win`) and emit helios-vgpu-<ver>.zip.
#
# This is the LOCAL counterpart to .github/workflows/package.yml. Unlike CI, it
# runs against the real Helios VM, so after building it can OPTIONALLY smoke-test
# the package on actual hardware (--test): install, reboot, and check that
# vulkaninfo enumerates a Venus GPU.
#
# It mirrors the manual bring-up flow proven on 2026-07-08:
#   KMD:  robocopy Z:\ -> local mirror, cargo make (in-mirror target dir)
#   ICD:  robocopy mesa -> local src (Z:\ breaks meson realpath), meson + ninja
#
# Usage:
#   tools/package/make-package.sh                 # build + zip
#   tools/package/make-package.sh --test          # + install/reboot/vulkaninfo
#   HELIOS_WIN_USER=Tibix tools/package/make-package.sh   # override VM user
#
set -euo pipefail

SSH_HOST="${HELIOS_WIN_SSH_HOST:-win}"
WIN_USER="${HELIOS_WIN_USER:-Tibix}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${HELIOS_PACKAGE_OUT:-$REPO_ROOT/dist}"
DO_TEST=0
[ "${1:-}" = "--test" ] && DO_TEST=1

MIRROR="C:\\Users\\$WIN_USER\\helios-vgpu"
MESA_SRC="C:\\Users\\$WIN_USER\\helios-mesa-src"
MESA_BUILD="C:\\Users\\$WIN_USER\\helios-mesa-build"
WINBUILD="C:\\Users\\$WIN_USER\\helios-win-build"
NATIVE_FILE="C:\\Users\\$WIN_USER\\helios-mingw-native.ini"
COMPAT_H="$WINBUILD\\helios_win_compat.h"

log()  { printf '\033[1;36m>>> %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

# Run a PowerShell snippet on the VM; fail the script if it returns nonzero.
winps() {
  ssh -o BatchMode=yes "$SSH_HOST" "powershell -NoProfile -Command \"$1\"" 2>&1 \
    | grep -v "Warning: Permanently" || true
}

VER="${HELIOS_PACKAGE_VERSION:-0.1.0+$(git -C "$REPO_ROOT" rev-parse --short HEAD)}"
COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD)"
STAGE="$OUT_DIR/helios-vgpu-$VER"

log "Helios package build - version $VER"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$SSH_HOST" "echo ok" >/dev/null 2>&1 \
  || fail "cannot reach the VM over 'ssh $SSH_HOST' - is it running? (distrobox enter helios -- bash tools/../helios-vm/start-vm.sh)"

# --- 1. KMD ------------------------------------------------------------------
log "[1/4] building KMD (.sys + .inf) on the VM"
winps "robocopy Z:\\ $MIRROR /MIR /XD target .git 'Z:\\icd\\mesa' /NFL /NDL /NJH /NJS /NP /R:1 /W:1 | Out-Null; if (\$LASTEXITCODE -ge 8) { throw 'mirror sync failed' }; \$env:LIBCLANG_PATH='C:\\Program Files\\LLVM\\bin'; Set-Location $MIRROR\\kmd; cargo make --makefile Cargo.make.toml 2>&1 | Select-Object -Last 3"
winps "if (-not (Test-Path '$MIRROR\\kmd\\target\\debug\\helios_kmd_package\\helios_kmd.sys')) { throw 'KMD .sys not produced' }; 'KMD OK'"

# --- 2. ICD ------------------------------------------------------------------
log "[2/4] building Mesa Venus ICD (vulkan_virtio.dll) on the VM"
# meson can't realpath the Z:\ WinFsp share - mirror mesa + the compat headers
# to local NTFS first (the ~8 symlink files that fail ROBOCOPY error 123 are
# non-build-critical).
winps "robocopy Z:\\icd\\mesa $MESA_SRC /MIR /XD .git /XF .git /NFL /NDL /NJH /NJS /NP /R:0 /W:0 | Out-Null; robocopy Z:\\icd\\win-build $WINBUILD /MIR /NFL /NDL /NJH /NJS /NP /R:1 /W:1 | Out-Null; 'mesa mirrored'"
winps "Remove-Item -Recurse -Force $MESA_BUILD -ErrorAction SilentlyContinue; meson setup $MESA_BUILD $MESA_SRC --native-file $NATIVE_FILE '-Dc_args=-include$COMPAT_H' -Dvulkan-drivers=virtio -Dgallium-drivers= -Dplatforms=windows -Dvideo-codecs= -Dvulkan-layers= -Degl=disabled -Dgbm=disabled -Dglx=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled -Dllvm=disabled -Dshader-cache=disabled -Dbuild-tests=false -Dperfetto=false --buildtype=debugoptimized 2>&1 | Select-Object -Last 2"
winps "meson compile -C $MESA_BUILD 2>&1 | Select-Object -Last 2; if (-not (Test-Path '$MESA_BUILD\\src\\virtio\\vulkan\\vulkan_virtio.dll')) { throw 'ICD dll not produced' }; 'ICD OK'"

# --- 3. Assemble the package locally ----------------------------------------
log "[3/4] assembling package into $STAGE"
rm -rf "$STAGE"; mkdir -p "$STAGE/driver" "$STAGE/icd"
# scp needs forward-slash remote paths (backslashes + globs die in SFTP).
scp -q "$SSH_HOST:${MIRROR//\\//}/kmd/target/debug/helios_kmd_package/*" "$STAGE/driver/" \
  || fail "failed to fetch driver package from VM"
DLL_TMP="$(mktemp -d)"
scp -q "$SSH_HOST:${MESA_BUILD//\\//}/src/virtio/vulkan/vulkan_virtio.dll" "$DLL_TMP/" \
  || fail "failed to fetch ICD dll from VM"

HASH="$(sha256sum "$DLL_TMP/vulkan_virtio.dll" | cut -c1-12)"
DLL_NAME="vulkan_virtio-$HASH.dll"
cp "$DLL_TMP/vulkan_virtio.dll" "$STAGE/icd/$DLL_NAME"
rm -rf "$DLL_TMP"
cat > "$STAGE/icd/virtio_devenv_icd.x86_64.json" <<JSON
{
    "file_format_version": "1.0.1",
    "ICD": { "library_path": "$DLL_NAME", "library_arch": "64", "api_version": "1.4.0" }
}
JSON
cp "$REPO_ROOT/tools/package/install.ps1" "$REPO_ROOT/tools/package/uninstall.ps1" \
   "$REPO_ROOT/tools/package/README.md" "$STAGE/"
cat > "$STAGE/versions.json" <<JSON
{ "version": "$VER", "commit": "$COMMIT", "built": "$(date -u +%FT%TZ)", "builder": "make-package.sh" }
JSON

( cd "$OUT_DIR" && zip -qr "helios-vgpu-$VER.zip" "helios-vgpu-$VER" )
log "package: $OUT_DIR/helios-vgpu-$VER.zip"

# --- 4. Optional on-hardware smoke test -------------------------------------
if [ "$DO_TEST" = "1" ]; then
  log "[4/4] smoke-testing the package on the VM"
  REMOTE="C:\\Users\\$WIN_USER\\helios-pkg-test"
  winps "Remove-Item -Recurse -Force $REMOTE -ErrorAction SilentlyContinue; New-Item -ItemType Directory -Force $REMOTE | Out-Null"
  scp -qr "$STAGE"/* "$SSH_HOST:${REMOTE//\\//}/" || fail "failed to upload package to VM"
  ssh -o BatchMode=yes "$SSH_HOST" \
    "powershell -NoProfile -ExecutionPolicy Bypass -File $REMOTE\\install.ps1" 2>&1 \
    | grep -v "Warning: Permanently" || fail "install.ps1 failed on the VM"
  log "rebooting VM to bind the driver cleanly ..."
  winps "shutdown /r /t 2" || true
  # Let the guest actually go down before probing for it coming back.
  sleep 25
  for _ in $(seq 1 40); do
    sleep 6
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$SSH_HOST" "echo up" >/dev/null 2>&1 && break
  done
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$SSH_HOST" "echo up" >/dev/null 2>&1 \
    || fail "VM did not come back after reboot"
  log "verifying vulkaninfo enumerates a Venus GPU ..."
  RESULT="$(winps "\$vi=(Get-Command vulkaninfo -ErrorAction SilentlyContinue).Source; if (-not \$vi) { \$vi=(Get-ChildItem C:\\VulkanSDK\\*\\Bin\\vulkaninfo.exe -ErrorAction SilentlyContinue | Select-Object -First 1).FullName }; if (-not \$vi) { throw 'vulkaninfo not found in guest' }; & \$vi --summary 2>&1 | Select-String 'deviceName|driverID' | Select-Object -First 4")"
  echo "$RESULT"
  echo "$RESULT" | grep -q 'Venus' \
    && log "SMOKE TEST PASSED - Venus GPU enumerates from the packaged build" \
    || fail "SMOKE TEST FAILED - no Venus GPU after install (check testsigning, viogpudo conflict, device attached)"
fi

log "done."
