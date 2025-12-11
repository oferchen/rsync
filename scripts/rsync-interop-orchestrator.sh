#!/usr/bin/env bash
# rsync-interop-orchestrator.sh
# ---------------------------------------------------------------------------
# WHAT WAS DONE / WHY:
# - Single entrypoint for CI or local: start server, wait, run client.
# - getopts allows overriding server/client/run-root/timeout.
# - Cleans up background server on any exit.
# ---------------------------------------------------------------------------
set -euo pipefail

print_usage() {
  printf 'Usage: %s [-s server_script] [-c client_script] [-r run_root] [-t timeout_secs]\n' "$0"
  printf '  -s  Path to rsync-interop-server.sh\n'
  printf '  -c  Path to rsync-interop-client.sh\n'
  printf '  -r  Run root where server writes descriptors\n'
  printf '  -t  Seconds to wait for descriptors (default: 120)\n'
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
readonly repo_root

server_script_default="${repo_root}/scripts/rsync-interop-server.sh"
client_script_default="${repo_root}/scripts/rsync-interop-client.sh"
run_root_default="${repo_root}/target/interop/run"
timeout_default=120

server_script="${server_script_default}"
client_script="${client_script_default}"
run_root="${run_root_default}"
wait_timeout="${timeout_default}"

while getopts ":s:c:r:t:h" opt; do
  case "${opt}" in
    s) server_script=${OPTARG} ;;
    c) client_script=${OPTARG} ;;
    r) run_root=${OPTARG} ;;
    t) wait_timeout=${OPTARG} ;;
    h)
      print_usage
      exit 0
      ;;
    :)
      printf 'Option -%s requires an argument\n' "${OPTARG}" >&2
      print_usage
      exit 1
      ;;
    \?)
      printf 'Unknown option: -%s\n' "${OPTARG}" >&2
      print_usage
      exit 1
      ;;
  esac
done
shift $((OPTIND - 1))

server_pid=""

cleanup() {
  local exit_code=$?
  if [[ -n "${server_pid}" ]]; then
    kill "${server_pid}" >/dev/null 2>&1 || true
    wait "${server_pid}" >/dev/null 2>&1 || true
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

wait_for_run_descriptors() {
  local max_wait=$1
  local waited=0
  while (( waited < max_wait )); do
    if [[ -d "${run_root}" ]] && compgen -G "${run_root}"'/*/env' >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  return 1
}

main() {
  if [[ ! -x "${server_script}" ]]; then
    printf 'server script not found or not executable: %s\n' "${server_script}" >&2
    exit 1
  fi
  if [[ ! -x "${client_script}" ]]; then
    printf 'client script not found or not executable: %s\n' "${client_script}" >&2
    exit 1
  fi

  "${server_script}" &
  server_pid=$!
  printf 'started rsync interop server (pid=%s)\n' "${server_pid}"

  if ! wait_for_run_descriptors "${wait_timeout}"; then
    printf 'server did not produce run descriptors in %s seconds (run_root=%s)\n' "${wait_timeout}" "${run_root}" >&2
    exit 1
  fi

  if ! "${client_script}"; then
    printf 'client reported interop failures\n' >&2
    exit 1
  fi

  printf 'orchestrator: interop succeeded\n'
}

main "$@"

