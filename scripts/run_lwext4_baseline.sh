#!/bin/sh
# Serial, Docker-only lwext4 baseline evidence runner.

set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
DATE=$(date +%Y-%m-%d)
STAMP=$(date +%Y%m%d-%H%M%S)
EVIDENCE_ROOT=${LWEXT4_BASELINE_EVIDENCE_ROOT:-"${REPO_ROOT}/docs/Work_Log/evidence/${DATE}/lwext4-baseline-${STAMP}"}
case "${EVIDENCE_ROOT}" in
    "${REPO_ROOT}"/*) ;;
    *)
        printf '%s\n' 'evidence root must be under the mounted repository' >&2
        exit 1
        ;;
esac
EVIDENCE_REL=${EVIDENCE_ROOT#"${REPO_ROOT}"/}
CONTAINER_EVIDENCE_ROOT=/app/${EVIDENCE_REL}
RUNTIME_DIR=${EVIDENCE_ROOT}/runtime-configs
STATUS_DIR=${EVIDENCE_ROOT}/command-status
LOG_DIR=${EVIDENCE_ROOT}/logs
EXCERPT_DIR=${EVIDENCE_ROOT}/qemu-excerpts
METRICS_DIR=${EVIDENCE_ROOT}/metrics
CONTAINER_TMP_PREFIX=${LWEXT4_BASELINE_TMP_PREFIX:-"/tmp/lwext4-baseline-${STAMP}"}
QEMU_TIMEOUT=${LWEXT4_BASELINE_QEMU_TIMEOUT:-300}

rv_image=${REPO_ROOT}/sdcard-rv.img
la_image=${REPO_ROOT}/sdcard-la.img
rv_card=${CONTAINER_TMP_PREFIX}/sdcard-rv.img
la_card=${CONTAINER_TMP_PREFIX}/sdcard-la.img
rv_conf=${RUNTIME_DIR}/rv64-os_test.conf
la_conf=${RUNTIME_DIR}/la64-os_test.conf
command_failures=0
integrity_failures=0

fail_integrity() {
    printf '%s\n' "collection-integrity failure: $1" >&2
    integrity_failures=1
}

record_command() {
    name=$1
    log=$2
    shift 2
    command_file=${STATUS_DIR}/${name}.command.txt
    status_file=${STATUS_DIR}/${name}.status.txt

    {
        printf '%s' 'command='
        printf '%s ' "$@"
        printf '\n'
    } >"${command_file}" || fail_integrity "cannot write ${command_file}"

    set +e
    "$@" >"${log}" 2>&1
    rc=$?
    set -e
    printf 'exit_status=%s\n' "${rc}" >"${status_file}" || fail_integrity "cannot write ${status_file}"
    if [ "${rc}" -ne 0 ]; then
        command_failures=1
    fi
    return 0
}

record_docker_shell() {
    name=$1
    log=$2
    shell_command=$3
    record_command "${name}" "${log}" docker compose exec -T os-dev sh -lc "${shell_command}"
}

make_excerpt() {
    log=$1
    excerpt=$2
    if [ -s "${log}" ]; then
        {
            sed -n '1,40p' "${log}"
            printf '%s\n' '--- qemu log tail ---'
            tail -n 40 "${log}"
        } >"${excerpt}" || fail_integrity "cannot write ${excerpt}"
    else
        printf '%s\n' 'status=missing reason=empty_qemu_log' >"${excerpt}" || fail_integrity "cannot write ${excerpt}"
    fi
}

write_metrics() {
    arch=$1
    log=$2
    summary=${METRICS_DIR}/${arch}-ltp-summary.txt
    summary_status=${METRICS_DIR}/${arch}-ltp-summary-status.txt
    perf=${METRICS_DIR}/${arch}-lwext4-perf.txt
    perf_status=${METRICS_DIR}/${arch}-lwext4-perf-status.txt

    awk '/TPASS|TFAIL|TBROK|TCONF|Summary|PASS|FAIL/ { print }' "${log}" >"${summary}" 2>/dev/null || true
    if [ ! -s "${summary}" ]; then
        printf '%s\n' 'status=unavailable reason=ltp_summary_not_observed' >"${summary}" || fail_integrity "cannot write ${summary}"
        printf '%s\n' 'status=unavailable reason=ltp_summary_not_observed' >"${summary_status}" || fail_integrity "cannot write ${summary_status}"
    else
        printf '%s\n' 'status=observed source=qemu-log' >"${summary_status}" || fail_integrity "cannot write ${summary_status}"
    fi

    awk '/\[ltprunner\] lwext4-perf|lwext4-perf.*status=unavailable|lwext4-perf.*status=missing/ { print }' "${log}" >"${perf}" 2>/dev/null || true
    if grep -q 'lwext4-perf' "${perf}" 2>/dev/null; then
        printf '%s\n' 'status=observed source=qemu-log' >"${perf_status}" || fail_integrity "cannot write ${perf_status}"
    else
        printf '%s\n' 'status=unavailable reason=lwext4_perf_line_not_observed' >"${perf_status}" || fail_integrity "cannot write ${perf_status}"
        printf '%s\n' 'status=unavailable reason=lwext4_perf_line_not_observed' >"${perf}" || fail_integrity "cannot write ${perf}"
    fi
}

cleanup() {
    cleanup_status=${STATUS_DIR}/cleanup.status.txt
    set +e
    docker compose exec -T os-dev sh -lc "rm -rf '${CONTAINER_TMP_PREFIX}'" >"${LOG_DIR}/cleanup.log" 2>&1
    rc=$?
    set -e
    printf 'exit_status=%s\n' "${rc}" >"${cleanup_status}" 2>/dev/null || integrity_failures=1
    [ "${rc}" -eq 0 ] || integrity_failures=1
    printf 'finished_utc=%s\ncommand_failures=%s\nintegrity_failures=%s\n' \
        "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${command_failures}" "${integrity_failures}" \
        >"${EVIDENCE_ROOT}/result-status.txt" 2>/dev/null || integrity_failures=1
}

mkdir -p "${RUNTIME_DIR}" "${STATUS_DIR}" "${LOG_DIR}" "${EXCERPT_DIR}" "${METRICS_DIR}" || exit 1
trap cleanup EXIT HUP INT TERM

record_command compose-up "${LOG_DIR}/compose-up.log" docker compose up -d os-dev

set +e
container_id=$(docker compose ps -q os-dev 2>/dev/null)
if [ -n "${container_id}" ]; then
    {
        printf 'container_id=%s\n' "${container_id}"
        docker inspect "${container_id}" --format '{{range .Mounts}}{{println .Source "->" .Destination}}{{end}}'
    } >"${EVIDENCE_ROOT}/container-id.txt"
    [ "$?" -eq 0 ] || integrity_failures=1
    docker_image=$(docker inspect "${container_id}" --format '{{.Config.Image}}' 2>/dev/null)
    [ -n "${docker_image}" ] || docker_image=unavailable
    printf 'docker_image=%s\n' "${docker_image}" >"${EVIDENCE_ROOT}/docker-image.txt"
else
    printf '%s\n' 'status=unavailable reason=os-dev_container_not_found' >"${EVIDENCE_ROOT}/container-id.txt"
    printf '%s\n' 'docker_image=unavailable' >"${EVIDENCE_ROOT}/docker-image.txt"
    integrity_failures=1
fi
set -e

for input in "${rv_image}" "${la_image}" "${REPO_ROOT}/os_test.conf"; do
    [ -f "${input}" ] || { fail_integrity "missing input ${input}"; exit 1; }
done

cp "${REPO_ROOT}/os_test.conf" "${rv_conf}" || fail_integrity 'cannot copy rv64 runtime config'
cp "${REPO_ROOT}/os_test.conf" "${la_conf}" || fail_integrity 'cannot copy la64 runtime config'
printf '%s\n' 'ltp_lwext4_perf_log=1' >>"${rv_conf}" || fail_integrity 'cannot append rv64 perf flag'
printf '%s\n' 'ltp_lwext4_perf_log=1' >>"${la_conf}" || fail_integrity 'cannot append la64 perf flag'

git -C "${REPO_ROOT}" rev-parse HEAD >"${EVIDENCE_ROOT}/git-head.txt" 2>/dev/null || {
    printf '%s\n' 'status=unavailable reason=git_head_unavailable' >"${EVIDENCE_ROOT}/git-head.txt"
    integrity_failures=1
}
git -C "${REPO_ROOT}" status --porcelain >"${EVIDENCE_ROOT}/git-status-porcelain.txt" 2>/dev/null || {
    printf '%s\n' 'status=unavailable reason=git_status_unavailable' >"${EVIDENCE_ROOT}/git-status-porcelain.txt"
    integrity_failures=1
}
sha256sum "${REPO_ROOT}/os_test.conf" "${rv_conf}" "${la_conf}" \
    >"${EVIDENCE_ROOT}/config-checksums.sha256" || fail_integrity 'cannot hash runtime configs'

{
    printf 'runner=run_lwext4_baseline.sh\n'
    printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'evidence_root=%s\n' "${EVIDENCE_ROOT}"
    printf 'container_evidence_root=%s\n' "${CONTAINER_EVIDENCE_ROOT}"
    printf 'git_head_file=%s\n' "${EVIDENCE_ROOT}/git-head.txt"
    printf 'git_status_file=%s\n' "${EVIDENCE_ROOT}/git-status-porcelain.txt"
    printf 'docker_image_file=%s\n' "${EVIDENCE_ROOT}/docker-image.txt"
    printf 'container_mounts_file=%s\n' "${EVIDENCE_ROOT}/container-id.txt"
    printf 'input_checksums_before=%s\n' "${EVIDENCE_ROOT}/inputs-before.sha256"
    printf 'input_checksums_after=%s\n' "${EVIDENCE_ROOT}/inputs-after.sha256"
    printf 'config_checksums=%s\n' "${EVIDENCE_ROOT}/config-checksums.sha256"
    printf 'qemu_timeout_seconds=%s\n' "${QEMU_TIMEOUT}"
    printf 'execution_order=rv64 build,rv64 inject,rv64 qemu,la64 build,la64 inject,la64 qemu\n'
    printf 'forbidden_commands=fs_test,run_full_test.py\n'
    printf 'metrics_policy=observed_ltp_summary_and_lwext4_perf_lines_only; no_raw_stats_snapshots\n'
} >"${EVIDENCE_ROOT}/manifest.txt" || fail_integrity 'cannot write manifest'

sha256sum "${rv_image}" "${la_image}" "${REPO_ROOT}/os_test.conf" >"${EVIDENCE_ROOT}/inputs-before.sha256" || fail_integrity 'cannot hash original inputs before run'

record_docker_shell copy-cards "${LOG_DIR}/copy-cards.log" \
    "set -eu; mkdir -p '${CONTAINER_TMP_PREFIX}'; cp '/app/sdcard-rv.img' '${rv_card}'; cp '/app/sdcard-la.img' '${la_card}'"

# RV64 is deliberately complete before any LA64 build starts.
record_docker_shell rv64-build "${LOG_DIR}/rv64-build.log" \
    'cd /app && make -C /app/os rv64-only EXTRA_FEATURES=perf_diag LOG=warn'
record_docker_shell rv64-inject "${LOG_DIR}/rv64-inject.log" \
    "cd /app && ARCH=rv64 BLK_MODE=virt CONF_FILE='${CONTAINER_EVIDENCE_ROOT}/runtime-configs/rv64-os_test.conf' IMAGE_PATH='${rv_card}' AUTO_REBUILD_MEM=0 MODE=release LOG=warn make -C /app/os conf-inject CONF_ARCH=rv64 CONF_BLK_MODE=virt CONF_FILE='${CONTAINER_EVIDENCE_ROOT}/runtime-configs/rv64-os_test.conf' CONF_IMAGE='${rv_card}'"
record_command rv64-qemu "${LOG_DIR}/rv64-qemu.log" timeout --foreground "${QEMU_TIMEOUT}s" docker compose exec -T os-dev sh -lc "cd /app && make -C /app/os rv64-run SDCARD_RV='${rv_card}' LOG=warn"
make_excerpt "${LOG_DIR}/rv64-qemu.log" "${EXCERPT_DIR}/rv64.txt"
write_metrics rv64 "${LOG_DIR}/rv64-qemu.log"

record_docker_shell la64-build "${LOG_DIR}/la64-build.log" \
    'cd /app && make -C /app/os la64-only EXTRA_FEATURES=perf_diag LOG=warn'
record_docker_shell la64-inject "${LOG_DIR}/la64-inject.log" \
    "cd /app && ARCH=la64 BLK_MODE=virt_pci CONF_FILE='${CONTAINER_EVIDENCE_ROOT}/runtime-configs/la64-os_test.conf' IMAGE_PATH='${la_card}' AUTO_REBUILD_MEM=0 MODE=release LOG=warn make -C /app/os conf-inject CONF_ARCH=la64 CONF_BLK_MODE=virt_pci CONF_FILE='${CONTAINER_EVIDENCE_ROOT}/runtime-configs/la64-os_test.conf' CONF_IMAGE='${la_card}'"
record_command la64-qemu "${LOG_DIR}/la64-qemu.log" timeout --foreground "${QEMU_TIMEOUT}s" docker compose exec -T os-dev sh -lc "cd /app && make -C /app/os la64-run SDCARD_LA='${la_card}' LOG=warn"
make_excerpt "${LOG_DIR}/la64-qemu.log" "${EXCERPT_DIR}/la64.txt"
write_metrics la64 "${LOG_DIR}/la64-qemu.log"

sha256sum "${rv_image}" "${la_image}" "${REPO_ROOT}/os_test.conf" >"${EVIDENCE_ROOT}/inputs-after.sha256" || fail_integrity 'cannot hash original inputs after run'
cmp -s "${EVIDENCE_ROOT}/inputs-before.sha256" "${EVIDENCE_ROOT}/inputs-after.sha256" || fail_integrity 'original input checksums changed'
printf 'finished_utc=%s\ncommand_failures=%s\nintegrity_failures=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "${command_failures}" "${integrity_failures}" \
    >"${EVIDENCE_ROOT}/result-status.txt" || fail_integrity 'cannot write result status'

if [ "${integrity_failures}" -ne 0 ] || [ "${command_failures}" -ne 0 ]; then
    exit 1
fi
exit 0
