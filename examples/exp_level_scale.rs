//! EXPERIMENT (not production): A/B `modify_level_scale` on the cqs HNSW build.
//!
//! Tests the upstream maintainer's lever (hnswlib-rs#32): does reducing the
//! HNSW level scale (fewer layers) cut the parallel-insert self-unreachable-node
//! rate (#1693/#1702), and what does it cost in recall?
//!
//! Part A — self-unreachable rate vs level_scale under CPU-saturated parallel
//!          insert (the conditions that triggered the bug).
//! Part B — recall@k (R@5/R@10) vs brute-force ground truth, default vs reduced.
//!
//! Uses `hnsw_rs` directly because `modify_level_scale` is not exposed through
//! cqs's `HnswGraph` wrapper. Build params (M / ef_construction / ef_search /
//! max_layer / DistCosine) mirror exactly what cqs's `build_batched_with_dim`
//! uses for the chosen corpus size, so the result transfers to production.
//!
//! Run (real cqs embeddings):
//!   cargo run --release --features cuda-index --example exp_level_scale -- \
//!     --store /mnt/c/Projects/cqs/.cqs/slots/gemma/index.db \
//!     --count 4000 --builds 300 --recall-queries 500
//!
//! Run (synthetic, no store):
//!   cargo run --release --features cuda-index --example exp_level_scale -- \
//!     --count 4000 --builds 300 --recall-queries 500

use hnsw_rs::anndists::dist::distances::DistCosine;
use hnsw_rs::hnsw::Hnsw;
use std::time::Instant;

const DIM: usize = 768;

// ---- cqs tier defaults (mirror of hnsw::hnsw_tier_defaults) ----
// (M, ef_construction, ef_search)
fn tier_defaults(n: usize) -> (usize, usize, usize) {
    if n < 5_000 {
        (16, 100, 50)
    } else if n < 100_000 {
        (24, 200, 100)
    } else {
        (32, 400, 200)
    }
}
const MAX_LAYER: usize = 16;

struct Args {
    store: Option<String>,
    count: usize,
    builds: usize,
    recall_queries: usize,
    recall_builds: usize,
    skip_default: bool,
    skip_recall: bool,
    skip_aggressive: bool,
    /// Override M (max_nb_connection). None = tier default for n.
    m: Option<usize>,
    /// Override ef_construction. None = tier default for n.
    ef_construction: Option<usize>,
}

fn parse_args() -> Args {
    let mut a = Args {
        store: None,
        count: 4000,
        builds: 300,
        recall_queries: 500,
        recall_builds: 3,
        skip_default: false,
        skip_recall: false,
        skip_aggressive: false,
        m: None,
        ef_construction: None,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--store" => {
                a.store = Some(argv[i + 1].clone());
                i += 2;
            }
            "--count" => {
                a.count = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--builds" => {
                a.builds = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--recall-queries" => {
                a.recall_queries = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--recall-builds" => {
                a.recall_builds = argv[i + 1].parse().unwrap();
                i += 2;
            }
            "--skip-default" => {
                a.skip_default = true;
                i += 1;
            }
            "--skip-recall" => {
                a.skip_recall = true;
                i += 1;
            }
            "--skip-aggressive" => {
                // Drop the 0.3 scale; keep default + 0.5 only.
                a.skip_aggressive = true;
                i += 1;
            }
            "--M" | "--m" => {
                a.m = Some(argv[i + 1].parse().unwrap());
                i += 2;
            }
            "--ef-construction" => {
                a.ef_construction = Some(argv[i + 1].parse().unwrap());
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {other}");
                i += 1;
            }
        }
    }
    a
}

/// Load real cqs embeddings (768-dim) from a store, or synthesize if no store.
///
/// Reads the `embedding` blob column directly via sqlx, bypassing the Store
/// schema gate: the installed binary writes the live index at an older schema
/// than this branch's code expects, and `Store::open_readonly` refuses the
/// mismatch. Embeddings are stored as plain little-endian f32 (bytemuck cast),
/// so a raw blob read is exact — no schema-coupled deserialization involved.
/// The db is read read-only/immutable so the live daemon is untouched.
fn load_vectors(args: &Args) -> Vec<Vec<f32>> {
    if let Some(path) = &args.store {
        eprintln!("Loading real embeddings (raw blob, schema-agnostic) from {path} ...");
        let out = read_embeddings_raw(path, args.count);
        eprintln!("Loaded {} real {DIM}-dim vectors.", out.len());
        out
    } else {
        eprintln!("No --store: synthesizing {} {DIM}-dim vectors.", args.count);
        synth_vectors(args.count)
    }
}

/// Raw-blob embedding read via sqlx (immutable, read-only), independent of the
/// Store schema version. Returns up to `count` finite, non-zero 768-dim vectors.
fn read_embeddings_raw(path: &str, count: usize) -> Vec<Vec<f32>> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Row;
    use std::str::FromStr;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        // immutable=true: read a snapshot without touching WAL or taking locks,
        // safe alongside the live daemon writer.
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{path}"))
            .expect("sqlite connect opts")
            .read_only(true)
            .immutable(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("open sqlite");

        // Static SQL with a bound LIMIT (sqlx 0.9 requires &'static str).
        let rows = sqlx::query(
            "SELECT embedding FROM chunks \
             WHERE embedding IS NOT NULL AND needs_embedding = 0 \
             ORDER BY rowid LIMIT ?1",
        )
        .bind(count as i64)
        .fetch_all(&pool)
        .await
        .expect("query embeddings");

        let mut out: Vec<Vec<f32>> = Vec::with_capacity(rows.len());
        for row in rows {
            let bytes: Vec<u8> = row.get::<Vec<u8>, _>(0);
            if bytes.len() != DIM * 4 {
                continue;
            }
            let v: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&bytes).to_vec();
            if !v.iter().any(|x| *x != 0.0) || v.iter().any(|x| !x.is_finite()) {
                continue;
            }
            out.push(v);
        }
        out
    })
}

/// Deterministic synthetic vectors (normalized, varied). Mirrors the spirit of
/// the repro's make_test_embedding but at DIM=768 and the requested count.
fn synth_vectors(n: usize) -> Vec<Vec<f32>> {
    let mut out = Vec::with_capacity(n);
    // Simple xorshift PRNG for reproducibility without a dep.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    for _ in 0..n {
        let mut v = vec![0.0f32; DIM];
        for x in v.iter_mut() {
            *x = next() * 2.0 - 1.0;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        out.push(v);
    }
    out
}

/// Build an HNSW with the given level-scale modification factor (None = default,
/// no modification). Uses parallel_insert (rayon) — the CPU-saturated path that
/// triggers the self-unreachable bug.
fn build(
    vectors: &[Vec<f32>],
    scale_factor: Option<f64>,
    m: usize,
    ef_c: usize,
) -> Hnsw<'_, f32, DistCosine> {
    let n = vectors.len();
    let mut hnsw = Hnsw::<f32, DistCosine>::new(m, n, MAX_LAYER, ef_c, DistCosine);
    if let Some(f) = scale_factor {
        // modify_level_scale prints to stdout; route experiment output to stderr
        // so the noise doesn't corrupt the report. (Library prints unconditionally.)
        hnsw.modify_level_scale(f);
    }
    let data: Vec<(&Vec<f32>, usize)> = vectors.iter().enumerate().map(|(i, v)| (v, i)).collect();
    hnsw.parallel_insert(&data);
    hnsw
}

/// Count self-unreachable nodes — the #1693/#1702 bug oracle.
///
/// A node is *graph-unreachable* (the bug) when, querying its OWN exact vector
/// (self cosine distance = 0, so it must be rank-1 if the graph can reach it),
/// it does not appear even with a generous ef and k. We use large ef and large
/// k deliberately so that near-duplicate "crowding" in a dense real-embedding
/// corpus cannot masquerade as a miss: with self-distance 0 and ef≫k, the only
/// way the node fails to surface is if the parallel-build graph topology
/// (nondeterministic layer heights + insertion order — not a data race)
/// orphaned it from the graph. This is exactly the condition the repro guards.
fn count_self_unreachable(hnsw: &Hnsw<f32, DistCosine>, vectors: &[Vec<f32>]) -> usize {
    let n = vectors.len();
    // Generous, count-aware budgets so crowding can't cause a false miss.
    let ef = 400usize.min(n.max(1));
    let k = 100usize.min(n.max(1));
    let mut misses = 0usize;
    for (i, v) in vectors.iter().enumerate() {
        let neighbours = hnsw.search(v, k, ef);
        if !neighbours.iter().any(|nb| nb.d_id == i) {
            misses += 1;
        }
    }
    misses
}

fn pct(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        100.0 * num as f64 / den as f64
    }
}

fn main() {
    let args = parse_args();
    let vectors = load_vectors(&args);
    let n = vectors.len();
    if n == 0 {
        eprintln!("No vectors loaded; aborting.");
        std::process::exit(1);
    }
    let (tier_m, tier_ef_c, ef_s) = tier_defaults(n);
    // Tier defaults unless overridden by --M / --ef-construction. ef_search (the
    // recall query budget) stays at the tier default — we vary M, not the query
    // width. The default level scale is 1/ln(M), so M is exactly the knob whose
    // M-dependence the re-confirm is about.
    let m = args.m.unwrap_or(tier_m);
    let ef_c = args.ef_construction.unwrap_or(tier_ef_c);
    eprintln!(
        "\n=== EXPERIMENT: modify_level_scale A/B ===\n\
         dim={DIM} count={n} M={m}{} ef_construction={ef_c}{} ef_search={ef_s} max_layer={MAX_LAYER}\n\
         default level scale = 1/ln(M) = {:.4}\n\
         threads(rayon)={}\n",
        if args.m.is_some() { " (override)" } else { " (tier)" },
        if args.ef_construction.is_some() { " (override)" } else { " (tier)" },
        1.0 / (m as f64).ln(),
        rayon::current_num_threads()
    );

    // Scale factors to test. None = library default (factor 1.0).
    // Reduced: 0.5 (halve), 0.3 (aggressive, near the 0.2 floor).
    let all_scales: [(&str, Option<f64>); 3] = [
        ("default(1.0)", None),
        ("0.5", Some(0.5)),
        ("0.3", Some(0.3)),
    ];
    let scales: Vec<(&str, Option<f64>)> = all_scales
        .into_iter()
        .filter(|(_, f)| !(args.skip_default && f.is_none()))
        .filter(|(label, _)| !(args.skip_aggressive && *label == "0.3"))
        .collect();

    // ---------------- Part A: self-unreachable rate ----------------
    eprintln!("---- Part A: self-unreachable rate vs level_scale ----");
    eprintln!(
        "Builds per scale: {}  (parallel insert under CPU saturation)",
        args.builds
    );
    eprintln!(
        "Oracle: query own exact vector at ef=min(400,n) k=min(100,n); miss = graph-unreachable"
    );
    let mut part_a_summary: Vec<(String, usize, usize, usize, Vec<usize>)> = Vec::new();
    for &(label, factor) in &scales {
        let t0 = Instant::now();
        let mut builds_with_miss = 0usize;
        let mut total_misses = 0usize;
        let mut miss_counts: Vec<usize> = Vec::new();
        for b in 0..args.builds {
            let hnsw = build(&vectors, factor, m, ef_c);
            let misses = count_self_unreachable(&hnsw, &vectors);
            if misses > 0 {
                builds_with_miss += 1;
                miss_counts.push(misses);
            }
            total_misses += misses;
            if (b + 1) % 50 == 0 {
                eprintln!(
                    "  [{label}] {}/{} builds done, {builds_with_miss} with >=1 miss so far",
                    b + 1,
                    args.builds
                );
            }
        }
        let elapsed = t0.elapsed();
        eprintln!(
            "  [{label}] DONE in {elapsed:?}: {builds_with_miss}/{} builds with >=1 self-unreachable ({:.2}%); \
             total self-unreachable nodes across all builds = {total_misses}",
            args.builds,
            pct(builds_with_miss, args.builds)
        );
        part_a_summary.push((
            label.to_string(),
            builds_with_miss,
            total_misses,
            args.builds,
            miss_counts,
        ));
    }

    // ---------------- Part B: recall cost ----------------
    let mut part_b_summary: Vec<(String, f64, f64)> = Vec::new();
    let nq = args.recall_queries.min(n);
    if args.skip_recall {
        eprintln!("\n---- Part B: SKIPPED (--skip-recall) ----");
    } else {
        eprintln!("\n---- Part B: recall@k vs level_scale ----");
        eprintln!(
            "Recall builds per scale: {}  queries per build: {}",
            args.recall_builds, args.recall_queries
        );
        // Query subset: first `recall_queries` vectors (queried as themselves +
        // brute-force ground truth so we measure true approximate recall, not
        // just self-recall). We query each with its own vector and ask: did
        // HNSW return the true top-K nearest (by brute force) within its top-K?
        // Precompute brute-force ground truth top-10 for each query vector ONCE
        // (independent of the graph).
        eprintln!("  Computing brute-force ground truth for {nq} queries (cosine)...");
        let gt: Vec<Vec<usize>> = (0..nq)
            .map(|qi| brute_force_topk(&vectors[qi], &vectors, 10))
            .collect();

        for &(label, factor) in &scales {
            let mut r5_sum = 0.0f64;
            let mut r10_sum = 0.0f64;
            for _ in 0..args.recall_builds {
                let hnsw = build(&vectors, factor, m, ef_c);
                let (r5, r10) = measure_recall(&hnsw, &vectors, &gt, nq, ef_s);
                r5_sum += r5;
                r10_sum += r10;
            }
            let r5 = r5_sum / args.recall_builds as f64;
            let r10 = r10_sum / args.recall_builds as f64;
            eprintln!("  [{label}] R@5={r5:.4}  R@10={r10:.4}");
            part_b_summary.push((label.to_string(), r5, r10));
        }
    }

    // ---------------- Final report (stdout) ----------------
    println!("\n================ EXPERIMENT REPORT ================");
    println!(
        "dataset: {} {DIM}-dim vectors ({}); M={m} ef_c={ef_c} ef_s={ef_s} max_layer={MAX_LAYER}; rayon_threads={}",
        n,
        if args.store.is_some() { "real cqs embeddings" } else { "synthetic" },
        rayon::current_num_threads()
    );
    println!("\n-- Part A: self-unreachable rate ({} builds each; oracle: own vector, ef=min(400,n), k=min(100,n)) --", args.builds);
    println!(
        "{:<14} {:>16} {:>18} {:>22}",
        "level_scale", "builds_w/_miss", "rate", "total_unreachable"
    );
    for (label, bw, total, builds, _mc) in &part_a_summary {
        println!(
            "{:<14} {:>10}/{:<5} {:>16.2}% {:>22}",
            label,
            bw,
            builds,
            pct(*bw, *builds),
            total
        );
    }
    println!("\n  per-build unreachable-count distribution (builds with >=1 miss):");
    for (label, _bw, _total, _builds, mc) in &part_a_summary {
        if mc.is_empty() {
            println!("    {label}: (none)");
        } else {
            let mut sorted = mc.clone();
            sorted.sort_unstable();
            let min = sorted[0];
            let max = *sorted.last().unwrap();
            let sum: usize = sorted.iter().sum();
            let mean = sum as f64 / sorted.len() as f64;
            println!(
                "    {label}: n={} min={min} max={max} mean={mean:.1} raw={:?}",
                sorted.len(),
                sorted
            );
        }
    }

    println!(
        "\n-- Part B: recall ({} builds each, {nq} queries) --",
        args.recall_builds
    );
    println!("{:<14} {:>10} {:>10}", "level_scale", "R@5", "R@10");
    for (label, r5, r10) in &part_b_summary {
        println!("{:<14} {:>10.4} {:>10.4}", label, r5, r10);
    }
    // Recall deltas vs default.
    if let Some((_, d5, d10)) = part_b_summary.first().cloned() {
        println!("\n  recall delta vs default:");
        for (label, r5, r10) in &part_b_summary {
            println!("    {label}: ΔR@5={:+.4}  ΔR@10={:+.4}", r5 - d5, r10 - d10);
        }
    }
    println!("==================================================");
}

/// Brute-force top-k nearest by cosine distance (1 - cosine similarity).
/// Vectors are normalized, so cosine similarity = dot product.
fn brute_force_topk(query: &[f32], vectors: &[Vec<f32>], k: usize) -> Vec<usize> {
    let mut sims: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let dot: f32 = query.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
            (i, dot)
        })
        .collect();
    sims.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sims.into_iter().take(k).map(|(i, _)| i).collect()
}

/// Recall@5 and Recall@10 averaged over `nq` queries: fraction of the brute-force
/// top-K that the HNSW search returns within its top-K.
fn measure_recall(
    hnsw: &Hnsw<f32, DistCosine>,
    vectors: &[Vec<f32>],
    gt: &[Vec<usize>],
    nq: usize,
    ef: usize,
) -> (f64, f64) {
    let mut r5_sum = 0.0f64;
    let mut r10_sum = 0.0f64;
    for qi in 0..nq {
        let neighbours = hnsw.search(&vectors[qi], 10, ef.max(20));
        let got: std::collections::HashSet<usize> = neighbours.iter().map(|nb| nb.d_id).collect();
        let gt_q = &gt[qi];
        // R@5
        let gt5 = &gt_q[..5.min(gt_q.len())];
        let hit5 = gt5.iter().filter(|&&i| got.contains(&i)).count();
        r5_sum += hit5 as f64 / gt5.len() as f64;
        // R@10
        let gt10 = &gt_q[..10.min(gt_q.len())];
        let hit10 = gt10.iter().filter(|&&i| got.contains(&i)).count();
        r10_sum += hit10 as f64 / gt10.len() as f64;
    }
    (r5_sum / nq as f64, r10_sum / nq as f64)
}
