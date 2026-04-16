#!/usr/bin/env python3
"""Pre-flight diagnostic for the validate-gold pipeline.

Runs in a few seconds and writes a JSON snapshot to ~/logs/diagnose-{ts}.json.
Use it as a sanity check before launching anything heavy, and after each crash
to compare baseline state.

Checks:
  - WSL: uptime, kernel, distro
  - Memory: free, swap, commit limit, vmmem from Windows side (if reachable)
  - Load: 1/5/15 min averages, runnable count
  - FDs: ulimit, currently open by current shell
  - Disk: /mnt/c write+read+delete latency, free space
  - GPU: nvidia-smi compute apps + free memory
  - cqs daemon: socket presence, simple search round-trip latency
  - Anthropic API: messages.create with 1-token max (small charge, ~$0.0001)

Skips anything that times out within 5 s — diagnostic must never hang.
"""

from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

LOG_DIR = Path(os.path.expanduser("~/logs"))
LOG_DIR.mkdir(parents=True, exist_ok=True)
SOCKET_PATH_HINT = Path("/run/user") / str(os.getuid()) if hasattr(os, "getuid") else None
TIMEOUT = 5.0


def _run(cmd: list[str], timeout: float = TIMEOUT) -> tuple[int, str, str]:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout.strip(), r.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", f"timeout after {timeout}s"
    except FileNotFoundError as e:
        return -2, "", f"not found: {e}"


def check_uptime() -> dict:
    rc, out, _ = _run(["uptime", "-s"], timeout=2)
    rc2, kver, _ = _run(["uname", "-r"], timeout=2)
    distro = ""
    try:
        for ln in Path("/etc/os-release").read_text().splitlines():
            if ln.startswith("PRETTY_NAME="):
                distro = ln.split("=", 1)[1].strip().strip('"')
                break
    except OSError:
        pass
    return {"booted_at": out if rc == 0 else None, "kernel": kver, "distro": distro}


def check_memory() -> dict:
    out = {}
    try:
        for ln in Path("/proc/meminfo").read_text().splitlines():
            if ":" in ln:
                k, v = ln.split(":", 1)
                v = v.strip()
                # values like "94204020 kB" — convert to bytes
                if v.endswith(" kB"):
                    try:
                        out[k.strip()] = int(v[:-3]) * 1024
                    except ValueError:
                        out[k.strip()] = v
                else:
                    out[k.strip()] = v
    except OSError as e:
        return {"error": str(e)}
    keep = ["MemTotal", "MemFree", "MemAvailable", "Buffers", "Cached",
            "SwapTotal", "SwapFree", "CommitLimit", "Committed_AS"]
    return {k: out.get(k) for k in keep}


def check_load() -> dict:
    try:
        ln = Path("/proc/loadavg").read_text().split()
        return {"1m": float(ln[0]), "5m": float(ln[1]), "15m": float(ln[2]),
                "runnable": ln[3], "last_pid": int(ln[4])}
    except OSError as e:
        return {"error": str(e)}


def check_fds() -> dict:
    rc, out, _ = _run(["bash", "-c", "ulimit -n"], timeout=2)
    soft = int(out) if rc == 0 and out.isdigit() else None
    open_now = None
    try:
        open_now = len(list(Path(f"/proc/{os.getpid()}/fd").iterdir()))
    except OSError:
        pass
    return {"soft_limit": soft, "open_now_self": open_now}


def check_mnt_c_io() -> dict:
    target = Path("/mnt/c/Projects/cqs/.cqs")
    out: dict = {"path": str(target), "exists": target.exists()}
    if not target.exists():
        return out
    rc, free_str, _ = _run(["df", "-h", str(target)], timeout=3)
    if rc == 0:
        lines = free_str.splitlines()
        if len(lines) > 1:
            out["df"] = lines[1].split()
    # Round-trip latency: write a small file, read it back, delete.
    try:
        t0 = time.monotonic()
        with tempfile.NamedTemporaryFile(
            dir=str(target), prefix=".diag_", suffix=".tmp", delete=False
        ) as f:
            f.write(b"diagnostic " * 1024)  # ~11 KB
            tmp_path = Path(f.name)
        write_dt = time.monotonic() - t0
        t0 = time.monotonic()
        _ = tmp_path.read_bytes()
        read_dt = time.monotonic() - t0
        t0 = time.monotonic()
        tmp_path.unlink()
        del_dt = time.monotonic() - t0
        out["write_ms"] = round(write_dt * 1000, 1)
        out["read_ms"] = round(read_dt * 1000, 1)
        out["delete_ms"] = round(del_dt * 1000, 1)
    except OSError as e:
        out["io_error"] = str(e)
    return out


def check_gpu() -> dict:
    rc, out, err = _run([
        "nvidia-smi",
        "--query-gpu=index,name,memory.used,memory.free,memory.total,utilization.gpu",
        "--format=csv,noheader,nounits",
    ], timeout=3)
    if rc != 0:
        return {"error": err or out, "rc": rc}
    rows = []
    for ln in out.splitlines():
        parts = [p.strip() for p in ln.split(",")]
        if len(parts) >= 6:
            rows.append({
                "idx": int(parts[0]),
                "name": parts[1],
                "mem_used_mib": int(parts[2]),
                "mem_free_mib": int(parts[3]),
                "mem_total_mib": int(parts[4]),
                "util_gpu_pct": int(parts[5]),
            })
    return {"gpus": rows}


def check_cqs_daemon() -> dict:
    out: dict = {}
    if SOCKET_PATH_HINT and SOCKET_PATH_HINT.exists():
        socks = [s for s in SOCKET_PATH_HINT.iterdir() if s.name.startswith("cqs-")]
        out["sockets"] = [str(s) for s in socks]
    rc, out_text, err = _run(["systemctl", "--user", "is-active", "cqs-watch"], timeout=2)
    out["systemd_active"] = out_text or err
    # Quick search round-trip; should be ~10-50 ms via daemon, ~2 s if cold.
    t0 = time.monotonic()
    rc, _, err = _run(["cqs", "ApiError", "--json", "--limit", "1"], timeout=10)
    out["search_ok"] = (rc == 0)
    out["search_ms"] = round((time.monotonic() - t0) * 1000, 1)
    if rc != 0:
        out["search_stderr_head"] = err[:200]
    return out


async def check_anthropic_api() -> dict:
    """Tiny ping to the API. Costs ~$0.0001."""
    if not os.environ.get("ANTHROPIC_API_KEY"):
        return {"error": "ANTHROPIC_API_KEY not set"}
    try:
        import anthropic
    except ImportError as e:
        return {"error": f"anthropic SDK missing: {e}"}
    try:
        client = anthropic.AsyncAnthropic()
        t0 = time.monotonic()
        resp = await asyncio.wait_for(
            client.messages.create(
                model="claude-haiku-4-5",
                max_tokens=1,
                messages=[{"role": "user", "content": "ok"}],
            ),
            timeout=15,
        )
        await client.close()
        return {
            "ok": True,
            "latency_ms": round((time.monotonic() - t0) * 1000, 1),
            "model": "claude-haiku-4-5",
            "input_tokens": resp.usage.input_tokens,
            "output_tokens": resp.usage.output_tokens,
        }
    except Exception as e:  # noqa: BLE001
        return {"error": f"{type(e).__name__}: {e}"}


def check_vmmem_windows() -> dict:
    """Get vmmem (WSL2 host process) memory from the Windows side."""
    rc, out, err = _run(
        ["powershell.exe", "-NoProfile", "-Command",
         "Get-Process vmmem | Select-Object Id,@{n='WS_GB';e={[math]::Round($_.WorkingSet64/1GB,2)}},@{n='Pri_GB';e={[math]::Round($_.PrivateMemorySize64/1GB,2)}} | ConvertTo-Json -Compress"],
        timeout=5,
    )
    if rc != 0:
        return {"error": err[:200] or out[:200], "rc": rc}
    try:
        return {"vmmem": json.loads(out)}
    except json.JSONDecodeError:
        return {"raw": out[:500]}


async def main() -> int:
    ts = int(time.time())
    out_path = LOG_DIR / f"diagnose-{ts}.json"

    snap: dict = {"ts": ts, "host_clock_iso": time.strftime("%Y-%m-%dT%H:%M:%S")}
    snap["uptime"] = check_uptime()
    snap["memory"] = check_memory()
    snap["load"] = check_load()
    snap["fds"] = check_fds()
    snap["mnt_c_io"] = check_mnt_c_io()
    snap["gpu"] = check_gpu()
    snap["vmmem_windows"] = check_vmmem_windows()
    snap["cqs_daemon"] = check_cqs_daemon()
    snap["anthropic_api"] = await check_anthropic_api()

    out_path.write_text(json.dumps(snap, indent=2))

    # Compact stdout summary so callers can eyeball the result.
    print(f"diagnose written to {out_path}")
    print(f"  load 1m={snap['load'].get('1m'):.2f} 5m={snap['load'].get('5m'):.2f} 15m={snap['load'].get('15m'):.2f}")
    mem = snap["memory"]
    if mem.get("MemAvailable") and mem.get("MemTotal"):
        print(f"  mem avail={mem['MemAvailable']/2**30:.1f} GB / total={mem['MemTotal']/2**30:.1f} GB")
    if snap["mnt_c_io"].get("write_ms") is not None:
        print(f"  /mnt/c io: write={snap['mnt_c_io']['write_ms']} ms read={snap['mnt_c_io']['read_ms']} ms")
    elif snap["mnt_c_io"].get("io_error"):
        print(f"  /mnt/c io ERROR: {snap['mnt_c_io']['io_error']}")
    cqs = snap["cqs_daemon"]
    print(f"  cqs daemon active={cqs.get('systemd_active')} search_ok={cqs.get('search_ok')} latency={cqs.get('search_ms')} ms")
    api = snap["anthropic_api"]
    if api.get("ok"):
        print(f"  anthropic API ok latency={api.get('latency_ms')} ms")
    else:
        print(f"  anthropic API ERROR: {api.get('error')}")
    vm = snap["vmmem_windows"].get("vmmem")
    if isinstance(vm, dict):
        print(f"  vmmem WS={vm.get('WS_GB')} GB Pri={vm.get('Pri_GB')} GB pid={vm.get('Id')}")

    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
