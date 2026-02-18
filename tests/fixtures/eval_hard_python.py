# Hard eval fixture for Python - confusable functions for semantic search testing

import time
import re
from typing import List, Optional


def merge_sort(arr: list) -> list:
    """Sort list using merge sort - stable divide and conquer algorithm.
    Preserves relative order of equal elements unlike quicksort."""
    if len(arr) <= 1:
        return arr
    mid = len(arr) // 2
    left = merge_sort(arr[:mid])
    right = merge_sort(arr[mid:])
    result = []
    i = j = 0
    while i < len(left) and j < len(right):
        if left[i] <= right[j]:
            result.append(left[i])
            i += 1
        else:
            result.append(right[j])
            j += 1
    result.extend(left[i:])
    result.extend(right[j:])
    return result


def heap_sort(arr: list) -> list:
    """Sort list using heap sort with binary max-heap.
    Builds a max heap then repeatedly extracts the maximum element."""
    def heapify(arr, n, i):
        largest = i
        left = 2 * i + 1
        right = 2 * i + 2
        if left < n and arr[left] > arr[largest]:
            largest = left
        if right < n and arr[right] > arr[largest]:
            largest = right
        if largest != i:
            arr[i], arr[largest] = arr[largest], arr[i]
            heapify(arr, n, largest)

    n = len(arr)
    for i in range(n // 2 - 1, -1, -1):
        heapify(arr, n, i)
    for i in range(n - 1, 0, -1):
        arr[0], arr[i] = arr[i], arr[0]
        heapify(arr, i, 0)
    return arr


def insertion_sort(arr: list) -> list:
    """Sort list using insertion sort - efficient for small or nearly sorted arrays.
    Shifts elements to make room for each new element in sorted position."""
    for i in range(1, len(arr)):
        key = arr[i]
        j = i - 1
        while j >= 0 and arr[j] > key:
            arr[j + 1] = arr[j]
            j -= 1
        arr[j + 1] = key
    return arr


def bubble_sort(arr: list) -> list:
    """Sort list using bubble sort with early termination.
    Repeatedly swaps adjacent elements, stops when no swaps needed."""
    n = len(arr)
    for i in range(n):
        swapped = False
        for j in range(0, n - i - 1):
            if arr[j] > arr[j + 1]:
                arr[j], arr[j + 1] = arr[j + 1], arr[j]
                swapped = True
        if not swapped:
            break
    return arr


def radix_sort(arr: list) -> list:
    """Sort non-negative integers using radix sort - processes digits from least significant.
    Non-comparison sort with O(d*n) time where d is digit count."""
    if not arr:
        return arr
    max_val = max(arr)
    exp = 1
    while max_val // exp > 0:
        counting_sort_by_digit(arr, exp)
        exp *= 10
    return arr


def counting_sort_by_digit(arr, exp):
    n = len(arr)
    output = [0] * n
    count = [0] * 10
    for val in arr:
        index = (val // exp) % 10
        count[index] += 1
    for i in range(1, 10):
        count[i] += count[i - 1]
    for val in reversed(arr):
        index = (val // exp) % 10
        count[index] -= 1
        output[count[index]] = val
    arr[:] = output


def pad_string(s: str, width: int, fill: str = ' ') -> str:
    """Pad string to fixed width with a fill character.
    If string is shorter than width, pads on the left with fill char."""
    if len(s) >= width:
        return s
    return fill * (width - len(s)) + s


def reverse_string(s: str) -> str:
    """Reverse the characters in a string."""
    return s[::-1]


def count_words(text: str) -> int:
    """Count the number of words in text separated by whitespace."""
    return len(text.split())


def extract_numbers(text: str) -> List[float]:
    """Extract all numeric values from a mixed text string.
    Returns integers and floating point numbers found in the text."""
    return [float(m) for m in re.findall(r'-?\d+\.?\d*', text)]


def validate_url(url: str) -> bool:
    """Validate URL format - checks for valid scheme and hostname."""
    if url.startswith('http://') or url.startswith('https://'):
        rest = url.split('://', 1)[1]
        host = rest.split('/')[0]
        return bool(host) and '.' in host
    return False


def validate_ip_address(addr: str) -> bool:
    """Validate IP address - supports both IPv4 and IPv6 formats."""
    # Try IPv4
    parts = addr.split('.')
    if len(parts) == 4:
        return all(p.isdigit() and 0 <= int(p) <= 255 for p in parts)
    # Try IPv6
    groups = addr.split(':')
    if len(groups) == 8:
        return all(len(g) <= 4 and all(c in '0123456789abcdefABCDEF' for c in g) for g in groups)
    return False


def validate_phone(phone: str) -> bool:
    """Validate phone number with international country code prefix.
    Accepts formats like +1-555-123-4567 or +44 20 7946 0958."""
    digits = ''.join(c for c in phone if c.isdigit())
    return phone.startswith('+') and 10 <= len(digits) <= 15


def hash_crc32(data: bytes) -> int:
    """Compute CRC32 checksum of byte data.
    Simple polynomial division checksum for error detection."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xEDB88320
            else:
                crc >>= 1
    return crc ^ 0xFFFFFFFF


class RateLimiter:
    """Rate limiter using token bucket algorithm.
    Allows N calls per time window, rejects excess calls."""

    def __init__(self, max_per_second: int):
        self.tokens = max_per_second
        self.max_tokens = max_per_second
        self.last_refill = time.time()

    def allow(self) -> bool:
        """Check if a call is allowed under the rate limit."""
        self._refill()
        if self.tokens > 0:
            self.tokens -= 1
            return True
        return False

    def _refill(self):
        now = time.time()
        if now - self.last_refill >= 1.0:
            self.tokens = self.max_tokens
            self.last_refill = now


class CircuitBreaker:
    """Circuit breaker - stops calling after consecutive failures.
    Transitions: Closed -> Open (after threshold) -> HalfOpen (after timeout) -> Closed."""

    def __init__(self, threshold: int, reset_timeout: float = 30.0):
        self.failure_count = 0
        self.threshold = threshold
        self.state = 'closed'
        self.last_failure: Optional[float] = None
        self.reset_timeout = reset_timeout

    def should_allow(self) -> bool:
        """Check if calls should be allowed through the circuit."""
        if self.state == 'closed':
            return True
        if self.state == 'open' and self.last_failure:
            if time.time() - self.last_failure >= self.reset_timeout:
                self.state = 'half_open'
                return True
            return False
        return self.state == 'half_open'

    def record_failure(self):
        """Record a failure - may trip the circuit to open state."""
        self.failure_count += 1
        self.last_failure = time.time()
        if self.failure_count >= self.threshold:
            self.state = 'open'

    def record_success(self):
        """Record a success - resets failure count and closes circuit."""
        self.failure_count = 0
        self.state = 'closed'
