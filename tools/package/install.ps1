# install.ps1 - Helios vGPU guest driver package installer.
#
# Installs the test-signed Helios KMD (System-class KMDF for virtio-gpu
# PCI\VEN_1AF4&DEV_1050) and the Mesa Venus Vulkan ICD, then asks for a reboot.
#
# Hard-won rules encoded here (see repo docs):
#  * Test signing must be ON before the driver can load (bcdedit /set testsigning on
#    + reboot). Secure Boot must be off for that to be possible.
#  * virtio-win's viogpudo.inf (Red Hat "VirtIO GPU DOD controller") matches the
#    same PCI ID and OUT-RANKS a test-signed driver - it must be removed or the
#    Helios KMD never binds.
#  * NEVER live-rebind the device (that produced an 0x7F BSOD + broken device
#    interface). This script only STAGES the driver (pnputil /add-driver without
#    /install); the clean bind happens at the next boot.
#
# Usage (elevated PowerShell, from the extracted package directory):
#   powershell -ExecutionPolicy Bypass -File .\install.ps1
#
#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    # Continue even if test signing is currently off (you still must enable it
    # and reboot before the driver will load).
    [switch]$SkipTestSigningCheck
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$driverDir = Join-Path $root 'driver'
$icdDir    = Join-Path $root 'icd'

function Fail([string]$msg, [int]$code = 1) {
    Write-Host "ERROR: $msg" -ForegroundColor Red
    exit $code
}

Write-Host '=== Helios vGPU driver package installer ==='
if (Test-Path (Join-Path $root 'versions.json')) {
    Write-Host (Get-Content (Join-Path $root 'versions.json') -Raw)
}

# --- 0. Sanity: package contents -------------------------------------------
$inf  = Join-Path $driverDir 'helios_kmd.inf'
$cert = Join-Path $driverDir 'WDRLocalTestCert.cer'
if (-not (Test-Path $inf))  { Fail "missing $inf - run from the extracted package" }
if (-not (Test-Path $cert)) { Fail "missing $cert" }
$icdJson = Get-ChildItem $icdDir -Filter '*.json' | Select-Object -First 1
$icdDll  = Get-ChildItem $icdDir -Filter '*.dll'  | Select-Object -First 1
if (-not $icdJson -or -not $icdDll) { Fail "missing ICD dll/json under $icdDir" }

# --- 1. Test signing --------------------------------------------------------
$testsigning = (bcdedit /enum '{current}' | Select-String -Pattern 'testsigning\s+Yes')
if (-not $testsigning) {
    Write-Host 'Test signing is OFF. The Helios KMD is test-signed and will not load.' -ForegroundColor Yellow
    Write-Host 'Enable it with:  bcdedit /set testsigning on   (requires Secure Boot off), then reboot.'
    if (-not $SkipTestSigningCheck) {
        Fail 'aborting (pass -SkipTestSigningCheck to stage the driver anyway)' 2
    }
} else {
    Write-Host '[1/5] test signing: ON'
}

# --- 2. Evict the conflicting Red Hat viogpudo driver ------------------------
# virtio-win guest tools install viogpudo.inf, which claims VEN_1AF4&DEV_1050
# with a better signature rank than any test-signed package.
Write-Host '[2/5] checking for conflicting viogpudo (Red Hat VirtIO GPU DOD) ...'
$enum = pnputil /enum-drivers | Out-String
$blocks = $enum -split '(?=Published Name:)' | Where-Object { $_ -match 'viogpudo\.inf' }
if ($blocks) {
    foreach ($b in $blocks) {
        if ($b -match 'Published Name:\s*(oem\d+\.inf)') {
            $oem = $Matches[1]
            Write-Host "  removing $oem (viogpudo.inf)"
            pnputil /delete-driver $oem /uninstall | Out-Null
        }
    }
    Write-Host '  removed. (Other virtio-win drivers are untouched.)'
} else {
    Write-Host '  none found.'
}

# --- 3. Trust the test certificate -------------------------------------------
Write-Host '[3/5] installing test certificate (Root + TrustedPublisher) ...'
certutil -addstore -f Root $cert | Out-Null
if ($LASTEXITCODE -ne 0) { Fail 'certutil -addstore Root failed' }
certutil -addstore -f TrustedPublisher $cert | Out-Null
if ($LASTEXITCODE -ne 0) { Fail 'certutil -addstore TrustedPublisher failed' }

# --- 4. Stage the KMD (bind happens at next boot - never live-rebind) --------
Write-Host '[4/5] staging Helios KMD into the driver store ...'
$out = pnputil /add-driver $inf | Out-String
Write-Host ($out.Trim())
if ($LASTEXITCODE -ne 0) {
    if ($out -match 'not within its validity period') {
        Fail 'signature/cert validity failure - check that this machine''s clock (including timezone) is correct'
    }
    Fail "pnputil /add-driver failed (exit $LASTEXITCODE)"
}

# --- 5. Install the Vulkan ICD ------------------------------------------------
Write-Host '[5/5] installing Mesa Venus ICD ...'
$icdInstallDir = 'C:\ProgramData\HeliosVulkan'
New-Item -ItemType Directory -Force -Path $icdInstallDir | Out-Null
Copy-Item $icdDll.FullName  (Join-Path $icdInstallDir $icdDll.Name)  -Force
Copy-Item $icdJson.FullName (Join-Path $icdInstallDir $icdJson.Name) -Force
$manifestPath = Join-Path $icdInstallDir $icdJson.Name
# The packaged manifest carries a bare DLL filename; the Vulkan loader treats
# bare names as LoadLibrary search-path lookups (NOT manifest-relative), so
# finalize library_path to the absolute installed location.
$manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
$manifest.ICD.library_path = ((Join-Path $icdInstallDir $icdDll.Name) -replace '\\','/')
$manifest | ConvertTo-Json | Set-Content $manifestPath -Encoding ascii

$key = 'HKLM:\SOFTWARE\Khronos\Vulkan\Drivers'
New-Item -Path $key -Force | Out-Null
# Drop stale Helios entries (old hashes / old paths), then register this one.
$props = (Get-Item $key).Property | Where-Object { $_ -like '*HeliosVulkan*' -and $_ -ne $manifestPath }
foreach ($p in $props) { Remove-ItemProperty -Path $key -Name $p }
New-ItemProperty -Path $key -Name $manifestPath -PropertyType DWord -Value 0 -Force | Out-Null
Write-Host "  registered $manifestPath"

# --- Status -------------------------------------------------------------------
$dev = Get-PnpDevice | Where-Object { $_.InstanceId -like '*VEN_1AF4&DEV_1050*' }
if ($dev) {
    Write-Host ("device now: {0} [{1}]" -f $dev.FriendlyName, $dev.Status)
} else {
    Write-Host 'virtio-gpu device not present in this VM yet - the driver will bind when it appears (at boot).'
}
Write-Host ''
Write-Host 'DONE. REBOOT NOW so the Helios KMD binds cleanly at boot.' -ForegroundColor Green
Write-Host 'After reboot, verify with: vulkaninfo --summary  (expect a "Virtio-GPU Venus (...)" GPU).'
