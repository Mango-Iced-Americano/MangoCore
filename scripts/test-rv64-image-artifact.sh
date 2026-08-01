#!/bin/sh
# Verify the built, deployable RV64 Image rather than source spelling.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
image=${1:-"$root/build/rv64/release/normal/kernel/Image"}
elf=${2:-"$root/build/rv64/release/normal/kernel/riscv64gc-unknown-none-elf/release/os"}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

[ -f "$image" ] || fail "missing RV64 Image: $image"
[ -f "$elf" ] || fail "missing RV64 debug ELF: $elf"

header=$(readelf -SW "$elf")
printf '%s\n' "$header" | grep -E -q '\.image_header.*[[:space:]]000040[[:space:]]' ||
    fail 'ELF must contain a 64-byte .image_header section'

header_vaddr=$(printf '%s\n' "$header" | awk '$3 == ".image_header" { print $5; exit }')
[ "$header_vaddr" = "ffffffc000200000" ] ||
    fail "Image header must link at the RV64 high virtual VMA, got ${header_vaddr:-missing}"

programs=$(readelf -lW "$elf")
printf '%s\n' "$programs" | grep -E -q 'INTERP|DYNAMIC' && fail 'Image ELF must not require an interpreter or dynamic loader'

printf '%s\n' "$header" | grep -E -q '[[:space:]]\.got[[:space:]]' &&
    fail 'Image ELF must not retain a static-VMA GOT'

relocations=$(readelf -rW "$elf")
printf '%s\n' "$relocations" | grep -E -q 'There are no relocations in this file' ||
    fail 'relocatable Image ELF must not require runtime relocations'

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT HUP INT TERM
rust-objcopy --dump-section .image_header="$tmp" "$elf"
cmp -n 64 "$tmp" "$image" >/dev/null || fail 'Image does not begin with its ELF Linux header'

dump=$(readelf -x .image_header "$elf")
printf '%s\n' "$dump" | grep -E -qi '52495343[[:space:]]+56000000[[:space:]]+52534305' ||
    fail 'Linux RISC-V header magic values are missing ("RISCV" u64 at 0x30, "RSC" u32 at 0x38)'

size=$(wc -c < "$image")
[ "$size" -gt 64 ] || fail 'Image must have a non-empty payload after its header'
text_offset=$(od -An -j 8 -N 8 -t u8 "$image" | tr -d '[:space:]')
[ "$text_offset" = "2097152" ] ||
    fail "Image text_offset must be 0x00200000, got ${text_offset:-missing}"
image_size=$(od -An -j 16 -N 8 -t u8 "$image" | tr -d '[:space:]')
[ "${image_size:-0}" -ge "$size" ] ||
    fail "Image footprint must cover payload: ${image_size:-missing} < $size"
flags=$(od -An -j 24 -N 8 -t u8 "$image" | tr -d '[:space:]')
[ "$flags" = "0" ] || fail "Image flags must select little-endian RV64, got ${flags:-missing}"
version=$(od -An -j 32 -N 4 -t u4 "$image" | tr -d '[:space:]')
[ "$version" = "2" ] || fail "Image header version must be 0.2, got ${version:-missing}"
printf 'RV64 Linux Image artifact contract: PASS (%s bytes)\n' "$size"
