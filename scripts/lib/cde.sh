# shellcheck shell=bash
# cde.sh — helpers to talk to a running Crucible Dev Env (CDE) compositor through `crucible msg`.
# Sourced by install.sh; defines functions only (no side effects at source time). Every call is
# guarded so a failed probe never aborts a caller running under `set -euo pipefail`.
#
# cde_bin               print the crucible binary (PATH, else ~/.local/bin/crucible); 1 if absent
# cde_socket            print the first live CDE socket ($CDE_SOCKET, then $XDG_RUNTIME_DIR/cde/*.sock); 1 if none
# cde_msg <sock> <args> run `crucible msg <args>` against <sock>; prints the reply, returns its exit code

cde_bin() {
  local b
  if b=$(command -v crucible 2>/dev/null) && [ -n "$b" ]; then
    printf '%s\n' "$b"
    return 0
  fi
  if [ -x "${HOME:-}/.local/bin/crucible" ]; then
    printf '%s\n' "$HOME/.local/bin/crucible"
    return 0
  fi
  return 1
}

cde_socket() {
  local bin s
  bin=$(cde_bin) || return 1
  if [ -n "${CDE_SOCKET:-}" ]; then
    if CDE_SOCKET=$CDE_SOCKET "$bin" msg candidates >/dev/null 2>&1 9>&-; then
      printf '%s\n' "$CDE_SOCKET"
      return 0
    fi
  fi
  for s in "${XDG_RUNTIME_DIR:-/nonexistent}"/cde/*.sock; do
    [ -e "$s" ] || continue
    if CDE_SOCKET=$s "$bin" msg candidates >/dev/null 2>&1 9>&-; then
      printf '%s\n' "$s"
      return 0
    fi
  done
  return 1
}

cde_msg() {
  local sock=$1 bin rc=0
  shift
  bin=$(cde_bin) || return 1
  CDE_SOCKET=$sock "$bin" msg "$@" 9>&- 2>&1 || rc=$?
  return "$rc"
}
