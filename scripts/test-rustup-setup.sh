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
            sh scripts/rustup-setup.sh
    ) >"$output_file" 2>&1
    status=$?
    set -e
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
export RUSTUP_HOME="$rustup_home" CARGO_HOME="$cargo_home"
snapshot_inputs
run_setup "$work_dir/integration.out"
check_unchanged "setup-to-preflight integration"
if [ "$status" -eq 0 ] && grep -Fqx \
    "Rust toolchain preflight passed: nightly-2099-01-01" "$work_dir/integration.out"; then
    echo "PASS: setup-to-preflight integration"
else
    echo "FAIL: setup-to-preflight integration (expected setup-created layout to pass preflight)"
    cat "$work_dir/integration.out" >&2
    overall=1
fi

exit "$overall"
