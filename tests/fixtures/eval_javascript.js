// Eval fixture for JavaScript - realistic patterns for semantic search testing

/**
 * Retry an operation with exponential backoff
 * @param {Function} fn - The async function to retry
 * @param {number} maxRetries - Maximum number of retry attempts
 * @param {number} initialDelay - Initial delay in milliseconds
 * @returns {Promise<*>} Result of the successful function call
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
 * @param {string} email - The email address to validate
 * @returns {boolean} True if the email format is valid
 */
function validateEmail(email) {
    const pattern = /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/;
    return pattern.test(email);
}

/**
 * Parse JSON configuration from string
 * @param {string} jsonString - The JSON string to parse
 * @returns {Object} Parsed configuration object
 */
function parseJsonConfig(jsonString) {
    return JSON.parse(jsonString);
}

/**
 * Compute SHA256 hash of string (browser-compatible)
 * @param {string} data - The string to hash
 * @returns {Promise<string>} Hex-encoded SHA256 hash
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
 * @param {number} amount - The amount to format
 * @returns {string} Formatted currency string
 */
function formatCurrency(amount) {
    return new Intl.NumberFormat('en-US', {
        style: 'currency',
        currency: 'USD',
    }).format(amount);
}

/**
 * Send HTTP POST request with JSON body
 * @param {string} url - The URL to send the request to
 * @param {Object} body - The JSON body to send
 * @returns {Promise<Object>} Parsed JSON response
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
 * @param {string} path - Path to the file
 * @returns {string} File contents as UTF-8 string
 */
function readFileUtf8(path) {
    const fs = require('fs');
    return fs.readFileSync(path, 'utf-8');
}

/**
 * Write string to file atomically (Node.js)
 * @param {string} path - Path to write to
 * @param {string} content - Content to write
 * @returns {void}
 */
function writeFileAtomic(path, content) {
    const fs = require('fs');
    const tempPath = path + '.tmp';
    fs.writeFileSync(tempPath, content, 'utf-8');
    fs.renameSync(tempPath, path);
}

/**
 * Calculate mean average of numbers
 * @param {number[]} numbers - Array of numbers
 * @returns {number} Mean average value
 */
function calculateMean(numbers) {
    if (numbers.length === 0) return 0;
    return numbers.reduce((sum, n) => sum + n, 0) / numbers.length;
}

/**
 * Find maximum value in array
 * @param {number[]} numbers - Array of numbers
 * @returns {number|undefined} Maximum value or undefined if empty
 */
function findMaximum(numbers) {
    if (numbers.length === 0) return undefined;
    return Math.max(...numbers);
}

/**
 * Convert camelCase to snake_case
 * @param {string} s - The camelCase string
 * @returns {string} The snake_case string
 */
function camelToSnake(s) {
    return s.replace(/([A-Z])/g, (match, p1, offset) =>
        (offset > 0 ? '_' : '') + p1.toLowerCase()
    );
}

/**
 * Truncate string to maximum length with ellipsis
 * @param {string} s - The string to truncate
 * @param {number} maxLen - Maximum length including ellipsis
 * @returns {string} Truncated string with ellipsis if needed
 */
function truncateString(s, maxLen) {
    if (s.length <= maxLen) return s;
    return s.slice(0, maxLen - 3) + '...';
}

/**
 * Check if string is valid UUID format
 * @param {string} s - The string to check
 * @returns {boolean} True if valid UUID format
 */
function isValidUuid(s) {
    const pattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    return pattern.test(s);
}

/**
 * Generate random alphanumeric string
 * @param {number} length - Desired length of the string
 * @returns {string} Random alphanumeric string
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
 * @param {string} data - String to compress
 * @returns {string} RLE-compressed string
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
 * @param {string[]} args - Array of command line arguments
 * @returns {Object} Parsed key-value pairs
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
 * @param {number[]} arr - Array to sort
 * @returns {number[]} Sorted array
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
 * @param {Function} fn - Function to debounce
 * @param {number} delayMs - Delay in milliseconds
 * @returns {Function} Debounced function
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
 * @param {Function} fn - Function to memoize
 * @returns {Function} Memoized function with cached results
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
 * @param {Array} nested - Nested array to flatten
 * @returns {Array} Flattened array
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
 * @param {Object} base - Base object
 * @param {Object} override - Object to merge into base
 * @returns {Object} Merged object
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
