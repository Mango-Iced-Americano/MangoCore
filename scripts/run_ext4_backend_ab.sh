#!/usr/bin/env bash
# Serial, Docker-only ext4 backend A/B runner with private image copies.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)
ARCH=${EXT4_AB_ARCH:-${ARCH:-rv64}}
BACKEND=${EXT4_AB_BACKEND:-${EXT4_BACKEND:-lwext4}}
EXTRA_FEATURES=${EXT4_AB_EXTRA_FEATURES-${EXTRA_FEATURES-perf_diag}}
CONF_FILE=${EXT4_AB_CONF_FILE:-"${REPO_ROOT}/os_test.conf"}
QEMU_TIMEOUT=${EXT4_AB_QEMU_TIMEOUT:-900}
RUN_ID=${EXT4_AB_RUN_ID:-"$(date +%Y%m%d-%H%M%S)-$$"}
PAIR_ID=${EXT4_AB_PAIR_ID:-"$(date +%Y%m%d-%H%M%S)-$$"}
SAMPLE_PHASE=${EXT4_AB_SAMPLE_PHASE:-single}
SAMPLE_INDEX=${EXT4_AB_SAMPLE_INDEX:-0}
BACKENDS=${EXT4_AB_BACKENDS:-lwext4,another}
IOZONE_LIBC=${EXT4_AB_IOZONE_LIBC:-glibc}
DATE=$(date +%Y-%m-%d)
EVIDENCE_ROOT=${EXT4_AB_EVIDENCE_ROOT:-"${REPO_ROOT}/docs/Work_Log/evidence/${DATE}/ext4-backend-ab-${ARCH}-${BACKEND}-${RUN_ID}"}
PRIVATE_ROOT=${EXT4_AB_PRIVATE_ROOT:-"/tmp/ext4-backend-ab-${ARCH}-${BACKEND}-${RUN_ID}"}
RUN_TOKEN=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
DRY_RUN=0
SELF_TEST=0
PAIRED=1

usage() {
    cat <<'EOF'
Run a fail-closed paired ext4 backend benchmark with private disks.

Usage:
   EXT4_AB_ARCH=rv64 bash scripts/run_ext4_backend_ab.sh --paired
   EXT4_AB_ARCH=rv64 EXT4_AB_BACKEND=lwext4 bash scripts/run_ext4_backend_ab.sh --single --dry-run
   bash scripts/run_ext4_backend_ab.sh --self-test
  EXT4_AB_ARCH=la64 EXT4_AB_BACKEND=legacy bash scripts/run_ext4_backend_ab.sh

Environment:
  EXT4_AB_ARCH             rv64 or la64 (default: rv64)
  EXT4_AB_BACKEND          lwext4, legacy, or another (default: lwext4)
  EXT4_AB_EXTRA_FEATURES   Extra kernel Cargo features (for example: perf_diag)
  EXT4_AB_CONF_FILE        Read-only source os_test.conf
   EXT4_AB_QEMU_TIMEOUT     QEMU timeout in seconds, at least 900 (default: 900)
   EXT4_AB_PAIR_ID          Shared paired-benchmark identity (default: timestamp-pid)
   EXT4_AB_BACKENDS         Exactly two comma-separated backends (default: lwext4,another)
   EXT4_AB_IOZONE_LIBC      libc key for iozone records: glibc or musl (default: glibc)
  EXT4_AB_EVIDENCE_ROOT    Persistent evidence directory under docs/Work_Log/evidence/
  EXT4_AB_PRIVATE_ROOT     Per-run private directory inside os-dev (default: /tmp/...)
  EXT4_AB_RUN_ID           Unique suffix for private images and evidence (default: timestamp-pid)

    --paired runs one excluded warmup and five required formal samples per backend,
    alternating backend order for every sample index. It writes a pair manifest,
    raw samples, and statistics only after every formal sample succeeds.

    --single runs exactly one backend/sample; it is used internally by --paired.
    --dry-run validates mode/mask in the source config and uses Docker Make -n to
print the build, inject, and fully-expanded QEMU commands. It never copies or
opens an image with QEMU.

--self-test performs deterministic local checks of the QEMU completion gates.
It does not contact Docker, build, copy images, or start QEMU.
EOF
}

while (($#)); do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --self-test) SELF_TEST=1 ;;
        --paired) PAIRED=1 ;;
        --single) PAIRED=0 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

if [[ "${DRY_RUN}" -eq 1 && "${SELF_TEST}" -eq 1 ]]; then
    printf '%s\n' '--dry-run and --self-test cannot be combined' >&2
    exit 2
fi
if [[ "${PAIRED}" -eq 1 && "${DRY_RUN}" -eq 1 ]]; then
    printf '%s\n' '--paired --dry-run is forbidden: paired evidence requires executed samples' >&2
    exit 2
fi

case "${ARCH}" in
    rv64)
        BUILD_TARGET=rv64-only
        RUN_TARGET=rv64-run
        ARCH_MAKEFILE=make/rv64.mk
        CONF_BLK_MODE=virt
        CARD_NAME=sdcard-rv.img
        DISK_NAME=disk.img
        CARD_OVERRIDE=SDCARD_RV
        DISK_OVERRIDE=DISK_RV
        ;;
    la64)
        BUILD_TARGET=la64-only
        RUN_TARGET=la64-run
        ARCH_MAKEFILE=make/la64.mk
        CONF_BLK_MODE=virt_pci
        CARD_NAME=sdcard-la.img
        DISK_NAME=disk-la.img
        CARD_OVERRIDE=SDCARD_LA
        DISK_OVERRIDE=DISK_LA
        ;;
    *) printf 'unsupported EXT4_AB_ARCH=%s; expected rv64 or la64\n' "${ARCH}" >&2; exit 2 ;;
esac

case "${BACKEND}" in
    lwext4|legacy|another) ;;
    *) printf 'unsupported EXT4_AB_BACKEND=%s; expected lwext4, legacy, or another\n' "${BACKEND}" >&2; exit 2 ;;
esac

[[ "${RUN_ID}" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,79}$ ]] || {
    printf '%s\n' 'EXT4_AB_RUN_ID must use only ASCII letters, digits, underscore, or hyphen' >&2
    exit 2
}
[[ "${PAIR_ID}" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,79}$ ]] || {
    printf '%s\n' 'EXT4_AB_PAIR_ID must use only ASCII letters, digits, underscore, or hyphen' >&2
    exit 2
}
[[ "${SAMPLE_PHASE}" =~ ^(single|warmup|formal)$ ]] || {
    printf '%s\n' 'EXT4_AB_SAMPLE_PHASE must be single, warmup, or formal' >&2
    exit 2
}
[[ "${SAMPLE_INDEX}" =~ ^[0-9]+$ ]] || {
    printf '%s\n' 'EXT4_AB_SAMPLE_INDEX must be a non-negative decimal integer' >&2
    exit 2
}
[[ "${RUN_TOKEN}" =~ ^[0-9a-f]{32}$ ]] || {
    printf '%s\n' 'failed to generate runner token' >&2
    exit 1
}
if ! [[ "${QEMU_TIMEOUT}" =~ ^[1-9][0-9]*$ ]] || [[ "${QEMU_TIMEOUT}" -lt 900 ]]; then
    printf '%s\n' 'EXT4_AB_QEMU_TIMEOUT must be a decimal integer of at least 900 seconds' >&2
    exit 2
fi
[[ "${EXTRA_FEATURES}" =~ ^[A-Za-z0-9_,.-]*$ ]] || {
    printf '%s\n' 'EXT4_AB_EXTRA_FEATURES may contain only feature identifiers separated by commas' >&2
    exit 2
}
[[ "${IOZONE_LIBC}" =~ ^(glibc|musl)$ ]] || {
    printf '%s\n' 'EXT4_AB_IOZONE_LIBC must be glibc or musl' >&2
    exit 2
}

case "${EVIDENCE_ROOT}" in
    "${REPO_ROOT}"/docs/Work_Log/evidence/*) ;;
    *) printf 'EXT4_AB_EVIDENCE_ROOT must be under docs/Work_Log/evidence/\n' >&2; exit 2 ;;
esac
case "${PRIVATE_ROOT}" in
    /tmp/*) ;;
    *) printf 'EXT4_AB_PRIVATE_ROOT must be under /tmp inside os-dev\n' >&2; exit 2 ;;
esac
EXPECTED_EVIDENCE_ROOT="${REPO_ROOT}/docs/Work_Log/evidence/${DATE}/ext4-backend-ab-${ARCH}-${BACKEND}-${RUN_ID}"
if [[ "${SAMPLE_PHASE}" != single ]]; then
    EXPECTED_EVIDENCE_ROOT="${REPO_ROOT}/docs/Work_Log/evidence/${DATE}/ext4-backend-ab-pair-${ARCH}-${PAIR_ID}/samples/${SAMPLE_PHASE}-${SAMPLE_INDEX}-${BACKEND}"
fi
[[ "${EVIDENCE_ROOT}" == "${EXPECTED_EVIDENCE_ROOT}" ]] || {
    printf '%s\n' 'EXT4_AB_EVIDENCE_ROOT must be the default per-run evidence path' >&2
    exit 2
}
[[ "${PRIVATE_ROOT}" == "/tmp/ext4-backend-ab-${ARCH}-${BACKEND}-${RUN_ID}" ]] || {
    printf '%s\n' 'EXT4_AB_PRIVATE_ROOT must be the default per-run private path' >&2
    exit 2
}

SOURCE_CARD="${REPO_ROOT}/${CARD_NAME}"
SOURCE_DISK="${REPO_ROOT}/${DISK_NAME}"
PRIVATE_CARD="${PRIVATE_ROOT}/${CARD_NAME}"
PRIVATE_DISK="${PRIVATE_ROOT}/${DISK_NAME}"
EVIDENCE_REL=${EVIDENCE_ROOT#"${REPO_ROOT}"/}
CONTAINER_EVIDENCE_ROOT="/app/${EVIDENCE_REL}"
CONTAINER_CONFIG="${CONTAINER_EVIDENCE_ROOT}/config.txt"
CONTAINER_QEMU_OWNER="${CONTAINER_EVIDENCE_ROOT}/qemu-owner.txt"
CONTAINER_QEMU_EXIT="${CONTAINER_EVIDENCE_ROOT}/qemu-exit-status.txt"
CONTAINER_QEMU_COMPLETION="${CONTAINER_EVIDENCE_ROOT}/qemu-complete.txt"
CONTAINER_PRIVATE_OWNER="${PRIVATE_ROOT}/.ext4-ab-owner-token"
OVERRIDES="${CARD_OVERRIDE}=${PRIVATE_CARD} ${DISK_OVERRIDE}=${PRIVATE_DISK}"

require_file() {
    [[ -f "$1" ]] || { printf 'missing required file: %s\n' "$1" >&2; exit 1; }
}

config_value() {
    local key=$1
    awk -F= -v wanted="${key}" '
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        {
            name=$1
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
            if (name == wanted) {
                value=substr($0, index($0, "=") + 1)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
                print value
                exit
            }
        }
    ' "${CONF_FILE}"
}

config_has_key() {
    local key=$1
    awk -F= -v wanted="${key}" '
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        {
            name=$1
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
            if (name == wanted) {
                found=1
                exit
            }
        }
        END { exit found ? 0 : 1 }
    ' "${CONF_FILE}"
}

validate_config() {
    local mode mask mask_value existing_run_id existing_backend
    mode=$(config_value mode)
    mask=$(config_value mask)
    [[ -n "${mode}" ]] || { printf 'config missing mode=: %s\n' "${CONF_FILE}" >&2; exit 1; }
    [[ -n "${mask}" ]] || { printf 'config missing mask=: %s\n' "${CONF_FILE}" >&2; exit 1; }
    [[ "${mask}" =~ ^(0[xX][0-9a-fA-F]+|[0-9]+)$ ]] || {
        printf 'config mask is not decimal or hexadecimal: %s\n' "${mask}" >&2
        exit 1
    }
    [[ "${mode}" == "run" ]] || {
        printf 'A/B config mode must be run: %s\n' "${mode}" >&2
        exit 1
    }
    if [[ "${mask}" =~ ^0[xX] ]]; then
        mask_value=$((16#${mask:2}))
    else
        mask_value=$((10#${mask}))
    fi
    [[ "${mask_value}" -eq 0x010 ]] || {
        printf 'A/B config mask must select only iozone (0x010): %s\n' "${mask}" >&2
        exit 1
    }
    existing_run_id=$(config_value ext4_ab_run_id)
    existing_backend=$(config_value ext4_ab_backend)
    [[ -z "${existing_run_id}" && -z "${existing_backend}" ]] || {
        printf '%s\n' 'A/B source config must not predefine ext4_ab identity fields' >&2
        exit 1
    }
    if config_has_key ext4_ab_iozone_libc; then
        printf '%s\n' 'A/B source config must not predefine ext4_ab_iozone_libc' >&2
        exit 1
    fi
    printf 'config mode=%s mask=%s file=%s\n' "${mode}" "${mask}" "${CONF_FILE}"
}

write_guest_config() {
    cp "${CONF_FILE}" "${EVIDENCE_ROOT}/config.txt"
    printf '\next4_ab_run_id=%s\next4_ab_backend=%s\next4_ab_diag=1\next4_ab_iozone_libc=%s\next4_ab_pair_id=%s\next4_ab_sample_phase=%s\next4_ab_sample_index=%s\n' \
        "${RUN_ID}" "${BACKEND}" "${IOZONE_LIBC}" "${PAIR_ID}" "${SAMPLE_PHASE}" "${SAMPLE_INDEX}" \
        >>"${EVIDENCE_ROOT}/config.txt"
}

capture_iozone_metrics() {
    local raw_csv metrics selected_span
    raw_csv="${EVIDENCE_ROOT}/iozone-raw-samples.log"
    metrics="${EVIDENCE_ROOT}/iozone-metrics.csv"
    selected_span="${EVIDENCE_ROOT}/iozone-${IOZONE_LIBC}-selected.log"
    extract_selected_iozone_group "${EVIDENCE_ROOT}/qemu-output.log" "${selected_span}" || {
        printf '%s\n' 'integrity_failure=missing_multiple_or_malformed_selected_iozone_group' >&2
        INTEGRITY_FAILURES=1
        return
    }
    awk '/iozone through(|t)put .* measurements|Children see throughput|Max throughput per process/ { print }' "${selected_span}" >"${raw_csv}"
    if [[ ! -s "${raw_csv}" ]]; then
        printf '%s\n' 'integrity_failure=missing_or_unparseable_iozone_metrics' >&2
        INTEGRITY_FAILURES=1
        return
    fi
    awk -v libc="${IOZONE_LIBC}" '
        function trim(value) { gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); return value }
        function normalize(value) { gsub(/\033\[[0-9;]*m/, "", value); value = trim(value); gsub(/[[:space:]]+/, " ", value); return value }
        function section_for(value) {
            value = trim(value)
            if (value == "iozone throughput write/read measurements") return "iozone write/read"
            if (value == "iozone throughput random-read measurements") return "iozone random-read"
            if (value == "iozone throughput read-backwards measurements") return "iozone read-backwards"
            if (value == "iozone throughput stride-read measurements") return "iozone stride-read"
            if (value == "iozone throughput fwrite/fread measurements") return "iozone fwrite/fread"
            if (value == "iozone throughput pwrite/pread measurements") return "iozone pwrite/pread"
            if (value == "iozone throughtput pwritev/preadv measurements") return "iozone pwritev/preadv"
            return ""
        }
        {
            normalized = normalize($0)
            candidate_section = section_for(normalized)
            if (candidate_section != "") { section = candidate_section; next }
            if (normalized ~ /^(iozone through|iozone throughtput)/) exit 1
            if (normalized ~ /^Children see throughput/) {
                if (normalized !~ /^Children see throughput for [0-9]+ [[:alpha:]-]+( [[:alpha:]-]+)* = [0-9]+(\.[0-9]+)? [kK]B\/sec$/) exit 1
                if (section == "") exit 1
                operation = normalized
                sub(/^Children see throughput for /, "", operation)
                sub(/ = [0-9]+(\.[0-9]+)? [kK]B\/sec$/, "", operation)
                pending_operation = operation
                next
            }
            if (normalized ~ /^Max throughput per process/) {
                if (normalized !~ /^Max throughput per process = [0-9]+(\.[0-9]+)? [kK]B\/sec$/) exit 1
                if (section == "" || pending_operation == "") exit 1
                value = normalized
                sub(/^Max throughput per process = /, "", value)
                sub(/ [kK]B\/sec$/, "", value)
                key = libc SUBSEP section SUBSEP pending_operation
                if (seen[key]++) exit 1
                raw = $0
                gsub(/"/, "\"\"", raw)
                printf "%s,%s,%s,%s,\"%s\"\n", libc, section, pending_operation, value, raw
                pending_operation = ""
            }
        }
    ' "${raw_csv}" >"${metrics}" || {
        printf '%s\n' 'integrity_failure=missing_or_unparseable_iozone_metrics' >&2
        INTEGRITY_FAILURES=1
    }
    [[ "$(wc -l <"${metrics}")" -eq 20 ]] || INTEGRITY_FAILURES=1
}

extract_selected_iozone_group() {
    local guest_log=$1 selected_span=$2
    awk -v expected="iozone-${IOZONE_LIBC}" '
        function normalize(value) { gsub(/\033\[[0-9;]*m/, "", value); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); return value }
        function fail() { failed = 1 }
        {
            if (failed) next
            normalized = normalize($0)
            if (normalized ~ /^#### OS COMP TEST GROUP (START|END) iozone-[a-z]+ ####$/) {
                split(normalized, parts, " ")
                marker_kind = parts[6]
                marker_group = parts[7]
                if (marker_kind == "START") {
                    if (active_group != "") { fail(); next }
                    active_group = marker_group
                    if (marker_group == expected) {
                        if (starts++) { fail(); next }
                        active = 1
                    }
                    next
                }
                if (active_group == "" || marker_group != active_group) { fail(); next }
                if (marker_group == expected) {
                    if (!active || ends++) { fail(); next }
                    active = 0
                }
                active_group = ""
                next
            }
            if (normalized ~ "^#### OS COMP TEST GROUP .* " expected "([[:space:]]|$)" || normalized ~ "^" expected "([[:space:]]|$)") { fail(); next }
            if (active) print
        }
        END { exit (!failed && starts == 1 && ends == 1 && !active && active_group == "") ? 0 : 1 }
    ' "${guest_log}" >"${selected_span}"
}

print_docker_make_dry_run() {
    local qemu_preview
    printf '+ docker compose exec -T os-dev sh -lc %q\n' \
        "cd /app && make -C /app/os ${BUILD_TARGET} EXT4_BACKEND='${BACKEND}' EXTRA_FEATURES='${EXTRA_FEATURES}' LOG=warn"
    printf '+ docker compose exec -T os-dev sh -lc %q\n' \
        "cd /app && make -C /app/os conf-inject CONF_ARCH='${ARCH}' CONF_BLK_MODE='${CONF_BLK_MODE}' CONF_FILE='${CONTAINER_CONFIG}' CONF_IMAGE='${PRIVATE_CARD}' AUTO_REBUILD_MEM=0 MODE=release LOG=warn"
    printf '+ timeout --foreground %ss docker compose exec -T os-dev sh -lc %q\n' "${QEMU_TIMEOUT}" \
        "cd /app && make -C /app/os ${RUN_TARGET} EXT4_BACKEND='${BACKEND}' EXTRA_FEATURES='${EXTRA_FEATURES}' ${OVERRIDES} LOG=warn"
    qemu_preview="cd /app/os && make -f ${ARCH_MAKEFILE} -n comp EXT4_BACKEND='${BACKEND}' EXTRA_FEATURES='${EXTRA_FEATURES}' ${OVERRIDES} LOG=warn"
    printf '%s\n' '+ expanded QEMU command:'
    docker compose exec -T os-dev sh -lc "${qemu_preview}"
}

write_result_status() {
    local outcome=$1 exit_status=$2 temp
    if [[ -e "${EVIDENCE_ROOT}/result-status.txt" ]] && \
        ! grep -qx "run_token=${RUN_TOKEN}" "${EVIDENCE_ROOT}/result-status.txt"; then
        printf '%s\n' 'result status belongs to another run token' >&2
        INTEGRITY_FAILURES=1
        return 1
    fi
    temp=$(mktemp "${EVIDENCE_ROOT}/.result-${RUN_TOKEN}.XXXXXX")
    printf 'run_id=%s\nrun_token=%s\noutcome=%s\nupdated_utc=%s\nrunner_exit_status=%s\ncommand_failures=%s\nintegrity_failures=%s\n' \
        "${RUN_ID}" "${RUN_TOKEN}" "${outcome}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        "${exit_status}" "${COMMAND_FAILURES}" "${INTEGRITY_FAILURES}" >"${temp}"
    mv -f "${temp}" "${EVIDENCE_ROOT}/result-status.txt"
}

create_exclusive_run_dirs() {
    local evidence_parent
    evidence_parent=$(dirname -- "${EVIDENCE_ROOT}")
    mkdir -p -- "${evidence_parent}"
    if ! mkdir -- "${EVIDENCE_ROOT}"; then
        printf 'refusing to reuse evidence directory: %s\n' "${EVIDENCE_ROOT}" >&2
        exit 1
    fi
    STATUS_DIR="${EVIDENCE_ROOT}/command-status"
    LOG_DIR="${EVIDENCE_ROOT}/logs"
    mkdir -- "${STATUS_DIR}" "${LOG_DIR}"
    write_result_status running 1
}

record_command() {
    local name=$1 log=$2
    shift 2
    {
        printf 'command='
        printf '%q ' "$@"
        printf '\n'
    } >"${STATUS_DIR}/${name}.command.txt"
    set +e
    "$@" >"${log}" 2>&1
    local rc=$?
    set -e
    printf 'exit_status=%s\n' "${rc}" >"${STATUS_DIR}/${name}.status.txt"
    {
        printf '%s\n' "[${name}]"
        cat "${STATUS_DIR}/${name}.command.txt"
        cat "${STATUS_DIR}/${name}.status.txt"
    } >>"${EVIDENCE_ROOT}/command-and-status.txt"
    [[ "${rc}" -eq 0 ]] || COMMAND_FAILURES=1
}

record_docker_shell() {
    local name=$1 log=$2 shell_command=$3
    record_command "${name}" "${log}" docker compose exec -T os-dev sh -lc "${shell_command}"
}

make_excerpt() {
    if [[ -s "${EVIDENCE_ROOT}/qemu-output.log" ]]; then
        {
            sed -n '1,40p' "${EVIDENCE_ROOT}/qemu-output.log"
            printf '%s\n' '--- qemu log tail ---'
            tail -n 40 "${EVIDENCE_ROOT}/qemu-output.log"
        } >"${EVIDENCE_ROOT}/qemu-head-tail.txt"
    else
        printf '%s\n' 'status=missing reason=empty_qemu_log' >"${EVIDENCE_ROOT}/qemu-head-tail.txt"
        INTEGRITY_FAILURES=1
    fi
}

record_source_identity() {
    record_command root-git-describe "${LOG_DIR}/root-git-describe.log" \
        git -C "${REPO_ROOT}" describe --always --dirty
    cp "${LOG_DIR}/root-git-describe.log" "${EVIDENCE_ROOT}/git-hash.txt"
    record_command root-git-status "${LOG_DIR}/root-git-status.log" \
        git -C "${REPO_ROOT}" status --short --branch
    cp "${LOG_DIR}/root-git-status.log" "${EVIDENCE_ROOT}/root-git-status.txt"
    # shellcheck disable=SC2016 # The child shell must expand $1 after bash -c receives the repository path.
    record_command root-git-diff-sha256 "${LOG_DIR}/root-git-diff-sha256.log" \
        bash -o pipefail -c 'git -C "$1" diff --binary HEAD | sha256sum' bash "${REPO_ROOT}"
    cp "${LOG_DIR}/root-git-diff-sha256.log" "${EVIDENCE_ROOT}/root-git-diff.sha256"
    record_command submodule-status "${LOG_DIR}/submodule-status.log" \
        git -C "${REPO_ROOT}" submodule status --recursive
    cp "${LOG_DIR}/submodule-status.log" "${EVIDENCE_ROOT}/submodule-status.txt"
    # shellcheck disable=SC2016 # git submodule foreach evaluates $sm_path in each child repository.
    record_command submodule-source-state "${LOG_DIR}/submodule-source-state.log" \
        git -C "${REPO_ROOT}" submodule foreach --quiet --recursive \
        'printf "[submodule]\\npath=%s\\nhead=" "$sm_path"; git rev-parse HEAD; printf "status_begin\\n"; git status --short; printf "status_end\\ndiff_sha256="; git diff --binary HEAD | sha256sum | cut -d " " -f 1; printf "\\n"'
    cp "${LOG_DIR}/submodule-source-state.log" "${EVIDENCE_ROOT}/submodule-source-state.txt"
    # shellcheck disable=SC2016 # The nested bash and submodule foreach shells expand these positional variables.
    record_command submodule-untracked-source-provenance "${LOG_DIR}/submodule-untracked-source-provenance.log" \
        bash -o pipefail -c '
            repo=$1
            git -C "$repo" submodule foreach --quiet '\''printf "%s\\0" "$sm_path"'\'' |
            while IFS= read -r -d "" submodule; do
                printf "[submodule]\\npath=%q\\nuntracked_begin\\n" "$submodule"
                git -C "$repo/$submodule" ls-files --others --exclude-standard -z |
                while IFS= read -r -d "" path; do
                    printf "path=%q\\n" "$path"
                    if [ -f "$repo/$submodule/$path" ]; then
                        printf "content_sha256="
                        sha256sum -- "$repo/$submodule/$path" | cut -d " " -f 1
                    else
                        printf "content_sha256=non_regular_file\\n"
                    fi
                done
                printf "untracked_end\\n"
            done
        ' bash "${REPO_ROOT}"
    cp "${LOG_DIR}/submodule-untracked-source-provenance.log" "${EVIDENCE_ROOT}/submodule-untracked-source-provenance.txt"
    # shellcheck disable=SC2016 # The child shell receives repo and exclude paths as positional parameters.
    record_command untracked-source-provenance "${LOG_DIR}/untracked-source-provenance.log" \
        bash -o pipefail -c '
            repo=$1 exclude=$2
            git -C "$repo" ls-files --others --exclude-standard -z |
            while IFS= read -r -d "" path; do
                case "$path" in "$exclude"/*) continue ;; esac
                printf "path=%q\n" "$path"
                if [ -f "$repo/$path" ]; then
                    printf "content_sha256="
                    sha256sum -- "$repo/$path" | cut -d " " -f 1
                else
                    printf "%s\n" "content_sha256=non_regular_file"
                fi
            done
        ' bash "${REPO_ROOT}" "${EVIDENCE_REL}"
    cp "${LOG_DIR}/untracked-source-provenance.log" "${EVIDENCE_ROOT}/untracked-source-provenance.txt"
}

require_evidence_file() {
    local path=$1 label=$2
    if [[ ! -s "${path}" ]]; then
        printf 'integrity_failure=missing_%s path=%s\n' "${label}" "${path}" >&2
        INTEGRITY_FAILURES=1
    fi
}

require_zero_exit_status() {
    local path=$1 label=$2
    require_evidence_file "${path}" "${label}"
    if [[ -s "${path}" ]] && ! grep -qx 'exit_status=0' "${path}"; then
        printf 'integrity_failure=nonzero_%s path=%s\n' "${label}" "${path}" >&2
        INTEGRITY_FAILURES=1
    fi
}

boot_backend_marker() {
    case "${BACKEND}" in
        lwext4) printf '%s\n' '[ext4] backend: lwext4' ;;
        legacy) printf '%s\n' '[ext4] backend: legacy' ;;
        another) printf '%s\n' '[ext4] backend: another_ext4' ;;
        *) return 1 ;;
    esac
}

verify_selected_iozone_libc_markers() {
    if ! awk -v expected="iozone-${IOZONE_LIBC}" '
        function normalize(value) { gsub(/\033\[[0-9;]*m/, "", value); sub(/\015$/, "", value); gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); return value }
        {
            normalized = normalize($0)
            if (normalized ~ /^#### OS COMP TEST GROUP (START|END) iozone-[a-z]+ ####$/) {
                split(normalized, parts, " ")
                if (parts[7] != expected) exit 1
            }
        }
    ' "${EVIDENCE_ROOT}/qemu-output.log"; then
        printf 'integrity_failure=unselected_iozone_libc_marker selected=%s\n' "${IOZONE_LIBC}" >&2
        INTEGRITY_FAILURES=1
    fi
}

verify_qemu_completion() {
    local boot_marker
    require_zero_exit_status "${STATUS_DIR}/qemu.status.txt" qemu_wrapper_status
    require_zero_exit_status "${EVIDENCE_ROOT}/qemu-exit-status.txt" qemu_status
    require_evidence_file "${EVIDENCE_ROOT}/qemu-complete.txt" qemu_completion_marker
    if [[ -s "${EVIDENCE_ROOT}/qemu-complete.txt" ]] && \
        { ! grep -qx "run_id=${RUN_ID}" "${EVIDENCE_ROOT}/qemu-complete.txt" || ! grep -qx "run_token=${RUN_TOKEN}" "${EVIDENCE_ROOT}/qemu-complete.txt"; }; then
        printf 'integrity_failure=invalid_qemu_completion_marker\n' >&2
        INTEGRITY_FAILURES=1
    fi
    boot_marker=$(boot_backend_marker) || {
        printf 'integrity_failure=unsupported_expected_backend backend=%s\n' "${BACKEND}" >&2
        INTEGRITY_FAILURES=1
        return
    }
    if ! awk '{ line = $0; sub(/\015$/, "", line); print line }' "${EVIDENCE_ROOT}/qemu-output.log" 2>/dev/null | \
        grep -Eq "^\\[ext4-ab\\] workload-success run_id=${RUN_ID} backend=${BACKEND} failures=0 perf_samples=[1-9][0-9]*$"; then
        printf 'integrity_failure=missing_or_invalid_guest_workload_success_marker\n' >&2
        printf 'required_marker=[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=<positive integer>\n' \
            "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/guest-completion-blocker.txt"
        INTEGRITY_FAILURES=1
    elif ! awk -v boot="${boot_marker}" -v run_id="${RUN_ID}" -v backend="${BACKEND}" '
        { line = $0; sub(/\015$/, "", line) }
        line == boot { boot_seen = 1; next }
        line ~ "^\\[ext4-ab\\] workload-success run_id=" run_id " backend=" backend " failures=0 perf_samples=[1-9][0-9]*$" && boot_seen { marker_seen = 1 }
        END { exit marker_seen ? 0 : 1 }
    ' "${EVIDENCE_ROOT}/qemu-output.log"; then
        printf 'integrity_failure=guest_marker_backend_does_not_match_boot_backend\n' >&2
        INTEGRITY_FAILURES=1
    fi
    verify_selected_iozone_libc_markers
    require_evidence_file "${EVIDENCE_ROOT}/private-image-hashes-after.txt" private_image_hashes_after
    require_evidence_file "${EVIDENCE_ROOT}/canonical-images-after.sha256" canonical_images_after
    require_evidence_file "${EVIDENCE_ROOT}/qemu-head-tail.txt" qemu_completion_excerpt
    require_evidence_file "${EVIDENCE_ROOT}/iozone-raw-samples.log" iozone_raw_samples
    require_evidence_file "${EVIDENCE_ROOT}/iozone-metrics.csv" iozone_metrics
    [[ "${POST_RUN_HASHES_RECORDED}" -eq 1 ]] || {
        printf '%s\n' 'integrity_failure=post_run_hashes_not_recorded' >&2
        INTEGRITY_FAILURES=1
    }
}

cleanup_owned_qemu() {
    local cleanup_rc
    [[ "${DRY_RUN}" -eq 0 && -n "${EVIDENCE_ROOT:-}" && -d "${EVIDENCE_ROOT}" ]] || return
    set +e
    docker compose exec -T os-dev sh -lc "
        owner_file='${CONTAINER_QEMU_OWNER}'
        [ -s \"\$owner_file\" ] || { printf '%s\\n' 'owner_state=missing'; exit 1; }
        pid=\$(awk -F= '\$1 == \"pid\" { print \$2; exit }' \"\$owner_file\")
        expected_start=\$(awk -F= '\$1 == \"starttime\" { print \$2; exit }' \"\$owner_file\")
        recorded_run_id=\$(awk -F= '\$1 == \"run_id\" { print \$2; exit }' \"\$owner_file\")
        recorded_token=\$(awk -F= '\$1 == \"run_token\" { print \$2; exit }' \"\$owner_file\")
        recorded_container=\$(awk -F= '\$1 == \"container_id\" { print \$2; exit }' \"\$owner_file\")
        pgid=\$(awk -F= '\$1 == \"pgid\" { print \$2; exit }' \"\$owner_file\")
        session=\$(awk -F= '\$1 == \"session\" { print \$2; exit }' \"\$owner_file\")
        if ! case \"\$pid:\$pgid:\$session\" in *[!0-9:]*|:*|*::*) false ;; *) true ;; esac || [ -z \"\$expected_start\" ] || [ \"\$pid\" != \"\$pgid\" ] || [ \"\$pid\" != \"\$session\" ] || [ \"\$recorded_run_id\" != '${RUN_ID}' ] || [ \"\$recorded_token\" != '${RUN_TOKEN}' ] || [ \"\$recorded_container\" != '${CONTAINER_ID}' ]; then
            printf '%s\\n' 'owner_state=invalid_record'
            exit 1
        fi
        if [ -r \"/proc/\$pid/stat\" ]; then
            set -- \$(awk '{ print \$5, \$6, \$22 }' \"/proc/\$pid/stat\")
            if [ \"\$1\" != \"\$pgid\" ] || [ \"\$2\" != \"\$session\" ] || [ \"\$3\" != \"\$expected_start\" ]; then
                printf '%s\\n' 'owner_state=identity_mismatch'
                exit 1
            fi
        fi
        members() { for stat in /proc/[0-9]*/stat; do set -- \$(awk '{ print \$1, \$5, \$6 }' \"\$stat\"); [ \"\$2\" = \"\$pgid\" ] && [ \"\$3\" = \"\$session\" ] && printf '%s ' \"\$1\"; done; }
        before=\$(members)
        [ -z \"\$before\" ] && { printf '%s\\n' 'owner_state=group_absent'; exit 0; }
        printf 'owner_state=group_alive pgid=%s members=%s\\n' \"\$pgid\" \"\$before\"
        kill -TERM \"-\$pgid\"
        attempts=0
        while [ \"\$attempts\" -lt 10 ] && [ -n \"\$(members)\" ]; do
            sleep 1
            attempts=\$((attempts + 1))
        done
        if [ -n \"\$(members)\" ]; then
            kill -KILL \"-\$pgid\"
            sleep 1
        fi
        after=\$(members)
        if [ -n \"\$after\" ]; then
            printf 'owner_state=survived_cleanup members=%s\\n' \"\$after\"
            exit 1
        fi
        printf '%s\\n' 'owner_state=terminated_by_cleanup'
    " >"${EVIDENCE_ROOT}/qemu-cleanup.log" 2>&1
    cleanup_rc=$?
    set -e
    printf 'exit_status=%s\n' "${cleanup_rc}" >"${STATUS_DIR}/qemu-cleanup.status.txt"
    {
        printf '%s\n' '[qemu-cleanup]'
        printf 'command=docker compose exec -T os-dev sh -lc <owned qemu cleanup>\n'
        cat "${STATUS_DIR}/qemu-cleanup.status.txt"
    } >>"${EVIDENCE_ROOT}/command-and-status.txt"
    if [[ "${cleanup_rc}" -ne 0 ]] || grep -q '^owner_state=group_alive ' "${EVIDENCE_ROOT}/qemu-cleanup.log"; then
        INTEGRITY_FAILURES=1
    fi
}

run_qemu() {
    record_command qemu "${EVIDENCE_ROOT}/qemu-output.log" timeout --foreground "${QEMU_TIMEOUT}s" \
        docker compose exec -T os-dev sh -lc "
            set -u
            export EXT4_AB_RUN_ID='${RUN_ID}'
            export EXT4_AB_RUN_TOKEN='${RUN_TOKEN}'
            export EXT4_AB_CONTAINER_ID='${CONTAINER_ID}'
            export QEMU_OWNER_FILE='${CONTAINER_QEMU_OWNER}'
            export QEMU_EXIT_FILE='${CONTAINER_QEMU_EXIT}'
            export QEMU_COMPLETION_FILE='${CONTAINER_QEMU_COMPLETION}'
            rm -f \"\$QEMU_OWNER_FILE\" \"\$QEMU_EXIT_FILE\" \"\$QEMU_COMPLETION_FILE\"
            setsid sh -c '
                starttime=\$(awk '\''{ print \$22 }'\'' \"/proc/\$\$/stat\")
                set -- \$(awk '\''{ print \$5, \$6 }'\'' \"/proc/\$\$/stat\")
                pgid=\$1
                session=\$2
                owner_tmp=\"\$QEMU_OWNER_FILE.\$EXT4_AB_RUN_TOKEN.tmp\"
                [ ! -e \"\$QEMU_OWNER_FILE\" ] || exit 125
                printf \"run_id=%s\\nrun_token=%s\\ncontainer_id=%s\\npid=%s\\nstarttime=%s\\npgid=%s\\nsession=%s\\n\" \\
                    \"\$EXT4_AB_RUN_ID\" \"\$EXT4_AB_RUN_TOKEN\" \"\$EXT4_AB_CONTAINER_ID\" \"\$\$\" \"\$starttime\" \"\$pgid\" \"\$session\" > \"\$owner_tmp\"
                mv -f \"\$owner_tmp\" \"\$QEMU_OWNER_FILE\"
                set +e
                cd /app && make -C /app/os ${RUN_TARGET} EXT4_BACKEND='\''${BACKEND}'\'' EXTRA_FEATURES='\''${EXTRA_FEATURES}'\'' ${OVERRIDES} LOG=warn
                qemu_rc=\$?
                printf \"run_id=%s\\nrun_token=%s\\nexit_status=%s\\nrecorded_utc=%s\\n\" \"\$EXT4_AB_RUN_ID\" \"\$EXT4_AB_RUN_TOKEN\" \"\$qemu_rc\" \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\" > \"\$QEMU_EXIT_FILE\"
                if [ \"\$qemu_rc\" -eq 0 ]; then
                    printf \"run_id=%s\\nrun_token=%s\\nqemu_exit_status=0\\ncompleted_utc=%s\\n\" \"\$EXT4_AB_RUN_ID\" \"\$EXT4_AB_RUN_TOKEN\" \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\" > \"\$QEMU_COMPLETION_FILE\"
                fi
                exit \"\$qemu_rc\"
            '
        "
}

handle_signal() {
    local signal_name=$1 signal_rc=$2
    RUN_INTERRUPTED=1
    if [[ -n "${EVIDENCE_ROOT:-}" && -d "${EVIDENCE_ROOT}" ]]; then
        printf 'signal=%s\ninterrupted_utc=%s\n' "${signal_name}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            >"${EVIDENCE_ROOT}/interruption.txt"
    fi
    exit "${signal_rc}"
}

cleanup() {
    local rc=$?
    local final_rc outcome
    trap - EXIT HUP INT TERM
    if [[ "${DRY_RUN}" -eq 0 && -n "${EVIDENCE_ROOT:-}" && -d "${EVIDENCE_ROOT}" ]]; then
        [[ "${rc}" -eq 0 && "${RUN_INTERRUPTED}" -eq 0 ]] || INTEGRITY_FAILURES=1
        cleanup_owned_qemu
        verify_qemu_completion
        local cleanup_rc=0
        if [[ "${rc}" -ne 0 || "${COMMAND_FAILURES}" -ne 0 || "${INTEGRITY_FAILURES}" -ne 0 ]]; then
            retain_failed_private_images
            printf 'retained private images after failed sample\n' >"${EVIDENCE_ROOT}/cleanup.log"
        else
            set +e
            docker compose exec -T os-dev sh -lc "owner='${CONTAINER_PRIVATE_OWNER}'; [ -f \"\$owner\" ] && [ \"\$(cat \"\$owner\")\" = '${RUN_TOKEN}' ] && rm -rf '${PRIVATE_ROOT}'" \
                >"${EVIDENCE_ROOT}/cleanup.log" 2>&1
            cleanup_rc=$?
            set -e
        fi
        printf 'exit_status=%s\n' "${cleanup_rc}" >"${STATUS_DIR}/cleanup.status.txt"
        [[ "${cleanup_rc}" -eq 0 ]] || INTEGRITY_FAILURES=1
        {
            printf '%s\n' '[cleanup-private-images]'
            printf 'command=docker compose exec -T os-dev sh -lc <token-validated private root cleanup>\n'
            cat "${STATUS_DIR}/cleanup.status.txt"
        } >>"${EVIDENCE_ROOT}/command-and-status.txt"
        final_rc=${rc}
        if [[ "${COMMAND_FAILURES}" -ne 0 || "${INTEGRITY_FAILURES}" -ne 0 ]]; then
            final_rc=1
            outcome=fail
        else
            outcome=pass
        fi
        write_result_status "${outcome}" "${final_rc}" || final_rc=1
        exit "${final_rc}"
    fi
    exit "${rc}"
}

retain_failed_private_images() {
    printf 'status=retained reason=failed_sample private_root=%s\nhash_artifact=%s\n' \
        "${PRIVATE_ROOT}" "${EVIDENCE_ROOT}/private-image-hashes-after.txt" >"${EVIDENCE_ROOT}/private-image-retention.txt"
}

run_integrity_self_test() {
    local fixture
    fixture=$(mktemp -d)
    EVIDENCE_ROOT=${fixture}
    STATUS_DIR="${fixture}/command-status"
    RUN_ID=self-test
    RUN_TOKEN=0123456789abcdef0123456789abcdef
    BACKEND=lwext4
    POST_RUN_HASHES_RECORDED=1
    COMMAND_FAILURES=0
    INTEGRITY_FAILURES=0
    mkdir -p "${STATUS_DIR}"

    emit_judge_baseline_group() {
        local libc=$1 section='' operation='' value='' previous_section=''
        printf '#### OS COMP TEST GROUP START iozone-%s ####\n' "${libc}"
        while IFS='|' read -r section operation value; do
            if [[ "${section}" != "${previous_section}" ]]; then
                printf '%s\n' "${section}"
                previous_section=${section}
            fi
            printf '\tChildren see throughput for  %s \t=   %s kB/sec\n' "${operation}" "${value}"
            printf '\tMax throughput per process \t\t\t=    %s kB/sec\n' "${value}"
        done <<'EOF'
iozone throughput write/read measurements|4 initial writers|3524.04
iozone throughput write/read measurements|4 rewriters|7471.07
iozone throughput write/read measurements|4 readers|13135.64
iozone throughput write/read measurements|4 re-readers|13064.75
iozone throughput random-read measurements|4 initial writers|3401.72
iozone throughput random-read measurements|4 rewriters|7196.32
iozone throughput random-read measurements|4 random readers|11082.03
iozone throughput random-read measurements|4 random writers|7140.81
iozone throughput read-backwards measurements|4 initial writers|3331.39
iozone throughput read-backwards measurements|4 rewriters|4906.05
iozone throughput read-backwards measurements|4 reverse readers|10743.42
iozone throughput stride-read measurements|4 initial writers|3468.09
iozone throughput stride-read measurements|4 rewriters|7004.64
iozone throughput stride-read measurements|4 stride readers|11848.11
iozone throughput fwrite/fread measurements|4 fwriters|3204.25
iozone throughput fwrite/fread measurements|4 freaders|7123.18
iozone throughput pwrite/pread measurements|4 pwrite writers|3724.00
iozone throughput pwrite/pread measurements|4 pread readers|14183.61
iozone throughtput pwritev/preadv measurements|4 initial writers|3609.25
iozone throughtput pwritev/preadv measurements|4 rewriters|8305.90
EOF
        printf '#### OS COMP TEST GROUP END iozone-%s ####\n' "${libc}"
    }

    printf 'mode=run\nmask=0x010\n' >"${fixture}/source.conf"
    CONF_FILE="${fixture}/source.conf"
    if ! validate_config; then
        printf '%s\n' 'self-test failed: valid source config was rejected' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    printf 'ext4_ab_iozone_libc=musl\n' >>"${fixture}/source.conf"
    if (validate_config); then
        printf '%s\n' 'self-test failed: source config iozone libc selector was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    printf 'mode=run\nmask=0x010\n' >"${fixture}/source.conf"
    write_guest_config
    if ! grep -qx "ext4_ab_run_id=${RUN_ID}" "${EVIDENCE_ROOT}/config.txt" || \
        ! grep -qx "ext4_ab_backend=${BACKEND}" "${EVIDENCE_ROOT}/config.txt" || \
        ! grep -qx 'ext4_ab_diag=1' "${EVIDENCE_ROOT}/config.txt" || \
        ! grep -qx "ext4_ab_iozone_libc=${IOZONE_LIBC}" "${EVIDENCE_ROOT}/config.txt" || \
        ! grep -qx "ext4_ab_pair_id=${PAIR_ID}" "${EVIDENCE_ROOT}/config.txt"; then
        printf '%s\n' 'self-test failed: guest A/B identity was not injected' >&2
        rm -rf "${fixture}"
        exit 1
    fi

    write_healthy_fixture() {
        local libc=${1:-${IOZONE_LIBC}}
        printf 'exit_status=0\n' >"${STATUS_DIR}/qemu.status.txt"
        printf 'run_id=%s\nrun_token=%s\nexit_status=0\n' "${RUN_ID}" "${RUN_TOKEN}" >"${EVIDENCE_ROOT}/qemu-exit-status.txt"
        printf 'run_id=%s\nrun_token=%s\nqemu_exit_status=0\n' "${RUN_ID}" "${RUN_TOKEN}" >"${EVIDENCE_ROOT}/qemu-complete.txt"
        printf 'private hash\n' >"${EVIDENCE_ROOT}/private-image-hashes-after.txt"
        printf 'canonical hash\n' >"${EVIDENCE_ROOT}/canonical-images-after.sha256"
        printf 'qemu excerpt\n' >"${EVIDENCE_ROOT}/qemu-head-tail.txt"
        printf '%s\n[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=1\n' "$(boot_backend_marker)" "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
        emit_judge_baseline_group "${libc}" >>"${EVIDENCE_ROOT}/qemu-output.log"
        capture_iozone_metrics
    }
    expect_integrity_failure() {
        local case_name=$1 expected=$2
        INTEGRITY_FAILURES=0
        verify_qemu_completion
        if [[ "${INTEGRITY_FAILURES}" -ne "${expected}" ]]; then
            printf 'self-test failed: %s expected_integrity_failures=%s actual=%s\n' \
                "${case_name}" "${expected}" "${INTEGRITY_FAILURES}" >&2
            rm -rf "${fixture}"
            exit 1
        fi
    }

    write_healthy_fixture
    expect_integrity_failure healthy 0
    IOZONE_LIBC=musl
    write_healthy_fixture
    expect_integrity_failure selected_musl_evidence 0
    IOZONE_LIBC=glibc
    write_healthy_fixture
    emit_judge_baseline_group musl | sed $'s/$/\r/' >>"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure unselected_libc_evidence 1
    write_healthy_fixture
    printf '%s\r\n[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=1\r\n' \
        "$(boot_backend_marker)" "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure healthy_crlf 0
    printf '[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=1\r\n%s\r\n' \
        "${RUN_ID}" "${BACKEND}" "$(boot_backend_marker)" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure crlf_workload_precedes_boot 1
    printf '%s\r\n[ext4-ab] workload-success run_id=other-run backend=%s failures=0 perf_samples=1\r\n' \
        "$(boot_backend_marker)" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure crlf_wrong_run_id 1
    printf '%s\r\n[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=1 \r\n' \
        "$(boot_backend_marker)" "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure crlf_trailing_space 1
    write_healthy_fixture
    printf '[ext4] backend: legacy\n[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=1\n' "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure guest_marker_backend_mismatches_boot_backend 1
    printf '[ext4-ab] workload-success run_id=%s backend=%s failures=1 perf_samples=1\n' "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure failed_guest_workload 1
    printf '[ext4-ab] workload-success run_id=%s backend=%s failures=0 perf_samples=0\n' "${RUN_ID}" "${BACKEND}" >"${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure guest_workload_without_samples 1
    rm "${EVIDENCE_ROOT}/qemu-output.log"
    expect_integrity_failure missing_guest_completion_marker 1
    write_healthy_fixture
    rm "${STATUS_DIR}/qemu.status.txt"
    expect_integrity_failure missing_qemu_wrapper_status 1
    write_healthy_fixture
    printf 'exit_status=124\n' >"${STATUS_DIR}/qemu.status.txt"
    expect_integrity_failure nonzero_qemu_wrapper_status 1
    write_healthy_fixture
    printf 'run_id=%s\nrun_token=%s\nexit_status=1\n' "${RUN_ID}" "${RUN_TOKEN}" >"${EVIDENCE_ROOT}/qemu-exit-status.txt"
    expect_integrity_failure nonzero_qemu_status 1
    write_healthy_fixture
    rm "${EVIDENCE_ROOT}/qemu-complete.txt"
    expect_integrity_failure missing_completion_marker 1
    write_healthy_fixture
    rm "${EVIDENCE_ROOT}/private-image-hashes-after.txt"
    expect_integrity_failure missing_post_run_hashes 1
    write_healthy_fixture
    rm "${EVIDENCE_ROOT}/canonical-images-after.sha256"
    expect_integrity_failure missing_canonical_post_run_hashes 1
    write_healthy_fixture
    POST_RUN_HASHES_RECORDED=0
    expect_integrity_failure incomplete_post_run_hash_recording 1
    POST_RUN_HASHES_RECORDED=1
    mkdir "${fixture}/exclusive"
    if mkdir "${fixture}/exclusive" 2>/dev/null; then
        printf '%s\n' 'self-test failed: concurrent/reuse directory claim succeeded' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    printf 'run_token=another-token\noutcome=pass\n' >"${EVIDENCE_ROOT}/result-status.txt"
    if write_result_status running 1; then
        printf '%s\n' 'self-test failed: stale PASS was replaced' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    rm -f "${EVIDENCE_ROOT}/result-status.txt"
    # shellcheck disable=SC2016 # The subshell, not this script, must expand its PID and child PID.
    setsid sh -c 'sleep 30 & printf "%s %s\n" "$$" "$!" > "$1"' sh "${fixture}/owned-group.txt" &
    local setsid_pid=$!
    wait "${setsid_pid}"
    local leader child
    read -r leader child <"${fixture}/owned-group.txt"
    if ! kill -0 "${child}" 2>/dev/null; then
        printf '%s\n' 'self-test failed: dummy child did not survive leader exit' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    kill -TERM -- "-${leader}"
    sleep 1
    if kill -0 "${child}" 2>/dev/null; then
        kill -KILL -- "-${leader}" 2>/dev/null || true
        printf '%s\n' 'self-test failed: owned process group survived cleanup' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    BACKENDS=lwext4,another,legacy
    if validate_paired_backends; then
        printf '%s\n' 'self-test failed: three backends were accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    for BACKENDS in ',lwext4,another' 'lwext4,another,' 'lwext4,,another'; do
        if validate_paired_backends; then
            printf 'self-test failed: empty backend field was accepted: %s\n' "${BACKENDS}" >&2
            rm -rf "${fixture}"
            exit 1
        fi
    done
    BACKENDS=lwext4,another
    if ! validate_paired_backends; then
        printf '%s\n' 'self-test failed: valid two-backend parsing was rejected' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    BACKENDS=lwext4,another
    mkdir "${fixture}/pair-stats"
    printf 'backend,libc,section,operation,phase,sample_index,value,raw_line\n' >"${fixture}/pair-stats/raw-samples.csv"
    for backend in lwext4 another; do
        for sample in 1 2 3 4 5; do
            printf '%s,glibc,children_1,initial_writers,formal,%s,%s,"Children see throughput"\n' "${backend}" "${sample}" "$((sample * 10))" >>"${fixture}/pair-stats/raw-samples.csv"
        done
    done
    if ! write_pair_statistics "${fixture}/pair-stats" lwext4 another || ! grep -qx 'lwext4,glibc,children_1,initial_writers,5,30.000000,14.000000,46.000000,32.000000' "${fixture}/pair-stats/summary.csv"; then
        printf '%s\n' 'self-test failed: formal statistics were not computed from five samples' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    sed -i '$d' "${fixture}/pair-stats/raw-samples.csv"
    if write_pair_statistics "${fixture}/pair-stats" lwext4 another; then
        printf '%s\n' 'self-test failed: incomplete formal samples emitted a summary' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    mkdir "${fixture}/identity-a" "${fixture}/identity-b"
    for identity_root in "${fixture}/identity-a" "${fixture}/identity-b"; do
        for identity_file in git-hash.txt root-git-status.txt root-git-diff.sha256 submodule-status.txt submodule-source-state.txt submodule-untracked-source-provenance.txt untracked-source-provenance.txt; do
            printf 'same canonical content\n' >"${identity_root}/${identity_file}"
        done
    done
    if [[ "$(pair_identity_digest "${fixture}/identity-a")" != "$(pair_identity_digest "${fixture}/identity-b")" ]]; then
        printf '%s\n' 'self-test failed: identity digest depended on sample directory path' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    printf 'changed content\n' >"${fixture}/identity-b/git-hash.txt"
    if [[ "$(pair_identity_digest "${fixture}/identity-a")" == "$(pair_identity_digest "${fixture}/identity-b")" ]]; then
        printf '%s\n' 'self-test failed: identity digest ignored content change' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    PRIVATE_ROOT=/tmp/ext4-ab-self-test-retained
    retain_failed_private_images
    if ! grep -qx 'status=retained reason=failed_sample private_root=/tmp/ext4-ab-self-test-retained' "${EVIDENCE_ROOT}/private-image-retention.txt"; then
        printf '%s\n' 'self-test failed: failed private image retention was not recorded' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    pair_fail "${fixture}/pair-stats" deliberate_test_failure 100
    if ! grep -qx 'outcome=fail' "${fixture}/pair-stats/result-status.txt" || ! grep -qx 'reason=deliberate_test_failure' "${fixture}/pair-stats/result-status.txt"; then
        printf '%s\n' 'self-test failed: pair failure state was not persisted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    emit_judge_baseline_group glibc >"${EVIDENCE_ROOT}/qemu-output.log"
    emit_judge_baseline_group musl >>"${EVIDENCE_ROOT}/qemu-output.log"
    IOZONE_LIBC=glibc
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -ne 0 ]] || [[ "$(wc -l <"${EVIDENCE_ROOT}/iozone-metrics.csv")" -ne 20 ]] || ! grep -q '^glibc,iozone write/read,4 initial writers,3524.04,' "${EVIDENCE_ROOT}/iozone-metrics.csv" || grep -q '^musl,' "${EVIDENCE_ROOT}/iozone-metrics.csv"; then
        printf '%s\n' 'self-test failed: semantic iozone parser did not preserve actual max-throughput output' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    emit_judge_baseline_group glibc | sed $'s/^/\033[0m/' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -ne 0 ]] || [[ "$(wc -l <"${EVIDENCE_ROOT}/iozone-metrics.csv")" -ne 20 ]]; then
        printf '%s\n' 'self-test failed: ANSI-styled iozone delimiters or metrics were rejected' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    emit_judge_baseline_group glibc >"${EVIDENCE_ROOT}/qemu-output.log"
    emit_judge_baseline_group musl >>"${EVIDENCE_ROOT}/qemu-output.log"
    INTEGRITY_FAILURES=0
    IOZONE_LIBC=musl
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -ne 0 ]] || [[ "$(wc -l <"${EVIDENCE_ROOT}/iozone-metrics.csv")" -ne 20 ]] || ! grep -q '^musl,iozone pwrite/pread,4 pread readers,14183.61,' "${EVIDENCE_ROOT}/iozone-metrics.csv" || grep -q '^glibc,' "${EVIDENCE_ROOT}/iozone-metrics.csv"; then
        printf '%s\n' 'self-test failed: selected musl group did not isolate exactly twenty metrics' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    IOZONE_LIBC=glibc
    printf '#### OS COMP TEST GROUP START iozone-glibc ####\niozone throughput write/read measurements\nChildren see throughput for malformed output\n#### OS COMP TEST GROUP END iozone-glibc ####\n' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: malformed iozone output was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    emit_judge_baseline_group glibc >"${EVIDENCE_ROOT}/qemu-output.log"
    emit_judge_baseline_group glibc >>"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: multiple selected iozone groups were accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    printf '#### OS COMP TEST GROUP START iozone-musl ####\n#### OS COMP TEST GROUP START iozone-glibc ####\n#### OS COMP TEST GROUP END iozone-glibc ####\n#### OS COMP TEST GROUP END iozone-musl ####\n' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: target group nested inside other libc was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    printf '#### OS COMP TEST GROUP START iozone-glibc ####\n#### OS COMP TEST GROUP START iozone-musl ####\n#### OS COMP TEST GROUP END iozone-musl ####\n#### OS COMP TEST GROUP END iozone-glibc ####\n' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: other libc nested inside target group was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    printf '#### OS COMP TEST GROUP START iozone-glibc ####\n#### OS COMP TEST GROUP END iozone-musl ####\n' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: cross-libc END marker was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    emit_judge_baseline_group glibc >"${EVIDENCE_ROOT}/qemu-output.log"
    printf '#### OS COMP TEST GROUP END iozone-glibc ####\n' >>"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: stray END after valid selected span was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    printf '#### OS COMP TEST GROUP BEGIN iozone-glibc ####\n' >"${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: malformed selected iozone delimiter was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    INTEGRITY_FAILURES=0
    emit_judge_baseline_group glibc >"${EVIDENCE_ROOT}/qemu-output.log"
    sed -i '/^#### OS COMP TEST GROUP END iozone-glibc ####$/i\Children see throughput for  4 initial writers = 101 kB/sec\nMax throughput per process = 26 kB/sec' "${EVIDENCE_ROOT}/qemu-output.log"
    capture_iozone_metrics
    if [[ "${INTEGRITY_FAILURES}" -eq 0 ]]; then
        printf '%s\n' 'self-test failed: duplicate semantic iozone metric was accepted' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    if ! EXT4_AB_TEST_DEFAULT_REACHABILITY=1 bash "${BASH_SOURCE[0]}" >/dev/null || EXT4_AB_TEST_DEFAULT_REACHABILITY=1 bash "${BASH_SOURCE[0]}" --paired --dry-run >/dev/null 2>&1; then
        printf '%s\n' 'self-test failed: default paired reachability or paired dry-run rejection regressed' >&2
        rm -rf "${fixture}"
        exit 1
    fi
    rm -rf "${fixture}"
    printf '%s\n' 'integrity self-test: PASS'
}

pair_identity_digest() {
    local sample_root=$1
    local file
    for file in git-hash.txt root-git-status.txt root-git-diff.sha256 submodule-status.txt submodule-source-state.txt submodule-untracked-source-provenance.txt untracked-source-provenance.txt; do
        [[ -s "${sample_root}/${file}" ]] || return 1
        sha256sum "${sample_root}/${file}" | cut -d ' ' -f 1
    done | sha256sum | cut -d ' ' -f 1
}

write_pair_statistics() {
    local pair_root=$1 raw_samples summary
    raw_samples="${pair_root}/raw-samples.csv"
    summary="${pair_root}/summary.csv"
    awk -F, -v expected_a="$2" -v expected_b="$3" '
        NR == 1 { next }
        {
            if ($1 != expected_a && $1 != expected_b) exit 1
            if ($5 != "formal" || $6 !~ /^[1-5]$/ || $7 !~ /^[0-9]+(\.[0-9]+)?$/) exit 1
            backend_seen[$1] = 1
            metric = $2 SUBSEP $3 SUBSEP $4
            sample_key = $1 SUBSEP metric SUBSEP $6
            if (seen_sample[sample_key]++) exit 1
            key = $1 SUBSEP metric
            values[key, ++count[key]] = $7 + 0
            metric_seen[$1 SUBSEP metric] = 1
        }
        END {
            print "backend,libc,section,operation,count,median,p10,p90,dispersal"
            if (!(expected_a in backend_seen) || !(expected_b in backend_seen)) exit 1
            for (metric_key in metric_seen) {
                split(metric_key, seen_parts, SUBSEP)
                backend = seen_parts[1]
                metric = seen_parts[2] SUBSEP seen_parts[3] SUBSEP seen_parts[4]
                other = (backend == expected_a ? expected_b : expected_a)
                if (!(other SUBSEP metric in metric_seen)) exit 1
            }
            for (key in count) {
                n = count[key]
                if (n != 5) exit 1
                split(key, parts, SUBSEP)
                for (i = 1; i <= n; ++i) for (j = i + 1; j <= n; ++j) if (values[key, i] > values[key, j]) { t = values[key, i]; values[key, i] = values[key, j]; values[key, j] = t }
                median = values[key, 3]
                p10 = values[key, 1] + 0.4 * (values[key, 2] - values[key, 1])
                p90 = values[key, 4] + 0.6 * (values[key, 5] - values[key, 4])
                dispersal = p90 - p10
                printf "%s,%s,%s,%s,%d,%.6f,%.6f,%.6f,%.6f\n", parts[1], parts[2], parts[3], parts[4], n, median, p10, p90, dispersal
            }
        }
    ' "${raw_samples}" >"${summary}.tmp" || return 1
    [[ -s "${summary}.tmp" ]] || return 1
    mv "${summary}.tmp" "${summary}"
}

validate_paired_backends() {
    local -a values
    [[ "${BACKENDS}" != ,* && "${BACKENDS}" != *, && "${BACKENDS}" != *,,* ]] || return 1
    IFS=, read -r -a values <<<"${BACKENDS}"
    [[ "${#values[@]}" -eq 2 && -n "${values[0]}" && -n "${values[1]}" && "${values[0]}" != "${values[1]}" ]] || return 1
    case "${values[0]},${values[1]}" in
        lwext4,another|another,lwext4|lwext4,legacy|legacy,lwext4|another,legacy|legacy,another) return 0 ;;
        *) return 1 ;;
    esac
}

pair_fail() {
    local pair_root=$1 reason=$2 started_epoch=$3 ended_epoch
    ended_epoch=$(date +%s)
    rm -f "${pair_root}/summary.csv" "${pair_root}/summary.csv.tmp"
    printf 'outcome=fail\nreason=%s\ncompleted_utc=%s\nended_epoch=%s\nwall_duration_seconds=%s\n' \
        "${reason}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${ended_epoch}" "$((ended_epoch - started_epoch))" >"${pair_root}/result-status.txt"
    printf 'pair_failure=%s\n' "${reason}" >&2
}

run_paired_protocol() {
    local pair_root first_backend second_backend phase index backend sample_root rc baseline_identity identity started_epoch
    local -a backend_values backend_order
    validate_paired_backends || {
        printf '%s\n' 'EXT4_AB_BACKENDS must name exactly two distinct comma-separated backends' >&2
        exit 2
    }
    IFS=, read -r -a backend_values <<<"${BACKENDS}"
    first_backend=${backend_values[0]}
    second_backend=${backend_values[1]}
    require_file "${SOURCE_CARD}"
    require_file "${SOURCE_DISK}"
    require_file "${CONF_FILE}"
    validate_config
    pair_root="${REPO_ROOT}/docs/Work_Log/evidence/${DATE}/ext4-backend-ab-pair-${ARCH}-${PAIR_ID}"
    mkdir "${pair_root}" || { printf 'refusing to reuse pair evidence directory: %s\n' "${pair_root}" >&2; exit 1; }
    mkdir "${pair_root}/samples"
    started_epoch=$(date +%s)
    printf 'outcome=running\nstarted_utc=%s\nstarted_epoch=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${started_epoch}" >"${pair_root}/result-status.txt"
    printf 'pair_id=%s\narch=%s\nbackends=%s\niozone_libc=%s\nwarmup_samples=1\nformal_samples=5\nqemu_timeout_seconds=%s\nextra_features=%s\nsource_config_sha256=%s\nsource_images_sha256=%s\nstarted_utc=%s\nexecution=serial; backend order rotates for every sample index\n' \
        "${PAIR_ID}" "${ARCH}" "${BACKENDS}" "${IOZONE_LIBC}" "${QEMU_TIMEOUT}" "${EXTRA_FEATURES}" \
        "$(sha256sum "${CONF_FILE}" | cut -d ' ' -f 1)" \
        "$(sha256sum "${SOURCE_CARD}" "${SOURCE_DISK}" | sha256sum | cut -d ' ' -f 1)" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"${pair_root}/manifest.txt"
    printf 'backend,libc,section,operation,phase,sample_index,value,raw_line\n' >"${pair_root}/raw-samples.csv"
    printf 'backend,phase,sample_index,run_id,outcome\n' >"${pair_root}/sample-status.csv"
    for index in 0 1 2 3 4 5; do
        phase=formal
        [[ "${index}" -eq 0 ]] && phase=warmup
        if (( index % 2 )); then
            backend_order=("${second_backend}" "${first_backend}")
        else
            backend_order=("${first_backend}" "${second_backend}")
        fi
        for backend in "${backend_order[@]}"; do
            sample_root="${pair_root}/samples/${phase}-${index}-${backend}"
            set +e
            EXT4_AB_ARCH="${ARCH}" EXT4_AB_BACKEND="${backend}" EXT4_AB_EXTRA_FEATURES="${EXTRA_FEATURES}" \
            EXT4_AB_CONF_FILE="${CONF_FILE}" EXT4_AB_QEMU_TIMEOUT="${QEMU_TIMEOUT}" EXT4_AB_PAIR_ID="${PAIR_ID}" EXT4_AB_IOZONE_LIBC="${IOZONE_LIBC}" \
            EXT4_AB_SAMPLE_PHASE="${phase}" EXT4_AB_SAMPLE_INDEX="${index}" \
            EXT4_AB_RUN_ID="${PAIR_ID}-${phase}-${index}-${backend}" EXT4_AB_EVIDENCE_ROOT="${sample_root}" \
            bash "${SCRIPT_DIR}/run_ext4_backend_ab.sh" --single
            rc=$?
            set -e
            printf '%s,%s,%s,%s,%s\n' "${backend}" "${phase}" "${index}" "${PAIR_ID}-${phase}-${index}-${backend}" "$([[ "${rc}" -eq 0 ]] && printf pass || printf fail)" >>"${pair_root}/sample-status.csv"
            [[ "${rc}" -eq 0 ]] || { pair_fail "${pair_root}" "sample_failed:${backend}:${phase}:${index}" "${started_epoch}"; return 1; }
            identity=$(pair_identity_digest "${sample_root}") || { pair_fail "${pair_root}" missing_source_identity "${started_epoch}"; return 1; }
            if [[ -z "${baseline_identity:-}" ]]; then baseline_identity=${identity}; elif [[ "${identity}" != "${baseline_identity}" ]]; then pair_fail "${pair_root}" source_identity_mismatch "${started_epoch}"; return 1; fi
            cmp -s "${sample_root}/canonical-images-before.sha256" "${pair_root}/expected-inputs.sha256" 2>/dev/null || {
                if [[ ! -e "${pair_root}/expected-inputs.sha256" ]]; then cp "${sample_root}/canonical-images-before.sha256" "${pair_root}/expected-inputs.sha256"; else pair_fail "${pair_root}" config_or_image_source_mismatch "${started_epoch}"; return 1; fi
            }
            grep -qx "extra_features=${EXTRA_FEATURES}" "${sample_root}/manifest.txt" || {
                pair_fail "${pair_root}" feature_source_mismatch "${started_epoch}"
                return 1
            }
            if [[ "${phase}" == formal ]] && ! awk -F, -v backend="${backend}" -v phase="${phase}" -v sample_index="${index}" 'BEGIN { OFS="," } { print backend, $1, $2, $3, phase, sample_index, $4, $5 }' "${sample_root}/iozone-metrics.csv" >>"${pair_root}/raw-samples.csv"; then
                pair_fail "${pair_root}" "formal_metric_append_failed:${backend}:${index}" "${started_epoch}"
                return 1
            fi
        done
    done
    write_pair_statistics "${pair_root}" "${first_backend}" "${second_backend}" || { pair_fail "${pair_root}" missing_or_incomplete_formal_metric_samples "${started_epoch}"; return 1; }
    printf 'outcome=pass\ncompleted_utc=%s\nended_epoch=%s\nwall_duration_seconds=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(date +%s)" "$(( $(date +%s) - started_epoch ))" >"${pair_root}/result-status.txt"
    printf 'paired benchmark: PASS pair_id=%s evidence=%s\n' "${PAIR_ID}" "${pair_root}"
}

if [[ "${SELF_TEST}" -eq 1 ]]; then
    run_integrity_self_test
    exit 0
fi

if [[ "${EXT4_AB_TEST_DEFAULT_REACHABILITY:-0}" -eq 1 ]]; then
    [[ "${PAIRED}" -eq 1 ]] || exit 1
    printf '%s\n' 'default invocation reaches paired protocol'
    exit 0
fi

if [[ "${PAIRED}" -eq 1 ]]; then
    run_paired_protocol
    exit 0
fi

require_file "${SOURCE_CARD}"
require_file "${SOURCE_DISK}"
require_file "${CONF_FILE}"
validate_config

if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf 'dry-run arch=%s backend=%s private_card=%s private_disk=%s\n' \
        "${ARCH}" "${BACKEND}" "${PRIVATE_CARD}" "${PRIVATE_DISK}"
    printf '%s\n' 'dry-run action=make-n; no private image copy, image injection, build, or QEMU execution'
    print_docker_make_dry_run
    exit 0
fi

COMMAND_FAILURES=0
INTEGRITY_FAILURES=0
RUN_INTERRUPTED=0
POST_RUN_HASHES_RECORDED=0
CONTAINER_ID=
create_exclusive_run_dirs
trap cleanup EXIT
trap 'handle_signal HUP 129' HUP
trap 'handle_signal INT 130' INT
trap 'handle_signal TERM 143' TERM

write_guest_config
sha256sum "${SOURCE_CARD}" "${SOURCE_DISK}" "${CONF_FILE}" >"${EVIDENCE_ROOT}/canonical-images-before.sha256"
record_source_identity

record_command compose-up "${LOG_DIR}/compose-up.log" docker compose up -d os-dev
CONTAINER_ID=$(docker compose ps -q os-dev)
if ! [[ "${CONTAINER_ID}" =~ ^[0-9a-f]{12,64}$ ]]; then
    printf 'invalid os-dev container ID: %s\n' "${CONTAINER_ID}" >&2
    CONTAINER_ID=invalid
    COMMAND_FAILURES=1
    exit 1
fi
{
    printf 'container_id=%s\n' "${CONTAINER_ID}"
    docker inspect "${CONTAINER_ID}" --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'
} >"${EVIDENCE_ROOT}/container-id.txt"

{
    printf 'runner=run_ext4_backend_ab.sh\n'
    printf 'run_id=%s\nrun_token=%s\n' "${RUN_ID}" "${RUN_TOKEN}"
    printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'arch=%s\nbackend=%s\n' "${ARCH}" "${BACKEND}"
    printf 'extra_features=%s\n' "${EXTRA_FEATURES}"
    printf 'execution_order=%s build,inject,qemu; one architecture/backend only\n' "${ARCH}"
    printf 'source_card=%s\nsource_disk=%s\n' "${SOURCE_CARD}" "${SOURCE_DISK}"
    printf 'private_card=%s\nprivate_disk=%s\n' "${PRIVATE_CARD}" "${PRIVATE_DISK}"
    printf 'qemu_timeout_seconds=%s\n' "${QEMU_TIMEOUT}"
    printf 'config=%s\n' "${CONTAINER_CONFIG}"
    printf 'overrides=%s\n' "${OVERRIDES}"
} >"${EVIDENCE_ROOT}/manifest.txt"

record_docker_shell copy-private-images "${LOG_DIR}/copy-private-images.log" \
    "set -eu; umask 077; mkdir '${PRIVATE_ROOT}'; printf '%s\\n' '${RUN_TOKEN}' > '${CONTAINER_PRIVATE_OWNER}'; cp '/app/${CARD_NAME}' '${PRIVATE_CARD}'; cp '/app/${DISK_NAME}' '${PRIVATE_DISK}'; sha256sum '${PRIVATE_CARD}' '${PRIVATE_DISK}'"
cp "${LOG_DIR}/copy-private-images.log" "${EVIDENCE_ROOT}/private-image-hashes-before.txt"
record_docker_shell build "${LOG_DIR}/build.log" \
    "cd /app && make -C /app/os ${BUILD_TARGET} EXT4_BACKEND='${BACKEND}' EXTRA_FEATURES='${EXTRA_FEATURES}' LOG=warn"
record_docker_shell inject "${LOG_DIR}/inject.log" \
    "cd /app && make -C /app/os conf-inject CONF_ARCH='${ARCH}' CONF_BLK_MODE='${CONF_BLK_MODE}' CONF_FILE='${CONTAINER_CONFIG}' CONF_IMAGE='${PRIVATE_CARD}' AUTO_REBUILD_MEM=0 MODE=release LOG=warn"
run_qemu
make_excerpt
capture_iozone_metrics
record_docker_shell private-image-hashes-after "${LOG_DIR}/private-image-hashes-after.log" \
    "sha256sum '${PRIVATE_CARD}' '${PRIVATE_DISK}'"
cp "${LOG_DIR}/private-image-hashes-after.log" "${EVIDENCE_ROOT}/private-image-hashes-after.txt"
sha256sum "${SOURCE_CARD}" "${SOURCE_DISK}" "${CONF_FILE}" >"${EVIDENCE_ROOT}/canonical-images-after.sha256"
cmp -s "${EVIDENCE_ROOT}/canonical-images-before.sha256" "${EVIDENCE_ROOT}/canonical-images-after.sha256" || INTEGRITY_FAILURES=1
POST_RUN_HASHES_RECORDED=1

if [[ "${COMMAND_FAILURES}" -ne 0 || "${INTEGRITY_FAILURES}" -ne 0 ]]; then
    exit 1
fi
