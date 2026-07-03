//! Loom model of the swappable shared-embedder protocol behind the stage-2
//! foreign-model slot-promote hot-swap — the interleaving-auditor lane.
//!
//! ## Why this module exists
//!
//! `SharedEmbedder` (`embedder/shared.rs`) is a `RwLock<Option<Arc<Embedder>>>`
//! cell that replaced the old `OnceLock<Arc<Embedder>>`: a foreign-model slot
//! promote must *replace* the resident embedder (new ONNX session, new dim), not
//! merely lazily set it once. Two cells exist after daemon boot:
//!
//!   * the **watch cell** (`cmd_watch`'s `shared_embedder`, watch/mod.rs:1299) —
//!     the write side; rebound by `try_init_embedder` (rebuild.rs `swap`, gated
//!     on `get_matching`) and `maybe_rebind_active_slot` (mod.rs `swap` on a
//!     foreign promote). Single deliberate writer: the watch loop.
//!   * the **query cell** (`BatchContext::embedder`, context.rs) — the read
//!     side; rebound by `reconcile_model_for_reopen` (`swap` on a detected model
//!     change, or `clear` when the new model's build fails) and filled lazily by
//!     `resolve_matching` from N concurrent connection-handler threads.
//!
//! The house daemon suite runs single-threaded by construction. It cannot place:
//!   * a lazy `resolve_matching` **between** another installer's read-miss and
//!     write-acquire (the double-check-under-write-lock branch, shared.rs:82-93 —
//!     unreachable single-threaded, untested), nor
//!   * a lazy `resolve_matching` **concurrent with** a deliberate `swap`/`clear`
//!     (the non-clobber guarantee, shared.rs:86-91).
//!
//! This module drives those with real loom threads and asserts the shared
//! invariant after EVERY interleaving loom explores.
//!
//! ## The invariant under test (two halves — both from the hot-swap contract)
//!
//! > **COHERENCE**: `resolve_matching(want)` NEVER returns an embedder for a
//! > model other than `want`. A query built its `(store, model_config)` pair
//! > coherently under the outer lock; the embedder it pairs with must match
//! > `model_config`, even if a concurrent `swap` moved the cell to a different
//! > model between the query's checkout and its embedder resolution.
//!
//! > **CONVERGENCE / NON-CLOBBER**: a deliberate `swap(B)` (the authoritative
//! > model change) is NEVER lost to a concurrent lazy `resolve_matching(A)` — the
//! > lazy path installs into the cell ONLY when it is empty, and the emptiness is
//! > re-checked UNDER the write lock, so it cannot overwrite the newer model a
//! > `swap` just published. And two concurrent `resolve_matching(M)` on an empty
//! > cell CONVERGE to one shared session (the double-check branch), never leaving
//! > the cell churning private builds.
//!
//! ## What the model abstracts (the bounded shape loom needs)
//!
//! `Embedder` cannot run under loom (ONNX). The only property the invariant turns
//! on is *which model* a cell / result belongs to, so the stand-in is a
//! `ModelTag` (`A`/`B`/`C`) carried by an `Emb` struct — mirroring how
//! `interleaving_model.rs` reduces `EpochCell<T>` to its `gen` tag and
//! `chunkloss_interleaving_model.rs` reduces a chunk to a `char`. `Embedder::new`
//! is lazy in production (no download at construction), so building the stand-in
//! for a *different* model is cheap and always succeeds — matching the common
//! runtime case where the model files are present.
//!
//! `SharedEmb` below is a line-for-line transcription of `SharedEmbedder`'s
//! `RwLock<Option<Arc<Emb>>>` protocol: `get_matching` / `resolve_matching` /
//! `swap` / `clear` / `set_if_empty` / `get_or_set` / `get`. The `RwLock` (not a
//! `Mutex`) is faithful because the load-bearing edge — `resolve_matching`'s
//! double-check reads the cell UNDER THE WRITE LOCK (shared.rs:82), serialized
//! against any concurrent `swap`'s write — depends on the reader/writer split.
//!
//! ## Running
//!
//! Loom tests do not run in the normal suite (the `mod` declaration in `mod.rs`
//! carries the `#[cfg(all(cqs_loom, test))]` gate). Build once, then run the
//! prebuilt binary directly (a bare `cargo test` re-explores slowly):
//!
//! ```bash
//! export CARGO_TARGET_DIR=/tmp/cqs-loom
//! RUSTFLAGS="--cfg cqs_loom" cargo test --features cuda-index --bin cqs \
//!     embedder_swap_interleaving_model --no-run
//! BIN=$(ls -t "$CARGO_TARGET_DIR"/debug/deps/cqs-* | grep -v '\.d$' | head -1)
//! LOOM_MAX_PREEMPTIONS=3 "$BIN" embedder_swap_interleaving_model --test-threads=1
//! # the ignored re-strand reproduction (a LIVE convergence gap, not a control):
//! LOOM_MAX_PREEMPTIONS=3 "$BIN" \
//!     resolve_matching_restrands_a_cleared_cell -- --ignored --test-threads=1
//! ```
//!
//! All non-`#[ignore]`d models here are GREEN: COHERENCE, NON-CLOBBER, and the
//! double-check SHARING hold under every interleaving, and the daemon boot-seed +
//! watch-cell convergence (the F1 `adopt_embedder` model gate) hold too. The one
//! `#[ignore]`d model reproduces a schedule that exists in the *protocol* — a
//! lazy `resolve_matching` re-stranding a `clear()`-emptied cell — but which is
//! NOT reachable in current production, because the only `clear()` caller
//! (`reconcile_model_for_reopen`'s build-failure arm, context.rs:682) sits behind
//! an effectively-infallible lazy `Embedder::new` (core.rs `new_inner` has no
//! fallible `?`; the ONNX load is deferred to first `embed_query`). It is kept as
//! a durable LATENT-FRAGILITY guard: it would go from "protocol-only" to "live"
//! the day `Embedder::new` is made eager/fallible, a second `clear()` caller is
//! added, or `warm`'s infallible boot backstop is weakened — without
//! `resolve_matching`'s `None` arm also being hardened. See its doc comment.

// The `mod embedder_swap_interleaving_model;` declaration in `mod.rs` carries the
// `#[cfg(all(cqs_loom, test))]` gate (canonical over a file-level `#![cfg]`, per
// the note in `interleaving_model.rs`), so a normal build never sees `use loom`.

use loom::sync::{Arc, RwLock};
use loom::thread;

/// A model identity. Production `ModelConfig` equality turns on `name`/`dim`;
/// here a single tag stands for each distinct model. `A` is the boot model,
/// `B`/`C` are foreign-promote targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelTag {
    A,
    B,
    C,
}

/// Stand-in for `Arc<Embedder>`: the only property the invariant depends on is
/// which model the session belongs to. Distinct `Arc<Emb>` allocations model
/// distinct ONNX sessions, so `Arc::ptr_eq` on two results answers "did the two
/// callers SHARE one session?" — the double-check branch's whole purpose.
#[derive(Debug)]
struct Emb {
    model: ModelTag,
}

fn emb(model: ModelTag) -> Arc<Emb> {
    Arc::new(Emb { model })
}

/// A line-for-line transcription of `SharedEmbedder`
/// (`embedder/shared.rs`): the `RwLock<Option<Arc<Embedder>>>` protocol reduced
/// to the model-tag stand-in. Every method mirrors its production sibling; the
/// comments cite the production line the arm transcribes.
#[derive(Default)]
struct SharedEmb {
    inner: RwLock<Option<Arc<Emb>>>,
}

impl SharedEmb {
    fn new() -> Self {
        Self::default()
    }

    /// `SharedEmbedder::get` (shared.rs:47).
    fn get(&self) -> Option<Arc<Emb>> {
        self.inner.read().unwrap().clone()
    }

    /// `SharedEmbedder::get_matching` (shared.rs:54). Production ends
    /// `.map(Arc::clone)`; `.cloned()` is the identical Arc-clone (clippy's
    /// preferred form for `Option<&Arc<_>>`).
    fn get_matching(&self, want: ModelTag) -> Option<Arc<Emb>> {
        self.inner
            .read()
            .unwrap()
            .as_ref()
            .filter(|e| e.model == want)
            .cloned()
    }

    /// `SharedEmbedder::resolve_matching` (shared.rs:73-94). The read-miss →
    /// build-outside-lock → double-check-under-write-lock protocol, verbatim.
    /// `Embedder::new(want)` always succeeds in the model (see module docs), so
    /// the `?` on the build never trips — the model exercises the *installed*
    /// paths, which is where the interleaving lives.
    fn resolve_matching(&self, want: ModelTag) -> Arc<Emb> {
        if let Some(e) = self.get_matching(want) {
            return e; // shared.rs:74-76 — fast read-lock hit.
        }
        // shared.rs:80 — build outside the read lock (heavy in production).
        let built = emb(want);
        let mut guard = self.inner.write().unwrap();
        match guard.as_ref() {
            // shared.rs:85 — a concurrent installer beat us with a matching
            // model; prefer it so the daemon threads share one session.
            Some(existing) if existing.model == want => Arc::clone(existing),
            // shared.rs:88 — resident is a DIFFERENT model (a `swap` may have
            // installed a newer one); DO NOT overwrite it, hand back the private
            // build.
            Some(_different) => built,
            // shared.rs:89-92 — empty cell: install our build and share it back.
            None => {
                *guard = Some(Arc::clone(&built));
                built
            }
        }
    }

    /// `SharedEmbedder::swap` (shared.rs:100) — unconditional install.
    fn swap(&self, e: Arc<Emb>) {
        *self.inner.write().unwrap() = Some(e);
    }

    /// `SharedEmbedder::clear` (shared.rs:110) — evict to empty (the reconcile
    /// build-failure path).
    fn clear(&self) {
        *self.inner.write().unwrap() = None;
    }

    /// `SharedEmbedder::set_if_empty` (shared.rs:118) — install iff empty.
    fn set_if_empty(&self, e: Arc<Emb>) -> bool {
        let mut guard = self.inner.write().unwrap();
        if guard.is_some() {
            false
        } else {
            *guard = Some(e);
            true
        }
    }

    /// `SharedEmbedder::get_or_set` (shared.rs:131) — install iff empty, return
    /// the resident. Currently only test-exercised in production, but modelled
    /// because target 4 names it: it is `set_if_empty`'s never-overwrite twin.
    #[allow(dead_code)]
    fn get_or_set(&self, e: Arc<Emb>) -> Arc<Emb> {
        let mut guard = self.inner.write().unwrap();
        match guard.as_ref() {
            Some(existing) => Arc::clone(existing),
            None => {
                *guard = Some(Arc::clone(&e));
                e
            }
        }
    }
}

// ===========================================================================
// MODEL A (safety, GREEN): COHERENCE — `resolve_matching(A)` racing `swap(B)`.
//
// A connection thread resolves its embedder for its checkout-time model_config
// (A) while a foreign-model reconcile `swap`s the cell to B. The result the
// resolver hands back to the query MUST be an A embedder — pairing a B embedder
// with an A store snapshot would embed the query in the wrong space and return
// garbage against a mismatched-dim index. `want` drives the build in every arm,
// so this holds structurally; the model proves loom finds no schedule that
// breaks it.
// ===========================================================================
#[test]
fn resolve_matching_stays_coherent_across_a_concurrent_swap() {
    loom::model(|| {
        let cell = Arc::new(SharedEmb::new());

        // Resolver: a query resolving its own model (A).
        let c1 = Arc::clone(&cell);
        let resolver = thread::spawn(move || c1.resolve_matching(ModelTag::A).model);

        // Reconcile: a deliberate foreign-model swap to B.
        let c2 = Arc::clone(&cell);
        let swapper = thread::spawn(move || c2.swap(emb(ModelTag::B)));

        let got = resolver.join().unwrap();
        swapper.join().unwrap();

        assert_eq!(
            got,
            ModelTag::A,
            "COHERENCE VIOLATED: resolve_matching(A) returned a {got:?} embedder — a \
             concurrent swap(B) leaked its model into a query that asked for A"
        );
    });
}

// ===========================================================================
// MODEL B (safety, GREEN): NON-CLOBBER — a lazy `resolve_matching(A)` must never
// overwrite the model a deliberate `swap(B)` installed.
//
// The cell starts EMPTY (a cold query window). The resolver misses
// `get_matching(A)`, builds A, and races to install; concurrently a reconcile
// `swap`s B in (the authoritative model change). After both join, the cell must
// hold B — the deliberate swap's model — never the lazy resolver's A.
//
// Why it holds under EVERY schedule: the resolver installs (the `None` arm) ONLY
// when the cell is empty AT ITS WRITE-LOCK acquisition. If `swap(B)` wrote first,
// the resolver sees `Some(B)` and takes the non-clobber arm (no write). If the
// resolver wrote A first, `swap(B)` overwrites it last (swap is authoritative).
// There is no interleaving where the resolver's A is the final resident.
// ===========================================================================
#[test]
fn lazy_resolve_never_clobbers_a_concurrent_swap() {
    loom::model(|| {
        let cell = Arc::new(SharedEmb::new()); // empty: cold window.

        let c1 = Arc::clone(&cell);
        let resolver = thread::spawn(move || c1.resolve_matching(ModelTag::A));

        let c2 = Arc::clone(&cell);
        let swapper = thread::spawn(move || c2.swap(emb(ModelTag::B)));

        let r = resolver.join().unwrap();
        swapper.join().unwrap();

        // The resolver's own result is always coherent (A).
        assert_eq!(r.model, ModelTag::A, "resolver must get its own model A");
        // The resident is always the deliberate swap's model B.
        let resident = cell.get().expect("cell must be populated after swap(B)");
        assert_eq!(
            resident.model,
            ModelTag::B,
            "NON-CLOBBER VIOLATED: the lazy resolve_matching(A) left A in the cell, \
             overwriting the model swap(B) deliberately installed"
        );
    });
}

// ===========================================================================
// MODEL C (safety, GREEN): the DOUBLE-CHECK SHARING branch (shared.rs:85) — two
// concurrent `resolve_matching(M)` on an empty cell CONVERGE to ONE shared
// session. This is the branch a single-threaded test can never reach: it fires
// only when a second matching installer lands between a resolver's read-miss and
// its write-acquire.
//
// Both resolvers miss `get_matching(M)` (cell empty), both build a private M,
// then serialize on the write lock. The first installs its build (`None` arm)
// and returns it; the second sees `Some(existing)` with a matching model and
// returns `Arc::clone(existing)` (the double-check), discarding its own build. So
// the two results are `Arc::ptr_eq` — the daemon threads share one ONNX session
// rather than churning two. Convergence, not just coherence.
// ===========================================================================
#[test]
fn concurrent_resolve_shares_one_session_via_double_check() {
    loom::model(|| {
        let cell = Arc::new(SharedEmb::new());

        let c1 = Arc::clone(&cell);
        let r1 = thread::spawn(move || c1.resolve_matching(ModelTag::A));
        let c2 = Arc::clone(&cell);
        let r2 = thread::spawn(move || c2.resolve_matching(ModelTag::A));

        let a = r1.join().unwrap();
        let b = r2.join().unwrap();

        // Both coherent with the requested model.
        assert_eq!(a.model, ModelTag::A);
        assert_eq!(b.model, ModelTag::A);
        // And SHARED: the double-check made the loser adopt the winner's Arc.
        assert!(
            Arc::ptr_eq(&a, &b),
            "SHARING VIOLATED: two concurrent resolve_matching(A) returned distinct \
             sessions — the double-check-under-write-lock branch failed to converge \
             them onto one shared embedder"
        );
        // The cell holds that shared session.
        let resident = cell.get().expect("cell populated by the first installer");
        assert!(Arc::ptr_eq(&resident, &a));
    });
}

// ===========================================================================
// MODEL D (target 2 + 4, GREEN): daemon BOOT-SEED CONVERGENCE under the F1
// `adopt_embedder` model gate, racing a foreign promote on the watch cell.
//
// Faithful to `spawn_daemon_thread` (watch/daemon.rs:97-125). Two cells:
//   * `watch_cell` — the shared `daemon_embedder`, which a concurrent watch-loop
//     foreign promote (`maybe_rebind_active_slot`) may `swap` to some model.
//   * `ctx_cell`   — the daemon BatchContext's own `embedder`, seeded here.
//
// The ctx resolved its own model `M_CTX` from its store (`create_context`). The
// separately-resolved `daemon_model_config` (watch/mod.rs:1393) may be STALE —
// a promote can land between the two resolutions — so this model runs the
// adversarial `daemon_model_config = A` while `M_CTX = B`.
//
// The boot seed, transcribed:
//   1. `adopt(watch_cell.get())` — install into ctx_cell iff the watch resident
//      matches M_CTX (the F1 gate, context.rs:1201-1211) AND ctx_cell is empty.
//   2. on decline: build `daemon_model_config`, `set_if_empty` it into the watch
//      cell, `adopt` it into ctx_cell (gate again — declines the A build for the
//      B ctx).
//   3. `warm()` — iff ctx_cell empty, build M_CTX and `set_if_empty` (the
//      backstop that guarantees convergence).
//
// > **BOOT CONVERGENCE**: after the seed, `ctx_cell` holds an embedder for
// > M_CTX — never empty (warm backstops), never a wrong model (every install is
// > adopt-gated). The two cells self-heal independently; the query side is
// > coherent with its own store no matter how the promote interleaves.
//
// The watch swap target is a PARAMETER so both the adopt-share path (`watch`
// holds M_CTX → step 1 may adopt+share) and the decline path (`watch` holds a
// foreign C → step 1 declines, warm builds M_CTX) are covered.
// ===========================================================================
fn boot_seed_converges_with_watch_promote_to(watch_target: ModelTag) {
    loom::model(move || {
        const M_CTX: ModelTag = ModelTag::B; // ctx's own store model.
        const DAEMON_MODEL_CONFIG: ModelTag = ModelTag::A; // stale sibling resolution.

        let watch_cell = Arc::new(SharedEmb::new());
        let ctx_cell = Arc::new(SharedEmb::new());

        // Concurrent watch-loop foreign promote: swaps the shared watch cell.
        let wc = Arc::clone(&watch_cell);
        let watch = thread::spawn(move || wc.swap(emb(watch_target)));

        // The daemon boot-seed thread.
        let wc2 = Arc::clone(&watch_cell);
        let cc = Arc::clone(&ctx_cell);
        let boot = thread::spawn(move || {
            // ctx.adopt_embedder: gate on M_CTX, then set_if_empty.
            let adopt = |shared: Arc<Emb>| -> bool {
                if shared.model != M_CTX {
                    return false; // F1 model gate declines the mismatch.
                }
                cc.set_if_empty(shared)
            };

            // Step 1: seed from the shared watch cell (daemon.rs:107-109).
            let adopted = wc2.get().is_some_and(adopt);
            if !adopted {
                // Step 2: build daemon_model_config, share back, adopt
                // (daemon.rs:113-118). Build always succeeds in the model.
                let arc = emb(DAEMON_MODEL_CONFIG);
                wc2.set_if_empty(Arc::clone(&arc));
                adopt(arc);
            }
            // Step 3: warm() backstop (context.rs:1145-1160) — build M_CTX iff
            // the ctx cell is still empty.
            if cc.get().is_none() {
                let _ = cc.set_if_empty(emb(M_CTX));
            }
        });

        watch.join().unwrap();
        boot.join().unwrap();

        let resident = ctx_cell
            .get()
            .expect("BOOT CONVERGENCE VIOLATED: ctx cell empty after seed — warm did not backstop");
        assert_eq!(
            resident.model, M_CTX,
            "BOOT CONVERGENCE VIOLATED: ctx cell holds {:?}, not its own store model {:?} — \
             a foreign resident was adopted past the F1 gate, or warm built the wrong model",
            resident.model, M_CTX,
        );
    });
}

#[test]
fn boot_seed_converges_when_watch_holds_the_ctx_model() {
    // Watch promotes to M_CTX (B): step 1 may adopt+share the watch session.
    boot_seed_converges_with_watch_promote_to(ModelTag::B);
}

#[test]
fn boot_seed_converges_when_watch_holds_a_foreign_model() {
    // Watch promotes to a THIRD model C (neither daemon_model_config nor M_CTX):
    // step 1 declines, and warm must still build M_CTX.
    boot_seed_converges_with_watch_promote_to(ModelTag::C);
}

// ===========================================================================
// MODEL E (target 4, GREEN): the WATCH cell — `try_init_embedder`'s `swap`
// (rebuild.rs:292) racing the daemon boot's `set_if_empty` (daemon.rs:116).
//
// After a foreign rebind the watch loop's `model_config` is B; `try_init_embedder`
// gates on `get_matching(B)` and `swap`s a freshly built B in on a miss. The
// daemon boot may concurrently `set_if_empty(A)` (its own daemon_model_config).
// The invariant: the watch loop can always serve ITS model — after `try_init`
// runs, `get_matching(B)` is `Some`. `set_if_empty` never overwrites a resident,
// and `swap` unconditionally installs B, so no `set_if_empty(A)` interleaving can
// strand the watch loop on the wrong model.
// ===========================================================================
#[test]
fn watch_try_init_swap_wins_over_daemon_set_if_empty() {
    loom::model(|| {
        let watch_cell = Arc::new(SharedEmb::new());

        // Daemon boot seeds its own model A into the shared watch cell.
        let d = Arc::clone(&watch_cell);
        let daemon = thread::spawn(move || {
            d.set_if_empty(emb(ModelTag::A));
        });

        // Watch loop's try_init_embedder for its (post-rebind) model B.
        let w = Arc::clone(&watch_cell);
        let watch = thread::spawn(move || {
            if w.get_matching(ModelTag::B).is_none() {
                w.swap(emb(ModelTag::B)); // rebuild.rs:292 — single-writer swap.
            }
            w.get_matching(ModelTag::B)
        });

        daemon.join().unwrap();
        let served = watch.join().unwrap();

        assert!(
            served.is_some(),
            "WATCH CONVERGENCE VIOLATED: after try_init_embedder(B) the watch loop could \
             not get_matching(B) — a racing set_if_empty(A) stranded it on the wrong model"
        );
        // And the resident is the watch loop's model B (swap is authoritative for
        // the watch cell's single writer).
        let resident = watch_cell.get().expect("watch cell populated");
        assert_eq!(resident.model, ModelTag::B);
    });
}

// ===========================================================================
// MODEL F (`#[ignore]`, reproduces a PROTOCOL-level convergence gap that is NOT
// reachable in current production — a LATENT-FRAGILITY guard, not a live race and
// not a reverted-fix control): a lazy `resolve_matching(A)` in flight during a
// reconcile `clear()` RE-STRANDS the cleared cell with the stale model A,
// defeating the convergence the clear was meant to guarantee.
//
// ## The two individually-correct ops
//   * Op 1 — `resolve_matching(A)` (view.rs:839, context.rs:1261): a cold-window
//     query lazily resolving its own model A. Correct in isolation: it installs A
//     into an empty cell and shares it (the intended first-query warm-up).
//   * Op 2 — `reconcile_model_for_reopen`'s build-failure `clear()`
//     (context.rs:682): a foreign promote to B whose embedder build FAILED, so
//     the stale resident is evicted and `model_config` moved to B. Correct in
//     isolation: "Clearing lets the next resolve_matching install the new model
//     on its first successful build — the cell converges" (shared.rs:108-109).
//
// ## The invariant they compose to break
//   > CONVERGENCE: after a reconcile clears the cell and moves the logical model
//   > to B, the cell must not end up holding the stale model A — otherwise every
//   > subsequent `resolve_matching(B)` hits the non-clobber branch and rebuilds a
//   > PRIVATE B session per query, forever (until the next model promote).
//
// ## The breaking schedule (loom finds it)
//   1. Cell empty, logical model A. A query's `resolve_matching(A)` misses
//      `get_matching(A)` and builds A, but has NOT yet taken the write lock.
//   2. Reconcile fires (promote to B, B's build FAILS): `clear()` — cell already
//      empty, stays empty — and `model_config` becomes B.
//   3. The step-1 resolver takes the write lock, sees `None`, and INSTALLS A (the
//      `None` arm). Cell = Some(A), logical model = B.
//   4. A later `resolve_matching(B)` misses `get_matching(B)` (cell holds A),
//      builds B, takes the write lock, sees `Some(A)` (different) → non-clobber →
//      returns a PRIVATE B and installs nothing. The cell stays A; the churn is
//      permanent.
//
// The `clear()`-then-`None`-arm-install is the hole: unlike `swap(B)` (which
// leaves `Some(B)` that blocks the resolver's `None` arm — see MODEL B, GREEN),
// `clear()` leaves the cell empty, so an in-flight stale-model resolver can
// re-populate it with the OLD model the reconcile just evicted.
//
// ## Why it is NOT a live production race today (the refutation)
//   * `clear()` fires ONLY from context.rs:682, the `Err` arm of
//     `match Embedder::new(new_model)`. But `Embedder::new` → `new_inner`
//     (core.rs:180-226) is fully LAZY and has no fallible `?` — the ONNX load is
//     deferred to first `embed_query`. So the `Err` arm is effectively dead:
//     reconcile always takes the `swap` arm, never `clear`.
//   * Even the step-1 precondition (an in-flight cold-window resolver) is
//     unreachable in the daemon: `warm()` (context.rs:1145) runs BEFORE the ctx
//     is served (daemon.rs:125 precedes the accept loop) and, being infallible,
//     installs M_CTX; and reconcile's `swap` keeps the cell consistent with
//     `model_config` under the outer lock, so `build_view`'s pre-seed
//     (`get_matching`, context.rs:1633) always hits and the lazy `resolve_matching`
//     fallback is never taken.
//   So the whole schedule rests on the currently-dead `clear()` path. It becomes
//   LIVE if any of those guards weaken: `Embedder::new` made eager/fallible, a
//   second `clear()` caller added, or `warm`'s backstop removed — WITHOUT also
//   hardening `resolve_matching`'s `None` arm (e.g. gating the install on the
//   current model, or dropping the lazy `None`-arm install entirely so only
//   `swap`/`warm`/`adopt` populate the cell).
//
// SEVERITY if it went live: persistent + self-perpetuating (in the churn state
// EVERY B query is an in-flight lazy resolver, so the next `clear`-reconcile
// re-strands trivially); blast radius a ~500ms ONNX rebuild per query on the hot
// path. Kept as a durable guard; production intentionally NOT changed here.
// ===========================================================================
#[test]
#[ignore = "reproduces a PROTOCOL-level convergence gap NOT reachable in \
            current production: a lazy resolve_matching(A) in flight during a reconcile \
            clear() re-strands the cleared cell with stale model A. Gated behind the \
            effectively-dead clear() path (Embedder::new is infallible-lazy) + warm's \
            infallible backstop. Latent-fragility guard; run with --ignored to observe."]
fn resolve_matching_restrands_a_cleared_cell() {
    loom::model(|| {
        // Cold window: cell empty, logical model A.
        let cell = Arc::new(SharedEmb::new());

        // Op 1: the in-flight cold-window resolver for the OLD model A.
        let c1 = Arc::clone(&cell);
        let stale_resolver = thread::spawn(move || c1.resolve_matching(ModelTag::A));

        // Op 2: reconcile's build-failure eviction (promote to B, B build failed).
        // The cell-visible effect is exactly `clear()`.
        let c2 = Arc::clone(&cell);
        let reconcile = thread::spawn(move || c2.clear());

        let _ = stale_resolver.join().unwrap();
        reconcile.join().unwrap();

        // The logical model is now B. A post-reconcile query resolves B; the cell
        // must converge to B (install it), not churn a private build against a
        // stranded A resident.
        let post = cell.resolve_matching(ModelTag::B);
        assert_eq!(
            post.model,
            ModelTag::B,
            "resolve always returns a coherent B"
        );
        let resident = cell.get();
        assert_eq!(
            resident.map(|e| e.model),
            Some(ModelTag::B),
            "CONVERGENCE VIOLATED: after the reconcile-clear-to-B, the cell holds {:?} — \
             an in-flight resolve_matching(A) re-stranded the cleared cell with the stale \
             model A, so every resolve_matching(B) now rebuilds a private session forever",
            cell.get().map(|e| e.model),
        );
    });
}
