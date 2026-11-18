"""build_id.py — fleet build-id helper (canonical copy-in template).

Self-contained copy-in template, stdlib only. Build id = last commit under the tool
root, ignoring control-plane paths (BUILD_ID_EXCLUDES, relative to the tool root), plus
`-dirty` when a tracked non-excluded file is modified; `unknown` without git metadata or
when no commit matches.

    from build_id import handle_build_flags
    if "--help" in sys.argv[1:]: ...usage...; sys.exit(0)        # --help wins
    rc = handle_build_flags(sys.argv[1:], __file__, name="tool", semver="1.2.3")
    if rc is not None: sys.exit(rc)
"""

import os
import subprocess
import sys

# Pathspecs every build-id git call uses, relative to the tool root (same literal text in every language).
BUILD_ID_EXCLUDES = (".", ":(exclude)plans", ":(exclude)references", ":(exclude,glob)**/FEEDBACK.md", ":(exclude,glob)**/INITIATIVE.md")


def _git(cwd, args):
    try:
        r = subprocess.run(["git", "--no-optional-locks", *args], cwd=cwd, stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    except OSError:
        return None
    return r.stdout if r.returncode == 0 else None


def build_id(path):
    """Tool-scoped build id of `path`: `<40-hex>[-dirty]`, or `unknown` without git metadata."""
    real = os.path.realpath(path)
    if not os.path.exists(real):
        return "unknown"
    d = real if os.path.isdir(real) else os.path.dirname(real)
    top = (_git(d, ["rev-parse", "--show-toplevel"]) or "").strip()
    if not top:
        return "unknown"
    tool = top
    while d == top or d.startswith(top + "/"):
        if os.path.lexists(os.path.join(d, "AGENTS.md")):
            tool = d
            break
        if d == top:
            break
        d = os.path.dirname(d)
    sha = (_git(top, ["-C", tool, "log", "-1", "--format=%H", "--", *BUILD_ID_EXCLUDES]) or "").strip()
    if not sha:
        return "unknown"
    st = _git(top, ["-C", tool, "status", "--porcelain", "--untracked-files=no", "--", *BUILD_ID_EXCLUDES])
    return f"{sha}-dirty" if st and st.strip() else sha


def handle_build_flags(argv, path, name=None, semver=None):
    """Answer a bare `--build` / `--version`; return the exit code, or None when neither is present."""
    b, v = "--build" in argv, "--version" in argv
    if not b and not v:
        return None
    bid = build_id(path)
    sys.stdout.write(f"{name} {semver} ({bid})\n" if v and not b and semver else f"{bid}\n")
    return 0
