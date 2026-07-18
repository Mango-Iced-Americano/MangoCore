#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/rustup-preflight-test.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

cp "$repo_root/rust-toolchain.toml" "$work_dir/rust-toolchain.toml"
mkdir "$work_dir/scripts"
cp "$repo_root/scripts/rustup-preflight.sh" "$work_dir/scripts/rustup-preflight.sh"

sed \
    -e 's/nightly-2026-05-10/nightly-2099-01-01/' \
    -e 's/riscv64gc-unknown-none-elf/fixture-rv64/' \
    -e 's/loongarch64-unknown-linux-gnu/fixture-la64/' \
    -e 's/rust-src/fixture-src/' \
    -e 's/llvm-tools-preview/fixture-llvm/' \
    "$work_dir/rust-toolchain.toml" >"$work_dir/rust-toolchain.toml.new"
mv "$work_dir/rust-toolchain.toml.new" "$work_dir/rust-toolchain.toml"

overall=0

snapshot_inputs() {
    scenario_manifest_hash=$(cksum "$work_dir/rust-toolchain.toml")
    scenario_preflight_hash=$(cksum "$work_dir/scripts/rustup-preflight.sh")
}

check_unchanged() {
    if [ "$(cksum "$work_dir/rust-toolchain.toml")" != "$scenario_manifest_hash" ]; then
        echo "FAIL: $1 mutated rust-toolchain.toml" >&2
        overall=1
    fi
    if [ "$(cksum "$work_dir/scripts/rustup-preflight.sh")" != "$scenario_preflight_hash" ]; then
        echo "FAIL: $1 mutated rustup-preflight.sh" >&2
        overall=1
    fi
}

make_fake_rustup_home() {
    fake_home=$1
    toolchain_dir="$fake_home/toolchains/nightly-2099-01-01-fixture"
    mkdir -p \
        "$toolchain_dir/lib/rustlib/fixture-rv64/lib" \
        "$toolchain_dir/lib/rustlib/fixture-la64/lib"
    printf '%s\n' fixture-src fixture-llvm >"$toolchain_dir/lib/rustlib/components"
}

run_preflight() {
    output_file=$1
    set +e
    (
        cd "$work_dir"
        RUSTUP_HOME="$fake_home" sh scripts/rustup-preflight.sh
    ) >"$output_file" 2>&1
    status=$?
    set -e
}

echo "SCENARIO manifest-driven success"
fake_home="$work_dir/success-rustup"
mkdir -p "$fake_home"
make_fake_rustup_home "$fake_home"
snapshot_inputs
run_preflight "$work_dir/success.out"
check_unchanged "manifest-driven success"
if [ "$status" -eq 0 ]; then
    echo "PASS: manifest-driven success"
else
    echo "FAIL: manifest-driven success (expected exit 0, got $status)"
    cat "$work_dir/success.out" >&2
    overall=1
fi

echo "SCENARIO missing toolchain"
fake_home="$work_dir/missing-toolchain-rustup"
mkdir "$fake_home"
snapshot_inputs
run_preflight "$work_dir/missing-toolchain.out"
check_unchanged "missing toolchain"
if [ "$status" -ne 0 ] \
    && grep -Fqx "missing Rust toolchain: nightly-2099-01-01" "$work_dir/missing-toolchain.out" \
    && grep -Fq "this read-only command does not provision" "$work_dir/missing-toolchain.out"; then
    echo "PASS: missing toolchain"
else
    echo "FAIL: missing toolchain (expected exact missing-toolchain and actionable diagnostics)"
    cat "$work_dir/missing-toolchain.out" >&2
    overall=1
fi

echo "SCENARIO missing target"
fake_home="$work_dir/missing-target-rustup"
mkdir -p "$fake_home"
make_fake_rustup_home "$fake_home"
rm -rf "$fake_home/toolchains/nightly-2099-01-01-fixture/lib/rustlib/fixture-la64"
snapshot_inputs
run_preflight "$work_dir/missing-target.out"
check_unchanged "missing target"
if [ "$status" -ne 0 ] && grep -Fqx \
    "missing Rust target for nightly-2099-01-01: fixture-la64" "$work_dir/missing-target.out"; then
    echo "PASS: missing target"
else
    echo "FAIL: missing target (expected exact synthetic diagnostic)"
    cat "$work_dir/missing-target.out" >&2
    overall=1
fi

echo "SCENARIO missing component"
fake_home="$work_dir/missing-component-rustup"
mkdir -p "$fake_home"
make_fake_rustup_home "$fake_home"
printf '%s\n' fixture-src >"$fake_home/toolchains/nightly-2099-01-01-fixture/lib/rustlib/components"
snapshot_inputs
run_preflight "$work_dir/missing-component.out"
check_unchanged "missing component"
if [ "$status" -ne 0 ] && grep -Fqx \
    "missing Rust component for nightly-2099-01-01: fixture-llvm" "$work_dir/missing-component.out"; then
    echo "PASS: missing component"
else
    echo "FAIL: missing component (expected exact synthetic diagnostic)"
    cat "$work_dir/missing-component.out" >&2
    overall=1
fi

exit "$overall"
