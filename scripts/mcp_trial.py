#!/usr/bin/env python3
"""Live-daemon MCP trial harness for cqs (companion to tests/mcp_trial_test.rs).

Unlike the hermetic mock-daemon integration test, this driver exercises the
DEPLOYED `cqs mcp` bridge against a REAL running `cqs watch --serve` daemon, so
realistic arguments resolve real symbol names in the served index. It is a
manual / on-demand trial (not CI): it needs a live daemon and a warm index.

  usage:  python3 scripts/mcp_trial.py [bridge_cwd]

`bridge_cwd` (default: the git toplevel of the current directory) is the working
directory the `cqs mcp` child is spawned in; the bridge resolves the served
`.cqs` from it and relays to that project's daemon. Run it from the repo (or a
registered worktree of it) so the bridge connects to the project's daemon.

READ-ONLY against the served project: the sweep and overlay/error/large/
concurrency sections never mutate the served notes/index. The mutation channel
is only probed at the FLAG-OFF bridge layer (the mutators must be absent +
unroutable); the full add/update/remove round-trip belongs in a throwaway
fixture project with its own flag-on daemon (see the issue #2176 trial notes),
never against a real project's notes.

Sections: 1 handshake+inventory, 2 per-read-tool sweep, 3 overlay tri-state,
4 error taxonomy, 5 large response, 6 concurrency, 7 mutation gate (flag off).
Exit code is non-zero if any hard check fails.
"""
import json
import os
import subprocess
import sys
import threading
import time

CQS = os.environ.get("CQS_BIN", os.path.expanduser("~/.cargo/bin/cqs"))


def git_toplevel(start):
    try:
        out = subprocess.run(["git", "-C", start, "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, check=True)
        return out.stdout.strip()
    except Exception:
        return start


CWD = sys.argv[1] if len(sys.argv) > 1 else git_toplevel(os.getcwd())

OVERLAY_KEYS = {"overlay", "no_overlay", "overlay_root"}
OVERLAY_CAPABLE = {
    "cqs_search", "cqs_gather", "cqs_scout", "cqs_task", "cqs_callers",
    "cqs_callees", "cqs_impact", "cqs_dead", "cqs_ci", "cqs_review", "cqs_impact_diff",
}
MUTATORS = ["cqs_notes_add", "cqs_notes_update", "cqs_notes_remove", "cqs_index"]

results = []  # (section, name, ok, note)


def record(section, name, ok, note=""):
    results.append((section, name, ok, note))


class Bridge:
    def __init__(self, cwd=CWD, env_extra=None):
        env = dict(os.environ)
        env.setdefault("CQS_DAEMON_TIMEOUT_MS", "60000")
        if env_extra:
            env.update(env_extra)
        self.p = subprocess.Popen([CQS, "mcp"], cwd=cwd, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, env=env, bufsize=1)

    def send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def send_raw(self, s):
        self.p.stdin.write(s)
        self.p.stdin.flush()

    def recv(self):
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("EOF from bridge (child exited)")
        return json.loads(line)

    def init(self):
        self.send({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                   "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                              "clientInfo": {"name": "trial", "version": "1"}}})
        return self.recv()

    def call(self, name, args, rid):
        t0 = time.time()
        self.send({"jsonrpc": "2.0", "id": rid, "method": "tools/call",
                   "params": {"name": name, "arguments": args}})
        return self.recv(), (time.time() - t0) * 1000

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=10)
        except Exception:
            self.p.kill()


def envelope_valid(result):
    if not isinstance(result, dict):
        return False
    content = result.get("content")
    if not isinstance(content, list) or not content or not isinstance(content[0].get("text"), str):
        return False
    if not isinstance(result.get("isError"), bool):
        return False
    if result["isError"] is False and "structuredContent" not in result:
        return False
    return True


# Realistic args resolve real names in the served index. Adjust the seed names
# if running against a project other than cqs.
READ_TOOL_ARGS = [
    ("cqs_search", {"query": "retry with exponential backoff", "limit": 5}),
    ("cqs_gather", {"query": "daemon socket json args request", "depth": 1, "limit": 5}),
    ("cqs_scout", {"query": "worktree overlay graph marker", "limit": 5}),
    ("cqs_task", {"description": "add a rerank flag to search", "limit": 5}),
    ("cqs_onboard", {"query": "mcp bridge stdio", "limit": 5}),
    ("cqs_similar", {"name": "search_filtered", "limit": 5}),
    ("cqs_callers", {"name": "search_filtered", "limit": 5}),
    ("cqs_callees", {"name": "search_filtered", "limit": 5}),
    ("cqs_deps", {"name": "search_filtered", "limit": 5}),
    ("cqs_impact", {"name": "search_filtered", "limit": 5}),
    ("cqs_test_map", {"name": "search_filtered", "max_depth": 3}),
    ("cqs_trace", {"source": "cmd_search", "target": "search_filtered", "max_depth": 6}),
    ("cqs_explain", {"name": "search_filtered"}),
    ("cqs_context", {"path": "src/cli/mcp/tools.rs"}),
    ("cqs_blame", {"name": "search_filtered"}),
    ("cqs_diff", {"source": "rust-stdlib", "threshold": 0.2}),
    ("cqs_drift", {"reference": "rust-stdlib", "limit": 5}),
    ("cqs_dead", {"limit": 5}),
    ("cqs_ci", {}),
    ("cqs_review", {}),
    ("cqs_plan", {"description": "add reranking to search"}),
    ("cqs_read", {"path": "src/cli/mcp/mod.rs"}),
    ("cqs_where", {"description": "a new cache eviction policy"}),
    ("cqs_related", {"name": "search_filtered"}),
    ("cqs_stale", {"count_only": True}),
    ("cqs_notes_list", {"warnings": True}),
    ("cqs_suggest", {}),
    ("cqs_impact_diff", {}),
    ("cqs_stats", {}),
    ("cqs_health", {}),
]


def section1(b):
    print("\n=== SECTION 1: handshake + inventory ===")
    init = b.init()
    pv = init.get("result", {}).get("protocolVersion")
    print(f"initialize protocolVersion: {pv} (expect 2025-11-25)")
    b.send({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
    tools = b.recv()["result"]["tools"]
    prefixed = all(t["name"].startswith("cqs_") for t in tools)
    schemas = all(t.get("inputSchema", {}).get("type") == "object"
                  and isinstance(t.get("inputSchema", {}).get("properties"), dict) for t in tools)
    advertised = {t["name"] for t in tools
                  if OVERLAY_KEYS <= set(t.get("inputSchema", {}).get("properties", {}))}
    leak = [t["name"] for t in tools if t["name"] not in OVERLAY_CAPABLE
            and (set(t.get("inputSchema", {}).get("properties", {})) & OVERLAY_KEYS)]
    print(f"tools={len(tools)} (expect 30)  cqs_-prefixed={prefixed}  schemas_ok={schemas}")
    print(f"overlay-advertised={len(advertised)} (expect 11)  match={advertised == OVERLAY_CAPABLE}  leak={leak}")
    ok = (len(tools) == 30 and prefixed and schemas and advertised == OVERLAY_CAPABLE and not leak)
    record("1", "inventory", ok, f"{len(tools)} tools")


def section2(b):
    print("\n=== SECTION 2: per-read-tool sweep (live daemon) ===")
    print(f"{'tool':16} {'ms':>7} {'isErr':6} valid  structuredContent/error")
    rid = 100
    for name, args in READ_TOOL_ARGS:
        rid += 1
        try:
            r, dt = b.call(name, args, rid)
        except Exception as e:
            print(f"{name:16} {'--':>7} EXC   {e}")
            record("2", name, False, f"EXC {e}")
            continue
        err = r.get("error")
        if err:
            note = f"PROTO {err.get('code')}: {err.get('message','')[:50]}"
            print(f"{name:16} {dt:7.0f} {'--':6} PROTO  {note}")
            record("2", name, False, note)
            continue
        res = r.get("result", {})
        valid = envelope_valid(res)
        is_err = res.get("isError")
        sc = res.get("structuredContent")
        detail = (f"keys={list(sc.keys())[:5]}" if isinstance(sc, dict)
                  else (f"type={type(sc).__name__}" if is_err is False else
                        res.get("content", [{}])[0].get("text", "")[:45]))
        meta = list((res.get("_meta") or {}).keys())
        if meta:
            detail += f" _meta={meta}"
        print(f"{name:16} {dt:7.0f} {str(is_err):6} {'OK ' if valid else 'BAD'}   {detail[:60]}")
        record("2", name, valid, f"isErr={is_err}")


def section3(b):
    print("\n=== SECTION 3: overlay tri-state (needs overlay:true to activate daemon-side) ===")
    wt = CWD  # a registered worktree of the served project passes the gate
    fab = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".cqs_trial_fab")
    os.makedirs(fab, exist_ok=True)
    os.makedirs(os.path.join(fab, ".git"), exist_ok=True)
    rid = 300

    def probe(label, args):
        nonlocal rid
        rid += 1
        r, dt = b.call("cqs_callers", {"name": "search_filtered", **args}, rid)
        res = r.get("result", {})
        err = r.get("error")
        meta = res.get("_meta") or {}
        print(f"{label}\n    proto={err.get('code') if err else None} isErr={res.get('isError')} "
              f"overlay_graph={meta.get('overlay_graph')} meta={list(meta.keys())} {dt:.0f}ms")
        return r, res, err

    _, res, err = probe("(a) overlay=true overlay_root=<this worktree/repo>",
                        {"overlay": True, "overlay_root": wt})
    # A clean worktree serves the parent (no marker); a registered worktree with a
    # delta stamps overlay_graph=full. Either is a pass; a proto error is not.
    record("3", "overlay_root registered", err is None, f"overlay_graph={(res.get('_meta') or {}).get('overlay_graph')}")

    _, res, err = probe("(b) overlay=true overlay_root=<fabricated non-worktree>",
                        {"overlay": True, "overlay_root": os.path.realpath(fab)})
    rejected = (err is not None) or (res.get("isError") is True)
    print(f"    -> fabricated overlay_root rejected by the gate: {rejected}")
    record("3", "overlay_root fabricated rejected", rejected, "security gate")

    _, res, err = probe("(c) no_overlay=true", {"no_overlay": True})
    record("3", "no_overlay", err is None)


def section4(b):
    print("\n=== SECTION 4: error taxonomy ===")
    rid = 400
    r, _ = b.call("cqs_callers", {"name": "zzz_definitely_not_a_real_symbol_xyz"}, rid)
    res = r.get("result", {})
    print(f"not_found name: isErr={res.get('isError')} (empty-but-ok or candidates)")
    record("4", "not_found", r.get("error") is None)

    r, _ = b.call("cqs_callers", {"name": 42}, rid + 1)
    code = (r.get("error") or {}).get("code")
    print(f"invalid arg (name=42): code={code} (expect -32602)")
    record("4", "invalid_arg", code == -32602, f"code={code}")

    t0 = time.time()
    b.send_raw("{ this is not valid json\n")
    code = (b.recv().get("error") or {}).get("code")
    print(f"malformed frame: code={code} (expect -32700)")
    record("4", "malformed_frame", code == -32700, f"code={code}")

    b.send({"jsonrpc": "2.0", "id": 499, "method": "no/such/method", "params": {}})
    code = (b.recv().get("error") or {}).get("code")
    print(f"unknown method: code={code} (expect -32601)")
    record("4", "unknown_method", code == -32601, f"code={code}")


def section5(b):
    print("\n=== SECTION 5: large response (context, high token budget) ===")
    r, dt = b.call("cqs_context", {"path": "src/cli/mcp/tools.rs", "tokens": 12000}, 500)
    res = r.get("result", {})
    sc = res.get("structuredContent")
    txt = res.get("content", [{}])[0].get("text", "")
    sc_bytes = len(json.dumps(sc)) if sc is not None else 0
    ok = r.get("error") is None and res.get("isError") is False and sc is not None
    print(f"context tokens=12000: {dt:.0f}ms structuredContent={sc_bytes}B "
          f"text_mirror={len(txt)}B elided={'elided' in txt} ok={ok}")
    record("5", "large_response", ok, f"sc={sc_bytes}B mirror={len(txt)}B")


def section6(b):
    print("\n=== SECTION 6: concurrency ===")
    reqs = [("cqs_callers", {"name": "search_filtered"}), ("cqs_search", {"query": "index", "limit": 3}),
            ("cqs_stats", {}), ("cqs_health", {}), ("cqs_callees", {"name": "search_filtered"}),
            ("cqs_related", {"name": "search_filtered"}), ("cqs_stale", {"count_only": True}),
            ("cqs_dead", {"limit": 3})]
    rid0 = 600
    t0 = time.time()
    for i, (n, a) in enumerate(reqs):
        b.send({"jsonrpc": "2.0", "id": rid0 + i, "method": "tools/call",
                "params": {"name": n, "arguments": a}})
    got = {}
    for _ in reqs:
        r = b.recv()
        got[r.get("id")] = r
    dt = (time.time() - t0) * 1000
    ids_ok = all((rid0 + i) in got and "result" in got[rid0 + i] for i in range(len(reqs)))
    print(f"(a) pipelined {len(reqs)} reqs on one bridge: all ok={ids_ok} {dt:.0f}ms")
    record("6", "pipelined", ids_ok)

    def worker(idx, out):
        try:
            bb = Bridge()
            bb.init()
            r, _ = bb.call("cqs_search", {"query": f"probe {idx}", "limit": 3}, 1)
            out[idx] = r.get("error") is None and r.get("result", {}).get("isError") is False
            bb.close()
        except Exception:
            out[idx] = False
    out = {}
    threads = [threading.Thread(target=worker, args=(i, out)) for i in range(6)]
    t0 = time.time()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    n_ok = sum(1 for v in out.values() if v)
    print(f"(b) 6 concurrent bridge children: {n_ok}/6 ok wall={(time.time()-t0)*1000:.0f}ms")
    record("6", "concurrent_children", n_ok == 6, f"{n_ok}/6")


def section7(b):
    print("\n=== SECTION 7: mutation gate (flag OFF, bridge layer) ===")
    b.send({"jsonrpc": "2.0", "id": 700, "method": "tools/list", "params": {}})
    names = [t["name"] for t in b.recv()["result"]["tools"]]
    absent = all(m not in names for m in MUTATORS)
    r, _ = b.call("cqs_notes_add", {"text": "SHOULD NOT WRITE", "sentiment": 0}, 701)
    code = (r.get("error") or {}).get("code")
    print(f"mutators absent (flag off): {absent}  cqs_notes_add call code={code} (expect -32601)")
    record("7", "mutation_gate_flag_off", absent and code == -32601, f"absent={absent} code={code}")


def main():
    ver = subprocess.run([CQS, "--version"], capture_output=True, text=True).stdout.strip()
    print(f"cqs: {CQS}  {ver}\nbridge cwd: {CWD}")
    b = Bridge()
    section1(b)
    section2(b)
    section3(b)
    section4(b)
    section5(b)
    section6(b)
    section7(b)
    b.close()

    print("\n=== SUMMARY ===")
    npass = sum(1 for _, _, ok, _ in results if ok)
    print(f"{npass}/{len(results)} checks pass")
    fails = [(s, n, note) for s, n, ok, note in results if not ok]
    for s, n, note in fails:
        print(f"  FAIL [{s}] {n}: {note}")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
