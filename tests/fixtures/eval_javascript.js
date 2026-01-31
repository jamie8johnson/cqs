// Eval fixture for JavaScript - realistic patterns for semantic search testing

/**
 * Retry an operation with exponential backoff
 */
async function retryWithBackoff(fn, maxRetries = 3, initialDelay = 100) {
    let delay = initialDelay;
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await fn();
        } catch (error) {
            if (attempt === maxRetries - 1) throw error;
            await new Promise(resolve => setTimeout(resolve, delay));
            delay *= 2;
        }
    }
}

/**
 * Validate an email address format
 */
function validateEmail(email) {
    const pattern = /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/;
    return pattern.test(email);
}

/**
 * Parse JSON configuration from string
 */
function parseJsonConfig(jsonString) {
    return JSON.parse(jsonString);
}

/**
 * Compute SHA256 hash of string (browser-compatible)
 */
async function hashSha256(data) {
    const encoder = new TextEncoder();
    const dataBuffer = encoder.encode(data);
    const hashBuffer = await crypto.subtle.digest('SHA-256', dataBuffer);
    const hashArray = Array.from(new Uint8Array(hashBuffer));
    return hashArray.map(b => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Format a number as currency with commas
 */
function formatCurrency(amount) {
    return new Intl.NumberFormat('en-US', {
        style: 'currency',
        currency: 'USD',
    }).format(amount);
}

/**
 * Send HTTP POST request with JSON body
 */
async function httpPostJson(url, body) {
    const response = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
    });
    return response.json();
}

/**
 * Read file contents (Node.js)
 */
function readFileUtf8(path) {
    const fs = require('fs');
    return fs.readFileSync(path, 'utf-8');
}

/**
 * Write string to file atomically (Node.js)
 */
function writeFileAtomic(path, content) {
    const fs = require('fs');
    const tempPath = path + '.tmp';
    fs.writeFileSync(tempPath, content, 'utf-8');
    fs.renameSync(tempPath, path);
}

/**
 * Calculate mean average of numbers
 */
function calculateMean(numbers) {
    if (numbers.length === 0) return 0;
    return numbers.reduce((sum, n) => sum + n, 0) / numbers.length;
}

/**
 * Find maximum value in array
 */
function findMaximum(numbers) {
    if (numbers.length === 0) return undefined;
    return Math.max(...numbers);
}

/**
 * Convert camelCase to snake_case
 */
function camelToSnake(s) {
    return s.replace(/([A-Z])/g, (match, p1, offset) =>
        (offset > 0 ? '_' : '') + p1.toLowerCase()
    );
}

/**
 * Truncate string to maximum length with ellipsis
 */
function truncateString(s, maxLen) {
    if (s.length <= maxLen) return s;
    return s.slice(0, maxLen - 3) + '...';
}

/**
 * Check if string is valid UUID format
 */
function isValidUuid(s) {
    const pattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    return pattern.test(s);
}

/**
 * Generate random alphanumeric string
 */
function generateRandomId(length = 16) {
    const chars = 'abcdefghijklmnopqrstuvwxyz0123456789';
    let result = '';
    for (let i = 0; i < length; i++) {
        result += chars.charAt(Math.floor(Math.random() * chars.length));
    }
    return result;
}

/**
 * Compress string using simple RLE encoding
 */
function compressRle(data) {
    if (!data) return '';
    let result = '';
    let i = 0;
    while (i < data.length) {
        const char = data[i];
        let count = 1;
        while (i + count < data.length && data[i + count] === char && count < 9) {
            count++;
        }
        result += count.toString() + char;
        i += count;
    }
    return result;
}

/**
 * Parse command line arguments into key-value pairs
 */
function parseCliArgs(args) {
    const result = {};
    for (let i = 0; i < args.length; i++) {
        if (args[i].startsWith('--')) {
            const key = args[i].slice(2);
            const value = args[i + 1] || '';
            result[key] = value;
            i++;
        }
    }
    return result;
}

/**
 * Sort array using quicksort algorithm
 */
function quicksort(arr) {
    if (arr.length <= 1) return arr;
    const pivot = arr[Math.floor(arr.length / 2)];
    const left = arr.filter(x => x < pivot);
    const middle = arr.filter(x => x === pivot);
    const right = arr.filter(x => x > pivot);
    return [...quicksort(left), ...middle, ...quicksort(right)];
}

/**
 * Debounce function calls with delay
 */
function debounce(fn, delayMs) {
    let timeoutId = null;
    return (...args) => {
        if (timeoutId) clearTimeout(timeoutId);
        timeoutId = setTimeout(() => fn(...args), delayMs);
    };
}

/**
 * Memoize function results in cache
 */
function memoize(fn) {
    const cache = new Map();
    return (...args) => {
        const key = JSON.stringify(args);
        if (!cache.has(key)) {
            cache.set(key, fn(...args));
        }
        return cache.get(key);
    };
}

/**
 * Flatten a nested array into a single array
 */
function flattenNestedArray(nested) {
    return nested.reduce((acc, item) => {
        if (Array.isArray(item)) {
            return [...acc, ...flattenNestedArray(item)];
        }
        return [...acc, item];
    }, []);
}

/**
 * Deep merge two objects
 */
function deepMergeObjects(base, override) {
    const result = { ...base };
    for (const key in override) {
        const baseVal = result[key];
        const overrideVal = override[key];
        if (
            typeof baseVal === 'object' && baseVal !== null &&
            typeof overrideVal === 'object' && overrideVal !== null &&
            !Array.isArray(baseVal) && !Array.isArray(overrideVal)
        ) {
            result[key] = deepMergeObjects(baseVal, overrideVal);
        } else if (overrideVal !== undefined) {
            result[key] = overrideVal;
        }
    }
    return result;
}

module.exports = {
    retryWithBackoff,
    validateEmail,
    parseJsonConfig,
    hashSha256,
    formatCurrency,
    httpPostJson,
    readFileUtf8,
    writeFileAtomic,
    calculateMean,
    findMaximum,
    camelToSnake,
    truncateString,
    isValidUuid,
    generateRandomId,
    compressRle,
    parseCliArgs,
    quicksort,
    debounce,
    memoize,
    flattenNestedArray,
    deepMergeObjects,
};
