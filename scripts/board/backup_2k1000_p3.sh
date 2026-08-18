#!/bin/sh
# Back up the fixed 2K1000LA P3 partition into P4 /persist.
# The output is split into U-Boot-loadable chunks and is never overwritten.

set -eu

P3_DEVICE=/dev/sda3
P3_BYTES=805306368
P3_START_LBA=0xa80800
P3_SECTORS=0x180000
P4_DEVICE=/dev/sda4
P4_BYTES=4294967296
P4_UUID=4d414e47-5354-4154-4500-000000000004
P4_LABEL=MANGO_STATE
CHUNK_MIB=64
CHUNK_BYTES=67108864
CHUNK_SECTORS=0x20000
CHUNK_COUNT=12
MIN_FREE_KIB=921600
BACKUP_ROOT=/persist/backups
BUSYBOX=/bin/busybox

fail() {
    echo "P3_BACKUP_ERROR $*" >&2
    exit 1
}

bb() {
    "$BUSYBOX" "$@"
}

backup_id=${1:-}
case "$backup_id" in
    ""|*[!A-Za-z0-9._-]*)
        fail "backup id must contain only A-Z, a-z, 0-9, dot, underscore or dash"
        ;;
esac

[ -x "$BUSYBOX" ] || fail "BusyBox is unavailable at $BUSYBOX"
for applet in blockdev sha256sum dd blkid stat df awk wc tr sync; do
    "$BUSYBOX" "$applet" --help >/dev/null 2>&1 || fail "BusyBox applet is unavailable: $applet"
done

p3_mount=$(bb awk '($1 == "/dev/sda3" || ($1 == "/sdcard" && $2 == "/sdcard")) && ($2 == "/tools" || $2 == "/" || $2 == "/sdcard") && $3 == "ext4" && $4 ~ /(^|,)ro(,|$)/ { print $2; exit }' /proc/mounts)
[ -n "$p3_mount" ] || fail "P3 must be mounted read-only at /tools, /sdcard, or /"
bb grep -Eq '^[^[:space:]]+[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' /proc/mounts \
    || fail "/persist is not the read-write P4 mount"
[ -r "$P3_DEVICE" ] || fail "$P3_DEVICE is not readable"
[ -r "$P4_DEVICE" ] || fail "$P4_DEVICE is not readable"
p3_mode=$(bb stat -c '%a' "$P3_DEVICE")
p4_mode=$(bb stat -c '%a' "$P4_DEVICE")
case "$p3_mode" in 440|660) ;; *) fail "unexpected P3 device mode: $p3_mode" ;; esac
case "$p4_mode" in 440|660) ;; *) fail "unexpected P4 device mode: $p4_mode" ;; esac

actual_bytes=$(bb blockdev --getsize64 "$P3_DEVICE")
[ "$actual_bytes" = "$P3_BYTES" ] \
    || fail "unexpected P3 size: $actual_bytes (expected $P3_BYTES)"
p4_bytes=$(bb blockdev --getsize64 "$P4_DEVICE")
[ "$p4_bytes" = "$P4_BYTES" ] \
    || fail "unexpected P4 size: $p4_bytes (expected $P4_BYTES)"

p4_blkid=$(bb blkid "$P4_DEVICE")
case "$p4_blkid" in *' TYPE="ext4"'*) ;; *) fail "P4 ext4 type is missing" ;; esac
case "$p4_blkid" in *" UUID=\"$P4_UUID\""*) ;; *) fail "P4 UUID mismatch" ;; esac
case "$p4_blkid" in *" LABEL=\"$P4_LABEL\""*) ;; *) fail "P4 label mismatch" ;; esac
free_kib=$(bb df -Pk /persist | bb awk 'NR == 2 { print $4 }')
case "$free_kib" in ""|*[!0-9]*) fail "cannot determine P4 free space" ;; esac
[ "$free_kib" -ge "$MIN_FREE_KIB" ] \
    || fail "insufficient P4 space: ${free_kib} KiB free, need ${MIN_FREE_KIB} KiB"

bb mkdir -p "$BACKUP_ROOT"
backup_dir="$BACKUP_ROOT/$backup_id"
[ ! -e "$backup_dir" ] || fail "destination already exists: $backup_dir"
bb mkdir "$backup_dir"

manifest_tmp="$backup_dir/MANIFEST.txt.tmp"
manifest="$backup_dir/MANIFEST.txt"
{
    echo "schema=1"
    echo "role=mangocore-2k1000la-p3-backup"
    echo "source_device=$P3_DEVICE"
    echo "source_bytes=$P3_BYTES"
    echo "source_start_lba=$P3_START_LBA"
    echo "source_sectors=$P3_SECTORS"
    echo "chunk_bytes=$CHUNK_BYTES"
    echo "chunk_sectors=$CHUNK_SECTORS"
    echo "chunk_count=$CHUNK_COUNT"
} > "$manifest_tmp"

i=0
while [ "$i" -lt "$CHUNK_COUNT" ]; do
    index=$(printf '%02d' "$i")
    file="p3-${index}.bin"
    path="$backup_dir/$file"
    skip_mib=$((i * CHUNK_MIB))
    start_lba=$((11012096 + i * 131072))

    echo "P3_BACKUP_CHUNK_BEGIN index=$index start_lba=$start_lba"
    bb dd if="$P3_DEVICE" of="$path" bs=1048576 skip="$skip_mib" count="$CHUNK_MIB" \
        2> "$backup_dir/p3-${index}.dd.log"
    copied_bytes=$(bb wc -c < "$path" | bb tr -d '[:space:]')
    [ "$copied_bytes" = "$CHUNK_BYTES" ] \
        || fail "short backup chunk $index: $copied_bytes bytes"

    destination_sha=$(bb sha256sum "$path" | bb awk '{ print $1 }')
    source_sha=$(bb dd if="$P3_DEVICE" bs=1048576 skip="$skip_mib" count="$CHUNK_MIB" \
        2>/dev/null | bb sha256sum | bb awk '{ print $1 }')
    [ "$source_sha" = "$destination_sha" ] \
        || fail "readback SHA-256 mismatch for chunk $index"

    printf 'chunk=%s start_lba=%s sectors=%s bytes=%s sha256=%s file=%s\n' \
        "$index" "$start_lba" 131072 "$CHUNK_BYTES" "$destination_sha" "$file" \
        >> "$manifest_tmp"
    bb sync
    echo "P3_BACKUP_CHUNK_OK index=$index bytes=$copied_bytes sha256=$destination_sha"
    i=$((i + 1))
done

bb mv "$manifest_tmp" "$manifest"
manifest_sha=$(bb sha256sum "$manifest" | bb awk '{ print $1 }')
{
    echo "backup_id=$backup_id"
    echo "manifest_sha256=$manifest_sha"
    echo "source_bytes=$P3_BYTES"
    echo "chunk_count=$CHUNK_COUNT"
} > "$backup_dir/COMPLETE.tmp"
bb sync
bb mv "$backup_dir/COMPLETE.tmp" "$backup_dir/COMPLETE"
bb sync

echo "P3_BACKUP_COMPLETE id=$backup_id dir=$backup_dir manifest_sha256=$manifest_sha"
