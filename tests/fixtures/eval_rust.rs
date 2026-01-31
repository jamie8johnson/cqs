// Eval fixture for Rust - realistic patterns for semantic search testing

use std::collections::HashMap;
use std::time::Duration;

/// Retry an operation with exponential backoff
pub fn retry_with_backoff<F, T, E>(mut op: F, max_retries: u32) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
{
    let mut delay = Duration::from_millis(100);
    for attempt in 0..max_retries {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if attempt == max_retries - 1 => return Err(e),
            Err(_) => {
                std::thread::sleep(delay);
                delay *= 2;
            }
        }
    }
    unreachable!()
}

/// Validate an email address format
pub fn validate_email(email: &str) -> bool {
    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return false;
    }
    !parts[0].is_empty() && parts[1].contains('.')
}

/// Parse JSON configuration from a file
pub fn parse_json_config(path: &str) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    // Simplified JSON parsing
    let mut config = HashMap::new();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once(':') {
            config.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(config)
}

/// Compute SHA256 hash of data
pub fn hash_sha256(data: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Format a number as currency with commas
pub fn format_currency(amount: f64) -> String {
    let formatted = format!("{:.2}", amount);
    let parts: Vec<&str> = formatted.split('.').collect();
    let int_part = parts[0];
    let dec_part = parts.get(1).unwrap_or(&"00");

    let with_commas: String = int_part
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| if i > 0 && i % 3 == 0 { format!(",{}", c) } else { c.to_string() })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!("${}.{}", with_commas, dec_part)
}

/// Send HTTP POST request with JSON body
pub fn http_post_json(url: &str, body: &str) -> Result<String, std::io::Error> {
    // Simplified - real impl would use reqwest
    Ok(format!("POST {} with body: {}", url, body))
}

/// Read file contents with UTF-8 encoding
pub fn read_file_utf8(path: &str) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

/// Write string to file atomically
pub fn write_file_atomic(path: &str, content: &str) -> Result<(), std::io::Error> {
    let temp_path = format!("{}.tmp", path);
    std::fs::write(&temp_path, content)?;
    std::fs::rename(&temp_path, path)
}

/// Calculate mean average of numbers
pub fn calculate_mean(numbers: &[f64]) -> f64 {
    if numbers.is_empty() {
        return 0.0;
    }
    numbers.iter().sum::<f64>() / numbers.len() as f64
}

/// Find maximum value in slice
pub fn find_maximum(numbers: &[i32]) -> Option<i32> {
    numbers.iter().copied().max()
}

/// Convert camelCase to snake_case
pub fn camel_to_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_lowercase().next().unwrap());
    }
    result
}

/// Truncate string to maximum length with ellipsis
pub fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Check if string is valid UUID format
pub fn is_valid_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let lengths = [8, 4, 4, 4, 12];
    parts.iter().zip(lengths.iter()).all(|(p, &len)| {
        p.len() == len && p.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Generate random alphanumeric string
pub fn generate_random_id(length: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", seed)[..length.min(16)].to_string()
}

/// Compress data using simple RLE encoding
pub fn compress_rle(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let byte = data[i];
        let mut count = 1u8;
        while i + count as usize < data.len() && data[i + count as usize] == byte && count < 255 {
            count += 1;
        }
        result.push(count);
        result.push(byte);
        i += count as usize;
    }
    result
}

/// Parse command line arguments into key-value pairs
pub fn parse_cli_args(args: &[String]) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if args[i].starts_with("--") {
            let key = args[i][2..].to_string();
            let value = args.get(i + 1).cloned().unwrap_or_default();
            result.insert(key, value);
            i += 2;
        } else {
            i += 1;
        }
    }
    result
}

/// Sort array using quicksort algorithm
pub fn quicksort<T: Ord + Clone>(arr: &mut [T]) {
    if arr.len() <= 1 {
        return;
    }
    let pivot = arr.len() / 2;
    let pivot_val = arr[pivot].clone();
    let (left, right): (Vec<_>, Vec<_>) = arr.iter().cloned().partition(|x| x < &pivot_val);
    let mut sorted: Vec<_> = left;
    sorted.extend(right);
    arr.clone_from_slice(&sorted);
}

/// Debounce function calls with delay
pub struct Debouncer {
    last_call: std::time::Instant,
    delay: Duration,
}

impl Debouncer {
    pub fn new(delay_ms: u64) -> Self {
        Self {
            last_call: std::time::Instant::now(),
            delay: Duration::from_millis(delay_ms),
        }
    }

    pub fn should_execute(&mut self) -> bool {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_call) >= self.delay {
            self.last_call = now;
            true
        } else {
            false
        }
    }
}

/// Memoize function results in cache
pub struct Memoizer<K, V> {
    cache: HashMap<K, V>,
}

impl<K: std::hash::Hash + Eq, V: Clone> Memoizer<K, V> {
    pub fn new() -> Self {
        Self { cache: HashMap::new() }
    }

    pub fn get_or_compute<F>(&mut self, key: K, compute: F) -> V
    where
        F: FnOnce() -> V,
    {
        self.cache.entry(key).or_insert_with(compute).clone()
    }
}
