// Hard eval fixture for TypeScript - confusable functions for semantic search testing

/** Sort array using merge sort - stable divide and conquer algorithm.
 * Preserves relative order of equal elements unlike quicksort. */
export function mergeSort<T>(arr: T[], compare: (a: T, b: T) => number = (a: any, b: any) => a - b): T[] {
    if (arr.length <= 1) return arr;
    const mid = Math.floor(arr.length / 2);
    const left = mergeSort(arr.slice(0, mid), compare);
    const right = mergeSort(arr.slice(mid), compare);
    const result: T[] = [];
    let i = 0, j = 0;
    while (i < left.length && j < right.length) {
        if (compare(left[i], right[j]) <= 0) {
            result.push(left[i++]);
        } else {
            result.push(right[j++]);
        }
    }
    return result.concat(left.slice(i), right.slice(j));
}

/** Sort array using heap sort with binary max-heap.
 * Builds a max heap then repeatedly extracts the maximum element. */
export function heapSort(arr: number[]): number[] {
    const n = arr.length;
    function heapify(size: number, i: number) {
        let largest = i;
        const left = 2 * i + 1;
        const right = 2 * i + 2;
        if (left < size && arr[left] > arr[largest]) largest = left;
        if (right < size && arr[right] > arr[largest]) largest = right;
        if (largest !== i) {
            [arr[i], arr[largest]] = [arr[largest], arr[i]];
            heapify(size, largest);
        }
    }
    for (let i = Math.floor(n / 2) - 1; i >= 0; i--) heapify(n, i);
    for (let i = n - 1; i > 0; i--) {
        [arr[0], arr[i]] = [arr[i], arr[0]];
        heapify(i, 0);
    }
    return arr;
}

/** Sort array using insertion sort - efficient for small or nearly sorted arrays.
 * Shifts elements to make room for each new element in sorted position. */
export function insertionSort<T>(arr: T[], compare: (a: T, b: T) => number = (a: any, b: any) => a - b): T[] {
    for (let i = 1; i < arr.length; i++) {
        const key = arr[i];
        let j = i - 1;
        while (j >= 0 && compare(arr[j], key) > 0) {
            arr[j + 1] = arr[j];
            j--;
        }
        arr[j + 1] = key;
    }
    return arr;
}

/** Sort array using bubble sort with early termination.
 * Repeatedly swaps adjacent elements, stops when no swaps needed. */
export function bubbleSort<T>(arr: T[], compare: (a: T, b: T) => number = (a: any, b: any) => a - b): T[] {
    const n = arr.length;
    for (let i = 0; i < n; i++) {
        let swapped = false;
        for (let j = 0; j < n - 1 - i; j++) {
            if (compare(arr[j], arr[j + 1]) > 0) {
                [arr[j], arr[j + 1]] = [arr[j + 1], arr[j]];
                swapped = true;
            }
        }
        if (!swapped) break;
    }
    return arr;
}

/** Sort non-negative integers using radix sort - processes digits from least significant.
 * Non-comparison sort with O(d*n) time where d is digit count. */
export function radixSort(arr: number[]): number[] {
    if (arr.length === 0) return arr;
    const maxVal = Math.max(...arr);
    let exp = 1;
    while (Math.floor(maxVal / exp) > 0) {
        const output = new Array(arr.length);
        const count = new Array(10).fill(0);
        for (const val of arr) count[Math.floor(val / exp) % 10]++;
        for (let i = 1; i < 10; i++) count[i] += count[i - 1];
        for (let i = arr.length - 1; i >= 0; i--) {
            const digit = Math.floor(arr[i] / exp) % 10;
            output[--count[digit]] = arr[i];
        }
        arr.splice(0, arr.length, ...output);
        exp *= 10;
    }
    return arr;
}

/** Pad string to fixed width with a fill character.
 * If string is shorter than width, pads on the left with fill char. */
export function padString(s: string, width: number, fill: string = ' '): string {
    return s.length >= width ? s : fill.repeat(width - s.length) + s;
}

/** Reverse the characters in a string. */
export function reverseString(s: string): string {
    return s.split('').reverse().join('');
}

/** Count the number of words in text separated by whitespace. */
export function countWords(text: string): number {
    return text.trim().split(/\s+/).filter(w => w.length > 0).length;
}

/** Extract all numeric values from a mixed text string.
 * Returns integers and floating point numbers found in the text. */
export function extractNumbers(text: string): number[] {
    const matches = text.match(/-?\d+\.?\d*/g);
    return matches ? matches.map(Number) : [];
}

/** Validate URL format - checks for valid scheme and hostname. */
export function validateUrl(url: string): boolean {
    const match = url.match(/^https?:\/\/([^/]+)/);
    if (!match) return false;
    const host = match[1];
    return host.length > 0 && host.includes('.');
}

/** Validate IP address - supports both IPv4 and IPv6 formats. */
export function validateIpAddress(addr: string): boolean {
    // IPv4
    const v4parts = addr.split('.');
    if (v4parts.length === 4) {
        return v4parts.every(p => /^\d{1,3}$/.test(p) && parseInt(p) >= 0 && parseInt(p) <= 255);
    }
    // IPv6
    const v6groups = addr.split(':');
    return v6groups.length === 8 && v6groups.every(g => /^[0-9a-fA-F]{1,4}$/.test(g));
}

/** Validate phone number with international country code prefix.
 * Accepts formats like +1-555-123-4567 or +44 20 7946 0958. */
export function validatePhone(phone: string): boolean {
    const digits = phone.replace(/\D/g, '');
    return phone.startsWith('+') && digits.length >= 10 && digits.length <= 15;
}

/** Compute CRC32 checksum of string data.
 * Simple polynomial division checksum for error detection. */
export function hashCrc32(data: string): number {
    let crc = 0xFFFFFFFF;
    for (let i = 0; i < data.length; i++) {
        crc ^= data.charCodeAt(i);
        for (let j = 0; j < 8; j++) {
            crc = (crc & 1) ? (crc >>> 1) ^ 0xEDB88320 : crc >>> 1;
        }
    }
    return (crc ^ 0xFFFFFFFF) >>> 0;
}

/** Rate limiter using token bucket algorithm.
 * Allows N calls per time window, rejects excess calls. */
export class RateLimiter {
    private tokens: number;
    private lastRefill: number;

    constructor(private maxPerSecond: number) {
        this.tokens = maxPerSecond;
        this.lastRefill = Date.now();
    }

    allow(): boolean {
        this.refill();
        if (this.tokens > 0) {
            this.tokens--;
            return true;
        }
        return false;
    }

    private refill(): void {
        const now = Date.now();
        if (now - this.lastRefill >= 1000) {
            this.tokens = this.maxPerSecond;
            this.lastRefill = now;
        }
    }
}

/** Circuit breaker - stops calling after consecutive failures.
 * Transitions: Closed -> Open (after threshold) -> HalfOpen (after timeout) -> Closed. */
export class CircuitBreaker {
    private failureCount = 0;
    private state: 'closed' | 'open' | 'half_open' = 'closed';
    private lastFailure: number | null = null;

    constructor(private threshold: number, private resetTimeoutMs: number = 30000) {}

    shouldAllow(): boolean {
        if (this.state === 'closed') return true;
        if (this.state === 'open' && this.lastFailure) {
            if (Date.now() - this.lastFailure >= this.resetTimeoutMs) {
                this.state = 'half_open';
                return true;
            }
            return false;
        }
        return this.state === 'half_open';
    }

    recordFailure(): void {
        this.failureCount++;
        this.lastFailure = Date.now();
        if (this.failureCount >= this.threshold) this.state = 'open';
    }

    recordSuccess(): void {
        this.failureCount = 0;
        this.state = 'closed';
    }
}

// Test functions
function testMergeSort(): void {
    const data = [5, 3, 8, 1, 2];
    const result = mergeSort(data);
    console.assert(JSON.stringify(result) === JSON.stringify([1, 2, 3, 5, 8]));
}

function testHeapSort(): void {
    const data = [9, 4, 7, 1, 3];
    const result = heapSort(data);
    console.assert(JSON.stringify(result) === JSON.stringify([1, 3, 4, 7, 9]));
}

function testValidateEmail(): void {
    console.assert(validateEmail("user@example.com"));
    console.assert(!validateEmail("not-an-email"));
}

function testValidatePhone(): void {
    console.assert(validatePhone("+1-555-0100"));
    console.assert(!validatePhone("abc"));
}

function testCircuitBreaker(): void {
    const cb = new CircuitBreaker(3, 30000);
    console.assert(cb.shouldAllow());
}

function _mergeSorted(left: number[], right: number[]): number[] {
    const result: number[] = [];
    let i = 0, j = 0;
    while (i < left.length && j < right.length) {
        if (left[i] <= right[j]) {
            result.push(left[i++]);
        } else {
            result.push(right[j++]);
        }
    }
    return result.concat(left.slice(i), right.slice(j));
}

function _insertionSortSmall(arr: number[]): void {
    for (let i = 1; i < arr.length; i++) {
        const key = arr[i];
        let j = i;
        while (j > 0 && arr[j - 1] > key) {
            arr[j] = arr[j - 1];
            j--;
        }
        arr[j] = key;
    }
}
