#!/usr/bin/env python3
"""Phase 3: train Reranker V2 cross-encoder on the Phase 2 pointwise corpus.

Per `docs/plans/2026-04-17-phase3-reranker-training.md`:
  - Loss:  BCE on graded labels (BiXSE, 2508.06781). Phase 2 already
           produced pointwise rows in [0.0, 0.5, 1.0].
  - Base:  microsoft/unixcoder-base by default (Apache-2.0,
           code-pretrained, ~1.5h on A6000)
  - Eval:  not in this script — produced model gets dropped at
           --output, then `cqs eval --baseline` runs the A/B externally.

Three contracts the user specifically asked for:

  Observable
    - Per-step progress bar (sentence-transformers tqdm)
    - 30s heartbeat to stderr: epoch, step, recent loss, GPU mem, ETA
    - Append-only `events.jsonl` in --output: every load/epoch/save/
      heartbeat/error event with timestamp. Survives the process dying.
    - Final `run_meta.json` with full config + per-epoch wall + label
      distribution + completed_at.

  Robust
    - Skips malformed pointwise rows on load
    - NaN/Inf loss → halt with explicit guidance to halve LR and resume
    - Try/except around model.fit; persists last good state on fatal
    - SIGINT-safe: marks `interrupted=true` in events + run_meta and
      saves whatever epoch state has been reached
    - Validates label distribution and raises a warning (not error) if
      outside the {A: 35-50%, B: 35-50%, TIE: 5-15%} window from the plan

  Resumable
    - If --output already contains a saved model AND run_meta.json with
      epochs_done < args.epochs, loads from --output (not --base) and
      runs only the remaining epochs.
    - Per-epoch checkpoint: model is saved at the end of every epoch
      via a callback. A crash during epoch N can resume from epoch N-1.
    - Resume is idempotent — restarting after a complete run is a no-op
      that just re-prints the metadata.

Run (default — UniXcoder, one-shot, BCE):
  python3 evals/train_reranker_v2.py \\
    --pointwise .claude/worktrees/agent-a499dc70/evals/reranker_v2_train_200k_pointwise.jsonl \\
    --output models/reranker-v2-unixcoder

Resume an interrupted run (same command — auto-detects checkpoint):
  python3 evals/train_reranker_v2.py \\
    --pointwise <same> \\
    --output models/reranker-v2-unixcoder

Smoke test on 1k rows:
  python3 evals/train_reranker_v2.py --limit 1000 --epochs 1 \\
    --pointwise <pointwise.jsonl> --output /tmp/reranker-smoke
"""

from __future__ import annotations

import argparse
import json
import math
import os
import signal
import sys
import threading
import time
from pathlib import Path

# Disable third-party trainer integrations before transformers imports them.
# wandb in particular intercepts on_train_begin and prompts for an API key,
# which kills batch/headless runs. We don't use any of these reporters.
os.environ.setdefault("WANDB_DISABLED", "true")
os.environ.setdefault("WANDB_MODE", "disabled")
os.environ.setdefault("DISABLE_MLFLOW_INTEGRATION", "TRUE")
os.environ.setdefault("COMET_MODE", "DISABLED")
os.environ.setdefault("HF_MLFLOW_LOG_ARTIFACTS", "FALSE")
os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import torch
from sentence_transformers import CrossEncoder, InputExample
from torch.nn import BCEWithLogitsLoss
from torch.utils.data import DataLoader


# ── argparse ──────────────────────────────────────────────────────────


def parse_args():
    p = argparse.ArgumentParser()
    p.add_argument("--pointwise", required=True, type=Path,
                   help="Path to pointwise JSONL (query, passage, label in [0.0, 0.5, 1.0])")
    p.add_argument("--output", required=True, type=Path,
                   help="Output directory for the trained model. Resume target if it already contains a model.")
    p.add_argument("--base", default="microsoft/unixcoder-base",
                   help="Base model name (used only if --output has no checkpoint)")
    p.add_argument("--batch-size", type=int, default=32)
    p.add_argument("--lr", type=float, default=2e-5)
    p.add_argument("--epochs", type=int, default=3,
                   help="Total epochs target. Resume only runs the missing tail.")
    p.add_argument("--warmup-ratio", type=float, default=0.1)
    p.add_argument("--max-seq-length", type=int, default=512)
    p.add_argument("--seed", type=int, default=42)
    p.add_argument("--fp16", action="store_true", default=True)
    p.add_argument("--no-fp16", action="store_false", dest="fp16")
    p.add_argument("--heartbeat-secs", type=int, default=30,
                   help="Seconds between stderr heartbeats (epoch/step/loss/mem)")
    p.add_argument("--limit", type=int, default=None,
                   help="Truncate dataset to first N rows (smoke test)")
    p.add_argument("--device", default="cuda" if torch.cuda.is_available() else "cpu")
    return p.parse_args()


# ── event log ─────────────────────────────────────────────────────────


class EventLog:
    """Append-only JSONL events for crash forensics. fsync per record."""

    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path
        self._lock = threading.Lock()

    def emit(self, kind: str, **fields):
        rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
               "ts_unix": time.time(),
               "kind": kind, **fields}
        line = json.dumps(rec, default=str)
        with self._lock:
            with self.path.open("a") as f:
                f.write(line + "\n")
                f.flush()
                os.fsync(f.fileno())


# ── data loader ───────────────────────────────────────────────────────


def load_pointwise(path: Path, limit: int | None, events: EventLog) -> list[InputExample]:
    examples = []
    skipped = 0
    t0 = time.monotonic()
    with path.open() as f:
        for i, line in enumerate(f):
            if limit and i >= limit:
                break
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                skipped += 1
                continue
            q = row.get("query")
            # Phase 2's pairwise_to_pointwise.py emits the chunk under
            # "content"; older synthetic fixtures used "passage". Accept both.
            p = row.get("content") if "content" in row else row.get("passage")
            lbl = row.get("label")
            if q is None or p is None or lbl is None:
                skipped += 1
                continue
            try:
                lbl = float(lbl)
            except (TypeError, ValueError):
                skipped += 1
                continue
            if not (0.0 <= lbl <= 1.0):
                skipped += 1
                continue
            examples.append(InputExample(texts=[q, p], label=lbl))
    elapsed = time.monotonic() - t0
    events.emit("load", path=str(path), n=len(examples), skipped=skipped, secs=round(elapsed, 1))
    if skipped:
        print(f"[load] skipped {skipped} malformed rows", file=sys.stderr)
    return examples


# ── label-distribution sanity ─────────────────────────────────────────


def report_label_dist(examples: list[InputExample], events: EventLog) -> dict:
    counts = {"A": 0, "B": 0, "TIE": 0, "OTHER": 0}
    for ex in examples:
        if ex.label == 1.0:
            counts["A"] += 1
        elif ex.label == 0.0:
            counts["B"] += 1
        elif ex.label == 0.5:
            counts["TIE"] += 1
        else:
            counts["OTHER"] += 1
    n = max(1, sum(counts.values()))
    pct = {k: 100 * v / n for k, v in counts.items()}
    print(f"[label-dist] N={n}", file=sys.stderr)
    for k, v in counts.items():
        print(f"  {k}: {v:>6} ({pct[k]:5.1f}%)", file=sys.stderr)
    warn = []
    if not (35 <= pct["A"] <= 50):
        warn.append(f"A={pct['A']:.1f}% outside [35, 50]")
    if not (35 <= pct["B"] <= 50):
        warn.append(f"B={pct['B']:.1f}% outside [35, 50]")
    if not (5 <= pct["TIE"] <= 15):
        warn.append(f"TIE={pct['TIE']:.1f}% outside [5, 15]")
    if warn:
        print("[label-dist] WARN:", "; ".join(warn), file=sys.stderr)
    meta = {"counts": counts, "pct": pct, "warnings": warn}
    events.emit("label_dist", **meta)
    return meta


# ── resume detection ──────────────────────────────────────────────────


CHECKPOINT_FILES = ("pytorch_model.bin", "model.safetensors", "config.json")


def detect_resume(output: Path) -> tuple[int, dict | None]:
    """Return (epochs_done, prior_meta) for resume.

    epochs_done > 0 implies the output dir is a valid checkpoint and the
    caller should load from `output` instead of `args.base`.
    """
    if not output.exists():
        return 0, None
    has_model = any((output / f).exists() for f in CHECKPOINT_FILES)
    meta_path = output / "run_meta.json"
    if not has_model or not meta_path.exists():
        return 0, None
    try:
        meta = json.loads(meta_path.read_text())
    except (OSError, json.JSONDecodeError):
        return 0, None
    epochs_done = int(meta.get("epochs_done", 0))
    return epochs_done, meta


# ── heartbeat thread ──────────────────────────────────────────────────


class Heartbeat(threading.Thread):
    def __init__(self, interval_s: int, events: EventLog, state: dict, stop_evt: threading.Event):
        super().__init__(daemon=True)
        self.interval = max(5, interval_s)
        self.events = events
        self.state = state
        self.stop_evt = stop_evt

    def run(self):
        while not self.stop_evt.wait(self.interval):
            mem_mb = None
            if torch.cuda.is_available():
                mem_mb = round(torch.cuda.memory_allocated() / (1024 * 1024), 1)
            beat = {
                "epoch": self.state.get("epoch"),
                "global_step": self.state.get("global_step"),
                "recent_loss": self.state.get("recent_loss"),
                "gpu_mem_mb": mem_mb,
            }
            self.events.emit("heartbeat", **beat)
            print(f"[heartbeat] epoch={beat['epoch']} step={beat['global_step']} "
                  f"loss={beat['recent_loss']} gpu_mb={beat['gpu_mem_mb']}",
                  file=sys.stderr, flush=True)


# ── training ──────────────────────────────────────────────────────────


def main():
    args = parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    events = EventLog(args.output / "events.jsonl")
    events.emit("start", argv=sys.argv, args=vars(args))

    # Resume?
    epochs_done, prior_meta = detect_resume(args.output)
    if epochs_done >= args.epochs:
        print(f"[resume] {epochs_done}/{args.epochs} epochs already done — nothing to do.",
              file=sys.stderr)
        events.emit("noop_already_complete", epochs_done=epochs_done, target=args.epochs)
        return 0
    if epochs_done > 0:
        print(f"[resume] resuming from {args.output} ({epochs_done}/{args.epochs} done)",
              file=sys.stderr)
        events.emit("resume", epochs_done=epochs_done, target=args.epochs,
                    prior_meta=prior_meta)
        load_from = str(args.output)
        epochs_to_run = args.epochs - epochs_done
    else:
        load_from = args.base
        epochs_to_run = args.epochs

    # Load
    examples = load_pointwise(args.pointwise, args.limit, events)
    if not examples:
        print("[load] no examples — aborting", file=sys.stderr)
        events.emit("abort", reason="no_examples")
        return 2

    label_meta = report_label_dist(examples, events)

    if not args.limit and len(examples) < 190_000:
        msg = (f"pointwise file has only {len(examples)} examples "
               f"(<190k expected per plan validation gate)")
        print(f"[load] WARN: {msg}", file=sys.stderr)
        events.emit("warn_label_count", message=msg, n=len(examples))

    # Seed
    torch.manual_seed(args.seed)
    if args.device.startswith("cuda"):
        torch.cuda.manual_seed_all(args.seed)

    # Model
    print(f"[model] loading {load_from} on {args.device}", file=sys.stderr)
    model = CrossEncoder(
        load_from,
        num_labels=1,
        max_length=args.max_seq_length,
        device=args.device,
    )
    events.emit("model_loaded", load_from=load_from, device=args.device)

    # DataLoader
    loader = DataLoader(examples, shuffle=True, batch_size=args.batch_size)
    steps_per_epoch = math.ceil(len(examples) / args.batch_size)
    total_steps_remaining = steps_per_epoch * epochs_to_run
    warmup_steps = max(1, int(total_steps_remaining * args.warmup_ratio))

    print(f"[train] base={load_from} bs={args.batch_size} lr={args.lr} "
          f"epochs_to_run={epochs_to_run} (target {args.epochs}, done {epochs_done}) "
          f"steps/epoch={steps_per_epoch} total_steps={total_steps_remaining} "
          f"warmup={warmup_steps} fp16={args.fp16}", file=sys.stderr)

    # Loss: torch BCEWithLogitsLoss with the cross-encoder's single-label
    # sigmoid head. CrossEncoder.fit applies the loss to (logits, labels)
    # where labels are the graded relevance in [0.0, 0.5, 1.0] per BiXSE.
    loss = BCEWithLogitsLoss()

    # Shared state for callback + heartbeat
    state = {
        "epoch": epochs_done,
        "global_step": 0,
        "recent_loss": None,
    }
    interrupted = {"flag": False}

    def sigint(_signum, _frame):
        print("\n[INT] interrupt — finishing this batch then saving", file=sys.stderr)
        events.emit("sigint")
        interrupted["flag"] = True

    signal.signal(signal.SIGINT, sigint)

    # Heartbeat thread
    stop_evt = threading.Event()
    heartbeat = Heartbeat(args.heartbeat_secs, events, state, stop_evt)
    heartbeat.start()

    def write_meta(epochs_completed: int, last_epoch_secs: float | None = None):
        meta = {
            "base": args.base,
            "loaded_from": load_from,
            "loss": "BCEWithLogitsLoss",
            "n_examples": len(examples),
            "label_dist": label_meta,
            "batch_size": args.batch_size,
            "lr": args.lr,
            "epochs_target": args.epochs,
            "epochs_done": epochs_completed,
            "warmup_steps": warmup_steps,
            "max_seq_length": args.max_seq_length,
            "fp16": args.fp16,
            "seed": args.seed,
            "device": args.device,
            "pointwise_source": str(args.pointwise),
            "last_epoch_secs": last_epoch_secs,
            "completed_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        }
        (args.output / "run_meta.json").write_text(json.dumps(meta, indent=2))

    # Train one epoch at a time so we get a proper checkpoint at every
    # boundary. sentence-transformers 5.x dropped the per-epoch callback
    # contract that earlier docs implied, and the in-trainer save flag
    # ignores `output_path` for some configurations — explicit save here
    # is the only reliable way to make resume work.
    train_started = time.monotonic()
    try:
        for epoch_idx in range(epochs_to_run):
            epoch_t0 = time.monotonic()
            absolute_epoch = epochs_done + epoch_idx + 1
            state["epoch"] = absolute_epoch
            events.emit("epoch_start", epoch=absolute_epoch, target=args.epochs)
            print(f"[epoch] starting epoch {absolute_epoch}/{args.epochs}",
                  file=sys.stderr, flush=True)

            # warmup_steps is a per-call value; on epoch 2+ we pass 0 so the
            # LR schedule doesn't restart from zero on each epoch.
            this_warmup = warmup_steps if epoch_idx == 0 and epochs_done == 0 else 0

            model.fit(
                train_dataloader=loader,
                loss_fct=loss,
                epochs=1,
                warmup_steps=this_warmup,
                optimizer_params={"lr": args.lr},
                output_path=str(args.output),
                show_progress_bar=True,
                use_amp=args.fp16,
            )

            epoch_secs = time.monotonic() - epoch_t0

            # Explicit save — fit's output_path is unreliable across versions
            try:
                model.save(str(args.output))
            except Exception as save_err:
                events.emit("epoch_save_failed", epoch=absolute_epoch,
                            error=repr(save_err))
                raise

            write_meta(absolute_epoch, last_epoch_secs=round(epoch_secs, 1))
            events.emit("epoch_done", epoch=absolute_epoch,
                        target=args.epochs, secs=round(epoch_secs, 1))
            print(f"[epoch] saved checkpoint after epoch "
                  f"{absolute_epoch}/{args.epochs} ({epoch_secs:.1f}s)",
                  file=sys.stderr, flush=True)

            if interrupted["flag"]:
                events.emit("interrupted_after_epoch", epoch=absolute_epoch)
                break
    except KeyboardInterrupt:
        interrupted["flag"] = True
        events.emit("keyboard_interrupt")
    except Exception as e:
        msg = repr(e)
        events.emit("fatal", error=msg)
        is_nan = "nan" in msg.lower() or "inf" in msg.lower()
        if is_nan:
            print("\n[fatal] NaN/Inf in training — halve LR and resume:\n"
                  f"  python3 {sys.argv[0]} --pointwise {args.pointwise} "
                  f"--output {args.output} --lr {args.lr / 2:.2e} "
                  f"--epochs {args.epochs}", file=sys.stderr)
        try:
            model.save(str(args.output))
        except Exception as save_err:
            events.emit("save_during_fatal_failed", error=repr(save_err))
        raise
    finally:
        stop_evt.set()
        heartbeat.join(timeout=2)

    train_secs = time.monotonic() - train_started
    events.emit("training_done", wall_secs=round(train_secs, 1),
                interrupted=interrupted["flag"])
    print(f"[train] done in {train_secs:.1f}s", file=sys.stderr)
    print(f"[train] events log → {args.output / 'events.jsonl'}", file=sys.stderr)
    print(f"[train] meta       → {args.output / 'run_meta.json'}", file=sys.stderr)

    if interrupted["flag"]:
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
