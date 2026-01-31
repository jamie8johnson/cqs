// Eval fixture for TypeScript - realistic patterns for semantic search testing

/**
 * Retry an operation with exponential backoff
 */
export async function retryWithBackoff<T>(
    fn: () => Promise<T>,
    maxRetries: number = 3,
    initialDelay: number = 100
): Promise<T> {
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
    throw new Error('Unreachable');
}

/**
 * Validate an email address format
 */
export function validateEmail(email: string): boolean {
    const pattern = /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/;
    return pattern.test(email);
}

/**
 * Parse JSON configuration from string
 */
export function parseJsonConfig<T>(jsonString: string): T {
    return JSON.parse(jsonString) as T;
}

/**
 * Compute SHA256 hash of string (browser-compatible)
 */
export async function hashSha256(data: string): Promise<string> {
    const encoder = new TextEncoder();
    const dataBuffer = encoder.encode(data);
    const hashBuffer = await crypto.subtle.digest('SHA-256', dataBuffer);
    const hashArray = Array.from(new Uint8Array(hashBuffer));
    return hashArray.map(b => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Format a number as currency with commas
 */
export function formatCurrency(amount: number): string {
    return new Intl.NumberFormat('en-US', {
        style: 'currency',
        currency: 'USD',
    }).format(amount);
}

/**
 * Send HTTP POST request with JSON body
 */
export async function httpPostJson<T>(url: string, body: object): Promise<T> {
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
export function readFileUtf8(path: string): Promise<string> {
    const fs = require('fs').promises;
    return fs.readFile(path, 'utf-8');
}

/**
 * Write string to file atomically (Node.js)
 */
export async function writeFileAtomic(path: string, content: string): Promise<void> {
    const fs = require('fs').promises;
    const tempPath = path + '.tmp';
    await fs.writeFile(tempPath, content, 'utf-8');
    await fs.rename(tempPath, path);
}

/**
 * Calculate mean average of numbers
 */
export function calculateMean(numbers: number[]): number {
    if (numbers.length === 0) return 0;
    return numbers.reduce((sum, n) => sum + n, 0) / numbers.length;
}

/**
 * Find maximum value in array
 */
export function findMaximum(numbers: number[]): number | undefined {
    if (numbers.length === 0) return undefined;
    return Math.max(...numbers);
}

/**
 * Convert camelCase to snake_case
 */
export function camelToSnake(s: string): string {
    return s.replace(/([A-Z])/g, (match, p1, offset) =>
        (offset > 0 ? '_' : '') + p1.toLowerCase()
    );
}

/**
 * Truncate string to maximum length with ellipsis
 */
export function truncateString(s: string, maxLen: number): string {
    if (s.length <= maxLen) return s;
    return s.slice(0, maxLen - 3) + '...';
}

/**
 * Check if string is valid UUID format
 */
export function isValidUuid(s: string): boolean {
    const pattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
    return pattern.test(s);
}

/**
 * Generate random alphanumeric string
 */
export function generateRandomId(length: number = 16): string {
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
export function compressRle(data: string): string {
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
export function parseCliArgs(args: string[]): Record<string, string> {
    const result: Record<string, string> = {};
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
export function quicksort<T>(arr: T[]): T[] {
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
export function debounce<T extends (...args: any[]) => any>(
    fn: T,
    delayMs: number
): (...args: Parameters<T>) => void {
    let timeoutId: ReturnType<typeof setTimeout> | null = null;
    return (...args: Parameters<T>) => {
        if (timeoutId) clearTimeout(timeoutId);
        timeoutId = setTimeout(() => fn(...args), delayMs);
    };
}

/**
 * Memoize function results in cache
 */
export function memoize<T extends (...args: any[]) => any>(fn: T): T {
    const cache = new Map<string, ReturnType<T>>();
    return ((...args: Parameters<T>): ReturnType<T> => {
        const key = JSON.stringify(args);
        if (!cache.has(key)) {
            cache.set(key, fn(...args));
        }
        return cache.get(key)!;
    }) as T;
}

/**
 * Flatten a nested array into a single array
 */
export function flattenNestedArray<T>(nested: (T | T[])[]): T[] {
    return nested.reduce<T[]>((acc, item) => {
        if (Array.isArray(item)) {
            return [...acc, ...flattenNestedArray(item as (T | T[])[])];
        }
        return [...acc, item];
    }, []);
}

/**
 * Deep merge two objects
 */
export function deepMergeObjects<T extends object>(base: T, override: Partial<T>): T {
    const result = { ...base };
    for (const key in override) {
        const baseVal = result[key];
        const overrideVal = override[key];
        if (
            typeof baseVal === 'object' && baseVal !== null &&
            typeof overrideVal === 'object' && overrideVal !== null &&
            !Array.isArray(baseVal) && !Array.isArray(overrideVal)
        ) {
            (result as any)[key] = deepMergeObjects(baseVal as object, overrideVal as object);
        } else if (overrideVal !== undefined) {
            (result as any)[key] = overrideVal;
        }
    }
    return result;
}
