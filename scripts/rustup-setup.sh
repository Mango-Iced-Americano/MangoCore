#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
manifest="$script_dir/../rust-toolchain.toml"

if [ -z "${RUSTUP_HOME:-}" ]; then
    echo "RUSTUP_HOME must be set and non-empty; setup is explicit and does not use HOME fallback" >&2
    exit 1
fi

# 可选：cargo git 依赖（libgit2 只读 gitconfig 文件，不读环境变量）通过
# ~/.gitconfig 的 url.insteadOf 走代理；与 make prepare-cargo-config 的
# GIT_SUBMODULE_PROXY 共用同一变量。默认不设置，保持直连。
if [ -n "${GIT_SUBMODULE_PROXY:-}" ]; then
    mkdir -p "$HOME"
    if ! git config --global --get "url.${GIT_SUBMODULE_PROXY}.insteadOf" >/dev/null 2>&1; then
        git config --global "url.${GIT_SUBMODULE_PROXY}.insteadOf" "https://github.com/"
    fi
fi

if sh "$script_dir/rustup-preflight.sh" >/dev/null 2>&1; then
    exit 0
fi

if [ ! -r "$manifest" ]; then
    echo "$manifest: missing or unreadable rust-toolchain.toml; setup is explicit" >&2
    exit 1
fi

manifest_values=$(awk '
function trim(value) {
    sub(/^[[:space:]]+/, "", value)
    sub(/[[:space:]]+$/, "", value)
    return value
}
function fail(field) {
    printf "%s: malformed or missing %s; setup is explicit\n", FILENAME, field > "/dev/stderr"
    exit 1
}
function parse_scalar(line, field, value) {
    value = line
    sub(/^[^=]*=[[:space:]]*/, "", value)
    if (value !~ /^"[^"]*"[[:space:]]*$/) {
        fail(field)
    }
    sub(/^"/, "", value)
    sub(/"[[:space:]]*$/, "", value)
    if (value == "") {
        fail(field)
    }
    return value
}
function parse_array(line, field, value, inner, count, item, i) {
    value = line
    sub(/^[^=]*=[[:space:]]*/, "", value)
    if (value !~ /^\[[^]]*\][[:space:]]*$/) {
        fail(field)
    }
    sub(/^\[/, "", value)
    sub(/\][[:space:]]*$/, "", value)
    if (trim(value) == "") {
        fail(field)
    }
    count = split(value, inner, ",")
    for (i = 1; i <= count; i++) {
        item = trim(inner[i])
        if (item !~ /^"[^"]*"$/) {
            fail(field)
        }
        sub(/^"/, "", item)
        sub(/"$/, "", item)
        if (item == "") {
            fail(field)
        }
        printf "%s\t%s\n", field, item
    }
}
{
    line = $0
    sub(/[[:space:]]*#.*/, "", line)
    line = trim(line)
    if (line == "") {
        next
    }
    if (line == "[toolchain]") {
        if (section) {
            fail("toolchain section")
        }
        section = 1
        next
    }
    if (!section) {
        fail("toolchain section")
    }
    if (line ~ /^[[:space:]]*channel[[:space:]]*=/) {
        if (channel_seen++) {
            fail("channel")
        }
        printf "channel\t%s\n", parse_scalar(line, "channel")
    } else if (line ~ /^[[:space:]]*targets[[:space:]]*=/) {
        if (targets_seen++) {
            fail("targets")
        }
        parse_array(line, "target")
        targets_present = 1
    } else if (line ~ /^[[:space:]]*components[[:space:]]*=/) {
        if (components_seen++) {
            fail("components")
        }
        parse_array(line, "component")
        components_present = 1
    } else {
        fail("toolchain fields")
    }
}
END {
    if (!section) fail("toolchain section")
    if (!channel_seen) fail("channel")
    if (!targets_seen || !targets_present) fail("targets")
    if (!components_seen || !components_present) fail("components")
}
' "$manifest")

toolchain=""
targets=""
components=""
while IFS="$(printf '\t')" read -r kind value; do
    case "$kind" in
        channel) toolchain=$value ;;
        target) targets="$targets${targets:+ }$value" ;;
        component) components="$components${components:+ }$value" ;;
    esac
done <<EOF
$manifest_values
EOF

set --
for component in $components; do
    set -- "$@" --component "$component"
done
for target in $targets; do
    set -- "$@" --target "$target"
done

# 默认从 rsproxy.cn 镜像下载 pinned nightly（实测容器内直连 ~5MB/s，
# USTC 对容器网络限速至 ~160B/s；评测机可覆盖为其它镜像，无需修改本文件）。
: "${RUSTUP_DIST_SERVER:=https://rsproxy.cn}"
: "${RUSTUP_UPDATE_ROOT:=https://rsproxy.cn/rustup}"
export RUSTUP_DIST_SERVER RUSTUP_UPDATE_ROOT

# 全新 HOME 下 CARGO_HOME 没有 rustup 本体时，toolchain install 收尾会失败；
# 从 PATH 自举复制一份，保证任意全新环境可直接冷启动。
if [ ! -x "${CARGO_HOME:-}/bin/rustup" ] && command -v rustup >/dev/null 2>&1; then
    mkdir -p "${CARGO_HOME}/bin"
    cp "$(command -v rustup)" "${CARGO_HOME}/bin/rustup"
fi

rustup toolchain install "$toolchain" --profile minimal "$@"

exec sh "$script_dir/rustup-preflight.sh"
