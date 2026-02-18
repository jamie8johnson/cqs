package evalhard

import (
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"
)

// MergeSort sorts a slice using merge sort - stable divide and conquer algorithm.
// Preserves relative order of equal elements unlike quicksort.
func MergeSort(arr []int) []int {
	if len(arr) <= 1 {
		return arr
	}
	mid := len(arr) / 2
	left := MergeSort(arr[:mid])
	right := MergeSort(arr[mid:])
	result := make([]int, 0, len(arr))
	i, j := 0, 0
	for i < len(left) && j < len(right) {
		if left[i] <= right[j] {
			result = append(result, left[i])
			i++
		} else {
			result = append(result, right[j])
			j++
		}
	}
	result = append(result, left[i:]...)
	result = append(result, right[j:]...)
	return result
}

// HeapSort sorts a slice using heap sort with binary max-heap.
// Builds a max heap then repeatedly extracts the maximum element.
func HeapSort(arr []int) {
	n := len(arr)
	for i := n/2 - 1; i >= 0; i-- {
		heapify(arr, n, i)
	}
	for i := n - 1; i > 0; i-- {
		arr[0], arr[i] = arr[i], arr[0]
		heapify(arr, i, 0)
	}
}

func heapify(arr []int, n, i int) {
	largest := i
	left := 2*i + 1
	right := 2*i + 2
	if left < n && arr[left] > arr[largest] {
		largest = left
	}
	if right < n && arr[right] > arr[largest] {
		largest = right
	}
	if largest != i {
		arr[i], arr[largest] = arr[largest], arr[i]
		heapify(arr, n, largest)
	}
}

// InsertionSort sorts a slice using insertion sort - efficient for small or nearly sorted arrays.
// Shifts elements to make room for each new element in sorted position.
func InsertionSort(arr []int) {
	for i := 1; i < len(arr); i++ {
		key := arr[i]
		j := i - 1
		for j >= 0 && arr[j] > key {
			arr[j+1] = arr[j]
			j--
		}
		arr[j+1] = key
	}
}

// BubbleSort sorts a slice using bubble sort with early termination.
// Repeatedly swaps adjacent elements, stops when no swaps needed.
func BubbleSort(arr []int) {
	n := len(arr)
	for i := 0; i < n; i++ {
		swapped := false
		for j := 0; j < n-1-i; j++ {
			if arr[j] > arr[j+1] {
				arr[j], arr[j+1] = arr[j+1], arr[j]
				swapped = true
			}
		}
		if !swapped {
			break
		}
	}
}

// RadixSort sorts non-negative integers using radix sort - processes digits from least significant.
// Non-comparison sort with O(d*n) time where d is digit count.
func RadixSort(arr []int) {
	if len(arr) == 0 {
		return
	}
	maxVal := arr[0]
	for _, v := range arr {
		if v > maxVal {
			maxVal = v
		}
	}
	for exp := 1; maxVal/exp > 0; exp *= 10 {
		countingSortByDigit(arr, exp)
	}
}

func countingSortByDigit(arr []int, exp int) {
	n := len(arr)
	output := make([]int, n)
	count := make([]int, 10)
	for _, v := range arr {
		count[(v/exp)%10]++
	}
	for i := 1; i < 10; i++ {
		count[i] += count[i-1]
	}
	for i := n - 1; i >= 0; i-- {
		digit := (arr[i] / exp) % 10
		count[digit]--
		output[count[digit]] = arr[i]
	}
	copy(arr, output)
}

// PadString pads string to fixed width with a fill character.
// If string is shorter than width, pads on the left with fill char.
func PadString(s string, width int, fill rune) string {
	if len(s) >= width {
		return s
	}
	return strings.Repeat(string(fill), width-len(s)) + s
}

// ReverseString reverses the characters in a string.
func ReverseString(s string) string {
	runes := []rune(s)
	for i, j := 0, len(runes)-1; i < j; i, j = i+1, j-1 {
		runes[i], runes[j] = runes[j], runes[i]
	}
	return string(runes)
}

// CountWords counts the number of words in text separated by whitespace.
func CountWords(text string) int {
	return len(strings.Fields(text))
}

// ExtractNumbers extracts all numeric values from a mixed text string.
// Returns integers and floating point numbers found in the text.
func ExtractNumbers(text string) []float64 {
	re := regexp.MustCompile(`-?\d+\.?\d*`)
	matches := re.FindAllString(text, -1)
	numbers := make([]float64, 0, len(matches))
	for _, m := range matches {
		if n, err := strconv.ParseFloat(m, 64); err == nil {
			numbers = append(numbers, n)
		}
	}
	return numbers
}

// ValidateUrl validates URL format - checks for valid scheme and hostname.
func ValidateUrl(url string) bool {
	for _, prefix := range []string{"http://", "https://"} {
		if strings.HasPrefix(url, prefix) {
			rest := url[len(prefix):]
			host := strings.SplitN(rest, "/", 2)[0]
			return len(host) > 0 && strings.Contains(host, ".")
		}
	}
	return false
}

// ValidateIpAddress validates IP address - supports both IPv4 and IPv6 formats.
func ValidateIpAddress(addr string) bool {
	// IPv4
	parts := strings.Split(addr, ".")
	if len(parts) == 4 {
		for _, p := range parts {
			n, err := strconv.Atoi(p)
			if err != nil || n < 0 || n > 255 {
				return false
			}
		}
		return true
	}
	// IPv6
	groups := strings.Split(addr, ":")
	if len(groups) != 8 {
		return false
	}
	for _, g := range groups {
		if len(g) > 4 {
			return false
		}
		for _, c := range g {
			if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
				return false
			}
		}
	}
	return true
}

// ValidatePhone validates phone number with international country code prefix.
// Accepts formats like +1-555-123-4567 or +44 20 7946 0958.
func ValidatePhone(phone string) bool {
	digits := regexp.MustCompile(`\d`).FindAllString(phone, -1)
	return strings.HasPrefix(phone, "+") && len(digits) >= 10 && len(digits) <= 15
}

// HashCrc32 computes CRC32 checksum of byte data.
// Simple polynomial division checksum for error detection.
func HashCrc32(data []byte) uint32 {
	crc := uint32(0xFFFFFFFF)
	for _, b := range data {
		crc ^= uint32(b)
		for i := 0; i < 8; i++ {
			if crc&1 != 0 {
				crc = (crc >> 1) ^ 0xEDB88320
			} else {
				crc >>= 1
			}
		}
	}
	return crc ^ 0xFFFFFFFF
}

// RateLimiterGo implements rate limiting using token bucket algorithm.
// Allows N calls per time window, rejects excess calls.
type RateLimiterGo struct {
	mu          sync.Mutex
	tokens      int
	maxTokens   int
	lastRefill  time.Time
}

// NewRateLimiter creates a rate limiter allowing maxPerSecond calls per second.
func NewRateLimiter(maxPerSecond int) *RateLimiterGo {
	return &RateLimiterGo{
		tokens:     maxPerSecond,
		maxTokens:  maxPerSecond,
		lastRefill: time.Now(),
	}
}

// Allow checks if a call is allowed under the rate limit.
func (r *RateLimiterGo) Allow() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.refill()
	if r.tokens > 0 {
		r.tokens--
		return true
	}
	return false
}

func (r *RateLimiterGo) refill() {
	if time.Since(r.lastRefill) >= time.Second {
		r.tokens = r.maxTokens
		r.lastRefill = time.Now()
	}
}

// CircuitBreakerGo stops calling after consecutive failures.
// Transitions: Closed -> Open (after threshold) -> HalfOpen (after timeout) -> Closed.
type CircuitBreakerGo struct {
	mu           sync.Mutex
	failureCount int
	threshold    int
	state        string
	lastFailure  time.Time
	resetTimeout time.Duration
}

// NewCircuitBreaker creates a circuit breaker with failure threshold and reset timeout.
func NewCircuitBreaker(threshold int, resetTimeout time.Duration) *CircuitBreakerGo {
	return &CircuitBreakerGo{
		threshold:    threshold,
		state:        "closed",
		resetTimeout: resetTimeout,
	}
}

// ShouldAllow checks if calls should be allowed through the circuit.
func (cb *CircuitBreakerGo) ShouldAllow() bool {
	cb.mu.Lock()
	defer cb.mu.Unlock()
	switch cb.state {
	case "closed":
		return true
	case "open":
		if time.Since(cb.lastFailure) >= cb.resetTimeout {
			cb.state = "half_open"
			return true
		}
		return false
	case "half_open":
		return true
	}
	return false
}

// RecordFailure records a failure - may trip the circuit to open state.
func (cb *CircuitBreakerGo) RecordFailure() {
	cb.mu.Lock()
	defer cb.mu.Unlock()
	cb.failureCount++
	cb.lastFailure = time.Now()
	if cb.failureCount >= cb.threshold {
		cb.state = "open"
	}
}

// RecordSuccess records a success - resets failure count and closes circuit.
func (cb *CircuitBreakerGo) RecordSuccess() {
	cb.mu.Lock()
	defer cb.mu.Unlock()
	cb.failureCount = 0
	cb.state = "closed"
}
