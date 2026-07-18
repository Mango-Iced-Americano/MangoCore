#!/bin/bash
# split_worklog.sh — Split docs/Work_Log.md into per-date files under docs/Work_Log/
#
# Usage: ./split_worklog.sh
#   - Reads docs/Work_Log.md
#   - Splits on "## YYYY-MM-DD" headers
#   - Writes to docs/Work_Log/YYYY-MM-DD.md (appends if file exists for same date)
#   - Creates docs/Work_Log.md as index
#   - Moves original to docs/Work_Log/legacy/Work_Log_legacy.md
#
# Each migrated entry gets a stable anchor: <date>-legacy-<seq>

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
SRC="$ROOT/docs/Work_Log.md"
DEST_DIR="$ROOT/docs/Work_Log"
LEGACY_DIR="$DEST_DIR/legacy"
TEMPLATE_FILE="$DEST_DIR/_TEMPLATE.md"
INDEX="$ROOT/docs/Work_Log.md"

if [ ! -f "$SRC" ]; then
    echo "ERROR: $SRC not found"
    exit 1
fi

mkdir -p "$DEST_DIR" "$LEGACY_DIR"

# ── Step 1: Extract all date blocks ──
# Pattern: lines starting with "## YYYY-MM-DD" or "## YYYY-MM-DD: ..."
# Each block runs until the next "## YYYY-MM-DD" or EOF

echo "[split] parsing $SRC ..."

awk '
/^## [12][0-9][0-9][0-9]-[01][0-9]-[0-3][0-9]/ {
    if (block != "" && date != "") {
        print date ":::" block > "/tmp/wl_block_" date ".txt"
    }
    date = substr($0, 4, 10)   # "YYYY-MM-DD"
    gsub(/[^0-9-]/, "", date)  # strip ": " suffix from "YYYY-MM-DD: title"
    block = $0 "\n"
    next
}
{
    if (date != "") {
        block = block $0 "\n"
    }
}
END {
    if (block != "" && date != "") {
        print date ":::" block > "/tmp/wl_block_" date ".txt"
    }
}
' "$SRC"

# ── Step 2: Write per-date files ──
LEGACY_SEQ=0
for blockfile in /tmp/wl_block_*.txt; do
    [ -f "$blockfile" ] || continue
    date=$(basename "$blockfile" .txt | sed 's/wl_block_//')
    dest="$DEST_DIR/$date.md"
    
    LEGACY_SEQ=$((LEGACY_SEQ + 1))
    anchor="${date}-legacy-$(printf '%03d' $LEGACY_SEQ)"
    
    # Extract content after "date:::"
    content=$(awk 'NR>1 || /^[0-9-]+:::/ { sub(/^[0-9-]+:::/, ""); print }' "$blockfile")
    
    {
        echo "---"
        echo "date: $date"
        echo "schema: legacy"
        echo "---"
        echo ""
        echo "# Work Log — $date (imported)"
        echo ""
        echo "<a id=\"$anchor\"></a>"
        echo ""
        echo "$content"
    } >> "$dest"
    
    echo "[split] $date → $dest (anchor=$anchor)"
    rm -f "$blockfile"
done

# ── Step 3: Create template for new entries ──
cat > "$TEMPLATE_FILE" << 'TEMPLATE'
---
date: YYYY-MM-DD
timezone: Asia/Shanghai
schema: work-log-v2
---

# Work Log — YYYY-MM-DD

<a id="YYYY-MM-DD-HHMM-short-slug"></a>
## HH:MM — Short title

**Date:** YYYY-MM-DD HH:MM +08:00
**Author:** AI:<agent/model/session> | Human:<name>
**Tags:** [area:fs, type:bugfix]
**Mango Gate:** skill loaded=yes; references=harness-patterns.md#section

**Summary:** One-sentence description.

**Files:**
- `path/to/file.rs` — what changed

**Verification:**
- `make rv64-kernel-build-only` — ✅
- `make la64-kernel-build-only` — ✅
- QEMU/LTP — ⏭ not run: (reason)

**Notes:** (required; write `无` if none)

**Related:** (optional links to issues, plans, previous anchors)
TEMPLATE

# ── Step 4: Move original to legacy ──
LEGACY_DEST="$LEGACY_DIR/Work_Log_legacy_$(date +%Y%m%d-%H%M%S).md"
mv "$SRC" "$LEGACY_DEST"
echo "[split] original moved to $LEGACY_DEST"

# ── Step 5: Create index ──
cat > "$INDEX" << 'INDEX'
# Work Log — Index

本项目的工作日志已拆分为按日期的独立文件，存放在 `docs/Work_Log/` 目录下。

## 文件命名规则

- `YYYY-MM-DD.md` — 当天的工作日志
- 新条目追加到对应日期文件的**顶部**
- 模板：`_TEMPLATE.md`

## 条目格式

每条记录包含：日期、时间、作者、标签、Mango Gate 状态、摘要、涉及文件、验证结果、备注。详见 `_TEMPLATE.md`。

## 历史记录

（旧版 Work_Log.md 已迁移到 `legacy/` 目录）

## 按日期索引

INDEX

# Add date links to index
for f in $(ls "$DEST_DIR"/*.md 2>/dev/null | grep -v _TEMPLATE | sort -r); do
    date=$(basename "$f" .md)
    echo "- [$date]($date.md)" >> "$INDEX"
done

echo ""
echo "[split] done. Index: $INDEX, Template: $TEMPLATE_FILE"
echo "[split] run 'git add docs/Work_Log/ && git rm docs/Work_Log.md && git add docs/Work_Log.md' to update git"
