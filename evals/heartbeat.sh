#!/usr/bin/env bash
# Heartbeat logger for diagnosing WSL crashes during long jobs.
#
# Writes one JSON line every INTERVAL seconds to ~/logs/heartbeat.log.
# Lives on the Linux home filesystem (NOT /mnt/c) so it survives /mnt/c
# 9P stalls. Survives the parent's death because it's a detached shell.
#
# Usage: nohup bash evals/heartbeat.sh >/dev/null 2>&1 &
# Stop:  pkill -f 'heartbeat.sh'
#
# Each line includes a Windows-side vmmem snapshot once every WIN_INTERVAL
# ticks (PowerShell call is slow so we don't do it every tick).

set -u
LOG="${HEARTBEAT_LOG:-$HOME/logs/heartbeat.log}"
INTERVAL="${HEARTBEAT_INTERVAL:-2}"
WIN_INTERVAL="${HEARTBEAT_WIN_INTERVAL:-15}"  # vmmem check every 30s by default

mkdir -p "$(dirname "$LOG")"

# Mark a session boundary so multiple runs are easy to separate.
echo "{\"event\":\"start\",\"ts\":$(date +%s),\"pid\":$$,\"interval_s\":$INTERVAL}" >> "$LOG"

tick=0
while true; do
  ts=$(date +%s)
  load_line=$(awk '{print $1, $2, $3}' /proc/loadavg)
  read -r load1 load5 load15 <<< "$load_line"
  mem_avail_kb=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
  mem_used_kb=$(awk '/^MemTotal:/ {tot=$2} /^MemAvailable:/ {avail=$2} END {print tot-avail}' /proc/meminfo)
  swap_used_kb=$(awk '/^SwapTotal:/ {tot=$2} /^SwapFree:/ {free=$2} END {print tot-free}' /proc/meminfo)
  n_proc=$(ps -e --no-headers | wc -l)
  cqs_count=$(pgrep -f '/cqs' 2>/dev/null | wc -l)
  py_count=$(pgrep -f 'python' 2>/dev/null | wc -l)
  # FD count is an approximation: count entries under /proc/{pid}/fd for our session leader's tree.
  # Use the simpler "files open system-wide" from /proc/sys/fs/file-nr (allocated, free, max).
  read -r fs_alloc fs_free fs_max < /proc/sys/fs/file-nr 2>/dev/null || { fs_alloc=0; fs_free=0; fs_max=0; }

  # Optional Windows-side vmmem check (slow — only every WIN_INTERVAL ticks).
  vmmem_ws_gb="null"
  if (( tick % WIN_INTERVAL == 0 )); then
    raw=$(timeout 4 powershell.exe -NoProfile -Command \
      "Get-Process vmmem 2>\$null | Select-Object -ExpandProperty WorkingSet64" 2>/dev/null \
      | tr -d '\r' | head -1)
    if [[ "$raw" =~ ^[0-9]+$ ]]; then
      vmmem_ws_gb=$(awk -v b="$raw" 'BEGIN { printf "%.2f", b/1073741824 }')
    fi
  fi

  printf '{"ts":%d,"load1":%s,"load5":%s,"load15":%s,"mem_used_kb":%d,"mem_avail_kb":%d,"swap_used_kb":%d,"n_proc":%d,"cqs":%d,"py":%d,"fd_alloc":%s,"fd_max":%s,"vmmem_ws_gb":%s}\n' \
    "$ts" "$load1" "$load5" "$load15" "${mem_used_kb:-0}" "${mem_avail_kb:-0}" "${swap_used_kb:-0}" \
    "$n_proc" "$cqs_count" "$py_count" "${fs_alloc:-0}" "${fs_max:-0}" "$vmmem_ws_gb" >> "$LOG"

  tick=$((tick + 1))
  sleep "$INTERVAL"
done
