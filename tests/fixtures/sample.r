# Sample R file for parser tests

# Calculate the mean of a numeric vector
calculate_mean <- function(x) {
    sum(x) / length(x)
}

# Filter values above a threshold
filter_above <- function(values, threshold) {
    values[values > threshold]
}

# Generate a summary report
generate_report <- function(data) {
    avg <- mean(data)
    std_dev <- sd(data)
    cat("Mean:", avg, "\n")
    cat("SD:", std_dev, "\n")
    return(list(mean = avg, sd = std_dev))
}
