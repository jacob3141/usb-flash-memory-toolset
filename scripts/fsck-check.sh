#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Validates the built-in formatters against independent tools: fsck.vfat from
# dosfstools and fsck.exfat from exfatprogs. Runs on Linux, in CI and under WSL.
#
#   scripts/fsck-check.sh path/to/usbflash [workdir]
#
# FAT_SIZES and EXFAT_SIZES (in MiB) override the tested sizes. Images are sparse
# on Linux filesystems. On NTFS (e.g. /mnt/c under WSL) they are not, so keep the
# sizes small there.
set -euo pipefail

BIN=${1:?usage: fsck-check.sh path/to/usbflash [workdir]}
WORK=${2:-$(mktemp -d)}
FAT_SIZES=${FAT_SIZES:-"2 8 15 16 64 256 511 512 600 2048 8192 33000"}
EXFAT_SIZES=${EXFAT_SIZES:-"2 8 64 256 257 2048 33000 40000"}
FSCK_VFAT=${FSCK_VFAT:-fsck.vfat}
FSCK_EXFAT=${FSCK_EXFAT:-fsck.exfat}
mkdir -p "$WORK"
IMG="$WORK/fsck-check.img"
PART="$WORK/fsck-check-part.img"
failures=0

check() { # name, fsck command...
    local name=$1; shift
    local out
    if out=$("$@" 2>&1); then
        echo "ok    $name"
    else
        echo "FAIL  $name"; echo "$out" | sed 's/^/      /'
        failures=$((failures + 1))
    fi
}

reset() { # size_mib fs table sector_size label
    rm -f "$IMG"
    truncate -s "${1}M" "$IMG"
    "$BIN" --image --sector-size "$4" --json reset "$IMG" --quick --fs "$2" --table "$3" --label "$5" --yes >/dev/null
}

extract_partition() { # sector_size of the image; sfdisk assumes 512 for files
    local start count
    read -r start count < <(sfdisk -l -o Start,Sectors "$IMG" | tail -1)
    rm -f "$PART"
    dd if="$IMG" of="$PART" bs="$1" skip="$start" count="$count" conv=sparse status=none
}

for mib in $FAT_SIZES; do
    reset "$mib" fat none 512 "TEST"
    check "FAT   ${mib} MiB, whole device" "$FSCK_VFAT" -n "$IMG"
done
for mib in $EXFAT_SIZES; do
    reset "$mib" exfat none 512 "TEST"
    check "exFAT ${mib} MiB, whole device" "$FSCK_EXFAT" -n "$IMG"
done

# With partition tables, and on small sizes to keep extraction fast.
for table in mbr gpt; do
    for spec in "fat 64" "fat 600" "exfat 64" "exfat 600"; do
        read -r fs mib <<<"$spec"
        reset "$mib" "$fs" "$table" 512 "PARTED"
        extract_partition 512
        if [ "$fs" = fat ]; then
            check "FAT   ${mib} MiB in $table" "$FSCK_VFAT" -n "$PART"
        else
            check "exFAT ${mib} MiB in $table" "$FSCK_EXFAT" -n "$PART"
        fi
        check "      $table table of the $fs image" sfdisk --verify "$IMG"
    done
done

# 4Kn sectors, as on some card readers and SSD bridges. fsck.exfat only.
reset 256 exfat none 4096 "4KN"
check "exFAT 256 MiB, 4K sectors" "$FSCK_EXFAT" -n "$IMG"

rm -f "$IMG" "$PART"
if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
