#!/bin/sh
# Prepare the currently booted P3 root for a byte-exact P3-to-P4 backup.
# This runs from /tmp so remounting P3 read-only cannot invalidate the script.

set -eu

BB=/bin/busybox
P4_MARKER='MangoCore 2K1000LA persistent state partition'

"$BB" mkdir -p /persist
if ! "$BB" mount | "$BB" grep -q ' /persist '; then
    "$BB" mount -t ext4 /dev/sda4 /persist
fi
"$BB" grep -q "^${P4_MARKER}$" /persist/MANGO_STATE.txt
"$BB" sync
# PID1 chroots into P3, but the kernel's authoritative P3 mount remains at
# /sdcard. Older rescue images used /tools, while no supported boot layout
# mounts the block filesystem directly over the VFS rootfs.
if ! "$BB" grep -Eq '^/sdcard[[:space:]]+/sdcard[[:space:]]+ext4[[:space:]]+ro([,[:space:]])' /proc/mounts; then
    "$BB" mount -o remount,ro /sdcard
fi
"$BB" grep -Eq '^/sdcard[[:space:]]+/sdcard[[:space:]]+ext4[[:space:]]+ro([,[:space:]])' /proc/mounts
echo P3_BACKUP_SOURCE_READY
