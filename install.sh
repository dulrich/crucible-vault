#!/usr/bin/env bash
# crucible-vault user installer: build + install `cv` and `cvault-editor`, close the running
# editor (gracefully), stop the daemon, apply pending migrations, restart, relaunch the editor.
#
# Re-runnable: builds are incremental, a restart is harmless, and each migration in
# deploy/migrations/ is applied once — its applied phases are recorded in
# ${XDG_STATE_HOME:-~/.local/state}/crucible-vault/install.applied and skipped afterwards.
# Contract for migrations: deploy/migrations/README.md.
set -euo pipefail

REPO=$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)
UNIT=crucible-vault.service
STATE_DIR=${XDG_STATE_HOME:-$HOME/.local/state}/crucible-vault
APPLIED=$STATE_DIR/install.applied
MIGRATIONS=$REPO/deploy/migrations
export REPO
export VAULT=${CV_VAULT:-~/vault}
export CONFIG_DIR=${CV_CONFIG_DIR:-$VAULT/_custom/crucible-vault}
export STATE_DB=${CV_STATE_DB:-$STATE_DIR/state.db}

skip_build=0
touch_editor=1
list_only=0

usage() {
  cat <<'EOF'
usage: ./install.sh [--list] [--skip-build] [--no-editor]

  --list        show migrations and whether each phase is applied, then exit
  --skip-build  do not run `cargo install` (restart + migrations only)
  --no-editor   leave cvault-editor alone (not closed, not relaunched)
EOF
}

for a in "$@"; do
  case $a in
    --list) list_only=1 ;;
    --skip-build) skip_build=1 ;;
    --no-editor) touch_editor=0 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

mkdir -p "$STATE_DIR"
touch "$APPLIED"
exec 9>"$STATE_DIR/install.lock"
flock -n 9 || die "another install.sh is running"

# --- migrations ----------------------------------------------------------------------------

migration_files() {
  [[ -d $MIGRATIONS ]] || return 0
  find "$MIGRATIONS" -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.sh' | sort
}

is_applied() { grep -qxF -- "$1" <(cut -d' ' -f1 "$APPLIED"); }

# Prints the phases (`stopped`, `running`) a migration file defines.
phases_of() {
  ( # shellcheck source=/dev/null
    source "$1"
    for p in stopped running; do
      declare -F "migrate_$p" >/dev/null && echo "$p"
    done )
}

# Backs up a file into this run's backup dir (migrations call it before editing).
backup() {
  local f=$1
  [[ -e $f ]] || return 0
  mkdir -p "$BACKUP_DIR"
  cp -p -- "$f" "$BACKUP_DIR/$(basename "$f")"
  echo "    backup: $BACKUP_DIR/$(basename "$f")"
}
export -f backup

run_phase() {
  local phase=$1 f id key ran=0
  while IFS= read -r f; do
    id=$(basename "$f" .sh)
    # Captured, not piped into `grep -q` (pipefail + an early grep exit = a false miss).
    [[ " $(phases_of "$f" | tr '\n' ' ') " == *" $phase "* ]] || continue
    key="$id:$phase"
    is_applied "$key" && continue
    say "migration $key"
    export BACKUP_DIR=$STATE_DIR/install-backups/$id
    if ! ( set -euo pipefail
           # shellcheck source=/dev/null
           source "$f"
           "migrate_$phase" ); then
      die "migration $key failed — nothing recorded; fix the cause and re-run ./install.sh"
    fi
    printf '%s %s %s\n' "$key" "$(date -Is)" "$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo -)" >>"$APPLIED"
    ran=1
  done < <(migration_files)
  [[ $ran = 1 ]] || echo "    (no pending $phase-phase migrations)"
}

if [[ $list_only = 1 ]]; then
  while IFS= read -r f; do
    id=$(basename "$f" .sh)
    for p in $(phases_of "$f"); do
      if is_applied "$id:$p"; then echo "applied  $id:$p"; else echo "pending  $id:$p"; fi
    done
  done < <(migration_files)
  exit 0
fi

# --- build ---------------------------------------------------------------------------------

if [[ $skip_build = 0 ]]; then
  command -v cargo >/dev/null || die "cargo not found on PATH"
  if [[ -n $(git -C "$REPO" status --porcelain -- crates Cargo.toml Cargo.lock 2>/dev/null) ]]; then
    warn "uncommitted changes under crates/ are being built"
  fi
  say "building $(git -C "$REPO" log -1 --format='%h %s' 2>/dev/null || echo "$REPO")"
  cargo install --locked --path "$REPO/crates/cv"
  cargo install --locked --path "$REPO/crates/editor"
fi

# --- close the editor ----------------------------------------------------------------------

editor_was_running=0
if [[ $touch_editor = 1 ]] && pgrep -x cvault-editor >/dev/null; then
  editor_was_running=1
  say "closing cvault-editor"
  # A window close request is the editor's Quit: dirty buffers flush first, and the
  # unsaved-edits dialog appears if a save fails. Never killed.
  if [[ -n ${DISPLAY:-} ]] && command -v wmctrl >/dev/null; then
    for pid in $(pgrep -x cvault-editor); do
      wmctrl -lp | awk -v p="$pid" '$3 == p { print $1 }' | while read -r w; do
        wmctrl -i -c "$w" || true
      done
    done
  else
    echo "    please quit cvault-editor yourself (no DISPLAY/wmctrl to ask it)"
  fi
  waited=0
  while pgrep -x cvault-editor >/dev/null; do
    if (( waited % 15 == 0 && waited > 0 )); then
      echo "    still waiting for cvault-editor to quit — answer its dialog if one is open (Ctrl-C aborts; nothing has been changed yet)"
    fi
    sleep 1
    waited=$((waited + 1))
  done
fi

# --- daemon stopped: migrations ------------------------------------------------------------

say "stopping $UNIT"
systemctl --user stop "$UNIT"
systemctl --user daemon-reload
run_phase stopped

# --- daemon running ------------------------------------------------------------------------

say "starting $UNIT"
systemctl --user start "$UNIT"
for _ in $(seq 1 60); do
  cv status 2>/dev/null | grep -qx 'daemon: running' && break
  sleep 1
done
cv status | grep -qx 'daemon: running' || die "daemon did not come up — journalctl --user -u $UNIT"
run_phase running

status=$(cv status)
echo "$status"
config_ok=1
if ! grep -qx 'config errors: none' <<<"$status"; then
  config_ok=0
  warn "config errors reported above — migration backups are under $STATE_DIR/install-backups/"
fi

# --- relaunch the editor -------------------------------------------------------------------

if [[ $editor_was_running = 1 ]]; then
  say "relaunching cvault-editor"
  setsid -f cvault-editor </dev/null >/dev/null 2>&1
fi

[[ $config_ok = 1 ]] || exit 1
say "done"
