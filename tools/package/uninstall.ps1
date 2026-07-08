# uninstall.ps1 - remove the Helios vGPU driver package installed by install.ps1.
#
#Requires -RunAsAdministrator
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Write-Host '=== Helios vGPU driver package uninstaller ==='

# KMD: find the staged package by its original INF name.
$enum = pnputil /enum-drivers | Out-String
$blocks = $enum -split '(?=Published Name:)' | Where-Object { $_ -match 'helios_kmd\.inf' }
if ($blocks) {
    foreach ($b in $blocks) {
        if ($b -match 'Published Name:\s*(oem\d+\.inf)') {
            $oem = $Matches[1]
            Write-Host "removing driver package $oem (helios_kmd.inf)"
            pnputil /delete-driver $oem /uninstall | Out-Null
        }
    }
} else {
    Write-Host 'no helios_kmd.inf package found in the driver store.'
}

# ICD: registry entries + files.
$key = 'HKLM:\SOFTWARE\Khronos\Vulkan\Drivers'
if (Test-Path $key) {
    $props = (Get-Item $key).Property | Where-Object { $_ -like '*HeliosVulkan*' }
    foreach ($p in $props) {
        Write-Host "removing Vulkan driver registration $p"
        Remove-ItemProperty -Path $key -Name $p
    }
}
if (Test-Path 'C:\ProgramData\HeliosVulkan') {
    Remove-Item -Recurse -Force 'C:\ProgramData\HeliosVulkan'
    Write-Host 'removed C:\ProgramData\HeliosVulkan'
}

# Test certificate.
certutil -delstore Root WDRLocalTestCert 2>&1 | Out-Null
certutil -delstore TrustedPublisher WDRLocalTestCert 2>&1 | Out-Null
Write-Host 'removed WDRLocalTestCert from Root/TrustedPublisher (if present).'

Write-Host ''
Write-Host 'DONE. Reboot to release the currently-loaded driver.' -ForegroundColor Green
