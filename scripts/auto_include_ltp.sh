#!/usr/bin/env bash
# auto_include_ltp.sh — 多轮自动收集有 TPASS 的 LTP 测例
#
# 与 auto_exclude_ltp.sh 对称，区别在于：
#   - 同时产出 ltp_include（有 TPASS 且无 panic）和 ltp_exclude_*（panic/超时）
#   - 多轮跑，每轮从上次失败点继续，直到全部跑完或达到最大轮次
#
# 用法：
#   bash scripts/auto_include_ltp.sh
#
# 环境变量：
#   ARCH            — rv64（默认）| la64
#   TIMEOUT_SEC          — 无输出超时秒数（默认 15）
#   HARD_TIMEOUT_SEC     — 单测例硬超时秒数（默认 30），超时即强杀
#   HARD_ROUND_TIMEOUT_SEC — 整轮硬超时秒数（默认 120），不管有没有输出
#   CONF_FILE       — os_test.conf 路径
#   LOG_DIR         — 日志输出目录
#   MAX_ROUNDS      — 最大轮次（默认 200）

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)

ARCH=${ARCH:-rv64}
BLK_MODE=${BLK_MODE:-}
TIMEOUT_SEC=${TIMEOUT_SEC:-15}
HARD_TIMEOUT_SEC=${HARD_TIMEOUT_SEC:-30}
HARD_ROUND_TIMEOUT_SEC=${HARD_ROUND_TIMEOUT_SEC:-120}
CONF_FILE=${CONF_FILE:-"${REPO_ROOT}/os_test.conf"}
LOG_DIR=${LOG_DIR:-"${REPO_ROOT}/testresult/auto_ltp"}
MAX_ROUNDS=${MAX_ROUNDS:-200}
MASK_OVERRIDE=${MASK_OVERRIDE:-0x800}
TEMP_CONF="/tmp/auto_include_${ARCH}.conf"

# ---------- 镜像路径 ----------
resolve_image_paths() {
    if [[ "${ARCH}" == "rv64" ]]; then
        IMG_FILE="${REPO_ROOT}/sdcard-rv.img"
        IMG_BACKUP="${REPO_ROOT}/fs-img-dir/sdcard-rv.img.xz"
    else
        IMG_FILE="${REPO_ROOT}/sdcard-la.img"
        IMG_BACKUP="${REPO_ROOT}/fs-img-dir/sdcard-la.img.xz"
    fi
}

restore_image() {
    if [[ ! -f "${IMG_BACKUP}" ]]; then
        log "ERROR: backup image not found: ${IMG_BACKUP}, cannot restore"
        return 1
    fi
    log "restoring ${IMG_FILE} from ${IMG_BACKUP} ..."
    xz -dkc "${IMG_BACKUP}" > "${IMG_FILE}"
    log "restore done"
}

run_pid=""

log() { echo "[auto-include] $*"; }
die() { echo "[auto-include] ERROR: $*" >&2; exit 1; }

normalize_arch() {
    case "${ARCH}" in
        rv|rv64) ARCH="rv64" ;;
        la|la64) ARCH="la64" ;;
        *) die "unsupported ARCH='${ARCH}', expected rv64 or la64" ;;
    esac
}

resolve_blk_mode() {
    if [[ -n "${BLK_MODE}" ]]; then
        echo "${BLK_MODE}"
        return
    fi
    if [[ "${ARCH}" == "rv64" ]]; then
        echo "virt"
    else
        echo "virt-pci"
    fi
}

resolve_run_target() {
    if [[ "${ARCH}" == "rv64" ]]; then
        echo "rv64-run"
    else
        echo "la64-run"
    fi
}

# ---------- 读/写 os_test.conf ----------
read_conf() {
    local key="$1"
    grep -E "^${key}=" "${CONF_FILE}" | tail -n 1 | cut -d= -f2- || echo ""
}

unique_list() {
    local raw="$1"
    declare -A seen=()
    local out=()
    local part
    IFS=',' read -ra parts <<< "${raw}"
    for part in "${parts[@]}"; do
        part="${part#"${part%%[![:space:]]*}"}"
        part="${part%"${part##*[![:space:]]}"}"
        [[ -z "${part}" ]] && continue
        if [[ -z "${seen[${part}]+x}" ]]; then
            seen["${part}"]=1
            out+=("${part}")
        fi
    done
    local IFS=','
    echo "${out[*]}"
}

append_item() {
    local list="$1"
    local item="$2"
    if [[ -z "${item}" ]]; then echo "${list}"; return; fi
    if [[ ",${list}," == *",${item},"* ]]; then echo "${list}"; return; fi
    if [[ -z "${list}" ]]; then echo "${item}"; else echo "${list},${item}"; fi
}

update_conf() {
    local key="$1"
    local val="$2"
    local tmp
    tmp=$(mktemp)
    if grep -qE "^${key}=" "${CONF_FILE}"; then
        awk -v k="${key}" -v v="${val}" 'BEGIN{done=0} {if ($0 ~ "^" k "=") {print k "=" v; done=1} else {print}} END{if (!done) print k "=" v}' "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "${key}=${val}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

update_ltp_from() {
    local case="$1"
    local tmp
    tmp=$(mktemp)
    if [[ -z "${case}" ]]; then
        grep -vE '^ltp_from=' "${CONF_FILE}" > "${tmp}"
    elif grep -qE '^ltp_from=' "${CONF_FILE}"; then
        sed "s/^ltp_from=.*/ltp_from=${case}/" "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "ltp_from=${case}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

# 写入临时配置文件（用于注入到镜像）
write_temp_conf() {
    local include="$1"
    local exclude="$2"
    local exclude_musl="$3"
    local exclude_glibc="$4"
    local from_case="$5"
    awk -v mask="${MASK_OVERRIDE}" \
        -v incl="${include}" \
        -v excl="${exclude}" \
        -v musl="${exclude_musl}" \
        -v glibc="${exclude_glibc}" \
        -v from="${from_case}" \
        -v libc="musl" '
        BEGIN{md=0; ii=0; ee=0; mm=0; gg=0; ff=0; ll=0}
        {
            if ($0 ~ /^mask=/)            {print "mask=" mask;       md=1; next}
            if ($0 ~ /^ltp_include=/)     {print "ltp_include=" incl; ii=1; next}
            if ($0 ~ /^ltp_exclude=/)     {print "ltp_exclude=" excl; ee=1; next}
            if ($0 ~ /^ltp_exclude_musl=/) {print "ltp_exclude_musl=" musl; mm=1; next}
            if ($0 ~ /^ltp_exclude_glibc=/) {print "ltp_exclude_glibc=" glibc; gg=1; next}
            if ($0 ~ /^ltp_from=/)        {print "ltp_from=" from;   ff=1; next}
            if ($0 ~ /^ltp_libc=/)        {print "ltp_libc=" libc;   ll=1; next}
            print
        }
        END{
            if (!md) print "mask=" mask
            if (!ii) print "ltp_include=" incl
            if (!ee) print "ltp_exclude=" excl
            if (!mm) print "ltp_exclude_musl=" musl
            if (!gg) print "ltp_exclude_glibc=" glibc
            if (!ff) print "ltp_from=" from
            if (!ll) print "ltp_libc=" libc
        }
    ' "${CONF_FILE}" > "${TEMP_CONF}"
}

count_items() {
    local list="$1"
    if [[ -z "${list}" ]]; then echo 0; return; fi
    IFS=',' read -ra parts <<< "${list}"
    echo "${#parts[@]}"
}

kill_run() {
    if [[ "${ARCH}" == "rv64" ]]; then
        pkill -f "qemu-system-riscv64" >/dev/null 2>&1 || true
    else
        pkill -f "qemu-system-loongarch64" >/dev/null 2>&1 || true
    fi
    if [[ -n "${run_pid}" ]]; then
        kill "${run_pid}" >/dev/null 2>&1 || true
        wait "${run_pid}" >/dev/null 2>&1 || true
        run_pid=""
    fi
}

trap 'kill_run; log "interrupted"; exit 130' INT TERM

# ============ 主流程 ============

normalize_arch
BLK_MODE=$(resolve_blk_mode)
RUN_TARGET=$(resolve_run_target)
resolve_image_paths

[[ -f "${CONF_FILE}" ]] || die "CONF_FILE not found: ${CONF_FILE}"
mkdir -p "${LOG_DIR}"

# 收集现有的 include/exclude 做增量
include_accum=$(unique_list "$(read_conf "ltp_include")")
exclude_accum=$(unique_list "$(read_conf "ltp_exclude")")
exclude_musl_accum=$(unique_list "$(read_conf "ltp_exclude_musl")")
exclude_glibc_accum=$(unique_list "$(read_conf "ltp_exclude_glibc")")

# 从现有的 ltp_from 开始（断点续跑）
ltp_from=$(read_conf "ltp_from" || true)

log "start arch=${ARCH} blk_mode=${BLK_MODE} timeout=${TIMEOUT_SEC}s"
log "include_accum=$(count_items "${include_accum}") so far"
log "exclude_accum=$(count_items "${exclude_accum}") so far"
if [[ -n "${ltp_from}" ]]; then
    log "ltp_from=${ltp_from} (will skip passed cases)"
fi

# 恢复原始镜像作为起点
restore_image

round=1
while [[ "${round}" -le "${MAX_ROUNDS}" ]]; do
    # 写入临时 conf
    write_temp_conf "" "${exclude_accum}" "${exclude_musl_accum}" "${exclude_glibc_accum}" "${ltp_from}"

    if [[ -n "${ltp_from}" ]]; then
        log "round=${round} ltp_from=${ltp_from} include_accum=$(count_items "${include_accum}") exclude=$(count_items "${exclude_accum}")"
    else
        log "round=${round} include_accum=$(count_items "${include_accum}") exclude=$(count_items "${exclude_accum}")"
    fi

    make -C "${REPO_ROOT}/os" conf-inject CONF_ARCH="${ARCH}" CONF_BLK_MODE="${BLK_MODE}" CONF_FILE="${TEMP_CONF}"

    log_file="${LOG_DIR}/include_round_${round}.log"
    : > "${log_file}"

    # 启动 QEMU
    make -C "${REPO_ROOT}/os" "${RUN_TARGET}" 2>&1 | tee "${log_file}" &
    run_pid=$!

    current_case=""
    current_libc=""
    panic=0
    timed_out=0
    # 本轮收集的 include 候选
    round_include=""
    case_has_tpass=false
    case_ran=false

    last_line=0
    last_activity=$(date +%s)
    case_start_time=0
    round_start=$(date +%s)
    while kill -0 "${run_pid}" >/dev/null 2>&1; do
        sleep 0.1
        total_lines=$(wc -l < "${log_file}" 2>/dev/null | tr -d ' ' || echo 0)
        if [[ -z "${total_lines}" ]]; then total_lines=0; fi

        if (( total_lines > last_line )); then
            last_activity=$(date +%s)
            new_lines=$(tail -n +"$((last_line + 1))" "${log_file}" 2>/dev/null || true)
            last_line=${total_lines}
            while IFS= read -r line; do
                case "${line}" in
                    RUN\ LTP\ CASE\ *)
                        # 上一个测例：如果跑过且有 TPASS，加入 include 候选
                        if [[ -n "${current_case}" ]] && ${case_ran} && ${case_has_tpass}; then
                            round_include=$(append_item "${round_include}" "${current_case}")
                            log "  include candidate: ${current_case}"
                        fi
                        current_case="${line#RUN LTP CASE }"
                        case_has_tpass=false
                        case_ran=true  # 跑到 RUN LTP CASE 说明确实被执行了
                        case_start_time=$(date +%s)
                        ;;
                    *START\ ltp-musl*)
                        current_libc="musl"
                        ;;
                    *START\ ltp-glibc*)
                        current_libc="glibc"
                        ;;
                    *TPASS*)
                        case_has_tpass=true
                        ;;
                    *panicked\ at*|*HEAP\ ALLOCATION\ FAILED*)
                        panic=1
                        log "PANIC detected, case=${current_case}"
                        break 2
                        ;;
                esac
            done <<< "${new_lines}"
        fi

        # 硬超时：单个测例跑超过 HARD_TIMEOUT_SEC 秒就强杀
        _now=$(date +%s)
        if (( case_start_time > 0 && _now - case_start_time >= HARD_TIMEOUT_SEC )); then
            timed_out=1
            log "hard timeout (${HARD_TIMEOUT_SEC}s) for case=${current_case}"
            break
        fi

        # 总轮次硬超时：整轮跑超过 HARD_ROUND_TIMEOUT_SEC 秒就强杀
        if (( _now - round_start >= HARD_ROUND_TIMEOUT_SEC )); then
            timed_out=1
            log "hard round timeout (${HARD_ROUND_TIMEOUT_SEC}s), case=${current_case}"
            break
        fi

        if (( _now - last_activity >= TIMEOUT_SEC )); then
            timed_out=1
            break
        fi
    done

    kill_run

    # ---- 补读 kill_run 后可能遗漏的日志行（tee 退出前刷盘） ----
    # 注意：必须用 here-string 而非管道，否则变量赋值在子 shell 中丢失
    {
        total_lines=$(wc -l < "${log_file}" 2>/dev/null | tr -d ' ' || echo 0)
        if [[ -n "${total_lines}" ]] && (( total_lines > last_line )); then
            remaining=$(tail -n +"$((last_line + 1))" "${log_file}" 2>/dev/null || true)
            while IFS= read -r line; do
                case "${line}" in
                    *RUN\ LTP\ CASE\ *)
                        current_case="${line#*RUN LTP CASE }"
                        case_ran=true
                        case_has_tpass=false
                        ;;
                    *TPASS*)  case_has_tpass=true ;;
                    *panicked\ at*|*HEAP\ ALLOCATION\ FAILED*) panic=1 ;;  # 补读区域不 break（已 kill），仅标记
                esac
            done <<< "${remaining}"
            last_line=${total_lines}
        fi
    } || true

    # ---- 如果 current_case 仍为空，从日志中捞最后一个 RUN LTP CASE ----
    if [[ -z "${current_case}" ]]; then
        current_case=$(grep -oP 'RUN LTP CASE \K.*' "${log_file}" 2>/dev/null | tail -n 1 || echo "")
        if [[ -n "${current_case}" ]]; then
            case_ran=true
            log "recovered current_case from log: ${current_case}"
        fi
    fi

    # ---- 处理本轮最后一个测例 ----
    if [[ -n "${current_case}" ]] && ${case_ran} && ${case_has_tpass}; then
        round_include=$(append_item "${round_include}" "${current_case}")
        log "  include candidate (last): ${current_case}"
    fi

    # ---- 合并本轮 include 到累积列表 ----
    if [[ -n "${round_include}" ]]; then
        include_accum=$(unique_list "${include_accum},${round_include}")
        log "round_include=$(count_items "${round_include}") new items"
    fi

    # ---- 处理 panic/超时 ----
    if (( panic || timed_out )); then
        if (( panic )); then
            log "panic detected, case=${current_case} libc=${current_libc}"
        else
            log "timeout detected (${TIMEOUT_SEC}s), case=${current_case} libc=${current_libc}"
        fi

        if [[ -n "${current_case}" ]]; then
            exclude_musl_accum=$(append_item "${exclude_musl_accum}" "${current_case}")
            exclude_musl_accum=$(unique_list "${exclude_musl_accum}")
            update_conf "ltp_exclude_musl" "${exclude_musl_accum}"
            log "excluded (musl): ${current_case}"

            ltp_from="${current_case}"
            update_ltp_from "${ltp_from}"
            # 将更新后的 os_test.conf 注入镜像
            make -C "${REPO_ROOT}/os" conf-inject CONF_ARCH="${ARCH}" CONF_BLK_MODE="${BLK_MODE}" CONF_FILE="${CONF_FILE}"
            round=$((round + 1))
            continue
        fi

        log "no RUN LTP CASE line found in log, cannot determine current case"
        log "image may be corrupted or log was empty — restoring image and retrying"
        restore_image
        round=$((round + 1))
        continue
    fi

    # ---- 整轮跑完无 panic/超时 ----
    log "round ${round} finished without panic/timeout"
    break
done

# ---- 全部完成：写回 include 列表 ----
include_accum=$(unique_list "${include_accum}")
update_conf "ltp_include" "${include_accum}"
update_ltp_from ""

# 将最终 os_test.conf 注入镜像
make -C "${REPO_ROOT}/os" conf-inject CONF_ARCH="${ARCH}" CONF_BLK_MODE="${BLK_MODE}" CONF_FILE="${CONF_FILE}"

log "===== Final Results ====="
log "ltp_include (${include_accum//[!,]}) = ${include_accum}"
log ""
log "done — ${count_items "${include_accum}"} cases in include list"
log "      ${count_items "${exclude_musl_accum}"} cases in musl exclude list"
log "      $(count_items "${exclude_accum}") cases in global exclude list"
log ""
log "os_test.conf updated"

rm -f "${TEMP_CONF}"
