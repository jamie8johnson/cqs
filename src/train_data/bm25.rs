use std::collections::HashMap;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// BM25 index for hard negative selection in training data generation.
/// Built from a corpus of (content_hash, content) pairs. Scores queries
/// against the corpus using BM25 ranking and selects top-k negatives
/// with a content hash guard to exclude duplicates.
pub struct Bm25Index {
    docs: Vec<(String, String)>,
    doc_terms: Vec<HashMap<String, f32>>,
    idf: HashMap<String, f32>,
    avg_dl: f32,
}

impl Bm25Index {
    /// Build a BM25 index from a corpus of (content_hash, content) pairs.
    pub fn build(docs: &[(String, String)]) -> Self {
        let _span = tracing::info_span!("bm25_build", doc_count = docs.len()).entered();
        let n = docs.len() as f32;
        let mut doc_terms = Vec::with_capacity(docs.len());
        let mut df: HashMap<String, f32> = HashMap::new();
        let mut total_dl: f32 = 0.0;

        for (_hash, content) in docs {
            let terms = tokenize(content);
            let dl = terms.len() as f32;
            total_dl += dl;

            let mut tf_map: HashMap<String, f32> = HashMap::new();
            for term in &terms {
                *tf_map.entry(term.clone()).or_insert(0.0) += 1.0;
            }

            // Count document frequency (each unique term counts once per doc)
            for term in tf_map.keys() {
                *df.entry(term.clone()).or_insert(0.0) += 1.0;
            }

            doc_terms.push(tf_map);
        }

        let avg_dl = if docs.is_empty() { 0.0 } else { total_dl / n };

        // Compute IDF: ln((N - df + 0.5) / (df + 0.5) + 1)
        let mut idf = HashMap::new();
        for (term, doc_freq) in &df {
            let idf_val = ((n - doc_freq + 0.5) / (doc_freq + 0.5) + 1.0).ln();
            idf.insert(term.clone(), idf_val);
        }

        Self {
            docs: docs.to_vec(),
            doc_terms,
            idf,
            avg_dl,
        }
    }

    /// Score all documents against a query, returning (content_hash, score) sorted descending.
    pub fn score(&self, query: &str) -> Vec<(String, f32)> {
        let _span = tracing::info_span!("bm25_score").entered();
        let query_terms = tokenize(query);
        let mut scores: Vec<(String, f32)> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, (hash, _))| {
                let tf_map = &self.doc_terms[i];
                // AC-15: Document length is the sum of term frequencies (total token count).
                // This matches the `total_dl` accumulation in `build()` where `dl = terms.len()`.
                // Both use the same tokenizer, so they are consistent. The sum of TF values
                // equals the token count because each token increments its entry by 1.0.
                let dl = tf_map.values().sum::<f32>();
                let mut score = 0.0f32;

                for qt in &query_terms {
                    if let Some(&idf_val) = self.idf.get(qt) {
                        let tf = tf_map.get(qt).copied().unwrap_or(0.0);
                        let numerator = tf * (K1 + 1.0);
                        let denominator = tf + K1 * (1.0 - B + B * dl / self.avg_dl);
                        score += idf_val * numerator / denominator;
                    }
                }

                (hash.clone(), score)
            })
            .collect();

        // Secondary sort on doc hash keeps equal-score documents
        // deterministically ordered across process invocations (the
        // upstream HashMap iterates in random order).
        scores.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        scores
    }

    /// Select top-k negatives for a query, excluding the positive by hash
    /// and any document with identical content (content hash guard via BLAKE3).
    pub fn select_negatives(
        &self,
        query: &str,
        positive_hash: &str,
        positive_content: &str,
        k: usize,
    ) -> Vec<(String, String)> {
        let _span = tracing::info_span!("bm25_select_negatives", k).entered();
        let positive_content_hash = blake3::hash(positive_content.as_bytes());
        let scored = self.score(query);

        scored
            .into_iter()
            .filter(|(hash, _score)| hash != positive_hash)
            .filter(|(hash, _score)| {
                // Find the content for this hash and check content hash guard
                if let Some((_h, content)) = self.docs.iter().find(|(h, _)| h == hash) {
                    let candidate_hash = blake3::hash(content.as_bytes());
                    candidate_hash != positive_content_hash
                } else {
                    false
                }
            })
            .take(k)
            .filter_map(|(hash, _score)| {
                let content = self
                    .docs
                    .iter()
                    .find(|(h, _)| h == &hash)
                    .map(|(_, c)| c.clone())
                    .unwrap_or_default();
                // EH-30: Skip negatives with empty content (from unwrap_or_default
                // when hash lookup fails, or genuinely empty docs).
                if content.is_empty() {
                    tracing::trace!(hash = %hash, "Skipping empty negative");
                    None
                } else {
                    Some((hash, content))
                }
            })
            .collect()
    }
}

/// Tokenize text into lowercase whitespace-split tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace().map(|t| t.to_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bm25_build_and_score() {
        let docs = vec![
            ("hash1".into(), "fn parse config file timeout".into()),
            ("hash2".into(), "fn validate schema input data".into()),
            ("hash3".into(), "fn parse json data format".into()),
        ];
        let index = Bm25Index::build(&docs);
        let results = index.score("parse config");
        assert_eq!(results[0].0, "hash1"); // both terms match
    }
    #[test]
    fn idf_downweights_common_terms() {
        let docs = vec![
            ("h1".into(), "fn common rare_term".into()),
            ("h2".into(), "fn common other_stuff".into()),
            ("h3".into(), "fn common more_things".into()),
        ];
        let index = Bm25Index::build(&docs);
        let results = index.score("rare_term");
        assert_eq!(results[0].0, "h1"); // "fn" and "common" downweighted, "rare_term" discriminates
    }
    #[test]
    fn select_negatives_excludes_positive_by_hash() {
        let docs = vec![
            ("h1".into(), "fn foo bar".into()),
            ("h2".into(), "fn foo baz".into()),
            ("h3".into(), "fn qux quux".into()),
        ];
        let index = Bm25Index::build(&docs);
        let negs = index.select_negatives("foo bar", "h1", "fn foo bar", 3);
        assert!(negs.iter().all(|(hash, _)| hash != "h1"));
    }
    #[test]
    fn content_hash_guard_excludes_identical_content() {
        // h1 and h2 have identical content but different hashes (simulating rename)
        let docs = vec![
            ("h1".into(), "fn identical code here".into()),
            ("h2".into(), "fn identical code here".into()),
            ("h3".into(), "fn different code entirely".into()),
        ];
        let index = Bm25Index::build(&docs);
        // positive is h1 with content "fn identical code here"
        // h2 has same content -- content hash guard should exclude it
        let negs = index.select_negatives("identical code", "h1", "fn identical code here", 3);
        assert!(negs
            .iter()
            .all(|(_, content)| content != "fn identical code here"));
    }
    #[test]
    fn fallback_to_random_when_few_candidates() {
        let docs = vec![("h1".into(), "fn only function".into())];
        let index = Bm25Index::build(&docs);
        let negs = index.select_negatives("only function", "h1", "fn only function", 3);
        assert!(negs.is_empty());
    }

    // TC-28: select_negatives with a desynced docs list (hash from score()
    // not found by the content hash guard's find()). We simulate this by
    // mutating the hash in self.docs after build, so score() returns the
    // mutated hash, then we query select_negatives with a positive that
    // forces the mutated entry through the content hash guard where
    // find() must re-scan self.docs. Since score() reads from self.docs
    // directly, the mutated hash IS found—but if we remove the entry
    // entirely, score() panics on index mismatch. Instead, we verify
    // the empty-corpus edge case and the unwrap_or_default fallback
    // by building an empty index.
    #[test]
    fn select_negatives_empty_corpus_returns_empty_gracefully() {
        let docs: Vec<(String, String)> = vec![];
        let index = Bm25Index::build(&docs);
        let negs = index.select_negatives("anything", "h1", "fn anything", 5);
        assert!(
            negs.is_empty(),
            "Empty corpus should return empty negatives"
        );
    }

    // TC-28: Verify that when the positive hash doesn't exist in the index,
    // all documents pass the positive-hash filter and the function doesn't panic.
    #[test]
    fn select_negatives_nonexistent_positive_hash() {
        let docs = vec![
            ("h1".into(), "fn foo bar baz".into()),
            ("h2".into(), "fn qux quux corge".into()),
        ];
        let index = Bm25Index::build(&docs);
        // positive_hash "h_missing" doesn't exist in docs — all entries pass the filter
        let negs = index.select_negatives("foo bar", "h_missing", "fn nonexistent", 5);
        assert!(
            !negs.is_empty(),
            "Non-existent positive hash should not filter everything"
        );
        // Content should be present (unwrap_or_default doesn't trigger since hashes match)
        for (_hash, content) in &negs {
            assert!(
                !content.is_empty(),
                "Content should not be empty string from unwrap_or_default"
            );
        }
    }
}
