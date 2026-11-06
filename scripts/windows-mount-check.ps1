# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lets Windows itself validate the built-in formatters: attaches VHD files created from
# images, then mounts them, writes a file, reads it back and runs chkdsk. The attached VHDs
# also exercise raw disk reading through \\.\PhysicalDriveN, and the policy refusing virtual
# disks. Touches only the given VHD files. Run elevated.
#
#   scripts\windows-mount-check.ps1 -Usbflash target\release\usbflash.exe -Vhd a.vhd,b.vhd
param(
    [Parameter(Mandatory)][string]$Usbflash,
    [Parameter(Mandatory)][string[]]$Vhd
)
$failures = 0
function Report($ok, $text) {
    if ($ok) { "ok    $text" } else { "FAIL  $text"; $script:failures++ }
}

foreach ($file in $Vhd) {
    $path = (Resolve-Path $file).Path
    "--- $path"
    $image = Mount-DiskImage -ImagePath $path -StorageType VHD -Access ReadWrite -PassThru
    try {
        Start-Sleep -Seconds 3
        $disk = $image | Get-DiskImage | Get-Disk
        $device = "\\.\PhysicalDrive$($disk.Number)"
        "disk: $device, $($disk.PartitionStyle), bus $($disk.BusType)"

        $partition = $disk | Get-Partition | Where-Object { $_.Type -notin 'Reserved', 'Unknown' } | Select-Object -First 1
        if (-not $partition.DriveLetter -or $partition.DriveLetter -eq "`0") {
            $partition | Add-PartitionAccessPath -AssignDriveLetter
            Start-Sleep -Seconds 2
            $partition = $partition | Get-Partition
        }
        $volume = $partition | Get-Volume
        $letter = $partition.DriveLetter
        "volume: $letter`:, $($volume.FileSystemType), label '$($volume.FileSystemLabel)', health $($volume.HealthStatus), $($volume.Size) bytes"
        Report ($volume.HealthStatus -eq 'Healthy') 'Windows reports the volume as healthy'

        $probe = "$letter`:\usbflash-mount-check.bin"
        $data = New-Object byte[] (20MB)
        (New-Object Random 42).NextBytes($data)
        [IO.File]::WriteAllBytes($probe, $data)
        $back = [IO.File]::ReadAllBytes($probe)
        $same = [Linq.Enumerable]::SequenceEqual([byte[]]$data, [byte[]]$back)
        Report $same 'A 20 MiB file can be written and read back'
        Remove-Item $probe

        $chk = & chkdsk.exe "$letter`:" 2>&1
        Report ($LASTEXITCODE -eq 0) "chkdsk exit code $LASTEXITCODE"
        if ($LASTEXITCODE -ne 0) { $chk | ForEach-Object { "      $_" } }

        # Raw access to the attached disk, elevated.
        $analysis = & $Usbflash --json analyze $device | ConvertFrom-Json
        $seen = $analysis.result.partitions[0].filesystem.name
        Report ($seen -and "$($volume.FileSystemType)" -like "*$($seen.Substring(0, 3))*") "usbflash analyze reads the raw disk and finds $seen"
        $health = & $Usbflash --json health $device | ConvertFrom-Json
        Report ($health.result.verdict -eq 'good') "usbflash health verdict: $($health.result.verdict), SMART $($health.result.smart.status)"

        # Without --yes and with empty stdin, so that nothing could happen even if the policy failed.
        $refusal = cmd.exe /c "`"$Usbflash`" wipe $device --allow-fixed < NUL 2>&1"
        Report ("$refusal" -match 'only USB') "A virtual disk is refused: $refusal"
    }
    finally {
        Dismount-DiskImage -ImagePath $path | Out-Null
    }
}
if ($failures) { "$failures check(s) failed"; exit 1 }
"all checks passed"
