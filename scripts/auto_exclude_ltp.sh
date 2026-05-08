#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)

ARCH=${ARCH:-rv64}
BLK_MODE=${BLK_MODE:-}
TIMEOUT_SEC=${TIMEOUT_SEC:-20}
CONF_FILE=${CONF_FILE:-"${REPO_ROOT}/os_test.conf"}
LOG_DIR=${LOG_DIR:-"${REPO_ROOT}/testresult/auto_ltp"}
MAX_ROUNDS=${MAX_ROUNDS:-200}
MASK_OVERRIDE=${MASK_OVERRIDE:-0x800}
TEMP_CONF="/tmp/auto_ltp_${ARCH}.conf"

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

log() {
    echo "[auto-ltp] $*"
}

die() {
    echo "[auto-ltp] ERROR: $*" >&2
    exit 1
}

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
        echo "mem"
    fi
}

resolve_run_target() {
    if [[ "${ARCH}" == "rv64" ]]; then
        echo "rv64-run"
    else
        echo "la64-run"
    fi
}

read_excludes() {
    local line
    line=$(grep -E '^ltp_exclude=' "${CONF_FILE}" | tail -n 1 || true)
    if [[ -z "${line}" ]]; then
        echo ""
        return
    fi
    echo "${line#ltp_exclude=}"
}

read_from() {
    local line
    line=$(grep -E '^ltp_from=' "${CONF_FILE}" | tail -n 1 || true)
    if [[ -z "${line}" ]]; then
        echo ""
        return
    fi
    echo "${line#ltp_from=}"
}

read_excludes_musl() {
    local line
    line=$(grep -E '^ltp_exclude_musl=' "${CONF_FILE}" | tail -n 1 || true)
    if [[ -z "${line}" ]]; then
        echo ""
        return
    fi
    echo "${line#ltp_exclude_musl=}"
}

read_excludes_glibc() {
    local line
    line=$(grep -E '^ltp_exclude_glibc=' "${CONF_FILE}" | tail -n 1 || true)
    if [[ -z "${line}" ]]; then
        echo ""
        return
    fi
    echo "${line#ltp_exclude_glibc=}"
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

append_exclude() {
    local list="$1"
    local item="$2"
    if [[ -z "${item}" ]]; then
        echo "${list}"
        return
    fi
    if [[ ",${list}," == *",${item},"* ]]; then
        echo "${list}"
        return
    fi
    if [[ -z "${list}" ]]; then
        echo "${item}"
    else
        echo "${list},${item}"
    fi
}

update_conf_exclude() {
    local list="$1"
    local tmp
    tmp=$(mktemp)
    if grep -qE '^ltp_exclude=' "${CONF_FILE}"; then
        awk -v v="${list}" 'BEGIN{done=0} {if ($0 ~ /^ltp_exclude=/) {print "ltp_exclude=" v; done=1} else {print}} END{if (!done) print "ltp_exclude=" v}' "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "" >> "${tmp}"
        echo "ltp_exclude=${list}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

update_conf_from() {
    local case="$1"
    local tmp
    tmp=$(mktemp)
    if [[ -z "${case}" ]]; then
        # 清空 ltp_from 行
        grep -vE '^ltp_from=' "${CONF_FILE}" > "${tmp}"
    elif grep -qE '^ltp_from=' "${CONF_FILE}"; then
        sed "s/^ltp_from=.*/ltp_from=${case}/" "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "" >> "${tmp}"
        echo "ltp_from=${case}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

update_conf_exclude_musl() {
    local list="$1"
    local tmp
    tmp=$(mktemp)
    if grep -qE '^ltp_exclude_musl=' "${CONF_FILE}"; then
        awk -v v="${list}" 'BEGIN{done=0} {if ($0 ~ /^ltp_exclude_musl=/) {print "ltp_exclude_musl=" v; done=1} else {print}} END{if (!done) print "ltp_exclude_musl=" v}' "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "" >> "${tmp}"
        echo "ltp_exclude_musl=${list}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

update_conf_exclude_glibc() {
    local list="$1"
    local tmp
    tmp=$(mktemp)
    if grep -qE '^ltp_exclude_glibc=' "${CONF_FILE}"; then
        awk -v v="${list}" 'BEGIN{done=0} {if ($0 ~ /^ltp_exclude_glibc=/) {print "ltp_exclude_glibc=" v; done=1} else {print}} END{if (!done) print "ltp_exclude_glibc=" v}' "${CONF_FILE}" > "${tmp}"
    else
        cat "${CONF_FILE}" > "${tmp}"
        echo "" >> "${tmp}"
        echo "ltp_exclude_glibc=${list}" >> "${tmp}"
    fi
    mv "${tmp}" "${CONF_FILE}"
}

write_temp_conf() {
    local list="$1"
    local from_case="$2"
    local musl_list
    local glibc_list
    musl_list=$(unique_list "$(read_excludes_musl)")
    glibc_list=$(unique_list "$(read_excludes_glibc)")
    awk -v mask="${MASK_OVERRIDE}" -v ltp="${list}" -v from="${from_case}" -v musl="${musl_list}" -v glibc="${glibc_list}" '
        BEGIN{mask_done=0; ltp_done=0; from_done=0; musl_done=0; glibc_done=0}
        {
            if ($0 ~ /^mask=/) {print "mask=" mask; mask_done=1; next}
            if ($0 ~ /^ltp_exclude_glibc=/) {print "ltp_exclude_glibc=" glibc; glibc_done=1; next}
            if ($0 ~ /^ltp_exclude_musl=/) {print "ltp_exclude_musl=" musl; musl_done=1; next}
            if ($0 ~ /^ltp_exclude=/) {print "ltp_exclude=" ltp; ltp_done=1; next}
            if ($0 ~ /^ltp_from=/) {print "ltp_from=" from; from_done=1; next}
            print
        }
        END{
            if (!mask_done) print "mask=" mask
            if (!ltp_done) print "ltp_exclude=" ltp
            if (musl != "" && !musl_done) print "ltp_exclude_musl=" musl
            if (glibc != "" && !glibc_done) print "ltp_exclude_glibc=" glibc
            if (from != "" && !from_done) print "ltp_from=" from
        }
    ' "${CONF_FILE}" > "${TEMP_CONF}"
}

count_excludes() {
    local list="$1"
    if [[ -z "${list}" ]]; then
        echo 0
        return
    fi
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

normalize_arch
BLK_MODE=$(resolve_blk_mode)
RUN_TARGET=$(resolve_run_target)
resolve_image_paths

[[ -f "${CONF_FILE}" ]] || die "CONF_FILE not found: ${CONF_FILE}"
mkdir -p "${LOG_DIR}"

excludes_csv=$(unique_list "$(read_excludes)")
if [[ -n "${excludes_csv}" ]]; then
    update_conf_exclude "${excludes_csv}"
fi

ltp_from="$(read_from || true)"

log "start arch=${ARCH} blk_mode=${BLK_MODE} timeout=${TIMEOUT_SEC}s"
log "conf=${CONF_FILE} temp_conf=${TEMP_CONF} log_dir=${LOG_DIR}"

if [[ -n "${ltp_from}" ]]; then
    log "ltp_from=${ltp_from} (will skip passed cases)"
fi

# 恢复原始镜像作为起点
restore_image

round=1
while [[ "${round}" -le "${MAX_ROUNDS}" ]]; do
    excludes_csv=$(unique_list "$(read_excludes)")
    write_temp_conf "${excludes_csv}" "${ltp_from}"
    if [[ -n "${ltp_from}" ]]; then
        log "round=${round} ltp_from=${ltp_from} excludes=$(count_excludes "${excludes_csv}")"
    else
        log "round=${round} excludes=$(count_excludes "${excludes_csv}")"
    fi
    make -C "${REPO_ROOT}/os" conf-inject CONF_ARCH="${ARCH}" CONF_BLK_MODE="${BLK_MODE}" CONF_FILE="${TEMP_CONF}"

    log_file="${LOG_DIR}/round_${round}.log"
    : > "${log_file}"

    # 用 tee 同时写入文件和显示到终端（默认显示）
    make -C "${REPO_ROOT}/os" "${RUN_TARGET}" 2>&1 | tee "${log_file}" &
    run_pid=$!

    current_case=""
    current_libc=""
    panic=0
    timed_out=0

    last_line=0
    last_activity=${SECONDS}
    SECONDS=0
    while kill -0 "${run_pid}" >/dev/null 2>&1; do
        sleep 0.1
        total_lines=$(wc -l < "${log_file}" 2>/dev/null | tr -d ' ' || echo 0)
        if [[ -z "${total_lines}" ]]; then total_lines=0; fi

        if (( total_lines > last_line )); then
            last_activity=${SECONDS}
            new_lines=$(tail -n +"$((last_line + 1))" "${log_file}" 2>/dev/null || true)
            last_line=${total_lines}
            while IFS= read -r line; do
                case "${line}" in
                    RUN\ LTP\ CASE\ *)
                        current_case="${line#RUN LTP CASE }"
                        ;;
                    *START\ ltp-musl*)
                        # 标记当前在 musl 轮
                        current_libc="musl"
                        ;;
                    *START\ ltp-glibc*)
                        # 标记当前在 glibc 轮
                        current_libc="glibc"
                        ;;
                esac
                if [[ "${line}" == *"panicked at"* ]]; then
                    panic=1
                    break 2
                fi
            done <<< "${new_lines}"
        fi

        if (( SECONDS - last_activity >= TIMEOUT_SEC )); then
            timed_out=1
            break
        fi
    done

    kill_run

    if (( panic || timed_out )); then
        if (( panic )); then
            log "panic detected, case=${current_case} libc=${current_libc}"
        else
            log "timeout detected (${TIMEOUT_SEC}s), case=${current_case} libc=${current_libc}"
        fi

        if [[ -n "${current_case}" ]]; then
            # 写入对应 libc 的专属排除列表
            if [[ "${current_libc}" == "musl" ]]; then
                mlist=$(unique_list "$(read_excludes_musl)")
                mlist=$(append_exclude "${mlist}" "${current_case}")
                update_conf_exclude_musl "${mlist}"
            elif [[ "${current_libc}" == "glibc" ]]; then
                glist=$(unique_list "$(read_excludes_glibc)")
                glist=$(append_exclude "${glist}" "${current_case}")
                update_conf_exclude_glibc "${glist}"
            else
                # libc 不确定，写入共用列表兜底
                excludes_csv=$(append_exclude "${excludes_csv}" "${current_case}")
                excludes_csv=$(unique_list "${excludes_csv}")
                update_conf_exclude "${excludes_csv}"
            fi
            # 下一轮从当前失败的 case 开始跑（跳过前面已通过的）
            ltp_from="${current_case}"
            update_conf_from "${ltp_from}"
            round=$((round + 1))
            continue
        fi

        # 没有抓到当前 case → 镜像可能损坏，恢复镜像再试
        log "no case captured, image may be corrupted, restoring..."
        restore_image
        # 不清除 ltp_from，保持断点继续
        round=$((round + 1))
        continue
    fi

    wait "${run_pid}" >/dev/null 2>&1 || true
    log "run finished without panic/timeout"
    break

done

# 全部通过后清除 ltp_from，避免影响后续测试
update_conf_from ""

log "done"