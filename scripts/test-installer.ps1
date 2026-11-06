# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs, upgrades and uninstalls the given setup silently, and checks the result. This changes
# the machine, including the system PATH, so it is meant for CI runners. Needs elevation.
#
#   scripts\test-installer.ps1 -Setup target\installer\...-setup.exe -Version 1.2.3
param(
    [Parameter(Mandatory)][string]$Setup,
    [Parameter(Mandatory)][string]$Version
)
$ErrorActionPreference = 'Stop'
$failures = 0
function Check($ok, $text) {
    if ($ok) { "ok    $text" } else { "FAIL  $text"; $script:failures++ }
}
function MachinePath { [Environment]::GetEnvironmentVariable('Path', 'Machine') }
function Install {
    $p = Start-Process $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS="desktopicon,addtopath"' -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "Setup failed with exit code $($p.ExitCode)" }
}

$app = "$env:ProgramFiles\USB Flash Memory Toolset"
$menu = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\USB Flash Memory Toolset.lnk"
$desktop = "$env:Public\Desktop\USB Flash Memory Toolset.lnk"
$pathBefore = MachinePath
$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{6F1D2C54-8A3B-4E07-9C61-2B7E5D90A1F3}_is1'

Install
foreach ($f in 'usbflash-gui.exe', 'usbflash.exe', 'LICENSE.txt', 'README.md', 'SAFETY.md', 'THIRD_PARTY_LICENSES.html') {
    Check (Test-Path "$app\$f") "installed $f"
}
Check (Test-Path $menu) 'Start Menu shortcut'
Check (Test-Path $desktop) 'desktop shortcut'
$reported = & "$app\usbflash.exe" --version
Check ($reported -eq "usbflash $Version") "installed CLI reports: $reported"
& "$app\usbflash.exe" list --all | Out-Null
Check ($LASTEXITCODE -eq 0) 'installed CLI lists disks'
Check ((Get-ItemProperty $uninstallKey).DisplayVersion -eq $Version) 'version in the list of installed programs'

$entries = @((MachinePath) -split ';' | Where-Object { $_ -eq $app })
Check ($entries.Count -eq 1) 'PATH contains the program once'

Install
$entries = @((MachinePath) -split ';' | Where-Object { $_ -eq $app })
Check ($entries.Count -eq 1) 'PATH still contains it once after installing over itself'

$p = Start-Process "$app\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait -PassThru
Check ($p.ExitCode -eq 0) "uninstaller exit code $($p.ExitCode)"
# The uninstaller hands over to a temporary copy of itself, which finishes later.
for ($i = 0; $i -lt 60 -and (Test-Path "$app\usbflash-gui.exe"); $i++) { Start-Sleep -Seconds 1 }
for ($i = 0; $i -lt 30 -and (Test-Path $app); $i++) { Start-Sleep -Seconds 1 }
Check (-not (Test-Path $app)) 'program folder removed'
Check (-not (Test-Path $menu)) 'Start Menu shortcut removed'
Check (-not (Test-Path $desktop)) 'desktop shortcut removed'
Check (-not (Test-Path $uninstallKey)) 'removed from the list of installed programs'
Check ((MachinePath) -eq $pathBefore) 'PATH restored exactly'

if ($failures) { throw "$failures check(s) failed" }
'all checks passed'
