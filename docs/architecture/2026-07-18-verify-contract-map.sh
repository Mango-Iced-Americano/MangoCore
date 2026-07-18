#!/usr/bin/env bash
# Static Phase-0 contract verifier. It intentionally runs no build or runtime tool.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OBSERVATION_COMMIT="883f73c2"
PLAN="${PLAN:-$ROOT/.omo/plans/mangocore-repository-rebaseline.md}"
MAP="${CONTRACT_MAP:-$ROOT/docs/architecture/2026-07-18-mangocore-contract-map.md}"
MATRIX="${CONTRACT_MATRIX:-$ROOT/docs/architecture/2026-07-18-mangocore-contract-matrix.yaml}"
SELF_TEST_PARENT="${SELF_TEST_PARENT:-false}"
TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mangocore-phase0.XXXXXX")"
trap 'rm -rf "$TEMP_DIR"' EXIT

PASS=0
FAIL=0

pass() {
    printf 'PASS: %s\n' "$1"
    PASS=$((PASS + 1))
}

fail() {
    printf 'FAIL: %s\n' "$1"
    FAIL=$((FAIL + 1))
}

check() {
    local description="$1"
    shift
    if "$@"; then
        pass "$description"
    else
        fail "$description"
    fi
}

contains() {
    grep -Fq -- "$2" "$1"
}

block_contains() {
    local file="$1"
    local first="$2"
    local after="$3"
    local needle="$4"
    awk -v first="$first" -v after="$after" -v needle="$needle" '
        $0 == first { inside = 1; next }
        inside && $0 == after { exit }
        inside && index($0, needle) { found = 1 }
        END { exit !found }
    ' "$file"
}

snapshot() {
    local path="$1"
    local destination="$2"
    GIT_MASTER=1 git -C "$ROOT" show "$OBSERVATION_COMMIT:$path" > "$destination"
}

validate_staged_paths() {
    local paths_file="$1"
    local path
    local valid=true
    local staged_plan=false
    local staged_map=false
    local staged_matrix=false
    local staged_verifier=false

    while IFS= read -r -d '' path; do
        case "$path" in
            docs/Work_Log|docs/Work_Log/*)
                printf 'forbidden staged path: %s\n' "$path" >&2
                valid=false
                ;;
            .omo/plans/mangocore-repository-rebaseline.md)
                if [ "$staged_plan" = true ]; then
                    printf 'duplicate staged Phase-0 path: %s\n' "$path" >&2
                    valid=false
                fi
                staged_plan=true
                ;;
            docs/architecture/2026-07-18-mangocore-contract-map.md)
                if [ "$staged_map" = true ]; then
                    printf 'duplicate staged Phase-0 path: %s\n' "$path" >&2
                    valid=false
                fi
                staged_map=true
                ;;
            docs/architecture/2026-07-18-mangocore-contract-matrix.yaml)
                if [ "$staged_matrix" = true ]; then
                    printf 'duplicate staged Phase-0 path: %s\n' "$path" >&2
                    valid=false
                fi
                staged_matrix=true
                ;;
            docs/architecture/2026-07-18-verify-contract-map.sh)
                if [ "$staged_verifier" = true ]; then
                    printf 'duplicate staged Phase-0 path: %s\n' "$path" >&2
                    valid=false
                fi
                staged_verifier=true
                ;;
            *)
                printf 'staged path outside Phase-0 allowlist: %s\n' "$path" >&2
                valid=false
                ;;
        esac
    done < "$paths_file"

    if [ "$staged_plan" != true ]; then
        printf 'missing staged Phase-0 path: .omo/plans/mangocore-repository-rebaseline.md\n' >&2
        valid=false
    fi
    if [ "$staged_map" != true ]; then
        printf 'missing staged Phase-0 path: docs/architecture/2026-07-18-mangocore-contract-map.md\n' >&2
        valid=false
    fi
    if [ "$staged_matrix" != true ]; then
        printf 'missing staged Phase-0 path: docs/architecture/2026-07-18-mangocore-contract-matrix.yaml\n' >&2
        valid=false
    fi
    if [ "$staged_verifier" != true ]; then
        printf 'missing staged Phase-0 path: docs/architecture/2026-07-18-verify-contract-map.sh\n' >&2
        valid=false
    fi

    [ "$valid" = true ]
}

snapshot Makefile "$TEMP_DIR/root-makefile"
snapshot os/Makefile "$TEMP_DIR/os-makefile"
snapshot os/make/rv64.mk "$TEMP_DIR/rv64-makefile"
snapshot os/make/la64.mk "$TEMP_DIR/la64-makefile"

check "Phase-0 plan is present" test -f "$PLAN"
check "contract map is present" test -f "$MAP"
check "contract matrix is present" test -f "$MATRIX"
check "baseline root Makefile is available from $OBSERVATION_COMMIT" test -s "$TEMP_DIR/root-makefile"
check "baseline os Makefile is available from $OBSERVATION_COMMIT" test -s "$TEMP_DIR/os-makefile"
check "baseline architecture Makefiles are available from $OBSERVATION_COMMIT" \
    bash -c 'test -s "$1" && test -s "$2"' _ "$TEMP_DIR/rv64-makefile" "$TEMP_DIR/la64-makefile"

check "plan records observation commit $OBSERVATION_COMMIT" contains "$PLAN" "Observation commit: \`$OBSERVATION_COMMIT\`"
check "map identity records observation commit $OBSERVATION_COMMIT" \
    contains "$MAP" "**观察提交：** \`$OBSERVATION_COMMIT\`"
check "matrix records observation commit $OBSERVATION_COMMIT" contains "$MATRIX" "observation_commit: $OBSERVATION_COMMIT"
check "map explicitly rejects stale 60800fa2 as the observation point" \
    contains "$MAP" "不是 \`60800fa2\`"
check "plan and matrix contain no stale 60800fa2 status claim" \
    bash -c '! grep -Fq -- 60800fa2 "$1" "$2"' _ "$PLAN" "$MATRIX"

check "baseline root all unconditionally cleans" \
    bash -c 'grep -A3 "^all:" "$1" | grep -Fq "clean"' _ "$TEMP_DIR/root-makefile"
check "baseline root all delegates to os all" \
    bash -c 'grep -A3 "^all:" "$1" | grep -Fq -- "-C os all"' _ "$TEMP_DIR/root-makefile"
check "baseline uses Rustup default or override mutation" \
    bash -c 'grep -Fq "rustup default" "$1" && grep -Fq "rustup override set" "$2"' _ "$TEMP_DIR/root-makefile" "$TEMP_DIR/os-makefile"
check "baseline architecture recipes mutate Rustup targets and components" \
    bash -c 'grep -Fq "rustup target add" "$1" && grep -Fq "rustup component add" "$1" && grep -Fq "rustup target add" "$2" && grep -Fq "rustup component add" "$2"' _ "$TEMP_DIR/rv64-makefile" "$TEMP_DIR/la64-makefile"
check "baseline copies tracked lang items and Cargo configuration" \
    bash -c 'grep -Fq "lang_items.rs.rv ./src/lang_items.rs" "$1" && grep -Fq "lang_items.rs.la ./src/lang_items.rs" "$1" && grep -Fq "cargo-config/os/config.toml" "$2" && grep -Fq "cargo-config/user/config.toml" "$2"' _ "$TEMP_DIR/os-makefile" "$TEMP_DIR/root-makefile"
check "baseline copies tracked linker inputs and touches initramfs inputs" \
    bash -c 'grep -Fq "cp -f src/hal/arch/riscv/linker-" "$1" && grep -Fq "src/hal/arch/riscv/linker.ld" "$1" && grep -Fq "cp -f src/hal/arch/loongarch64/linker-" "$2" && grep -Fq "src/hal/arch/loongarch64/linker.ld" "$2" && grep -Fq "touch src/initramfs-rv.S" "$1" && grep -Fq "touch src/initramfs-la.S" "$2"' _ "$TEMP_DIR/rv64-makefile" "$TEMP_DIR/la64-makefile"
check "baseline lwext4 recipe mutates its tracked dependency source" \
    bash -c 'grep -Fq "musl-generic.cmake" "$1" && grep -Fq "src/ulibc.c" "$1" && grep -Fq "sed -i" "$1" && grep -Fq "musl-generic.cmake" "$2" && grep -Fq "src/ulibc.c" "$2" && grep -Fq "sed -i" "$2"' _ "$TEMP_DIR/rv64-makefile" "$TEMP_DIR/la64-makefile"

check "matrix labels baseline root behavior as observed" \
    block_contains "$MATRIX" "baseline:" "candidate:" "status: \"observed at $OBSERVATION_COMMIT\""
check "matrix records unconditional clean and Rustup mutation" \
    bash -c 'grep -Fq "unconditional_clean: true" "$1" && grep -Fq "normal_path_mutates_rustup: true" "$1"' _ "$MATRIX"
check "matrix records all baseline source mutation classes" \
    bash -c 'grep -Fq "lang_items:" "$1" && grep -Fq "cargo_configuration:" "$1" && grep -Fq "linker_inputs:" "$1" && grep -Fq "initramfs_inputs:" "$1" && grep -Fq "lwext4_source:" "$1"' _ "$MATRIX"
check "matrix records non-isolated baseline outputs as observed" \
    bash -c 'grep -Fq "output_isolation: false" "$1" && grep -Fq "scattered_outside_build: true" "$1"' _ "$MATRIX"
check "map records clean, Rustup, source mutation, and lwext4 as observed facts" \
    bash -c 'grep -Fq "无条件" "$1" && grep -Fq "Rustup" "$1" && grep -Fq "lang item" "$1" && grep -Fq "Cargo" "$1" && grep -Fq "linker" "$1" && grep -Fq "initramfs" "$1" && grep -Fq "lwext4" "$1"' _ "$MAP"

check "future toolchain requires a dated nightly and explicit provisioning" \
    bash -c 'grep -Fq "dated_nightly: true" "$1" && grep -Fq "explicit_setup_only: true" "$1" && grep -Fq "rustup_auto_install: 0" "$1"' _ "$MATRIX"
check "future build scripts are OUT_DIR-only and linkers are external" \
    bash -c 'grep -Fq "build_scripts_write_only_out_dir: true" "$1" && grep -Fq "external_configuration_required: true" "$1"' _ "$MATRIX"
check "source purity and output isolation remain unverified" \
    bash -c 'awk '\''$0 == "  source_purity:" { in_block = 1; next } in_block && $0 == "  output_isolation:" { exit } in_block && $0 == "    proof_status: \"unverified\"" { found = 1 } END { exit !found }'\'' "$1" && awk '\''$0 == "  output_isolation:" { in_block = 1; next } in_block && $0 == "  linker_configuration:" { exit } in_block && $0 == "    proof_status: \"unverified\"" { found = 1 } END { exit !found }'\'' "$1"' _ "$MATRIX"
check "CI and QEMU remain unverified" \
    bash -c 'grep -Fq "CI equivalence and contract compliance" "$1" && grep -Fq "QEMU boot, PID 1, runner, and shutdown behavior" "$1" && grep -Fq "proof_status: \"unverified\"" "$1"' _ "$MATRIX"
check "map describes future requirements without completion claims" \
    bash -c 'grep -Fq "required future contract" "$1" && grep -Fq "source purity" "$1" && grep -Fq "unverified" "$1" && grep -Fq "本文件不声明以下任何事项已经完成" "$1"' _ "$MAP"
check "matrix explicitly denies Phase-0 and rebaseline completion" \
    bash -c 'grep -Fq "phase_0_complete: false" "$1" && grep -Fq "rebaseline_complete: false" "$1" && grep -Fq "status: \"unverified\"" "$1"' _ "$MATRIX"

check "plan defines a documentation-only Phase-0 task" \
    bash -c 'grep -Fq "No build, QEMU run, lint, CI run, or repository purity check" "$1" && grep -Fq "no-evidence-commit policy" "$1"' _ "$PLAN"
check "matrix publishes the exact four-file Phase-0 allowlist" \
    bash -c 'for path in ".omo/plans/mangocore-repository-rebaseline.md" "docs/architecture/2026-07-18-mangocore-contract-map.md" "docs/architecture/2026-07-18-mangocore-contract-matrix.yaml" "docs/architecture/2026-07-18-verify-contract-map.sh"; do grep -Fxq "    - $path" "$1" || exit 1; done' _ "$MATRIX"
check "matrix forbids Work_Log and evidence in the Phase-0 commit" \
    bash -c 'grep -Fxq "    - docs/Work_Log/" "$1" && grep -Fxq "    - docs/Work_Log/evidence/" "$1"' _ "$MATRIX"

GIT_MASTER=1 git -C "$ROOT" diff --cached --name-only -z > "$TEMP_DIR/staged-paths"
check "staged paths are exactly the four Phase-0 files and exclude Work_Log" \
    validate_staged_paths "$TEMP_DIR/staged-paths"

if [ "$SELF_TEST_PARENT" != true ]; then
    awk -v hash="$OBSERVATION_COMMIT" '
        !changed && index($0, hash) {
            sub(hash, "00000000")
            changed = 1
        }
        { print }
    ' "$MAP" > "$TEMP_DIR/negative-observation-map.md"
    set +e
    SELF_TEST_PARENT=true CONTRACT_MAP="$TEMP_DIR/negative-observation-map.md" bash "$0" > "$TEMP_DIR/negative-observation.log" 2>&1
    negative_observation_status=$?
    set -e
    check "negative probe rejects a corrupt observation hash" test "$negative_observation_status" -ne 0

    awk '
        $0 == "  source_purity:" { in_source_purity = 1 }
        in_source_purity && $0 == "  output_isolation:" { in_source_purity = 0 }
        in_source_purity && $0 == "    proof_status: \"unverified\"" {
            sub("unverified", "complete")
        }
        { print }
    ' "$MATRIX" > "$TEMP_DIR/negative-purity-matrix.yaml"
    set +e
    SELF_TEST_PARENT=true CONTRACT_MATRIX="$TEMP_DIR/negative-purity-matrix.yaml" bash "$0" > "$TEMP_DIR/negative-purity.log" 2>&1
    negative_purity_status=$?
    set -e
    check "negative probe rejects source purity marked complete" test "$negative_purity_status" -ne 0

    printf 'docs/Work_Log/negative-probe.md\0' > "$TEMP_DIR/negative-staged-work-log"
    set +e
    validate_staged_paths "$TEMP_DIR/negative-staged-work-log" > "$TEMP_DIR/negative-staged.log" 2>&1
    negative_staged_status=$?
    set -e
    check "negative probe rejects a simulated staged Work_Log path" test "$negative_staged_status" -ne 0

    printf '%s\0' \
        docs/architecture/2026-07-18-mangocore-contract-map.md \
        docs/architecture/2026-07-18-mangocore-contract-matrix.yaml \
        docs/architecture/2026-07-18-verify-contract-map.sh \
        > "$TEMP_DIR/negative-missing-plan"
    set +e
    validate_staged_paths "$TEMP_DIR/negative-missing-plan" > "$TEMP_DIR/negative-missing-plan.log" 2>&1
    negative_missing_plan_status=$?
    set -e
    check "negative probe rejects a missing staged Phase-0 plan" test "$negative_missing_plan_status" -ne 0
fi

printf 'Passed: %s\nFailed: %s\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
