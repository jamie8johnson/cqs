<?php
// Eval fixture for PHP - realistic patterns for semantic search testing

/**
 * Retry an operation with exponential backoff.
 */
function retryWithBackoff(callable $op, int $maxRetries = 3, float $initialDelay = 0.1): mixed {
    $delay = $initialDelay;
    $lastError = null;
    for ($attempt = 0; $attempt < $maxRetries; $attempt++) {
        try {
            return $op();
        } catch (\Exception $e) {
            $lastError = $e;
            if ($attempt < $maxRetries - 1) {
                usleep((int)($delay * 1_000_000));
                $delay *= 2;
            }
        }
    }
    throw $lastError;
}

/**
 * Validate an email address format.
 */
function validateEmail(string $email): bool {
    return (bool)preg_match('/^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/', $email);
}

/**
 * Parse JSON configuration from a file.
 */
function parseJsonConfig(string $path): array {
    $content = file_get_contents($path);
    if ($content === false) throw new \RuntimeException("Cannot read file: $path");
    $config = json_decode($content, true);
    if ($config === null && json_last_error() !== JSON_ERROR_NONE) {
        throw new \RuntimeException("Invalid JSON: " . json_last_error_msg());
    }
    return $config;
}

/**
 * Compute SHA256 hash of data.
 */
function hashSha256(string $data): string {
    return hash('sha256', $data);
}

/**
 * Format a number as currency with commas and dollar sign.
 */
function formatCurrency(float $amount): string {
    return '$' . number_format($amount, 2, '.', ',');
}

/**
 * Send HTTP POST request with JSON body.
 */
function httpPostJson(string $url, array $body): array {
    $opts = [
        'http' => [
            'method' => 'POST',
            'header' => "Content-Type: application/json\r\n",
            'content' => json_encode($body),
        ],
    ];
    $context = stream_context_create($opts);
    $response = file_get_contents($url, false, $context);
    if ($response === false) throw new \RuntimeException("HTTP POST failed");
    return json_decode($response, true);
}

/**
 * Read file contents with UTF-8 encoding.
 */
function readFileUtf8(string $path): string {
    $content = file_get_contents($path);
    if ($content === false) throw new \RuntimeException("Cannot read: $path");
    return mb_convert_encoding($content, 'UTF-8', mb_detect_encoding($content));
}

/**
 * Safely write data to file without corruption on crash.
 * Writes to a temporary file first, then atomically renames it into place.
 */
function writeFileAtomic(string $path, string $content): void {
    $tmp = $path . '.tmp.' . getmypid();
    if (file_put_contents($tmp, $content) === false) {
        throw new \RuntimeException("Failed to write temp file");
    }
    if (!rename($tmp, $path)) {
        unlink($tmp);
        throw new \RuntimeException("Failed to rename temp file");
    }
}

/**
 * Compute arithmetic average of a list of numbers.
 */
function calculateMean(array $values): float {
    if (empty($values)) return 0.0;
    return array_sum($values) / count($values);
}

/**
 * Find the largest element in an array.
 */
function findMaximum(array $arr): float {
    if (empty($arr)) throw new \InvalidArgumentException("Empty array");
    return max($arr);
}

/**
 * Create a unique random identifier string.
 * Generates a hex string from random bytes for use as a unique ID.
 */
function generateRandomId(): string {
    return bin2hex(random_bytes(16));
}

/**
 * Compress data using run-length encoding.
 * Consecutive repeated characters are replaced with character and count.
 */
function compressRle(string $input): string {
    if ($input === '') return '';
    $result = '';
    $current = $input[0];
    $count = 1;
    for ($i = 1; $i < strlen($input); $i++) {
        if ($input[$i] === $current) {
            $count++;
        } else {
            $result .= $current;
            if ($count > 1) $result .= $count;
            $current = $input[$i];
            $count = 1;
        }
    }
    $result .= $current;
    if ($count > 1) $result .= $count;
    return $result;
}

/**
 * Parse command-line flags and arguments into a map.
 * Supports --key=value and --flag (boolean) styles.
 */
function parseCliArgs(array $args): array {
    $result = [];
    foreach ($args as $arg) {
        if (str_starts_with($arg, '--')) {
            $stripped = substr($arg, 2);
            if (str_contains($stripped, '=')) {
                [$key, $value] = explode('=', $stripped, 2);
            } else {
                $key = $stripped;
                $value = 'true';
            }
            $result[$key] = $value;
        }
    }
    return $result;
}

/**
 * Delay function execution until input stops changing.
 * Returns a debounced callable that waits for quiet period before executing.
 */
function debounce(callable $func, float $delaySec): callable {
    $timer = null;
    return function() use ($func, $delaySec, &$timer) {
        if ($timer !== null) {
            // In real PHP, would use event loop for async delay
            $timer = null;
        }
        $timer = microtime(true) + $delaySec;
        // Simplified: in production use an event loop like ReactPHP
        $func();
    };
}

/**
 * Convert camelCase string to snake_case.
 */
function camelToSnake(string $input): string {
    return strtolower(preg_replace('/([A-Z])/', '_$1', lcfirst($input)));
}

/**
 * Truncate string to maximum length with ellipsis.
 */
function truncateString(string $s, int $maxLen): string {
    if (strlen($s) <= $maxLen) return $s;
    return substr($s, 0, $maxLen - 3) . '...';
}

/**
 * Check if string is a valid UUID format (8-4-4-4-12 hex).
 */
function isValidUuid(string $s): bool {
    return (bool)preg_match(
        '/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/', $s);
}

/**
 * Quicksort - partition-based in-place sorting using pivot selection.
 */
function quicksort(array &$arr, int $low, int $high): void {
    if ($low < $high) {
        $pivot = $arr[$high];
        $i = $low - 1;
        for ($j = $low; $j < $high; $j++) {
            if ($arr[$j] <= $pivot) {
                $i++;
                [$arr[$i], $arr[$j]] = [$arr[$j], $arr[$i]];
            }
        }
        [$arr[$i + 1], $arr[$high]] = [$arr[$high], $arr[$i + 1]];
        $pi = $i + 1;
        quicksort($arr, $low, $pi - 1);
        quicksort($arr, $pi + 1, $high);
    }
}

/**
 * Memoization wrapper - caches function results by serialized arguments.
 */
function memoize(callable $func): callable {
    $cache = [];
    return function() use ($func, &$cache) {
        $key = serialize(func_get_args());
        if (!array_key_exists($key, $cache)) {
            $cache[$key] = call_user_func_array($func, func_get_args());
        }
        return $cache[$key];
    };
}

/**
 * Recursively flatten nested arrays into a single flat array.
 */
function flattenNestedArray(array $nested): array {
    $result = [];
    foreach ($nested as $item) {
        if (is_array($item)) {
            $result = array_merge($result, flattenNestedArray($item));
        } else {
            $result[] = $item;
        }
    }
    return $result;
}

/**
 * Recursively merge two nested arrays (deep merge).
 */
function deepMergeArrays(array $base, array $override): array {
    $result = $base;
    foreach ($override as $key => $value) {
        if (isset($result[$key]) && is_array($result[$key]) && is_array($value)) {
            $result[$key] = deepMergeArrays($result[$key], $value);
        } else {
            $result[$key] = $value;
        }
    }
    return $result;
}

/**
 * Serialize array data to CSV format string.
 */
function serializeToCsv(array $rows): string {
    if (empty($rows)) return '';
    $headers = array_keys($rows[0]);
    $output = implode(',', $headers) . "\n";
    foreach ($rows as $row) {
        $values = [];
        foreach ($headers as $h) {
            $val = $row[$h] ?? '';
            if (str_contains($val, ',') || str_contains($val, '"')) {
                $val = '"' . str_replace('"', '""', $val) . '"';
            }
            $values[] = $val;
        }
        $output .= implode(',', $values) . "\n";
    }
    return $output;
}

/**
 * Serialize array data to simple XML string.
 */
function serializeToXml(array $data, string $rootTag): string {
    $xml = "<$rootTag>";
    foreach ($data as $key => $value) {
        $escaped = htmlspecialchars((string)$value, ENT_XML1);
        $xml .= "<$key>$escaped</$key>";
    }
    $xml .= "</$rootTag>";
    return $xml;
}

/**
 * Match string against a glob pattern with * and ? wildcards.
 */
function globMatch(string $pattern, string $input): bool {
    $regex = '/^' . str_replace(['\*', '\?'], ['.*', '.'], preg_quote($pattern, '/')) . '$/';
    return (bool)preg_match($regex, $input);
}

/**
 * Match string against a regular expression and return all capture groups.
 */
function regexMatchGroups(string $pattern, string $input): array {
    if (preg_match($pattern, $input, $matches)) {
        return $matches;
    }
    return [];
}

/**
 * Retry with fallback - try primary operation, on failure try fallback.
 */
function retryWithFallback(callable $primary, callable $fallback, int $maxRetries = 3): mixed {
    $lastError = null;
    for ($i = 0; $i < $maxRetries; $i++) {
        try {
            return $primary();
        } catch (\Exception $e) {
            $lastError = $e;
        }
    }
    try {
        return $fallback();
    } catch (\Exception $e) {
        throw new \RuntimeException("Primary and fallback both failed", 0, $lastError);
    }
}
