#!/bin/sh
set -eu

usage() {
    echo "usage: $0 restore|snapshot [repo-root]" >&2
}

mode="${1:-restore}"
repo_root="${2:-.}"
checksum_root="$repo_root/cargo-checksums"

restore_checksums() {
    [ -d "$checksum_root" ] || exit 0

    find "$checksum_root" -type f -name cargo-checksum.json | while IFS= read -r src; do
        rel="${src#"$checksum_root"/}"
        crate_dir="${rel%/cargo-checksum.json}"
        dst="$repo_root/$crate_dir/.cargo-checksum.json"
        mkdir -p "$(dirname "$dst")"
        cp -f "$src" "$dst"
    done
}

snapshot_checksums() {
    mkdir -p "$checksum_root"

    for vendor_dir in "$repo_root/os/vendor" "$repo_root/user/vendor"; do
        [ -d "$vendor_dir" ] || continue
        find "$vendor_dir" -type f -name .cargo-checksum.json | while IFS= read -r src; do
            rel="${src#"$repo_root"/}"
            crate_dir="${rel%/.cargo-checksum.json}"
            dst="$checksum_root/$crate_dir/cargo-checksum.json"
            mkdir -p "$(dirname "$dst")"
            cp -f "$src" "$dst"
        done
    done
}

case "$mode" in
    restore)
        restore_checksums
        ;;
    snapshot)
        snapshot_checksums
        ;;
    *)
        usage
        exit 2
        ;;
esac
