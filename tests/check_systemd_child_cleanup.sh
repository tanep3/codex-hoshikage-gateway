#!/usr/bin/env bash
set -euo pipefail

unit="codex-hoshikage-child-probe-$$"
scratch="$(mktemp -d -t codex-hoshikage-child-probe.XXXXXX)"
cleanup() {
  systemctl --user stop "$unit.service" >/dev/null 2>&1 || true
  unlink "$scratch/pids" 2>/dev/null || true
  rmdir "$scratch" 2>/dev/null || true
}
trap cleanup EXIT

systemd-run --user --collect --unit="$unit" \
  --property=KillMode=control-group --property=Restart=no \
  /usr/bin/python3 "$(realpath "$(dirname "$0")/fixtures/owned_child.py")" "$scratch/pids" >/dev/null
for _ in $(seq 1 100); do
  [[ -s "$scratch/pids" ]] && break
  sleep 0.1
done
[[ -s "$scratch/pids" ]]
read -r parent child < "$scratch/pids"
kill -0 "$parent"
kill -0 "$child"
systemctl --user kill --kill-whom=main --signal=SIGKILL "$unit.service"
for _ in $(seq 1 100); do
  if ! kill -0 "$parent" 2>/dev/null && ! kill -0 "$child" 2>/dev/null; then
    echo "PASS: main and child PIDs were both reaped after main SIGKILL"
    exit 0
  fi
  sleep 0.1
done
echo "FAIL: an old PID remains after main SIGKILL" >&2
exit 1
