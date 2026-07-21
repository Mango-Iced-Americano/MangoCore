#!/bin/sh
set -eu

usage() {
    printf '%s\n' 'usage: test-rebaseline-purity-delta.sh --serial-kernel-builds' >&2
    exit 2
}

fail() {
    printf '%s\n' "FAIL: $*" >&2
    exit 1
}

[ "$#" -eq 1 ] && [ "$1" = '--serial-kernel-builds' ] || usage

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
snapshot_before=$(mktemp "${TMPDIR:-/tmp}/rebaseline-purity-before.XXXXXX")
snapshot_after=$(mktemp "${TMPDIR:-/tmp}/rebaseline-purity-after.XXXXXX")
trap 'rm -f "$snapshot_before" "$snapshot_after"' EXIT HUP INT TERM

snapshot_tracked() {
    git -C "$repo_root" ls-files --stage | while IFS=' ' read -r mode object stage path; do
        case "$mode" in
            160000)
                printf 'GITLINK  %s  %s\n' "$object" "$path"
                ;;
            *)
                if [ -f "$repo_root/$path" ]; then
                    sha256sum "$repo_root/$path"
                else
                    printf 'MISSING  %s\n' "$repo_root/$path"
                fi
                ;;
        esac
    done
}

cd "$repo_root"
snapshot_tracked >"$snapshot_before"

make -C os rv64-kernel-build-only
make -C os la64-kernel-build-only

snapshot_tracked >"$snapshot_after"
cmp -s "$snapshot_before" "$snapshot_after" ||
    fail 'serial RV64 then LA64 kernel builds changed a tracked source, vendor, or configuration path'

printf '%s\n' 'PASS: serial RV64 then LA64 kernel builds preserve every tracked source, vendor, configuration, and protected candidate'
