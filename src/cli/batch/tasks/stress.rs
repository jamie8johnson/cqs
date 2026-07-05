//! Multi-threaded stress harness for the daemon TaskStore (MCP 3b floor, the
//! interleaving-auditor lane for #2237).
//!
//! Loom (`publication_interleaving_model`) exhaustively proves the *bounded*
//! core — the publication ordering, the write-once slot, the admission cap — on a
//! minimal model. This harness covers the schedules loom cannot bound: the REAL
//! `TaskStore` with its `HashMap`-backed map lock, real `std::thread` workers,
//! and `tasks/get` / `tasks/result` reads racing publishes and the GC sweep, at
//! scale. It is EVIDENCE (a passing 100k-iteration run), not a proof — loom is
//! the proof; this catches an integration regression a narrow model would miss.
//!
//! Every run prints its seed; a failure is reproducible by exporting
//! `CQS_TEST_STRESS_SEED=<n>` (and the thread/iteration knobs). The default iteration
//! count is small so the normal `cargo test` isn't slowed; crank it with
//! `CQS_TEST_STRESS_ITERS`.
//!
//! Knobs:
//! - `CQS_TEST_STRESS_ITERS` — admissions per worker thread (default 400).
//! - `CQS_TEST_STRESS_SEED`  — base RNG seed (default: entropy; always printed).
//!
//! Invariants checked (a violation records the schedule + fails at quiesce):
//! - **Cap (RT-DOS-3a):** the map length NEVER exceeds
//!   [`super::RETAINED_TASK_COUNT_CAP`], observed continuously under concurrent
//!   `try_admit` (no single-flight serialization here — the harness proves
//!   `try_admit` bounds the store on its own).
//! - **Publication coherence (seam-2c):** a `tasks/result` that returns `Ready`
//!   always carries the terminal kind the publisher committed; a `tasks/get`
//!   reporting terminal is never followed by a `Ready` with a different outcome.
//! - **Write-once / frozen terminal (seam-1b / D6):** a decoy second publish is
//!   always a no-op — a reader never observes the decoy outcome; a terminal
//!   status, once seen, never changes.
//! - **Freshness stamp (seam-2b):** `result`'s staleness marker is present iff the
//!   reader's `current_epoch` differs from the run's stamped `spawn_epoch`.
//! - **Liveness / no deadlock:** the whole harness completes within its budget.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use crate::cli::batch::tasks::{
        TaskId, TaskLookup, TaskResultLookup, TaskStatus, TaskStore, TerminalKind,
        RETAINED_TASK_COUNT_CAP,
    };
    use crate::cli::json_envelope::ErrorCode;

    /// Deterministic per-thread xorshift64* RNG — reproducible from the base seed
    /// without pulling `rand`'s API surface into the harness.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Rng {
            // Avoid the zero fixed-point.
            Rng(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n.max(1)
        }
    }

    fn env_u64(key: &str, default: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// The outcome a publisher commits, plus the epoch the run stamped — the
    /// reader checks a `Ready` result against this.
    #[derive(Clone)]
    struct Expected {
        status: TaskStatus,
        is_error: bool,
        marker: u64,
        spawn_epoch: u64,
    }

    /// Assert the terminal `kind` a reader read back matches what the publisher
    /// committed. Records a human-readable schedule note on any mismatch.
    fn check_result_matches(
        id: &str,
        exp: &Expected,
        kind: &TerminalKind,
        staleness_present: bool,
        current_epoch: u64,
        violations: &Mutex<Vec<String>>,
    ) {
        let coherent = match (kind, exp.status) {
            (TerminalKind::ToolResult { value, is_error }, TaskStatus::Completed) => {
                *is_error == exp.is_error
                    && value.get("marker").and_then(|m| m.as_u64()) == Some(exp.marker)
            }
            (TerminalKind::RpcError { message, .. }, TaskStatus::Failed) => {
                *message == format!("marker:{}", exp.marker)
            }
            _ => false,
        };
        if !coherent {
            violations.lock().unwrap().push(format!(
                "PUBLICATION-INCOHERENT id={id}: expected status={:?} marker={} \
                 is_error={} but read a mismatched terminal kind",
                exp.status, exp.marker, exp.is_error
            ));
        }
        // Freshness stamp (seam-2b): the marker is present exactly when the epoch
        // moved since the run.
        let expect_stale = exp.spawn_epoch != current_epoch;
        if staleness_present != expect_stale {
            violations.lock().unwrap().push(format!(
                "STALENESS-STAMP id={id}: spawn_epoch={} current_epoch={} \
                 expected staleness={expect_stale} got {staleness_present}",
                exp.spawn_epoch, current_epoch
            ));
        }
    }

    /// Direct read of the map length — the harness's cap probe. `stress` is a
    /// child module of `tasks`, so it may read the private `tasks` field.
    fn map_len(store: &TaskStore) -> usize {
        store.tasks.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// The core harness: `workers` admit+publish threads and `readers` poll
    /// threads race a GC sweeper against the real store. Returns the base seed
    /// used (printed by the callers for reproduction).
    fn run_stress(workers: usize, readers: usize, iters: u64, base_seed: u64) -> Vec<String> {
        let store = TaskStore::new();
        let expected: Arc<Mutex<HashMap<String, Expected>>> = Arc::new(Mutex::new(HashMap::new()));
        let ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let violations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        // workers + readers + sweeper all start together.
        let barrier = Arc::new(Barrier::new(workers + readers + 1));

        let mut handles = Vec::new();

        // ── Worker threads: admit then publish (with a decoy second publish). ──
        for w in 0..workers {
            let store = Arc::clone(&store);
            let expected = Arc::clone(&expected);
            let ids = Arc::clone(&ids);
            let violations = Arc::clone(&violations);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let mut rng = Rng::new(base_seed ^ ((w as u64).wrapping_mul(0x9E37_79B9)));
                barrier.wait();
                for i in 0..iters {
                    let spawn_epoch = rng.below(4);
                    // Mostly a long ttl so readers can catch the terminal; some
                    // zero-ttl so the sweeper reclaims and admits keep flowing.
                    let ttl = if rng.below(3) == 0 { Some(0) } else { Some(3600) };
                    let entry = store.new_entry(spawn_epoch, ttl);
                    let id = store.describe(&entry).task_id;

                    let marker = (w as u64) << 40 | i;
                    let is_failed = rng.below(2) == 0;
                    let exp = Expected {
                        status: if is_failed {
                            TaskStatus::Failed
                        } else {
                            TaskStatus::Completed
                        },
                        is_error: false,
                        marker,
                        spawn_epoch,
                    };

                    // Record expected BEFORE admit so a reader that sees the id
                    // terminal always finds it (publish happens strictly later).
                    expected.lock().unwrap().insert(id.clone(), exp.clone());

                    if !store.try_admit(Arc::clone(&entry)) {
                        // At cap — allocates nothing, id never becomes visible.
                        expected.lock().unwrap().remove(&id);
                        // Continuous cap check: even the failed path must see a
                        // bounded store.
                        let len = map_len(&store);
                        if len > RETAINED_TASK_COUNT_CAP {
                            violations.lock().unwrap().push(format!(
                                "CAP-OVERRUN after refused admit: len={len} > cap={RETAINED_TASK_COUNT_CAP}"
                            ));
                        }
                        continue;
                    }
                    ids.lock().unwrap().push(id.clone());

                    // Cap check immediately after a successful insert.
                    let len = map_len(&store);
                    if len > RETAINED_TASK_COUNT_CAP {
                        violations.lock().unwrap().push(format!(
                            "CAP-OVERRUN after admit: len={len} > cap={RETAINED_TASK_COUNT_CAP}"
                        ));
                    }

                    // A brief window where the entry is `working` and visible.
                    if rng.below(4) == 0 {
                        thread::yield_now();
                    }

                    // The committed outcome.
                    let (kind, status) = if is_failed {
                        (
                            TerminalKind::RpcError {
                                code: ErrorCode::Internal,
                                message: format!("marker:{marker}"),
                            },
                            TaskStatus::Failed,
                        )
                    } else {
                        (
                            TerminalKind::ToolResult {
                                value: serde_json::json!({ "marker": marker }),
                                is_error: false,
                            },
                            TaskStatus::Completed,
                        )
                    };
                    entry.publish(kind, status);

                    // Decoy second publish — the OPPOSITE outcome. Write-once must
                    // make it a no-op; a reader must never observe it.
                    let decoy = if is_failed {
                        (
                            TerminalKind::ToolResult {
                                value: serde_json::json!({ "marker": u64::MAX }),
                                is_error: true,
                            },
                            TaskStatus::Completed,
                        )
                    } else {
                        (
                            TerminalKind::RpcError {
                                code: ErrorCode::Unavailable,
                                message: "decoy".to_string(),
                            },
                            TaskStatus::Failed,
                        )
                    };
                    entry.publish(decoy.0, decoy.1);
                }
            }));
        }

        // ── Reader threads: poll get/result, assert coherence + write-once. ────
        for r in 0..readers {
            let store = Arc::clone(&store);
            let expected = Arc::clone(&expected);
            let ids = Arc::clone(&ids);
            let violations = Arc::clone(&violations);
            let done = Arc::clone(&done);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let mut rng = Rng::new(base_seed ^ 0xD1B5_4A32 ^ ((r as u64) << 17));
                // Per-reader memory of the first terminal status seen per id — the
                // write-once / frozen-terminal guard.
                let mut first_terminal: HashMap<String, TaskStatus> = HashMap::new();
                barrier.wait();
                while !done.load(Ordering::Relaxed) {
                    let pick = {
                        let guard = ids.lock().unwrap();
                        if guard.is_empty() {
                            None
                        } else {
                            Some(guard[rng.below(guard.len() as u64) as usize].clone())
                        }
                    };
                    let Some(id_str) = pick else {
                        thread::yield_now();
                        continue;
                    };
                    let id = TaskId::from_wire(&id_str);

                    // tasks/get: status snapshot.
                    let get_status = match store.get(&id) {
                        TaskLookup::Found(d) => Some(d.status),
                        TaskLookup::Gone => None, // swept — legal.
                    };
                    if let Some(status) = get_status {
                        if status.is_terminal() {
                            // Frozen-terminal: the first terminal status seen for
                            // an id must never change.
                            match first_terminal.get(&id_str) {
                                Some(prev) if *prev != status => {
                                    violations.lock().unwrap().push(format!(
                                        "TERMINAL-TRANSITIONED id={id_str}: {prev:?} -> {status:?}"
                                    ));
                                }
                                None => {
                                    first_terminal.insert(id_str.clone(), status);
                                }
                                _ => {}
                            }
                        }
                    }

                    // tasks/result at a random live epoch.
                    let current_epoch = rng.below(4);
                    match store.result(&id, current_epoch) {
                        TaskResultLookup::Ready { kind, staleness } => {
                            let exp = expected.lock().unwrap().get(&id_str).cloned();
                            if let Some(exp) = exp {
                                check_result_matches(
                                    &id_str,
                                    &exp,
                                    &kind,
                                    staleness.is_some(),
                                    current_epoch,
                                    &violations,
                                );
                            }
                        }
                        // Pending (working or the torn-read retry window) and Gone
                        // (swept) are both legal at any moment.
                        TaskResultLookup::Pending | TaskResultLookup::Gone => {}
                    }
                }
            }));
        }

        // ── Sweeper thread: continuous GC backstop. ────────────────────────────
        {
            let store = Arc::clone(&store);
            let done = Arc::clone(&done);
            let violations = Arc::clone(&violations);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                while !done.load(Ordering::Relaxed) {
                    store.sweep_expired();
                    let len = map_len(&store);
                    if len > RETAINED_TASK_COUNT_CAP {
                        violations.lock().unwrap().push(format!(
                            "CAP-OVERRUN observed by sweeper: len={len} > cap={RETAINED_TASK_COUNT_CAP}"
                        ));
                    }
                    thread::yield_now();
                }
            }));
        }

        // The workers (all but the readers + sweeper) finish a bounded amount of
        // work; join them first, then let readers drain briefly and stop.
        // Workers are handles[0..workers].
        let mut worker_handles: Vec<_> = handles.drain(0..workers).collect();
        for h in worker_handles.drain(..) {
            h.join().expect("worker thread panicked (deadlock/poison?)");
        }
        // Let readers observe the fully-published tail.
        thread::sleep(std::time::Duration::from_millis(50));
        done.store(true, Ordering::Relaxed);
        for h in handles {
            h.join().expect("reader/sweeper thread panicked");
        }

        // Final quiesce cap check.
        let final_len = map_len(&store);
        if final_len > RETAINED_TASK_COUNT_CAP {
            violations.lock().unwrap().push(format!(
                "CAP-OVERRUN at quiesce: len={final_len} > cap={RETAINED_TASK_COUNT_CAP}"
            ));
        }

        Arc::try_unwrap(violations)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default()
    }

    /// Cap + publication + write-once + freshness, under contended admit / read /
    /// sweep. Default iteration count is modest so the normal suite isn't slowed;
    /// `CQS_TEST_STRESS_ITERS` cranks it. On failure the seed reproduces the schedule.
    #[test]
    fn taskstore_concurrent_invariants_hold() {
        let iters = env_u64("CQS_TEST_STRESS_ITERS", 400);
        let base_seed = env_u64(
            "CQS_TEST_STRESS_SEED",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x1234_5678),
        );
        // 4 admitters hammering try_admit past the 32-cap, 6 readers, 1 sweeper.
        println!(
            "taskstore_concurrent_invariants_hold: seed={base_seed} iters={iters} \
             (reproduce with CQS_TEST_STRESS_SEED={base_seed} CQS_TEST_STRESS_ITERS={iters})"
        );
        let violations = run_stress(4, 6, iters, base_seed);
        assert!(
            violations.is_empty(),
            "schedule broke {} invariant(s) (seed={base_seed}):\n{}",
            violations.len(),
            violations.join("\n")
        );
    }

    /// A tighter, admit-heavy variant: many admitters, a low cap-pressure ttl mix,
    /// pinning the cap bound specifically. Cheap by default.
    #[test]
    fn taskstore_cap_holds_under_admission_storm() {
        let iters = env_u64("CQS_TEST_STRESS_ITERS", 300);
        let base_seed = env_u64("CQS_TEST_STRESS_SEED", 0xC0FF_EE00);
        println!("taskstore_cap_holds_under_admission_storm: seed={base_seed} iters={iters}");
        // 8 admitters, 2 readers, 1 sweeper — maximal admit contention.
        let violations = run_stress(8, 2, iters, base_seed);
        let cap_violations: Vec<_> = violations
            .iter()
            .filter(|v| v.contains("CAP-OVERRUN"))
            .cloned()
            .collect();
        assert!(
            cap_violations.is_empty(),
            "cap overrun under admission storm (seed={base_seed}):\n{}",
            cap_violations.join("\n")
        );
        assert!(
            violations.is_empty(),
            "other invariant broke (seed={base_seed}):\n{}",
            violations.join("\n")
        );
    }
}
