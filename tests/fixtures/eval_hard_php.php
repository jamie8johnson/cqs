<?php
// Hard eval fixture for PHP - confusable functions that test fine-grained semantic distinction

/**
 * Sort array using merge sort - stable divide and conquer algorithm.
 * Preserves relative order of equal elements unlike quicksort.
 */
function mergeSort(array &$arr): void {
    $n = count($arr);
    if ($n <= 1) return;
    $mid = intdiv($n, 2);
    $left = array_slice($arr, 0, $mid);
    $right = array_slice($arr, $mid);
    mergeSort($left);
    mergeSort($right);
    $i = 0; $j = 0; $k = 0;
    while ($i < count($left) && $j < count($right)) {
        if ($left[$i] <= $right[$j]) {
            $arr[$k++] = $left[$i++];
        } else {
            $arr[$k++] = $right[$j++];
        }
    }
    while ($i < count($left)) $arr[$k++] = $left[$i++];
    while ($j < count($right)) $arr[$k++] = $right[$j++];
}

/**
 * Sort array using heap sort with binary max-heap.
 * Builds a max heap then repeatedly extracts the maximum element.
 */
function heapSort(array &$arr): void {
    $n = count($arr);
    for ($i = intdiv($n, 2) - 1; $i >= 0; $i--) {
        heapifyArray($arr, $n, $i);
    }
    for ($i = $n - 1; $i > 0; $i--) {
        [$arr[0], $arr[$i]] = [$arr[$i], $arr[0]];
        heapifyArray($arr, $i, 0);
    }
}

function heapifyArray(array &$arr, int $n, int $i): void {
    $largest = $i;
    $left = 2 * $i + 1;
    $right = 2 * $i + 2;
    if ($left < $n && $arr[$left] > $arr[$largest]) $largest = $left;
    if ($right < $n && $arr[$right] > $arr[$largest]) $largest = $right;
    if ($largest !== $i) {
        [$arr[$i], $arr[$largest]] = [$arr[$largest], $arr[$i]];
        heapifyArray($arr, $n, $largest);
    }
}

/**
 * Sort array using insertion sort - efficient for small or nearly sorted arrays.
 * Shifts elements to make room for each new element in sorted position.
 */
function insertionSort(array &$arr): void {
    $n = count($arr);
    for ($i = 1; $i < $n; $i++) {
        $key = $arr[$i];
        $j = $i - 1;
        while ($j >= 0 && $arr[$j] > $key) {
            $arr[$j + 1] = $arr[$j];
            $j--;
        }
        $arr[$j + 1] = $key;
    }
}

/**
 * Sort array using bubble sort with early termination.
 * Repeatedly swaps adjacent elements, stops when no swaps needed.
 */
function bubbleSort(array &$arr): void {
    $n = count($arr);
    for ($i = 0; $i < $n; $i++) {
        $swapped = false;
        for ($j = 0; $j < $n - 1 - $i; $j++) {
            if ($arr[$j] > $arr[$j + 1]) {
                [$arr[$j], $arr[$j + 1]] = [$arr[$j + 1], $arr[$j]];
                $swapped = true;
            }
        }
        if (!$swapped) break;
    }
}

/**
 * Sort non-negative integers using radix sort - processes digits from least significant.
 * Non-comparison sort with O(d*n) time where d is digit count.
 */
function radixSort(array &$arr): void {
    if (empty($arr)) return;
    $max = max($arr);
    for ($exp = 1; intdiv($max, $exp) > 0; $exp *= 10) {
        $output = array_fill(0, count($arr), 0);
        $count = array_fill(0, 10, 0);
        foreach ($arr as $val) {
            $count[intdiv($val, $exp) % 10]++;
        }
        for ($i = 1; $i < 10; $i++) {
            $count[$i] += $count[$i - 1];
        }
        for ($i = count($arr) - 1; $i >= 0; $i--) {
            $digit = intdiv($arr[$i], $exp) % 10;
            $count[$digit]--;
            $output[$count[$digit]] = $arr[$i];
        }
        $arr = $output;
    }
}

/**
 * Pad string to fixed width with a fill character.
 * If string is shorter than width, pads on the left with fill char.
 */
function padString(string $s, int $width, string $fill): string {
    if (strlen($s) >= $width) return $s;
    return str_repeat($fill, $width - strlen($s)) . $s;
}

/**
 * Reverse the characters in a string.
 */
function reverseString(string $s): string {
    return strrev($s);
}

/**
 * Count the number of words in text separated by whitespace.
 */
function countWords(string $text): int {
    $trimmed = trim($text);
    if ($trimmed === '') return 0;
    return count(preg_split('/\s+/', $trimmed));
}

/**
 * Extract all numeric values from a mixed text string.
 * Returns integers and floating point numbers found in the text.
 */
function extractNumbers(string $text): array {
    preg_match_all('/\d+\.?\d*/', $text, $matches);
    return array_map('floatval', $matches[0]);
}

/**
 * Validate URL format - checks for valid scheme and hostname.
 */
function validateUrl(string $url): bool {
    if (strpos($url, 'http://') === 0) {
        $rest = substr($url, 7);
    } elseif (strpos($url, 'https://') === 0) {
        $rest = substr($url, 8);
    } else {
        return false;
    }
    $host = explode('/', $rest)[0];
    return !empty($host) && strpos($host, '.') !== false;
}

/**
 * Validate IP address - supports IPv4 format with four octets 0-255.
 */
function validateIpAddress(string $addr): bool {
    $parts = explode('.', $addr);
    if (count($parts) !== 4) return false;
    foreach ($parts as $part) {
        if (!ctype_digit($part)) return false;
        $val = intval($part);
        if ($val < 0 || $val > 255) return false;
    }
    return true;
}

/**
 * Validate phone number with international country code prefix.
 * Accepts formats like +1-555-123-4567 or +44 20 7946 0958.
 */
function validatePhone(string $phone): bool {
    $digits = preg_replace('/[^0-9]/', '', $phone);
    return strpos($phone, '+') === 0 && strlen($digits) >= 10 && strlen($digits) <= 15;
}

/**
 * Compute CRC32 checksum of string data.
 * Polynomial division checksum for error detection.
 */
function hashCrc32(string $data): int {
    return crc32($data);
}

/**
 * Rate limiter using token bucket algorithm.
 * Allows N calls per time window, rejects excess calls.
 */
class RateLimiter {
    private int $tokens;
    private int $maxTokens;
    private float $lastRefill;

    public function __construct(int $maxPerSecond) {
        $this->tokens = $maxPerSecond;
        $this->maxTokens = $maxPerSecond;
        $this->lastRefill = microtime(true);
    }

    public function allow(): bool {
        $this->refill();
        if ($this->tokens > 0) {
            $this->tokens--;
            return true;
        }
        return false;
    }

    private function refill(): void {
        $now = microtime(true);
        if ($now - $this->lastRefill >= 1.0) {
            $this->tokens = $this->maxTokens;
            $this->lastRefill = $now;
        }
    }
}

/**
 * Circuit breaker - stops calling after consecutive failures.
 * Transitions: Closed -> Open (after threshold) -> HalfOpen (after timeout) -> Closed.
 */
class CircuitBreaker {
    private int $failureCount = 0;
    private int $threshold;
    private string $state = 'closed';
    private float $lastFailureTime = 0;
    private float $resetTimeoutSec;

    public function __construct(int $threshold, float $resetTimeoutSec) {
        $this->threshold = $threshold;
        $this->resetTimeoutSec = $resetTimeoutSec;
    }

    public function shouldAllow(): bool {
        if ($this->state === 'closed') return true;
        if ($this->state === 'open') {
            if (microtime(true) - $this->lastFailureTime >= $this->resetTimeoutSec) {
                $this->state = 'half_open';
                return true;
            }
            return false;
        }
        return true; // half_open: allow one probe
    }

    public function recordFailure(): void {
        $this->failureCount++;
        $this->lastFailureTime = microtime(true);
        if ($this->failureCount >= $this->threshold) {
            $this->state = 'open';
        }
    }

    public function recordSuccess(): void {
        $this->failureCount = 0;
        $this->state = 'closed';
    }
}

/**
 * Breadth-first search traversal of a graph from a starting node.
 * Visits all nodes reachable from start, level by level using a queue.
 */
function bfsTraversal(array $graph, int $start): array {
    $visited = [];
    $seen = [$start => true];
    $queue = [$start];
    while (!empty($queue)) {
        $node = array_shift($queue);
        $visited[] = $node;
        foreach ($graph[$node] ?? [] as $neighbor) {
            if (!isset($seen[$neighbor])) {
                $seen[$neighbor] = true;
                $queue[] = $neighbor;
            }
        }
    }
    return $visited;
}

/**
 * Depth-first search traversal of a graph from a starting node.
 * Visits nodes by exploring as deep as possible before backtracking, using a stack.
 */
function dfsTraversal(array $graph, int $start): array {
    $visited = [];
    $seen = [];
    $stack = [$start];
    while (!empty($stack)) {
        $node = array_pop($stack);
        if (isset($seen[$node])) continue;
        $seen[$node] = true;
        $visited[] = $node;
        $neighbors = $graph[$node] ?? [];
        foreach (array_reverse($neighbors) as $neighbor) {
            if (!isset($seen[$neighbor])) {
                $stack[] = $neighbor;
            }
        }
    }
    return $visited;
}

/**
 * LRU cache that evicts the least recently used entry when capacity is exceeded.
 * Uses array ordering to track access recency.
 */
class LruCache {
    private int $capacity;
    private array $cache = [];

    public function __construct(int $capacity) {
        $this->capacity = $capacity;
    }

    public function get(string $key): mixed {
        if (!array_key_exists($key, $this->cache)) return null;
        $value = $this->cache[$key];
        unset($this->cache[$key]);
        $this->cache[$key] = $value;
        return $value;
    }

    public function put(string $key, mixed $value): void {
        if (array_key_exists($key, $this->cache)) {
            unset($this->cache[$key]);
        } elseif (count($this->cache) >= $this->capacity) {
            reset($this->cache);
            unset($this->cache[key($this->cache)]);
        }
        $this->cache[$key] = $value;
    }
}

/**
 * TTL cache that expires entries after a configurable time-to-live duration.
 * Each entry has its own expiration timestamp; stale entries are removed on access.
 */
class TtlCache {
    private float $ttlSec;
    private array $store = [];

    public function __construct(float $ttlSec) {
        $this->ttlSec = $ttlSec;
    }

    public function get(string $key): mixed {
        if (!isset($this->store[$key])) return null;
        $entry = $this->store[$key];
        if (microtime(true) > $entry['expires_at']) {
            unset($this->store[$key]);
            return null;
        }
        return $entry['value'];
    }

    public function put(string $key, mixed $value): void {
        $this->store[$key] = [
            'value' => $value,
            'expires_at' => microtime(true) + $this->ttlSec,
        ];
    }

    public function evictExpired(): void {
        $now = microtime(true);
        foreach ($this->store as $key => $entry) {
            if ($now > $entry['expires_at']) {
                unset($this->store[$key]);
            }
        }
    }
}
