// Hard eval fixture for Rust - confusable functions that test fine-grained semantic distinction

use std::collections::HashMap;

/// Sort array using merge sort - stable divide and conquer algorithm
/// Preserves relative order of equal elements unlike quicksort
pub fn merge_sort<T: Ord + Clone>(arr: &mut [T]) {
    if arr.len() <= 1 {
        return;
    }
    let mid = arr.len() / 2;
    let mut left = arr[..mid].to_vec();
    let mut right = arr[mid..].to_vec();
    merge_sort(&mut left);
    merge_sort(&mut right);
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        if left[i] <= right[j] {
            arr[k] = left[i].clone();
            i += 1;
        } else {
            arr[k] = right[j].clone();
            j += 1;
        }
        k += 1;
    }
    while i < left.len() {
        arr[k] = left[i].clone();
        i += 1;
        k += 1;
    }
    while j < right.len() {
        arr[k] = right[j].clone();
        j += 1;
        k += 1;
    }
}

/// Sort array using heap sort with binary max-heap
/// Builds a max heap then repeatedly extracts the maximum element
pub fn heap_sort(arr: &mut [i32]) {
    let n = arr.len();
    // Build max heap
    for i in (0..n / 2).rev() {
        heapify(arr, n, i);
    }
    // Extract elements from heap one by one
    for i in (1..n).rev() {
        arr.swap(0, i);
        heapify(arr, i, 0);
    }
}

fn heapify(arr: &mut [i32], n: usize, i: usize) {
    let mut largest = i;
    let left = 2 * i + 1;
    let right = 2 * i + 2;
    if left < n && arr[left] > arr[largest] {
        largest = left;
    }
    if right < n && arr[right] > arr[largest] {
        largest = right;
    }
    if largest != i {
        arr.swap(i, largest);
        heapify(arr, n, largest);
    }
}

/// Sort array using insertion sort - efficient for small or nearly sorted arrays
/// Shifts elements to make room for each new element in sorted position
pub fn insertion_sort<T: Ord + Clone>(arr: &mut [T]) {
    for i in 1..arr.len() {
        let key = arr[i].clone();
        let mut j = i;
        while j > 0 && arr[j - 1] > key {
            arr[j] = arr[j - 1].clone();
            j -= 1;
        }
        arr[j] = key;
    }
}

/// Sort array using bubble sort with early termination
/// Repeatedly swaps adjacent elements, stops when no swaps needed
pub fn bubble_sort<T: Ord>(arr: &mut [T]) {
    let n = arr.len();
    for i in 0..n {
        let mut swapped = false;
        for j in 0..n - 1 - i {
            if arr[j] > arr[j + 1] {
                arr.swap(j, j + 1);
                swapped = true;
            }
        }
        if !swapped {
            break;
        }
    }
}

/// Sort non-negative integers using radix sort - processes digits from least significant
/// Non-comparison sort with O(d*n) time where d is digit count
pub fn radix_sort(arr: &mut [u32]) {
    if arr.is_empty() {
        return;
    }
    let max_val = *arr.iter().max().unwrap();
    let mut exp = 1u32;
    let mut output = vec![0u32; arr.len()];
    while max_val / exp > 0 {
        let mut count = [0usize; 10];
        for &val in arr.iter() {
            count[((val / exp) % 10) as usize] += 1;
        }
        for i in 1..10 {
            count[i] += count[i - 1];
        }
        for &val in arr.iter().rev() {
            let digit = ((val / exp) % 10) as usize;
            count[digit] -= 1;
            output[count[digit]] = val;
        }
        arr.copy_from_slice(&output);
        exp *= 10;
    }
}

/// Pad string to fixed width with a fill character
/// If string is shorter than width, pads on the left with fill char
pub fn pad_string(s: &str, width: usize, fill: char) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        let padding = width - s.len();
        let pad: String = std::iter::repeat(fill).take(padding).collect();
        format!("{}{}", pad, s)
    }
}

/// Reverse the characters in a string
pub fn reverse_string(s: &str) -> String {
    s.chars().rev().collect()
}

/// Count the number of words in text separated by whitespace
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Extract all numeric values from a mixed text string
/// Returns integers and floating point numbers found in the text
pub fn extract_numbers(text: &str) -> Vec<f64> {
    let mut numbers = Vec::new();
    let mut current = String::new();
    let mut has_dot = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            current.push(c);
        } else if c == '.' && !has_dot && !current.is_empty() {
            current.push(c);
            has_dot = true;
        } else if !current.is_empty() {
            if let Ok(n) = current.parse::<f64>() {
                numbers.push(n);
            }
            current.clear();
            has_dot = false;
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<f64>() {
            numbers.push(n);
        }
    }
    numbers
}

/// Validate URL format - checks for valid scheme and hostname
pub fn validate_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) {
        let host = rest.split('/').next().unwrap_or("");
        !host.is_empty() && host.contains('.')
    } else {
        false
    }
}

/// Validate IP address - supports both IPv4 and IPv6 formats
pub fn validate_ip_address(addr: &str) -> bool {
    // Try IPv4: four octets 0-255
    if let Some(ipv4) = try_parse_ipv4(addr) {
        return ipv4;
    }
    // Try IPv6: eight groups of hex
    let groups: Vec<&str> = addr.split(':').collect();
    groups.len() == 8 && groups.iter().all(|g| {
        g.len() <= 4 && g.chars().all(|c| c.is_ascii_hexdigit())
    })
}

fn try_parse_ipv4(addr: &str) -> Option<bool> {
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    Some(parts.iter().all(|p| p.parse::<u8>().is_ok()))
}

/// Validate phone number with international country code prefix
/// Accepts formats like +1-555-123-4567 or +44 20 7946 0958
pub fn validate_phone(phone: &str) -> bool {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    let has_plus = phone.starts_with('+');
    has_plus && digits.len() >= 10 && digits.len() <= 15
}

/// Compute CRC32 checksum of byte data
/// Simple polynomial division checksum for error detection
pub fn hash_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Rate limiter using token bucket algorithm
/// Allows N calls per time window, rejects excess calls
pub struct RateLimiter {
    tokens: u32,
    max_tokens: u32,
    last_refill: std::time::Instant,
    refill_interval: std::time::Duration,
}

impl RateLimiter {
    pub fn new(max_per_second: u32) -> Self {
        Self {
            tokens: max_per_second,
            max_tokens: max_per_second,
            last_refill: std::time::Instant::now(),
            refill_interval: std::time::Duration::from_secs(1),
        }
    }

    /// Check if a call is allowed under the rate limit
    pub fn allow(&mut self) -> bool {
        self.refill();
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_refill) >= self.refill_interval {
            self.tokens = self.max_tokens;
            self.last_refill = now;
        }
    }
}

/// Circuit breaker - stops calling after consecutive failures
/// Transitions: Closed -> Open (after threshold failures) -> HalfOpen (after timeout) -> Closed
pub struct CircuitBreaker {
    failure_count: u32,
    threshold: u32,
    state: CircuitState,
    last_failure: Option<std::time::Instant>,
    reset_timeout: std::time::Duration,
}

#[derive(PartialEq)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitBreaker {
    pub fn new(threshold: u32, reset_timeout_ms: u64) -> Self {
        Self {
            failure_count: 0,
            threshold,
            state: CircuitState::Closed,
            last_failure: None,
            reset_timeout: std::time::Duration::from_millis(reset_timeout_ms),
        }
    }

    /// Check if calls should be allowed through the circuit
    pub fn should_allow(&mut self) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if let Some(last) = self.last_failure {
                    if std::time::Instant::now().duration_since(last) >= self.reset_timeout {
                        self.state = CircuitState::HalfOpen;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a failure - may trip the circuit to open state
    pub fn record_failure(&mut self) {
        self.failure_count += 1;
        self.last_failure = Some(std::time::Instant::now());
        if self.failure_count >= self.threshold {
            self.state = CircuitState::Open;
        }
    }

    /// Record a success - resets failure count and closes circuit
    pub fn record_success(&mut self) {
        self.failure_count = 0;
        self.state = CircuitState::Closed;
    }
}
