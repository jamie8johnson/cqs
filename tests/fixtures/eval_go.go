// Eval fixture for Go - realistic patterns for semantic search testing
package fixtures

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"regexp"
	"strings"
	"sync"
	"time"
	"unicode"
)

// RetryWithBackoff retries an operation with exponential backoff
func RetryWithBackoff(op func() error, maxRetries int) error {
	delay := 100 * time.Millisecond
	for attempt := 0; attempt < maxRetries; attempt++ {
		err := op()
		if err == nil {
			return nil
		}
		if attempt == maxRetries-1 {
			return err
		}
		time.Sleep(delay)
		delay *= 2
	}
	return nil
}

// ValidateEmail validates an email address format
func ValidateEmail(email string) bool {
	pattern := `^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$`
	matched, _ := regexp.MatchString(pattern, email)
	return matched
}

// ParseJsonConfig parses JSON configuration from a file
func ParseJsonConfig(path string) (map[string]interface{}, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var config map[string]interface{}
	err = json.Unmarshal(data, &config)
	return config, err
}

// HashSha256 computes SHA256 hash of data
func HashSha256(data []byte) string {
	hash := sha256.Sum256(data)
	return hex.EncodeToString(hash[:])
}

// FormatCurrency formats a number as currency with commas
func FormatCurrency(amount float64) string {
	// Simple implementation without locale
	str := fmt.Sprintf("%.2f", amount)
	parts := strings.Split(str, ".")
	intPart := parts[0]
	decPart := parts[1]

	var result []byte
	for i, c := range intPart {
		if i > 0 && (len(intPart)-i)%3 == 0 {
			result = append(result, ',')
		}
		result = append(result, byte(c))
	}
	return "$" + string(result) + "." + decPart
}

// HttpPostJson sends HTTP POST request with JSON body (stub)
func HttpPostJson(url string, body interface{}) ([]byte, error) {
	// Real impl would use net/http
	jsonBody, err := json.Marshal(body)
	if err != nil {
		return nil, err
	}
	return []byte(fmt.Sprintf("POST %s with body: %s", url, jsonBody)), nil
}

// ReadFileUtf8 reads file contents with UTF-8 encoding
func ReadFileUtf8(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

// WriteFileAtomic writes string to file atomically
func WriteFileAtomic(path, content string) error {
	tempPath := path + ".tmp"
	if err := os.WriteFile(tempPath, []byte(content), 0644); err != nil {
		return err
	}
	return os.Rename(tempPath, path)
}

// CalculateMean calculates mean average of numbers
func CalculateMean(numbers []float64) float64 {
	if len(numbers) == 0 {
		return 0
	}
	sum := 0.0
	for _, n := range numbers {
		sum += n
	}
	return sum / float64(len(numbers))
}

// FindMaximum finds maximum value in slice
func FindMaximum(numbers []int) (int, bool) {
	if len(numbers) == 0 {
		return 0, false
	}
	max := numbers[0]
	for _, n := range numbers[1:] {
		if n > max {
			max = n
		}
	}
	return max, true
}

// CamelToSnake converts camelCase to snake_case
func CamelToSnake(s string) string {
	var result strings.Builder
	for i, r := range s {
		if unicode.IsUpper(r) && i > 0 {
			result.WriteRune('_')
		}
		result.WriteRune(unicode.ToLower(r))
	}
	return result.String()
}

// TruncateString truncates string to maximum length with ellipsis
func TruncateString(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen-3] + "..."
}

// IsValidUuid checks if string is valid UUID format
func IsValidUuid(s string) bool {
	pattern := `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`
	matched, _ := regexp.MatchString(pattern, strings.ToLower(s))
	return matched
}

// GenerateRandomId generates random alphanumeric string
func GenerateRandomId(length int) string {
	// Simple implementation using time-based seed
	chars := "abcdefghijklmnopqrstuvwxyz0123456789"
	result := make([]byte, length)
	seed := time.Now().UnixNano()
	for i := 0; i < length; i++ {
		result[i] = chars[(seed+int64(i))%int64(len(chars))]
	}
	return string(result)
}

// CompressRle compresses data using simple RLE encoding
func CompressRle(data []byte) []byte {
	if len(data) == 0 {
		return nil
	}
	var result []byte
	i := 0
	for i < len(data) {
		b := data[i]
		count := 1
		for i+count < len(data) && data[i+count] == b && count < 255 {
			count++
		}
		result = append(result, byte(count), b)
		i += count
	}
	return result
}

// ParseCliArgs parses command line arguments into key-value pairs
func ParseCliArgs(args []string) map[string]string {
	result := make(map[string]string)
	for i := 0; i < len(args); i++ {
		if strings.HasPrefix(args[i], "--") {
			key := args[i][2:]
			value := ""
			if i+1 < len(args) {
				value = args[i+1]
			}
			result[key] = value
			i++
		}
	}
	return result
}

// Quicksort sorts slice using quicksort algorithm
func Quicksort(arr []int) []int {
	if len(arr) <= 1 {
		return arr
	}
	pivot := arr[len(arr)/2]
	var left, middle, right []int
	for _, x := range arr {
		switch {
		case x < pivot:
			left = append(left, x)
		case x == pivot:
			middle = append(middle, x)
		default:
			right = append(right, x)
		}
	}
	result := Quicksort(left)
	result = append(result, middle...)
	result = append(result, Quicksort(right)...)
	return result
}

// Debouncer debounces function calls with delay
type Debouncer struct {
	mu       sync.Mutex
	lastCall time.Time
	delay    time.Duration
}

// NewDebouncer creates a new debouncer
func NewDebouncer(delayMs int) *Debouncer {
	return &Debouncer{
		delay: time.Duration(delayMs) * time.Millisecond,
	}
}

// ShouldExecute checks if enough time has passed since last call
func (d *Debouncer) ShouldExecute() bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	now := time.Now()
	if now.Sub(d.lastCall) >= d.delay {
		d.lastCall = now
		return true
	}
	return false
}

// Memoizer memoizes function results in cache
type Memoizer struct {
	mu    sync.RWMutex
	cache map[string]interface{}
}

// NewMemoizer creates a new memoizer
func NewMemoizer() *Memoizer {
	return &Memoizer{
		cache: make(map[string]interface{}),
	}
}

// GetOrCompute gets cached value or computes and stores it
func (m *Memoizer) GetOrCompute(key string, compute func() interface{}) interface{} {
	m.mu.RLock()
	if val, ok := m.cache[key]; ok {
		m.mu.RUnlock()
		return val
	}
	m.mu.RUnlock()

	m.mu.Lock()
	defer m.mu.Unlock()
	if val, ok := m.cache[key]; ok {
		return val
	}
	val := compute()
	m.cache[key] = val
	return val
}

// FlattenNestedSlice flattens a nested slice into a single slice
func FlattenNestedSlice(nested []interface{}) []interface{} {
	var result []interface{}
	for _, item := range nested {
		if slice, ok := item.([]interface{}); ok {
			result = append(result, FlattenNestedSlice(slice)...)
		} else {
			result = append(result, item)
		}
	}
	return result
}

// DeepMergeMaps deep merges two maps
func DeepMergeMaps(base, override map[string]interface{}) map[string]interface{} {
	result := make(map[string]interface{})
	for k, v := range base {
		result[k] = v
	}
	for k, v := range override {
		if baseVal, ok := result[k]; ok {
			if baseMap, okBase := baseVal.(map[string]interface{}); okBase {
				if overrideMap, okOver := v.(map[string]interface{}); okOver {
					result[k] = DeepMergeMaps(baseMap, overrideMap)
					continue
				}
			}
		}
		result[k] = v
	}
	return result
}
