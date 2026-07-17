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
P4_UUID=4d414e47535441544500000000000004
P4_LABEL=MANGO_STATE
CHUNK_MIB=64
CHUNK_BYTES=67108864
CHUNK_SECTORS=0x20000
CHUNK_COUNT=12
MIN_FREE_KIB=921600
BACKUP_ROOT=/persist/backups

fail() {
    echo "P3_BACKUP_ERROR $*" >&2
    exit 1
}

backup_id=${1:-}
case "$backup_id" in
    ""|*[!A-Za-z0-9._-]*)
        fail "backup id must contain only A-Z, a-z, 0-9, dot, underscore or dash"
        ;;
esac

command -v blockdev >/dev/null 2>&1 || fail "blockdev is unavailable"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is unavailable"
command -v dd >/dev/null 2>&1 || fail "dd is unavailable"

grep -Eq '^/tools[[:space:]]+/tools[[:space:]]+ext4[[:space:]]+ro([,[:space:]])' /proc/mounts \
    || fail "/tools is not the read-only P3 mount"
grep -Eq '^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' /proc/mounts \
    || fail "/persist is not the read-write P4 mount"
[ -r "$P3_DEVICE" ] || fail "$P3_DEVICE is not readable"
[ -r "$P4_DEVICE" ] || fail "$P4_DEVICE is not readable"
p3_mode=$(stat -c '%a' "$P3_DEVICE")
p4_mode=$(stat -c '%a' "$P4_DEVICE")
[ "$p3_mode" = "440" ] || fail "unexpected P3 device mode: $p3_mode"
[ "$p4_mode" = "440" ] || fail "unexpected P4 device mode: $p4_mode"

actual_bytes=$(blockdev --getsize64 "$P3_DEVICE")
[ "$actual_bytes" = "$P3_BYTES" ] \
    || fail "unexpected P3 size: $actual_bytes (expected $P3_BYTES)"
p4_bytes=$(blockdev --getsize64 "$P4_DEVICE")
[ "$p4_bytes" = "$P4_BYTES" ] \
    || fail "unexpected P4 size: $p4_bytes (expected $P4_BYTES)"

p4_info=$(/tools/tests/cpython/python3-wrapper.sh -c \
    "import struct as u;f=open('$P4_DEVICE','rb');f.seek(1024);s=f.read(136);v=lambda x:u.unpack_from('<I',s,x)[0];print(s[56:58].hex(),v(12)*(1024<<v(24))//1024,s[104:120].hex(),s[120:136].rstrip(bytes([0])).decode())")
set -- $p4_info
[ "${1:-}" = "53ef" ] || fail "P4 ext4 magic mismatch: ${1:-missing}"
free_kib=${2:-}
[ "${3:-}" = "$P4_UUID" ] || fail "P4 UUID mismatch: ${3:-missing}"
[ "${4:-}" = "$P4_LABEL" ] || fail "P4 label mismatch: ${4:-missing}"
case "$free_kib" in ""|*[!0-9]*) fail "cannot determine P4 free space" ;; esac
[ "$free_kib" -ge "$MIN_FREE_KIB" ] \
    || fail "insufficient P4 space: ${free_kib} KiB free, need ${MIN_FREE_KIB} KiB"

mkdir -p "$BACKUP_ROOT"
backup_dir="$BACKUP_ROOT/$backup_id"
[ ! -e "$backup_dir" ] || fail "destination already exists: $backup_dir"
mkdir "$backup_dir"

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
    dd if="$P3_DEVICE" of="$path" bs=1048576 skip="$skip_mib" count="$CHUNK_MIB" \
        2> "$backup_dir/p3-${index}.dd.log"
    copied_bytes=$(wc -c < "$path" | tr -d '[:space:]')
    [ "$copied_bytes" = "$CHUNK_BYTES" ] \
        || fail "short backup chunk $index: $copied_bytes bytes"

    destination_sha=$(sha256sum "$path" | awk '{ print $1 }')
    source_sha=$(dd if="$P3_DEVICE" bs=1048576 skip="$skip_mib" count="$CHUNK_MIB" \
        2>/dev/null | sha256sum | awk '{ print $1 }')
    [ "$source_sha" = "$destination_sha" ] \
        || fail "readback SHA-256 mismatch for chunk $index"

    printf 'chunk=%s start_lba=%s sectors=%s bytes=%s sha256=%s file=%s\n' \
        "$index" "$start_lba" 131072 "$CHUNK_BYTES" "$destination_sha" "$file" \
        >> "$manifest_tmp"
    sync
    echo "P3_BACKUP_CHUNK_OK index=$index bytes=$copied_bytes sha256=$destination_sha"
    i=$((i + 1))
done

mv "$manifest_tmp" "$manifest"
manifest_sha=$(sha256sum "$manifest" | awk '{ print $1 }')
{
    echo "backup_id=$backup_id"
    echo "manifest_sha256=$manifest_sha"
    echo "source_bytes=$P3_BYTES"
    echo "chunk_count=$CHUNK_COUNT"
} > "$backup_dir/COMPLETE.tmp"
sync
mv "$backup_dir/COMPLETE.tmp" "$backup_dir/COMPLETE"
sync

echo "P3_BACKUP_COMPLETE id=$backup_id dir=$backup_dir manifest_sha256=$manifest_sha"
