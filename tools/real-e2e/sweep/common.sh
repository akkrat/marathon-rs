# shellcheck shell=bash
# Shared by the WP 7.5 closure sweep scripts. Sourced, never executed; the callers run `set -euo pipefail`.

sweep_die() { echo "sweep: $*" >&2; exit 1; }

sweep_need_abs() {   # sweep_need_abs NAME VALUE
  case "$2" in /*) ;; *) sweep_die "$1 must be an absolute path (got '$2')" ;; esac
}

# Integer part of the one-minute load average. MRS_LOAD_CMD is the test seam.
sweep_load() {
  if [ -n "${MRS_LOAD_CMD:-}" ]; then "$MRS_LOAD_CMD"; return; fi
  case "$(uname -s)" in
    Darwin) uptime | awk -F'load averages?: ' '{print $2}' | awk '{gsub(",", ".", $1); print int($1)}' ;;
    *) cut -d' ' -f1 /proc/loadavg | cut -d. -f1 ;;
  esac
}

# A stray `yes` process of this user: status 0 = present, 1 = none, 2 = the query itself failed (not 0/1, e.g. pgrep missing).
sweep_yes_running() {
  local rc=0 cmd=${MRS_PGREP:-pgrep}
  # bash 3.2 (macOS) reports a missing command as status 1 under `set -e`, which would read as "no match".
  command -v "$cmd" > /dev/null 2>&1 || { echo "sweep: $cmd not found; refusing to continue" >&2; return 2; }
  if "$cmd" -u "$(id -u)" -x yes > /dev/null 2>&1; then rc=0; else rc=$?; fi
  case $rc in
    0) return 0 ;;
    1) return 1 ;;
    *) echo "sweep: the process query (pgrep) failed with status $rc; refusing to continue" >&2; return 2 ;;
  esac
}

# Succeeds only when no stray `yes` runs and the load is below the limit within the tries. Returns 3 when the machine
# never settles and 5 when a query fails (a query error is never "no match"). The caller must stop on a non-zero status
# (`sweep_guard || exit $?`).
sweep_guard() {
  local max=${MRS_GUARD_MAX_LOAD:-10} tries=${MRS_GUARD_TRIES:-60} pause=${MRS_GUARD_PAUSE:-20} i load yes_rc
  for ((i = 0; i < tries; i++)); do
    "${MRS_PKILL:-pkill}" -u "$(id -u)" -x yes 2>/dev/null || true
    load=$(sweep_load) || { echo "sweep: the load query failed; refusing to continue" >&2; return 5; }
    case $load in '' | *[!0-9]*) echo "sweep: unreadable load average '$load'; refusing to continue" >&2; return 5 ;; esac
    yes_rc=0; sweep_yes_running || yes_rc=$?
    [ "$yes_rc" -le 1 ] || return 5
    if [ "$load" -lt "$max" ] && [ "$yes_rc" -eq 1 ]; then
      return 0
    fi
    sleep "$pause"
  done
  echo "sweep: load average stayed at ${load:-?} (limit $max) or a stray 'yes' persists; not capturing" >&2
  return 3
}

# Anything a finished capture must not leave behind. Echoes it and fails when something remains or cannot be inspected.
sweep_leftovers() {
  local found=0 path processes
  for path in ${MRS_LEFTOVER_PATHS:-/tmp/marathon /tmp/marathon-real-e2e.lock}; do
    if [ -e "$path" ]; then echo "leftover: $path" >&2; found=1; fi
  done
  if [ "${MRS_UNAME:-$(uname -s)}" = Linux ]; then
    # The process inventory is taken on its own, so that a failing ps is an error and not an empty inventory.
    command -v "${MRS_PS:-ps}" > /dev/null 2>&1 || { echo "sweep: ps not found; cannot prove the worker is empty" >&2; return 5; }
    if ! processes=$("${MRS_PS:-ps}" -eo pid,args=); then
      echo "sweep: the process inventory (ps) failed; cannot prove the worker is empty" >&2
      return 5
    fi
    if grep -E 'adb|emulator|qemu' <<< "$processes" | grep -v -E 'grep|sweep' >&2; then found=1; fi
  fi
  return $found
}

# sweep_capture HOST ARGS...: guard, the capture-pair command, then the leftover check. Returns the first failure.
sweep_capture() {
  local host=$1; shift
  sweep_guard || return $?
  local rc=0
  "${MRS_PY:-python3}" -I "$MRS_RUN_TOOL" capture-pair --host "$host" "$@" --approved || rc=$?
  sweep_leftovers || { [ "$rc" -ne 0 ] || rc=4; }
  return "$rc"
}

# sweep_reject_appledouble LISTING WHAT: fails when a tar listing (one entry per line) has a member, or a directory, named `._*` (AppleDouble).
sweep_reject_appledouble() {
  local entry
  while IFS= read -r entry; do
    case "/$entry" in */._*) sweep_die "$2 holds the AppleDouble entry '$entry' (build it with COPYFILE_DISABLE=1)" ;; esac
  done <<< "$1"
}

sweep_sha256() {   # sweep_sha256 FILE: the hex digest, on macOS and Linux
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}
