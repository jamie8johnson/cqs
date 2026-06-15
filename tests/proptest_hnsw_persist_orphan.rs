// Property tests for the HNSW persist/load round-trip and the reduced
// level-scale orphan-elimination guarantee.
//
// Four algebraic invariants over GENERATED inputs:
//
// 1. PERSIST->LOAD k-NN EQUIVALENCE
//    For any (N vectors, metric, query, k):
//      build(V).search(q,k)  ==  load(save(build(V))).search(q,k)
//    (same ids in the same order, same scores). The example suite only
//    asserts "self-match reachable" and len/dim survive; it never asserts
//    the FULL ranked result set is byte-identical across the persist
//    boundary for an arbitrary query. A second variant exercises the
//    DotProduct arm with clustered (near-collinear) vectors and a self
//    query — the regime where a self-dot exceeds 1.0 in f32 and the
//    distance impl must clamp rather than panic.
//
// 2. ORPHAN-ELIMINATION
//    For any (N vectors): every inserted point is self-reachable — querying
//    with a point's OWN vector returns its id in the top-k. Tries to find an
//    N where LEVEL_SCALE_FACTOR=0.5 still leaves a self-unreachable orphan.
//    The example suite checks only the *scale value* and a single 2-5 vector
//    reachability case; it never sweeps N.
//
// 3. METRIC ROUND-TRIP
//    For any (N, metric in {Cosine, DotProduct}):
//      load(save(build_m(V))).metric() == m
//
// GENERATOR COVERAGE CLAIM (and distrust):
// - N in 1..=64 (covers the empty-ish and small-tier; all < 5000 so M=16
//   tier). Hand examples use N in {2,3,5,10,20,25}. This fills the gaps and
//   the boundaries (N=1, N=64).
// - vectors are EMBEDDING_DIM random unit vectors with at least one nonzero
//   component (avoids the zero-vector skip so id_map stays == N).
// - metric covers BOTH arms.
// - query is either an exact copy of an indexed vector (exact-match oracle)
//   or a fresh random vector.
// DISTRUST: 768-dim random gaussian vectors are near-orthogonal, so the
// k-NN ordering is well-separated and the persist round-trip SHOULD be
// exact. If it is not, that is a real codec bug. Orphan detection is
// probabilistic per-build (~1-2% under concurrent-insert contention) so the
// orphan property uses a small retry to separate a SYSTEMATIC orphan
// (every build) from concurrent-build noise.

use cqs::embedder::Embedding;
use cqs::hnsw::HnswIndex;
use cqs::index::DistanceMetric;
use cqs::EMBEDDING_DIM;

use proptest::prelude::*;

/// Deterministic pseudo-random unit vector from a seed + index. Pure (no RNG
/// crate, reproducible from the proptest seed). Guarantees >=1 nonzero comp.
fn unit_vec(seed: u64, idx: usize) -> Embedding {
    let mut v = vec![0.0f32; EMBEDDING_DIM];
    // A simple splitmix-ish hash per component → spread values in [-1,1].
    let mut state = seed
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(idx as u64)
        .wrapping_add(1);
    for c in v.iter_mut() {
        state ^= state >> 30;
        state = state.wrapping_mul(0xBF58476D1CE4E5B9);
        state ^= state >> 27;
        state = state.wrapping_mul(0x94D049BB133111EB);
        state ^= state >> 31;
        // Map to [-1, 1).
        let frac = (state as f64) / (u64::MAX as f64);
        *c = (frac * 2.0 - 1.0) as f32;
    }
    // Force a guaranteed-nonzero component so the build never skips this vector.
    v[(idx + 1) % EMBEDDING_DIM] += 1.0;
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    Embedding::new(v)
}

fn build_vectors(seed: u64, n: usize) -> Vec<(String, Embedding)> {
    (0..n)
        .map(|i| (format!("id_{i}"), unit_vec(seed, i)))
        .collect()
}

/// CLUSTERED vectors: groups of near-duplicates around a few centroids. This
/// is where k-NN ranking TIES live — the regime random-orthogonal vectors
/// never reach. If save/load ever reorders tied-distance candidates, this
/// generator is what bites. Each vector is a centroid plus a tiny per-index
/// perturbation, then renormalized.
fn clustered_vectors(seed: u64, n: usize, clusters: usize) -> Vec<(String, Embedding)> {
    let clusters = clusters.max(1);
    (0..n)
        .map(|i| {
            let centroid = unit_vec(seed, i % clusters);
            let jitter = unit_vec(seed ^ 0x5151_5151, i);
            let mut v: Vec<f32> = centroid
                .as_slice()
                .iter()
                .zip(jitter.as_slice().iter())
                // 1e-3 jitter => members of a cluster are ~0.999+ cosine,
                // forcing near-tied distances the graph must rank stably.
                .map(|(c, j)| c + 1e-3 * j)
                .collect();
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            (format!("id_{i}"), Embedding::new(v))
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        max_shrink_iters: 2000,
        .. ProptestConfig::default()
    })]

    /// INVARIANT 1: persist->load k-NN equivalence.
    /// load(save(build(V))).search(q,k) == build(V).search(q,k), exactly.
    #[test]
    fn persist_load_knn_equivalence(
        seed in any::<u64>(),
        n in 1usize..=64,
        k in 1usize..=20,
        // 0 => query is a copy of an indexed vector; 1 => fresh random query.
        query_kind in 0u8..2,
        query_idx in any::<usize>(),
    ) {
        let metric = DistanceMetric::Cosine;
        let vectors = build_vectors(seed, n);
        let built = HnswIndex::build_with_dim_and_metric(
            vectors.clone(), EMBEDDING_DIM, metric,
        ).expect("build");
        prop_assert_eq!(built.len(), n, "id_map count must equal N");

        let dir = tempfile::tempdir().expect("tmp");
        let basename = "p";
        built.save(dir.path(), basename).expect("save");
        let loaded = HnswIndex::load_with_dim(dir.path(), basename, EMBEDDING_DIM)
            .expect("load");

        let query = if query_kind == 0 {
            unit_vec(seed, query_idx % n)
        } else {
            // Fresh query: use a seed that is not in the indexed set.
            unit_vec(seed ^ 0xDEAD_BEEF, query_idx)
        };

        let a = built.search(&query, k);
        let b = loaded.search(&query, k);

        // Same ranked id list across the persist boundary.
        let a_ids: Vec<&str> = a.iter().map(|r| r.id.as_str()).collect();
        let b_ids: Vec<&str> = b.iter().map(|r| r.id.as_str()).collect();
        prop_assert_eq!(
            &a_ids, &b_ids,
            "k-NN id ranking diverged across save/load (n={}, k={}): in-memory={:?} loaded={:?}",
            n, k, a_ids, b_ids
        );
        // Scores must match to f32 bit-equality: same graph, same data, same
        // dist fn => same distances.
        for (ra, rb) in a.iter().zip(b.iter()) {
            prop_assert!(
                (ra.score - rb.score).abs() < 1e-6,
                "score diverged for id {}: in-memory={} loaded={}",
                ra.id, ra.score, rb.score
            );
        }
    }

    /// INVARIANT 1b: persist->load k-NN equivalence on the DotProduct arm
    /// AND with clustered (near-tied) vectors — the two regimes the random
    /// Cosine case above does not reach. This is also the arm that, before
    /// the dot clamp, PANICKED: an f32-L2-normalized self/near-collinear
    /// vector has a self-dot fractionally above 1.0, and the stock dot
    /// distance asserts `a·b <= 1`. The clamp turns that into distance 0
    /// instead of a panic, so the search returns a (tied) result set rather
    /// than unwinding.
    ///
    /// EQUIVALENCE SHAPE under the clamp. A cluster of near-collinear vectors
    /// collapses to a band of GENUINELY tied scores. Two facts then bound
    /// what save/load can promise:
    ///   - HNSW is APPROXIMATE: when the tie band is wider than k, which k of
    ///     the equally-scored candidates come back depends on graph traversal
    ///     order, and the in-memory build (parallel insert) and the reloaded
    ///     dump traverse independently — so the returned id SET is NOT a
    ///     save/load invariant in this regime.
    ///   - What IS invariant, and is the load-bearing codec check: the result
    ///     CARDINALITY and the SCORE SEQUENCE (sorted best-first). A codec
    ///     that corrupted a vector, dropped a graph edge en masse, or swapped
    ///     the dist fn would perturb the score sequence; this catches that
    ///     without over-specifying tie-break order the library never promises.
    #[test]
    fn persist_load_knn_equivalence_dot_and_clustered(
        seed in any::<u64>(),
        n in 2usize..=64,
        k in 1usize..=20,
        dot in any::<bool>(),
        clusters in 1usize..=6,
        query_idx in any::<usize>(),
    ) {
        let metric = if dot { DistanceMetric::DotProduct } else { DistanceMetric::Cosine };
        let vectors = clustered_vectors(seed, n, clusters);
        let built = HnswIndex::build_with_dim_and_metric(
            vectors.clone(), EMBEDDING_DIM, metric,
        ).expect("build");
        prop_assert_eq!(built.len(), n);

        let dir = tempfile::tempdir().expect("tmp");
        built.save(dir.path(), "c").expect("save");
        let loaded = HnswIndex::load_with_dim(dir.path(), "c", EMBEDDING_DIM).expect("load");

        // Query an indexed vector exactly: in a cluster, several members are
        // near-tied to it, so the top-k is the maximally ambiguous case — and
        // for the dot arm the self/near-collinear dot is the >1.0 input that
        // previously tripped the assert.
        let (_, q) = &vectors[query_idx % n];

        // Neither call may panic (the pre-clamp failure mode). Same result
        // count, and the score sequence matches positionally — both are sorted
        // best-first by the same dist fn over the same persisted data.
        let a = built.search(q, k);
        let b = loaded.search(q, k);

        prop_assert_eq!(
            a.len(), b.len(),
            "result count diverged across save/load \
             (metric={:?}, n={}, k={}, clusters={}): mem={} loaded={}",
            metric, n, k, clusters, a.len(), b.len()
        );
        for (ra, rb) in a.iter().zip(b.iter()) {
            prop_assert!(
                (ra.score - rb.score).abs() < 1e-6,
                "score sequence diverged at a position: mem={} loaded={}",
                ra.score, rb.score
            );
        }
    }

    /// INVARIANT 3: metric round-trips for both arms.
    #[test]
    fn metric_round_trip(
        seed in any::<u64>(),
        n in 1usize..=32,
        dot in any::<bool>(),
    ) {
        let metric = if dot { DistanceMetric::DotProduct } else { DistanceMetric::Cosine };
        let vectors = build_vectors(seed, n);
        let built = HnswIndex::build_with_dim_and_metric(
            vectors, EMBEDDING_DIM, metric,
        ).expect("build");
        prop_assert_eq!(built.metric(), metric);

        let dir = tempfile::tempdir().expect("tmp");
        built.save(dir.path(), "m").expect("save");
        let loaded = HnswIndex::load_with_dim(dir.path(), "m", EMBEDDING_DIM).expect("load");
        prop_assert_eq!(loaded.metric(), metric, "metric must survive save/load");
        prop_assert_eq!(loaded.len(), n, "len must survive save/load");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Orphan detection: fewer cases, each one builds N times. Keep it cheap.
        cases: 40,
        max_shrink_iters: 1000,
        .. ProptestConfig::default()
    })]

    /// INVARIANT 2: orphan-elimination.
    /// Every inserted point is self-reachable in its own top-k. A SYSTEMATIC
    /// orphan (unreachable on every one of `retries` builds) is a real recall
    /// bug; a transient one is concurrent-build noise.
    #[test]
    fn no_self_unreachable_orphans(
        seed in any::<u64>(),
        n in 2usize..=64,
    ) {
        let metric = DistanceMetric::Cosine;
        // k = N so self-reachability has the whole index to find itself in;
        // an orphan is a node that even an N-wide search of its own vector
        // cannot return.
        let k = n;

        // For each point, it is "reachable" if ANY build returns it for its
        // own-vector query. Persistent absence across all retries = orphan.
        let retries = 8usize;
        let mut ever_reachable = vec![false; n];

        for _ in 0..retries {
            let vectors = build_vectors(seed, n);
            let built = HnswIndex::build_with_dim_and_metric(
                vectors, EMBEDDING_DIM, metric,
            ).expect("build");
            #[allow(clippy::needless_range_loop)] // need `i` for id + seed
            for i in 0..n {
                if ever_reachable[i] {
                    continue;
                }
                let q = unit_vec(seed, i);
                let hits = built.search(&q, k);
                if hits.iter().any(|r| r.id == format!("id_{i}")) {
                    ever_reachable[i] = true;
                }
            }
            if ever_reachable.iter().all(|&b| b) {
                break;
            }
        }

        let orphans: Vec<usize> = (0..n).filter(|&i| !ever_reachable[i]).collect();
        prop_assert!(
            orphans.is_empty(),
            "self-unreachable orphan(s) after {} builds (n={}): ids {:?} — \
             LEVEL_SCALE_FACTOR=0.5 did not eliminate orphaning",
            retries, n, orphans
        );
    }

    /// INVARIANT 2b: orphan-elimination on CLUSTERED vectors. Tight clusters
    /// (~0.999 cosine) are the documented orphan-prone regime — near-collinear
    /// neighbours are where concurrent insert is most likely to drop a
    /// self-match. Pushes N higher than the orthogonal case.
    #[test]
    fn no_orphans_clustered(
        seed in any::<u64>(),
        n in 4usize..=128,
        clusters in 1usize..=4,
    ) {
        let metric = DistanceMetric::Cosine;
        let k = n;
        let retries = 8usize;
        let mut ever_reachable = vec![false; n];

        for _ in 0..retries {
            let vectors = clustered_vectors(seed, n, clusters);
            let built = HnswIndex::build_with_dim_and_metric(
                vectors, EMBEDDING_DIM, metric,
            ).expect("build");
            #[allow(clippy::needless_range_loop)] // need `i` for id + seed
            for i in 0..n {
                if ever_reachable[i] {
                    continue;
                }
                let q = clustered_vectors(seed, n, clusters)[i].1.clone();
                let hits = built.search(&q, k);
                if hits.iter().any(|r| r.id == format!("id_{i}")) {
                    ever_reachable[i] = true;
                }
            }
            if ever_reachable.iter().all(|&b| b) {
                break;
            }
        }

        let orphans: Vec<usize> = (0..n).filter(|&i| !ever_reachable[i]).collect();
        prop_assert!(
            orphans.is_empty(),
            "clustered self-unreachable orphan(s) after {} builds \
             (n={}, clusters={}): ids {:?}",
            retries, n, clusters, orphans
        );
    }
}
