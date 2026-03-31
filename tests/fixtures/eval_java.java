// Eval fixture for Java - realistic patterns for semantic search testing

import java.io.*;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.security.MessageDigest;
import java.util.*;
import java.util.regex.*;

/**
 * Holdout evaluation functions for Java.
 */
public class EvalJava {

    /**
     * Retry an operation with exponential backoff.
     */
    public static <T> T retryWithBackoff(java.util.concurrent.Callable<T> op, int maxRetries) throws Exception {
        long delay = 100;
        Exception lastError = null;
        for (int attempt = 0; attempt < maxRetries; attempt++) {
            try {
                return op.call();
            } catch (Exception e) {
                lastError = e;
                if (attempt < maxRetries - 1) {
                    Thread.sleep(delay);
                    delay *= 2;
                }
            }
        }
        throw lastError;
    }

    /**
     * Validate an email address format.
     */
    public static boolean validateEmail(String email) {
        Pattern pattern = Pattern.compile("^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}$");
        return pattern.matcher(email).matches();
    }

    /**
     * Parse JSON configuration from a file.
     * Simplified key-value parser for config files.
     */
    public static Map<String, String> parseJsonConfig(String path) throws IOException {
        Map<String, String> config = new HashMap<>();
        List<String> lines = Files.readAllLines(Paths.get(path), StandardCharsets.UTF_8);
        for (String line : lines) {
            line = line.trim();
            if (line.contains(":")) {
                String[] parts = line.split(":", 2);
                String key = parts[0].trim().replaceAll("[\"{}]", "");
                String value = parts[1].trim().replaceAll("[,\"{}]", "");
                if (!key.isEmpty() && !value.isEmpty()) {
                    config.put(key, value);
                }
            }
        }
        return config;
    }

    /**
     * Compute SHA256 hash of data.
     */
    public static String hashSha256(byte[] data) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] hash = digest.digest(data);
            StringBuilder hex = new StringBuilder();
            for (byte b : hash) {
                hex.append(String.format("%02x", b));
            }
            return hex.toString();
        } catch (Exception e) {
            throw new RuntimeException("SHA-256 not available", e);
        }
    }

    /**
     * Format a number as currency with commas and dollar sign.
     */
    public static String formatCurrency(double amount) {
        return String.format("$%,.2f", amount);
    }

    /**
     * Send HTTP POST request with JSON body.
     */
    public static String httpPostJson(String urlStr, String jsonBody) throws IOException {
        URL url = new URL(urlStr);
        HttpURLConnection conn = (HttpURLConnection) url.openConnection();
        conn.setRequestMethod("POST");
        conn.setRequestProperty("Content-Type", "application/json");
        conn.setDoOutput(true);
        try (OutputStream os = conn.getOutputStream()) {
            os.write(jsonBody.getBytes(StandardCharsets.UTF_8));
        }
        try (BufferedReader br = new BufferedReader(new InputStreamReader(conn.getInputStream()))) {
            StringBuilder response = new StringBuilder();
            String line;
            while ((line = br.readLine()) != null) {
                response.append(line);
            }
            return response.toString();
        }
    }

    /**
     * Read file contents with UTF-8 encoding.
     */
    public static String readFileUtf8(String path) throws IOException {
        return new String(Files.readAllBytes(Paths.get(path)), StandardCharsets.UTF_8);
    }

    /**
     * Safely write data to file without corruption on crash.
     * Writes to a temporary file first, then atomically moves it into place.
     */
    public static void writeFileAtomic(String path, String content) throws IOException {
        Path target = Paths.get(path);
        Path tmp = Paths.get(path + ".tmp");
        Files.write(tmp, content.getBytes(StandardCharsets.UTF_8));
        Files.move(tmp, target, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
    }

    /**
     * Compute arithmetic average of a list of numbers.
     */
    public static double calculateMean(double[] values) {
        if (values.length == 0) return 0.0;
        double sum = 0;
        for (double v : values) sum += v;
        return sum / values.length;
    }

    /**
     * Find the largest element in an array.
     */
    public static double findMaximum(double[] arr) {
        if (arr.length == 0) throw new IllegalArgumentException("Empty array");
        double max = arr[0];
        for (int i = 1; i < arr.length; i++) {
            if (arr[i] > max) max = arr[i];
        }
        return max;
    }

    /**
     * Create a unique random identifier string.
     * Generates a hex string from random bytes for use as a unique ID.
     */
    public static String generateRandomId() {
        byte[] bytes = new byte[16];
        new Random().nextBytes(bytes);
        StringBuilder sb = new StringBuilder();
        for (byte b : bytes) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }

    /**
     * Compress data using run-length encoding.
     * Consecutive repeated characters are replaced with character and count.
     */
    public static String compressRle(String input) {
        if (input.isEmpty()) return "";
        StringBuilder result = new StringBuilder();
        char current = input.charAt(0);
        int count = 1;
        for (int i = 1; i < input.length(); i++) {
            if (input.charAt(i) == current) {
                count++;
            } else {
                result.append(current);
                if (count > 1) result.append(count);
                current = input.charAt(i);
                count = 1;
            }
        }
        result.append(current);
        if (count > 1) result.append(count);
        return result.toString();
    }

    /**
     * Parse command-line flags and arguments into a map.
     * Supports --key=value and --flag (boolean) styles.
     */
    public static Map<String, String> parseCliArgs(String[] args) {
        Map<String, String> result = new HashMap<>();
        for (String arg : args) {
            if (arg.startsWith("--")) {
                String key;
                String value;
                if (arg.contains("=")) {
                    String[] parts = arg.substring(2).split("=", 2);
                    key = parts[0];
                    value = parts[1];
                } else {
                    key = arg.substring(2);
                    value = "true";
                }
                result.put(key, value);
            }
        }
        return result;
    }

    /**
     * Delay function execution until input stops changing.
     * Returns a debounced version that waits for quiet period before executing.
     */
    public static class Debouncer {
        private final long delayMs;
        private Timer timer;

        public Debouncer(long delayMs) {
            this.delayMs = delayMs;
        }

        public synchronized void debounce(Runnable action) {
            if (timer != null) timer.cancel();
            timer = new Timer();
            timer.schedule(new TimerTask() {
                @Override
                public void run() {
                    action.run();
                }
            }, delayMs);
        }
    }

    /**
     * Convert camelCase string to snake_case.
     */
    public static String camelToSnake(String input) {
        StringBuilder result = new StringBuilder();
        for (int i = 0; i < input.length(); i++) {
            char c = input.charAt(i);
            if (Character.isUpperCase(c)) {
                if (i > 0) result.append('_');
                result.append(Character.toLowerCase(c));
            } else {
                result.append(c);
            }
        }
        return result.toString();
    }

    /**
     * Truncate string to maximum length with ellipsis.
     */
    public static String truncateString(String s, int maxLen) {
        if (s.length() <= maxLen) return s;
        return s.substring(0, maxLen - 3) + "...";
    }

    /**
     * Check if string is a valid UUID format (8-4-4-4-12 hex).
     */
    public static boolean isValidUuid(String s) {
        return Pattern.matches(
            "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$", s);
    }

    /**
     * Quicksort - partition-based in-place sorting using pivot selection.
     */
    public static void quicksort(int[] arr, int low, int high) {
        if (low < high) {
            int pivot = arr[high];
            int i = low - 1;
            for (int j = low; j < high; j++) {
                if (arr[j] <= pivot) {
                    i++;
                    int temp = arr[i];
                    arr[i] = arr[j];
                    arr[j] = temp;
                }
            }
            int temp = arr[i + 1];
            arr[i + 1] = arr[high];
            arr[high] = temp;
            int pi = i + 1;
            quicksort(arr, low, pi - 1);
            quicksort(arr, pi + 1, high);
        }
    }

    /**
     * Memoization cache wrapper - caches function results by input key.
     */
    public static class Memoizer<K, V> {
        private final Map<K, V> cache = new HashMap<>();
        private final java.util.function.Function<K, V> compute;

        public Memoizer(java.util.function.Function<K, V> compute) {
            this.compute = compute;
        }

        public V getOrCompute(K key) {
            return cache.computeIfAbsent(key, compute);
        }
    }

    /**
     * Recursively flatten nested lists into a single flat list.
     */
    public static List<Object> flattenNestedList(List<?> nested) {
        List<Object> result = new ArrayList<>();
        for (Object item : nested) {
            if (item instanceof List) {
                result.addAll(flattenNestedList((List<?>) item));
            } else {
                result.add(item);
            }
        }
        return result;
    }

    /**
     * Recursively merge two nested maps (deep merge).
     */
    @SuppressWarnings("unchecked")
    public static Map<String, Object> deepMergeMaps(Map<String, Object> base, Map<String, Object> override) {
        Map<String, Object> result = new HashMap<>(base);
        for (Map.Entry<String, Object> entry : override.entrySet()) {
            String key = entry.getKey();
            Object overrideVal = entry.getValue();
            Object baseVal = result.get(key);
            if (baseVal instanceof Map && overrideVal instanceof Map) {
                result.put(key, deepMergeMaps((Map<String, Object>) baseVal, (Map<String, Object>) overrideVal));
            } else {
                result.put(key, overrideVal);
            }
        }
        return result;
    }

    /**
     * Serialize a map to CSV format with header row.
     */
    public static String serializeToCsv(List<Map<String, String>> rows) {
        if (rows.isEmpty()) return "";
        Set<String> headers = rows.get(0).keySet();
        StringBuilder sb = new StringBuilder();
        sb.append(String.join(",", headers)).append("\n");
        for (Map<String, String> row : rows) {
            List<String> values = new ArrayList<>();
            for (String h : headers) {
                String val = row.getOrDefault(h, "");
                if (val.contains(",") || val.contains("\"")) {
                    val = "\"" + val.replace("\"", "\"\"") + "\"";
                }
                values.add(val);
            }
            sb.append(String.join(",", values)).append("\n");
        }
        return sb.toString();
    }

    /**
     * Serialize a map to simple XML string.
     */
    public static String serializeToXml(Map<String, String> data, String rootTag) {
        StringBuilder sb = new StringBuilder();
        sb.append("<").append(rootTag).append(">");
        for (Map.Entry<String, String> entry : data.entrySet()) {
            sb.append("<").append(entry.getKey()).append(">");
            sb.append(escapeXml(entry.getValue()));
            sb.append("</").append(entry.getKey()).append(">");
        }
        sb.append("</").append(rootTag).append(">");
        return sb.toString();
    }

    private static String escapeXml(String s) {
        return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
                .replace("\"", "&quot;").replace("'", "&apos;");
    }

    /**
     * Match string against a glob pattern with * and ? wildcards.
     */
    public static boolean globMatch(String pattern, String input) {
        return globMatchHelper(pattern, 0, input, 0);
    }

    private static boolean globMatchHelper(String pattern, int pi, String input, int si) {
        if (pi == pattern.length() && si == input.length()) return true;
        if (pi == pattern.length()) return false;
        if (pattern.charAt(pi) == '*') {
            for (int i = si; i <= input.length(); i++) {
                if (globMatchHelper(pattern, pi + 1, input, i)) return true;
            }
            return false;
        }
        if (si == input.length()) return false;
        if (pattern.charAt(pi) == '?' || pattern.charAt(pi) == input.charAt(si)) {
            return globMatchHelper(pattern, pi + 1, input, si + 1);
        }
        return false;
    }

    /**
     * Match string against a regular expression pattern and return all capture groups.
     */
    public static List<String> regexMatchGroups(String pattern, String input) {
        List<String> groups = new ArrayList<>();
        Matcher matcher = Pattern.compile(pattern).matcher(input);
        if (matcher.find()) {
            for (int i = 0; i <= matcher.groupCount(); i++) {
                groups.add(matcher.group(i));
            }
        }
        return groups;
    }

    /**
     * Retry with fallback - try primary operation, on failure try fallback.
     */
    public static <T> T retryWithFallback(java.util.concurrent.Callable<T> primary,
                                            java.util.concurrent.Callable<T> fallback,
                                            int maxRetries) throws Exception {
        Exception lastError = null;
        for (int i = 0; i < maxRetries; i++) {
            try {
                return primary.call();
            } catch (Exception e) {
                lastError = e;
            }
        }
        try {
            return fallback.call();
        } catch (Exception e) {
            throw new RuntimeException("Primary and fallback both failed", lastError);
        }
    }
}
