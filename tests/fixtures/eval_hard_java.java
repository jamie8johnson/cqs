// Hard eval fixture for Java - confusable functions that test fine-grained semantic distinction

import java.util.*;

/**
 * Sort array using merge sort - stable divide and conquer algorithm.
 * Preserves relative order of equal elements unlike quicksort.
 */
public class EvalHardJava {

    public static void mergeSort(int[] arr) {
        if (arr.length <= 1) return;
        int mid = arr.length / 2;
        int[] left = Arrays.copyOfRange(arr, 0, mid);
        int[] right = Arrays.copyOfRange(arr, mid, arr.length);
        mergeSort(left);
        mergeSort(right);
        int i = 0, j = 0, k = 0;
        while (i < left.length && j < right.length) {
            if (left[i] <= right[j]) {
                arr[k++] = left[i++];
            } else {
                arr[k++] = right[j++];
            }
        }
        while (i < left.length) arr[k++] = left[i++];
        while (j < right.length) arr[k++] = right[j++];
    }

    /**
     * Sort array using heap sort with binary max-heap.
     * Builds a max heap then repeatedly extracts the maximum element.
     */
    public static void heapSort(int[] arr) {
        int n = arr.length;
        for (int i = n / 2 - 1; i >= 0; i--) {
            heapify(arr, n, i);
        }
        for (int i = n - 1; i > 0; i--) {
            int temp = arr[0];
            arr[0] = arr[i];
            arr[i] = temp;
            heapify(arr, i, 0);
        }
    }

    private static void heapify(int[] arr, int n, int i) {
        int largest = i;
        int left = 2 * i + 1;
        int right = 2 * i + 2;
        if (left < n && arr[left] > arr[largest]) largest = left;
        if (right < n && arr[right] > arr[largest]) largest = right;
        if (largest != i) {
            int swap = arr[i];
            arr[i] = arr[largest];
            arr[largest] = swap;
            heapify(arr, n, largest);
        }
    }

    /**
     * Sort array using insertion sort - efficient for small or nearly sorted arrays.
     * Shifts elements to make room for each new element in sorted position.
     */
    public static void insertionSort(int[] arr) {
        for (int i = 1; i < arr.length; i++) {
            int key = arr[i];
            int j = i - 1;
            while (j >= 0 && arr[j] > key) {
                arr[j + 1] = arr[j];
                j--;
            }
            arr[j + 1] = key;
        }
    }

    /**
     * Sort array using bubble sort with early termination.
     * Repeatedly swaps adjacent elements, stops when no swaps needed.
     */
    public static void bubbleSort(int[] arr) {
        int n = arr.length;
        for (int i = 0; i < n; i++) {
            boolean swapped = false;
            for (int j = 0; j < n - 1 - i; j++) {
                if (arr[j] > arr[j + 1]) {
                    int temp = arr[j];
                    arr[j] = arr[j + 1];
                    arr[j + 1] = temp;
                    swapped = true;
                }
            }
            if (!swapped) break;
        }
    }

    /**
     * Sort non-negative integers using radix sort - processes digits from least significant.
     * Non-comparison sort with O(d*n) time where d is digit count.
     */
    public static void radixSort(int[] arr) {
        if (arr.length == 0) return;
        int max = Arrays.stream(arr).max().orElse(0);
        for (int exp = 1; max / exp > 0; exp *= 10) {
            int[] output = new int[arr.length];
            int[] count = new int[10];
            for (int val : arr) count[(val / exp) % 10]++;
            for (int i = 1; i < 10; i++) count[i] += count[i - 1];
            for (int i = arr.length - 1; i >= 0; i--) {
                int digit = (arr[i] / exp) % 10;
                output[--count[digit]] = arr[i];
            }
            System.arraycopy(output, 0, arr, 0, arr.length);
        }
    }

    /**
     * Pad string to fixed width with a fill character.
     * If string is shorter than width, pads on the left with fill char.
     */
    public static String padString(String s, int width, char fill) {
        if (s.length() >= width) return s;
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < width - s.length(); i++) {
            sb.append(fill);
        }
        sb.append(s);
        return sb.toString();
    }

    /**
     * Reverse the characters in a string.
     */
    public static String reverseString(String s) {
        return new StringBuilder(s).reverse().toString();
    }

    /**
     * Count the number of words in text separated by whitespace.
     */
    public static int countWords(String text) {
        if (text == null || text.trim().isEmpty()) return 0;
        return text.trim().split("\\s+").length;
    }

    /**
     * Extract all numeric values from a mixed text string.
     * Returns integers and floating point numbers found in the text.
     */
    public static List<Double> extractNumbers(String text) {
        List<Double> numbers = new ArrayList<>();
        StringBuilder current = new StringBuilder();
        boolean hasDot = false;
        for (char c : text.toCharArray()) {
            if (Character.isDigit(c)) {
                current.append(c);
            } else if (c == '.' && !hasDot && current.length() > 0) {
                current.append(c);
                hasDot = true;
            } else if (current.length() > 0) {
                try {
                    numbers.add(Double.parseDouble(current.toString()));
                } catch (NumberFormatException ignored) {}
                current.setLength(0);
                hasDot = false;
            }
        }
        if (current.length() > 0) {
            try {
                numbers.add(Double.parseDouble(current.toString()));
            } catch (NumberFormatException ignored) {}
        }
        return numbers;
    }

    /**
     * Validate URL format - checks for valid scheme and hostname.
     */
    public static boolean validateUrl(String url) {
        if (url.startsWith("http://") || url.startsWith("https://")) {
            String rest = url.startsWith("https://") ? url.substring(8) : url.substring(7);
            String host = rest.split("/")[0];
            return !host.isEmpty() && host.contains(".");
        }
        return false;
    }

    /**
     * Validate IP address - supports IPv4 format with four octets 0-255.
     */
    public static boolean validateIpAddress(String addr) {
        String[] parts = addr.split("\\.");
        if (parts.length != 4) return false;
        for (String part : parts) {
            try {
                int val = Integer.parseInt(part);
                if (val < 0 || val > 255) return false;
            } catch (NumberFormatException e) {
                return false;
            }
        }
        return true;
    }

    /**
     * Validate phone number with international country code prefix.
     * Accepts formats like +1-555-123-4567 or +44 20 7946 0958.
     */
    public static boolean validatePhone(String phone) {
        String digits = phone.replaceAll("[^0-9]", "");
        return phone.startsWith("+") && digits.length() >= 10 && digits.length() <= 15;
    }

    /**
     * Compute CRC32 checksum of byte data.
     * Simple polynomial division checksum for error detection.
     */
    public static long hashCrc32(byte[] data) {
        long crc = 0xFFFFFFFFL;
        for (byte b : data) {
            crc ^= (b & 0xFF);
            for (int i = 0; i < 8; i++) {
                if ((crc & 1) != 0) {
                    crc = (crc >>> 1) ^ 0xEDB88320L;
                } else {
                    crc >>>= 1;
                }
            }
        }
        return ~crc & 0xFFFFFFFFL;
    }

    /**
     * Rate limiter using token bucket algorithm.
     * Allows N calls per time window, rejects excess calls.
     */
    public static class RateLimiter {
        private int tokens;
        private final int maxTokens;
        private long lastRefill;
        private final long refillIntervalMs;

        public RateLimiter(int maxPerSecond) {
            this.tokens = maxPerSecond;
            this.maxTokens = maxPerSecond;
            this.lastRefill = System.currentTimeMillis();
            this.refillIntervalMs = 1000;
        }

        public synchronized boolean allow() {
            refill();
            if (tokens > 0) {
                tokens--;
                return true;
            }
            return false;
        }

        private void refill() {
            long now = System.currentTimeMillis();
            if (now - lastRefill >= refillIntervalMs) {
                tokens = maxTokens;
                lastRefill = now;
            }
        }
    }

    /**
     * Circuit breaker - stops calling after consecutive failures.
     * Transitions: Closed -> Open (after threshold) -> HalfOpen (after timeout) -> Closed.
     */
    public static class CircuitBreaker {
        private int failureCount;
        private final int threshold;
        private String state; // "closed", "open", "half_open"
        private long lastFailureTime;
        private final long resetTimeoutMs;

        public CircuitBreaker(int threshold, long resetTimeoutMs) {
            this.threshold = threshold;
            this.resetTimeoutMs = resetTimeoutMs;
            this.state = "closed";
            this.failureCount = 0;
        }

        public boolean shouldAllow() {
            if ("closed".equals(state)) return true;
            if ("open".equals(state)) {
                if (System.currentTimeMillis() - lastFailureTime >= resetTimeoutMs) {
                    state = "half_open";
                    return true;
                }
                return false;
            }
            return true; // half_open: allow one probe request
        }

        public void recordFailure() {
            failureCount++;
            lastFailureTime = System.currentTimeMillis();
            if (failureCount >= threshold) {
                state = "open";
            }
        }

        public void recordSuccess() {
            failureCount = 0;
            state = "closed";
        }
    }

    /**
     * Breadth-first search traversal of a graph from a starting node.
     * Visits all nodes reachable from start, level by level using a queue.
     */
    public static List<Integer> bfsTraversal(Map<Integer, List<Integer>> graph, int start) {
        List<Integer> visited = new ArrayList<>();
        Set<Integer> seen = new HashSet<>();
        Queue<Integer> queue = new LinkedList<>();
        queue.add(start);
        seen.add(start);
        while (!queue.isEmpty()) {
            int node = queue.poll();
            visited.add(node);
            List<Integer> neighbors = graph.getOrDefault(node, Collections.emptyList());
            for (int neighbor : neighbors) {
                if (!seen.contains(neighbor)) {
                    seen.add(neighbor);
                    queue.add(neighbor);
                }
            }
        }
        return visited;
    }

    /**
     * Depth-first search traversal of a graph from a starting node.
     * Visits nodes by exploring as deep as possible before backtracking, using a stack.
     */
    public static List<Integer> dfsTraversal(Map<Integer, List<Integer>> graph, int start) {
        List<Integer> visited = new ArrayList<>();
        Set<Integer> seen = new HashSet<>();
        Deque<Integer> stack = new ArrayDeque<>();
        stack.push(start);
        while (!stack.isEmpty()) {
            int node = stack.pop();
            if (seen.contains(node)) continue;
            seen.add(node);
            visited.add(node);
            List<Integer> neighbors = graph.getOrDefault(node, Collections.emptyList());
            for (int i = neighbors.size() - 1; i >= 0; i--) {
                if (!seen.contains(neighbors.get(i))) {
                    stack.push(neighbors.get(i));
                }
            }
        }
        return visited;
    }

    /**
     * LRU cache that evicts the least recently used entry when capacity is exceeded.
     * Uses a LinkedHashMap with access-order iteration for O(1) get and put.
     */
    public static class LruCache<K, V> {
        private final int capacity;
        private final LinkedHashMap<K, V> map;

        public LruCache(int capacity) {
            this.capacity = capacity;
            this.map = new LinkedHashMap<K, V>(capacity, 0.75f, true) {
                @Override
                protected boolean removeEldestEntry(Map.Entry<K, V> eldest) {
                    return size() > LruCache.this.capacity;
                }
            };
        }

        public V get(K key) {
            return map.get(key);
        }

        public void put(K key, V value) {
            map.put(key, value);
        }

        public int size() {
            return map.size();
        }
    }

    /**
     * TTL cache that expires entries after a configurable time-to-live duration.
     * Each entry has its own expiration timestamp; stale entries are removed on access.
     */
    public static class TtlCache<K, V> {
        private final long ttlMs;
        private final Map<K, CacheEntry<V>> store = new HashMap<>();

        private static class CacheEntry<V> {
            final V value;
            final long expiresAt;

            CacheEntry(V value, long expiresAt) {
                this.value = value;
                this.expiresAt = expiresAt;
            }
        }

        public TtlCache(long ttlMs) {
            this.ttlMs = ttlMs;
        }

        public V get(K key) {
            CacheEntry<V> entry = store.get(key);
            if (entry == null) return null;
            if (System.currentTimeMillis() > entry.expiresAt) {
                store.remove(key);
                return null;
            }
            return entry.value;
        }

        public void put(K key, V value) {
            store.put(key, new CacheEntry<>(value, System.currentTimeMillis() + ttlMs));
        }

        public void evictExpired() {
            long now = System.currentTimeMillis();
            store.entrySet().removeIf(e -> now > e.getValue().expiresAt);
        }
    }
}
