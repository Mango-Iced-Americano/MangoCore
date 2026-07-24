#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/rustup-setup-test.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

fixture_root=$work_dir/fixture
mkdir -p "$fixture_root/scripts" "$work_dir/bin"
cp "$repo_root/rust-toolchain.toml" "$fixture_root/rust-toolchain.toml"
cp "$repo_root/scripts/rustup-setup.sh" "$fixture_root/scripts/rustup-setup.sh"
cp "$repo_root/scripts/rustup-preflight.sh" "$fixture_root/scripts/rustup-preflight.sh"
mv "$fixture_root/scripts/rustup-preflight.sh" \
    "$fixture_root/scripts/rustup-preflight-real.sh"
cat >"$fixture_root/scripts/rustup-preflight.sh" <<'EOF'
#!/bin/sh
set -eu

if [ -n "${FAKE_SETUP_SEQUENCE:-}" ]; then
    printf '%s\n' preflight >>"$FAKE_SETUP_SEQUENCE"
fi
exec sh "$(dirname "$0")/rustup-preflight-real.sh"
EOF
chmod +x "$fixture_root/scripts/rustup-preflight.sh"

sed \
    -e 's/nightly-2026-05-10/nightly-2099-01-01/' \
    -e 's/riscv64gc-unknown-none-elf/fixture-rv64/' \
    -e 's/loongarch64-unknown-linux-gnu/fixture-la64/' \
    -e 's/rust-src/fixture-src/' \
    -e 's/llvm-tools-preview/fixture-llvm/' \
    "$fixture_root/rust-toolchain.toml" >"$fixture_root/rust-toolchain.toml.new"
mv "$fixture_root/rust-toolchain.toml.new" "$fixture_root/rust-toolchain.toml"

cat >"$work_dir/bin/rustup" <<'EOF'
#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$1" != toolchain ] || [ "$2" != install ]; then
    echo "fake rustup: unsupported command" >&2
    exit 64
fi

printf '%s\n' "$@" >"$FAKE_RUSTUP_LOG"
printf '%s\n' "${RUSTUP_HOME-}" >"$FAKE_RUSTUP_RUSTUP_HOME"
printf '%s\n' "${CARGO_HOME-}" >"$FAKE_RUSTUP_CARGO_HOME"
if [ -n "${FAKE_SETUP_SEQUENCE:-}" ]; then
    printf '%s\n' rustup-install >>"$FAKE_SETUP_SEQUENCE"
fi

if [ "${FAKE_RUSTUP_FAIL:-0}" -ne 0 ]; then
    exit "$FAKE_RUSTUP_FAIL"
fi

toolchain_dir="$RUSTUP_HOME/toolchains/nightly-2099-01-01-fixture"
mkdir -p \
    "$toolchain_dir/lib/rustlib/fixture-rv64/lib" \
    "$toolchain_dir/lib/rustlib/fixture-la64/lib"
printf '%s\n' fixture-src fixture-llvm >"$toolchain_dir/lib/rustlib/components"
EOF
chmod +x "$work_dir/bin/rustup"

overall=0

snapshot_inputs() {
    manifest_hash=$(cksum "$fixture_root/rust-toolchain.toml")
    setup_hash=$(cksum "$fixture_root/scripts/rustup-setup.sh")
    preflight_hash=$(cksum "$fixture_root/scripts/rustup-preflight.sh")
}

check_unchanged() {
    if [ "$(cksum "$fixture_root/rust-toolchain.toml")" != "$manifest_hash" ]; then
        echo "FAIL: $1 mutated rust-toolchain.toml" >&2
        overall=1
    fi
    if [ "$(cksum "$fixture_root/scripts/rustup-setup.sh")" != "$setup_hash" ]; then
        echo "FAIL: $1 mutated rustup-setup.sh" >&2
        overall=1
    fi
    if [ "$(cksum "$fixture_root/scripts/rustup-preflight.sh")" != "$preflight_hash" ]; then
        echo "FAIL: $1 mutated rustup-preflight.sh" >&2
        overall=1
    fi
}

run_setup() {
    output_file=$1
    set +e
    (
        cd "$fixture_root"
        PATH="$work_dir/bin:$PATH" \
            FAKE_RUSTUP_LOG="$work_dir/rustup.argv" \
            FAKE_RUSTUP_RUSTUP_HOME="$work_dir/rustup-home.env" \
            FAKE_RUSTUP_CARGO_HOME="$work_dir/cargo-home.env" \
            FAKE_SETUP_SEQUENCE="$work_dir/setup.sequence" \
            sh scripts/rustup-setup.sh
    ) >"$output_file" 2>&1
    status=$?
    set -e
}

seed_complete_layout() {
    rustup_home=$1
    toolchain_dir="$rustup_home/toolchains/nightly-2099-01-01-fixture"
    mkdir -p \
        "$toolchain_dir/lib/rustlib/fixture-rv64/lib" \
        "$toolchain_dir/lib/rustlib/fixture-la64/lib"
    printf '%s\n' fixture-src fixture-llvm >"$toolchain_dir/lib/rustlib/components"
}

echo "SCENARIO missing RUSTUP_HOME"
rm -f "$work_dir/rustup.argv"
snapshot_inputs
set +e
(
    cd "$fixture_root"
    HOME="$work_dir/default-home" PATH="$work_dir/bin:$PATH" \
        FAKE_RUSTUP_LOG="$work_dir/rustup.argv" \
        FAKE_RUSTUP_RUSTUP_HOME="$work_dir/rustup-home.env" \
        FAKE_RUSTUP_CARGO_HOME="$work_dir/cargo-home.env" \
        sh scripts/rustup-setup.sh
) >"$work_dir/missing-home.out" 2>&1
status=$?
set -e
check_unchanged "missing RUSTUP_HOME"
if [ "$status" -ne 0 ] && [ ! -e "$work_dir/rustup.argv" ]; then
    echo "PASS: missing RUSTUP_HOME"
else
    echo "FAIL: missing RUSTUP_HOME (expected failure without fake rustup invocation)"
    cat "$work_dir/missing-home.out" >&2
    overall=1
fi

echo "SCENARIO complete layout preflight fast path"
rm -f "$work_dir/rustup.argv"
rustup_home=$work_dir/complete-rustup
cargo_home=$work_dir/complete-cargo
mkdir -p "$rustup_home" "$cargo_home"
seed_complete_layout "$rustup_home"
: >"$work_dir/setup.sequence"
export RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home"
snapshot_inputs
run_setup "$work_dir/complete.out"
check_unchanged "complete layout preflight fast path"
if [ "$status" -eq 0 ] && [ ! -e "$work_dir/rustup.argv" ] \
    && [ "$(cat "$work_dir/setup.sequence")" = preflight ] \
    && [ ! -s "$work_dir/complete.out" ]; then
    echo "PASS: complete layout preflight fast path"
else
    echo "FAIL: complete layout preflight fast path (expected no rustup invocation)"
    cat "$work_dir/complete.out" >&2
    overall=1
fi

echo "SCENARIO manifest-driven argument forwarding"
rm -f "$work_dir/rustup.argv"
rustup_home=$work_dir/forward-rustup
cargo_home=$work_dir/forward-cargo
mkdir -p "$rustup_home" "$cargo_home"
export RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home"
snapshot_inputs
run_setup "$work_dir/forwarding.out"
check_unchanged "manifest-driven argument forwarding"
expected_args=$work_dir/expected.args
cat >"$expected_args" <<'EOF'
toolchain
install
nightly-2099-01-01
--profile
minimal
--component
fixture-src
--component
fixture-llvm
--target
fixture-rv64
--target
fixture-la64
EOF
if [ "$status" -eq 0 ] && cmp -s "$expected_args" "$work_dir/rustup.argv" \
    && [ "$(cat "$work_dir/rustup-home.env")" = "$rustup_home" ] \
    && [ "$(cat "$work_dir/cargo-home.env")" = "$cargo_home" ]; then
    echo "PASS: manifest-driven argument forwarding"
else
    echo "FAIL: manifest-driven argument forwarding (expected exact manifest invocation and environment)"
    cat "$work_dir/forwarding.out" >&2
    overall=1
fi

echo "SCENARIO rustup failure propagation"
rm -f "$work_dir/rustup.argv"
rustup_home=$work_dir/failure-rustup
mkdir -p "$rustup_home"
snapshot_inputs
set +e
(
    cd "$fixture_root"
    PATH="$work_dir/bin:$PATH" RUSTUP_HOME="$rustup_home" \
        FAKE_RUSTUP_FAIL=37 FAKE_RUSTUP_LOG="$work_dir/rustup.argv" \
        FAKE_RUSTUP_RUSTUP_HOME="$work_dir/rustup-home.env" \
        FAKE_RUSTUP_CARGO_HOME="$work_dir/cargo-home.env" \
        sh scripts/rustup-setup.sh
) >"$work_dir/failure.out" 2>&1
status=$?
set -e
check_unchanged "rustup failure propagation"
if [ "$status" -eq 37 ] && [ ! -d "$rustup_home/toolchains" ] \
    && ! grep -Fq "Rust toolchain preflight passed:" "$work_dir/failure.out"; then
    echo "PASS: rustup failure propagation"
else
    echo "FAIL: rustup failure propagation (expected exit 37 and no preflight layout)"
    cat "$work_dir/failure.out" >&2
    overall=1
fi

echo "SCENARIO setup-to-preflight integration"
rustup_home=$work_dir/integration-rustup
cargo_home=$work_dir/integration-cargo
mkdir -p "$rustup_home" "$cargo_home"
: >"$work_dir/setup.sequence"
export RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home"
snapshot_inputs
run_setup "$work_dir/integration.out"
check_unchanged "setup-to-preflight integration"
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.sequence")" = "preflight
rustup-install
preflight" ] \
    && grep -Fqx "Rust toolchain preflight passed: nightly-2099-01-01" "$work_dir/integration.out"; then
    echo "PASS: incomplete layout installs before preflight"
else
    echo "FAIL: incomplete layout installs before preflight (expected ordered setup-created layout)"
    cat "$work_dir/integration.out" >&2
    overall=1
fi

echo "SCENARIO install then immediate repeat fast path"
snapshot_inputs
run_setup "$work_dir/integration-repeat.out"
check_unchanged "install then immediate repeat fast path"
if [ "$status" -eq 0 ] \
    && [ "$(cat "$work_dir/setup.sequence")" = "preflight
rustup-install
preflight
preflight" ] \
    && cmp -s "$expected_args" "$work_dir/rustup.argv" \
    && [ ! -s "$work_dir/integration-repeat.out" ]; then
    echo "PASS: install then immediate repeat fast path"
else
    echo "FAIL: install then immediate repeat fast path (expected exactly one install, then preflight-only repeat)"
    cat "$work_dir/integration-repeat.out" >&2
    overall=1
fi

exit "$overall"
