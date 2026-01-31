"""Eval fixture for Python - realistic patterns for semantic search testing"""

import re
import json
import hashlib
import time
from typing import Dict, List, Optional, Any
from functools import wraps


def retry_with_backoff(func, max_retries: int = 3, initial_delay: float = 0.1):
    """Retry a function with exponential backoff."""
    delay = initial_delay
    for attempt in range(max_retries):
        try:
            return func()
        except Exception as e:
            if attempt == max_retries - 1:
                raise e
            time.sleep(delay)
            delay *= 2


def validate_email(email: str) -> bool:
    """Validate an email address format."""
    pattern = r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$'
    return bool(re.match(pattern, email))


def parse_json_config(path: str) -> Dict[str, Any]:
    """Parse JSON configuration from a file."""
    with open(path, 'r') as f:
        return json.load(f)


def hash_sha256(data: bytes) -> str:
    """Compute SHA256 hash of data."""
    return hashlib.sha256(data).hexdigest()


def format_currency(amount: float) -> str:
    """Format a number as currency with commas."""
    return "${:,.2f}".format(amount)


def http_post_json(url: str, body: dict) -> dict:
    """Send HTTP POST request with JSON body."""
    import urllib.request
    data = json.dumps(body).encode('utf-8')
    req = urllib.request.Request(url, data=data, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req) as response:
        return json.loads(response.read())


def read_file_utf8(path: str) -> str:
    """Read file contents with UTF-8 encoding."""
    with open(path, 'r', encoding='utf-8') as f:
        return f.read()


def write_file_atomic(path: str, content: str) -> None:
    """Write string to file atomically."""
    temp_path = path + '.tmp'
    with open(temp_path, 'w', encoding='utf-8') as f:
        f.write(content)
    import os
    os.rename(temp_path, path)


def calculate_mean(numbers: List[float]) -> float:
    """Calculate mean average of numbers."""
    if not numbers:
        return 0.0
    return sum(numbers) / len(numbers)


def find_maximum(numbers: List[int]) -> Optional[int]:
    """Find maximum value in list."""
    if not numbers:
        return None
    return max(numbers)


def camel_to_snake(s: str) -> str:
    """Convert camelCase to snake_case."""
    result = []
    for i, c in enumerate(s):
        if c.isupper() and i > 0:
            result.append('_')
        result.append(c.lower())
    return ''.join(result)


def truncate_string(s: str, max_len: int) -> str:
    """Truncate string to maximum length with ellipsis."""
    if len(s) <= max_len:
        return s
    return s[:max_len - 3] + '...'


def is_valid_uuid(s: str) -> bool:
    """Check if string is valid UUID format."""
    pattern = r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    return bool(re.match(pattern, s.lower()))


def generate_random_id(length: int = 16) -> str:
    """Generate random alphanumeric string."""
    import secrets
    return secrets.token_hex(length // 2)


def compress_rle(data: bytes) -> bytes:
    """Compress data using simple RLE encoding."""
    if not data:
        return b''
    result = []
    i = 0
    while i < len(data):
        byte = data[i]
        count = 1
        while i + count < len(data) and data[i + count] == byte and count < 255:
            count += 1
        result.extend([count, byte])
        i += count
    return bytes(result)


def parse_cli_args(args: List[str]) -> Dict[str, str]:
    """Parse command line arguments into key-value pairs."""
    result = {}
    i = 0
    while i < len(args):
        if args[i].startswith('--'):
            key = args[i][2:]
            value = args[i + 1] if i + 1 < len(args) else ''
            result[key] = value
            i += 2
        else:
            i += 1
    return result


def quicksort(arr: List[int]) -> List[int]:
    """Sort array using quicksort algorithm."""
    if len(arr) <= 1:
        return arr
    pivot = arr[len(arr) // 2]
    left = [x for x in arr if x < pivot]
    middle = [x for x in arr if x == pivot]
    right = [x for x in arr if x > pivot]
    return quicksort(left) + middle + quicksort(right)


def debounce(delay_seconds: float):
    """Debounce decorator for function calls."""
    last_call = [0.0]

    def decorator(func):
        @wraps(func)
        def wrapper(*args, **kwargs):
            now = time.time()
            if now - last_call[0] >= delay_seconds:
                last_call[0] = now
                return func(*args, **kwargs)
        return wrapper
    return decorator


def memoize(func):
    """Memoize function results in cache."""
    cache = {}

    @wraps(func)
    def wrapper(*args):
        if args not in cache:
            cache[args] = func(*args)
        return cache[args]
    return wrapper


def flatten_nested_list(nested: List) -> List:
    """Flatten a nested list into a single list."""
    result = []
    for item in nested:
        if isinstance(item, list):
            result.extend(flatten_nested_list(item))
        else:
            result.append(item)
    return result


def deep_merge_dicts(base: dict, override: dict) -> dict:
    """Deep merge two dictionaries."""
    result = base.copy()
    for key, value in override.items():
        if key in result and isinstance(result[key], dict) and isinstance(value, dict):
            result[key] = deep_merge_dicts(result[key], value)
        else:
            result[key] = value
    return result
