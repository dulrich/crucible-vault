#!/usr/bin/env bash
# Hermetic test of scripts/lib/cde.sh with a stub `crucible`. Never touches a real CDE socket.
set -euo pipefail

HERE=$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/run/cde" "$T/home/.local/bin"

# Stub: logs "<CDE_SOCKET> <args>"; reply/exit come from "$CDE_SOCKET.reply" ("<rc> <text>").
cat >"$T/stub" <<'STUB'
#!/usr/bin/env bash
echo "$CDE_SOCKET $*" >>"$STUB_LOG"
f="$CDE_SOCKET.reply"
[ -f "$f" ] || { echo "err no such socket"; exit 1; }
read -r rc text <"$f"
echo "$text"
exit "$rc"
STUB
chmod +x "$T/stub"
cp "$T/stub" "$T/bin/crucible"
export PATH=$T/bin:$PATH
export STUB_LOG=$T/log
: >"$STUB_LOG"

# shellcheck source=../lib/cde.sh
. "$HERE/../lib/cde.sh"

fails=0
check() { # name, expected, actual
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: expected '$2' got '$3'"; fails=$((fails + 1)); fi
}

export XDG_RUNTIME_DIR=$T/run
export HOME=$T/home
unset CDE_SOCKET
A=$T/run/cde/a.sock B=$T/run/cde/b.sock
touch "$A" "$B"

# no socket answers
got=$(cde_socket) && rc=0 || rc=$?
check "no live socket returns 1" "1" "$rc"
check "no live socket prints nothing" "" "$got"

# stale first, live second
echo "0 ok 1" >"$B.reply"
got=$(cde_socket) && rc=0 || rc=$?
check "stale socket skipped" "$B" "$got"
check "stale skip rc" "0" "$rc"

# CDE_SOCKET wins when it answers
C=$T/run/c.sock
echo "0 ok 1" >"$C.reply"
got=$(CDE_SOCKET=$C cde_socket)
check "CDE_SOCKET wins" "$C" "$got"

# CDE_SOCKET not answering falls through to the scan
got=$(CDE_SOCKET=$T/run/dead.sock cde_socket)
check "dead CDE_SOCKET falls back to scan" "$B" "$got"

# cde_msg passes through reply and exit code of an err
echo "1 err session locked" >"$A.reply"
out=$(cde_msg "$A" close-pid 42) && rc=0 || rc=$?
check "cde_msg err reply" "err session locked" "$out"
check "cde_msg err rc" "1" "$rc"
out=$(cde_msg "$B" run /x/y) && rc=0 || rc=$?
check "cde_msg ok reply" "ok 1" "$out"
check "cde_msg ok rc" "0" "$rc"
check "cde_msg targets socket and args" "1" "$(grep -c "^$B msg run /x/y\$\|^$B run /x/y\$" "$STUB_LOG")"

# cde_bin: PATH first, then ~/.local/bin, else 1
check "cde_bin from PATH" "$T/bin/crucible" "$(PATH=$T/bin:$PATH cde_bin)"
rm "$T/bin/crucible"
cp "$T/stub" "$T/home/.local/bin/crucible"
got=$(PATH=/usr/bin:/bin cde_bin)
check "cde_bin from ~/.local/bin" "$T/home/.local/bin/crucible" "$got"
rm "$T/home/.local/bin/crucible"
got=$(PATH=/usr/bin:/bin cde_bin) && rc=0 || rc=$?
check "cde_bin absent returns 1" "1" "$rc"

[ "$fails" = 0 ] || { echo "$fails failure(s)"; exit 1; }
echo "all ok"
