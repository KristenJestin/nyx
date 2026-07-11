#!/usr/bin/env bash
# =============================================================================
# nyx v2 — RE-PLAYABLE DEMO SEED (dogfooding tool)
# =============================================================================
# Populates a nyx v2 database with a RICH, coherent demo dataset so every feature
# can be exercised for real: projects, workspaces, auto-detected repos, specs in
# every lifecycle state (draft/ready/in_progress/in_review/completed) with phases,
# revisions and varied tasks (slice/human/review × todo/in_progress/done/blocked/
# cancelled, with dependencies), a full review (global/line/range comments, human
# AND agent authors, threads, open/resolved/OUTDATED + RE-MAPPED anchors, promoted
# tasks in proposed/structured/validated), managed commands and terminals — over
# dedicated demo git repos AND the REAL grafted nyx-v2 working tree (self-review).
#
# All writes go through the shared nyx_core authorities (repository/db/review_comment)
# via the companion Rust example `crates/nyx-core/examples/seed_demo.rs` — NO raw SQL.
#
# -----------------------------------------------------------------------------
# USAGE
#   scripts/seed-demo.sh [--data-dir DIR] [--no-reset] [--no-backup]
#
#   --data-dir DIR   nyx v2 data dir whose  DIR/nyx.db  is seeded.
#                    Default: ~/.config/nyx-v2  (v2 identity default, see
#                    apps/desktop/src/main/identity.ts). NOTE: the installed
#                    nyx-v2.desktop launcher overrides this with
#                    NYX_DATA_DIR=~/.local/share/nyx-v2-specs — to seed the DB
#                    THAT launcher opens, pass --data-dir ~/.local/share/nyx-v2-specs
#                    (with the v2 app CLOSED, or the run is refused).
#   --no-reset       Append to the existing DB instead of starting clean.
#   --no-backup      Skip the DB backup (NOT recommended).
#
# The v2 app MUST be closed for the DB it currently holds; the script refuses to
# write to a locked DB. It NEVER touches nyx v1 (~/.config/@nyx/electron).
# =============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${HOME}/.config/nyx-v2"
DO_RESET=1
DO_BACKUP=1
DEMO_ROOT="${HOME}/nyx-demo"
DEMO_ROOT2="${HOME}/nyx-demo-feature"
NYX_V2_REPO="${REPO_ROOT}"
BACKUP_SUFFIX=".bak-seed-demo"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --no-reset) DO_RESET=0; shift ;;
    --no-backup) DO_BACKUP=0; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

DB="${DATA_DIR}/nyx.db"
say() { printf '\033[1;36m[seed]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[seed:ERROR]\033[0m %s\n' "$*" >&2; exit 1; }

# --- Safety rails ------------------------------------------------------------
case "$DATA_DIR" in
  *"@nyx/electron"*) die "refusing to target the nyx v1 data dir (${DATA_DIR})." ;;
esac

# STOP if the target DB is held open by a running process (a live v2 app).
if [[ -e "$DB" ]] && command -v fuser >/dev/null 2>&1; then
  if fuser "$DB" >/dev/null 2>&1; then
    die "the target DB is LOCKED by a running process: ${DB}
     Close the nyx v2 app that holds this data dir and re-run, or target a
     different --data-dir. (Detected holders: $(fuser "$DB" 2>&1 | tr '\n' ' '))"
  fi
fi
if [[ -f "${DB}-wal" ]]; then
  say "warning: a -wal file exists next to ${DB} (an app may have it open). Continuing."
fi

# ===== STEP 0 — backup ========================================================
mkdir -p "$DATA_DIR"
if [[ "$DO_BACKUP" == "1" ]]; then
  for suffix in "" "-wal" "-shm"; do
    src="${DB}${suffix}"
    [[ -f "$src" ]] || continue
    dst="${src}${BACKUP_SUFFIX}"
    # cp -n: keep the FIRST (pristine) backup across re-runs.
    cp -n "$src" "$dst" && say "backup: $dst" || say "backup kept (exists): $dst"
  done
  # Also snapshot the launcher's DB (nyx-v2-specs) if present, for completeness.
  ALT="${HOME}/.local/share/nyx-v2-specs/nyx.db"
  [[ -f "$ALT" ]] && { cp -n "$ALT" "${ALT}${BACKUP_SUFFIX}" 2>/dev/null && say "backup: ${ALT}${BACKUP_SUFFIX}" || true; }
else
  say "backup skipped (--no-backup)"
fi

# ===== STEP 1 — demo git repos ================================================
git_c() { git -C "$1" "${@:2}"; }
init_repo() {
  local d="$1"; mkdir -p "$d"; git -C "$d" init -q
  git -C "$d" config user.email "demo@nyx.local"
  git -C "$d" config user.name "nyx demo"
  git -C "$d" config commit.gpgsign false
  git -C "$d" config gc.auto 0
}
wf() { local d="$1" rel="$2"; mkdir -p "$(dirname "$d/$rel")"; printf '%s' "$3" > "$d/$rel"; }

seed_api() {
  local d="$1"; init_repo "$d"
  local f l body
  for f in $(seq 0 11); do
    body=""
    for l in $(seq 0 59); do body+="fn base_${f}_${l}() { let x = ${l}; return x + ${f}; }"$'\n'; done
    wf "$d" "src/mod_${f}.rs" "$body"
  done
  wf "$d" "src/anchor.rs" $'fn anchor_demo() {\n    old_stub();\n}\n'
  git_c "$d" add -A; git_c "$d" commit -q -m base; git_c "$d" branch -M base
  git_c "$d" checkout -q -b feature
  for f in $(seq 0 11); do
    body=""
    for l in $(seq 0 59); do body+="fn feature_${f}_${l}() { let y = ${l} * 2; return y - ${f}; }"$'\n'; done
    wf "$d" "src/mod_${f}.rs" "$body"
  done
  # feature anchor.rs: KEEP on line 4, GONE on line 5 (see seed_demo.rs).
  wf "$d" "src/anchor.rs" $'fn anchor_demo() {\n    setup();\n    prepare(items);\n    let result = compute_total(items);\n    legacy_side_effect();\n    finish();\n}\n'
  git_c "$d" add -A; git_c "$d" commit -q -m "feature: rewrite modules"
  # A dirty working tree so the LIVING diff is non-empty.
  wf "$d" "src/worktree_change.rs" $'fn uncommitted() { return 42; }\n'
}

advance_api() {
  local d="$1"
  # Insert two lines above KEEP (shift it down) and DELETE the GONE line → re-map + outdated.
  wf "$d" "src/anchor.rs" $'fn anchor_demo() {\n    setup();\n    validate(items);\n    normalize(items);\n    prepare(items);\n    let result = compute_total(items);\n    finish();\n}\n'
  git_c "$d" add -A; git_c "$d" commit -q -m "advance: shift anchor + drop legacy"
}

seed_web() {
  local d="$1"; init_repo "$d"
  wf "$d" "index.js" $'const a = 1;\nconst b = 2;\nconsole.log(a + b);\n'
  git_c "$d" add -A; git_c "$d" commit -q -m base; git_c "$d" branch -M base
  git_c "$d" checkout -q -b feature
  wf "$d" "index.js" $'const a = 10;\nconst b = 20;\nconst c = 30;\nconsole.log(a + b + c);\n'
  git_c "$d" add -A; git_c "$d" commit -q -m "feature: web change"
}

say "building demo git repos under ${DEMO_ROOT} (+ ${DEMO_ROOT2})"
rm -rf "$DEMO_ROOT" "$DEMO_ROOT2"
mkdir -p "$DEMO_ROOT"
seed_api "${DEMO_ROOT}/api"
seed_web "${DEMO_ROOT}/web"
cp -r "$DEMO_ROOT" "$DEMO_ROOT2"   # a 2nd multi-repo worktree (feature workspace)

# ===== STEP 2 — reset target DB (clean demo) ==================================
if [[ "$DO_RESET" == "1" ]]; then
  say "reset: starting from a fresh DB (old one is backed up)"
  rm -f "$DB" "${DB}-wal" "${DB}-shm"
fi

# ===== STEP 3 — build + run the seed (setup → advance → remap) ================
say "building seed binary (cargo build -p nyx-core --example seed_demo)"
( cd "$REPO_ROOT" && cargo build -q -p nyx-core --example seed_demo )
BIN="${REPO_ROOT}/target/debug/examples/seed_demo"
[[ -x "$BIN" ]] || die "seed binary not found at $BIN"

export NYX_SEED_DEMO_ROOT="$DEMO_ROOT"
export NYX_SEED_DEMO_ROOT2="$DEMO_ROOT2"
export NYX_SEED_NYX_V2="$NYX_V2_REPO"

say "seeding DB: $DB (setup)"
"$BIN" "$DB" setup

say "advancing the api repo (shift anchor + drop legacy line)"
advance_api "${DEMO_ROOT}/api"

say "re-mapping anchored comments against the advanced api HEAD (remap)"
"$BIN" "$DB" remap

# ===== STEP 4 — verify =======================================================
say "verification (row counts):"
if command -v sqlite3 >/dev/null 2>&1; then
  for t in projects workspaces repositories specs spec_revisions tasks task_dependencies \
           review_comments review_tasks managed_commands terminals domain_events; do
    n=$(sqlite3 "$DB" "SELECT count(*) FROM $t;" 2>/dev/null || echo "n/a")
    printf '   %-20s %s\n' "$t" "$n"
  done
  echo "   --- review comment anchor states (api) ---"
  sqlite3 -header -column "$DB" \
    "SELECT substr(id,1,8) id, coalesce(file_path,'(global)') file, author_type, status, outdated, line_start
     FROM review_comments ORDER BY created_at;" 2>/dev/null || true
  echo "   --- review tasks (grill) ---"
  sqlite3 -header -column "$DB" \
    "SELECT substr(id,1,8) id, grill_state, coalesce(severity,'-') sev, title FROM review_tasks ORDER BY created_at;" 2>/dev/null || true
else
  say "sqlite3 not found — skipping row-count verification"
fi

say "DONE. Seeded ${DB}."
say "To SEE it: launch nyx v2 pointed at ${DATA_DIR}."
