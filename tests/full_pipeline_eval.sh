#!/usr/bin/env bash
# Full-pipeline hard eval — uses cqs search (enriched embeddings + RRF + FTS)
# Run after: cqs index && cqs index --llm-summaries
#
# This tests the PRODUCTION search path, not raw embeddings like model_eval.rs.
# Results are comparable to the 92.7% R@1 baseline (v5 + full pipeline).

set -euo pipefail

HITS=0
TOTAL=0
MISSES=""

eval_query() {
    local query="$1"
    local expected="$2"
    local lang="$3"
    local also_accept="$4"  # comma-separated

    TOTAL=$((TOTAL + 1))

    # Search with cqs, get top-5 results as JSON
    local results
    results=$(cqs "$query" --lang "$lang" --json 2>/dev/null)

    # Extract top-5 names
    local top5
    top5=$(echo "$results" | python3 -c "
import json, sys
data = json.load(sys.stdin)
names = [r['name'] for r in data.get('results', [])[:5]]
print(' '.join(names))
" 2>/dev/null)

    local rank1
    rank1=$(echo "$results" | python3 -c "
import json, sys
data = json.load(sys.stdin)
names = [r['name'] for r in data.get('results', [])[:10]]
expected = '$expected'
also = '$also_accept'.split(',') if '$also_accept' else []
accept = [expected] + [a for a in also if a]
for i, n in enumerate(names):
    if n in accept:
        print(i + 1)
        sys.exit(0)
print('miss')
" 2>/dev/null)

    if [ "$rank1" = "1" ]; then
        HITS=$((HITS + 1))
        echo "  + [$lang] \"$query\" -> $expected (rank 1) top5: $top5"
    elif [ "$rank1" != "miss" ] && [ "$rank1" -le 5 ] 2>/dev/null; then
        echo "  ~ [$lang] \"$query\" -> $expected (rank $rank1) top5: $top5"
    else
        echo "  - [$lang] \"$query\" -> $expected (rank $rank1) top5: $top5"
        MISSES="$MISSES\n  [$lang] \"$query\" exp=$expected got=$top5"
    fi
}

echo "=== Full Pipeline Hard Eval ==="
echo "Model: $(cqs --version 2>&1)"
echo ""

# Rust (11)
eval_query "stable sort preserving relative order of equal elements" "merge_sort" "rust" ""
eval_query "sort using binary max-heap data structure" "heap_sort" "rust" ""
eval_query "simple sort efficient for small nearly sorted arrays" "insertion_sort" "rust" ""
eval_query "non-comparison integer sort processing digits" "radix_sort" "rust" ""
eval_query "validate phone number with international country code" "validate_phone" "rust" ""
eval_query "check if URL has valid protocol and hostname" "validate_url" "rust" ""
eval_query "pad string to fixed width with fill character" "pad_string" "rust" ""
eval_query "count number of words in text" "count_words" "rust" ""
eval_query "extract numeric values from mixed text string" "extract_numbers" "rust" ""
eval_query "stop calling service after consecutive failures" "CircuitBreaker" "rust" "should_allow,record_failure"
eval_query "check whether circuit allows request through" "should_allow" "rust" "CircuitBreaker"

# Python (11)
eval_query "stable sort preserving relative order of equal elements" "merge_sort" "python" ""
eval_query "sort using binary max-heap data structure" "heap_sort" "python" ""
eval_query "simple sort efficient for small nearly sorted arrays" "insertion_sort" "python" ""
eval_query "non-comparison integer sort processing digits" "radix_sort" "python" ""
eval_query "validate phone number with international country code" "validate_phone" "python" ""
eval_query "check if URL has valid protocol and hostname" "validate_url" "python" ""
eval_query "pad string to fixed width with fill character" "pad_string" "python" ""
eval_query "count number of words in text" "count_words" "python" ""
eval_query "extract numeric values from mixed text string" "extract_numbers" "python" ""
eval_query "stop calling service after consecutive failures" "CircuitBreaker" "python" "should_allow,record_failure"
eval_query "check whether circuit allows request through" "should_allow" "python" "CircuitBreaker"

# TypeScript (11)
eval_query "stable sort preserving relative order of equal elements" "mergeSort" "typescript" ""
eval_query "sort using binary max-heap data structure" "heapSort" "typescript" ""
eval_query "simple sort efficient for small nearly sorted arrays" "insertionSort" "typescript" ""
eval_query "non-comparison integer sort processing digits" "radixSort" "typescript" ""
eval_query "validate phone number with international country code" "validatePhone" "typescript" ""
eval_query "check if URL has valid protocol and hostname" "validateUrl" "typescript" ""
eval_query "pad string to fixed width with fill character" "padString" "typescript" ""
eval_query "count number of words in text" "countWords" "typescript" ""
eval_query "extract numeric values from mixed text string" "extractNumbers" "typescript" ""
eval_query "stop calling service after consecutive failures" "CircuitBreaker" "typescript" "shouldAllow,recordFailure"
eval_query "check whether circuit allows request through" "shouldAllow" "typescript" "CircuitBreaker"

# JavaScript (11)
eval_query "stable sort preserving relative order of equal elements" "mergeSort" "javascript" ""
eval_query "sort using binary max-heap data structure" "heapSort" "javascript" ""
eval_query "simple sort efficient for small nearly sorted arrays" "insertionSort" "javascript" ""
eval_query "non-comparison integer sort processing digits" "radixSort" "javascript" ""
eval_query "validate phone number with international country code" "validatePhone" "javascript" ""
eval_query "check if URL has valid protocol and hostname" "validateUrl" "javascript" ""
eval_query "pad string to fixed width with fill character" "padString" "javascript" ""
eval_query "count number of words in text" "countWords" "javascript" ""
eval_query "extract numeric values from mixed text string" "extractNumbers" "javascript" ""
eval_query "stop calling service after consecutive failures" "CircuitBreaker" "javascript" "shouldAllow,recordFailure"
eval_query "check whether circuit allows request through" "shouldAllow" "javascript" "CircuitBreaker"

# Go (11)
eval_query "stable sort preserving relative order of equal elements" "MergeSort" "go" ""
eval_query "sort using binary max-heap data structure" "HeapSort" "go" ""
eval_query "simple sort efficient for small nearly sorted arrays" "InsertionSort" "go" ""
eval_query "non-comparison integer sort processing digits" "RadixSort" "go" ""
eval_query "validate phone number with international country code" "ValidatePhone" "go" ""
eval_query "check if URL has valid protocol and hostname" "ValidateURL" "go" ""
eval_query "pad string to fixed width with fill character" "PadString" "go" ""
eval_query "count number of words in text" "CountWords" "go" ""
eval_query "extract numeric values from mixed text string" "ExtractNumbers" "go" ""
eval_query "stop calling service after consecutive failures" "CircuitBreakerGo" "go" "RecordFailure"
eval_query "check whether circuit allows request through" "ShouldAllow" "go" "CircuitBreakerGo"

echo ""
echo "=== Results ==="
echo "Recall@1: $HITS/$TOTAL ($(echo "scale=1; $HITS * 100 / $TOTAL" | bc)%)"
if [ -n "$MISSES" ]; then
    echo ""
    echo "Misses:"
    echo -e "$MISSES"
fi
