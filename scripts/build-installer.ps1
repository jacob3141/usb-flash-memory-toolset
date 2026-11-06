# SPDX-License-Identifier: GPL-3.0-or-later
#
# Builds the Windows installer with Inno Setup. Expects these from the repository root:
#   cargo build --workspace --release --locked
#   cargo about generate --output-file THIRD_PARTY_LICENSES.html about.hbs
#
#   scripts\build-installer.ps1 -Version 1.2.3
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'

$iscc = Get-Command iscc.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
if (-not (Test-Path $iscc)) { throw 'Inno Setup 6 was not found. Install it with: winget install JRSoftware.InnoSetup' }

$root = Split-Path $PSScriptRoot -Parent
& $iscc "/DAppVersion=$Version" /Qp (Join-Path $root 'packaging\windows\usbflash-toolset.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }

Join-Path $root "target\installer\usbflash-toolset-$Version-windows-x64-setup.exe"
