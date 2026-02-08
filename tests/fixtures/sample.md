# Sample Documentation

Introduction to the sample documentation project.

This paragraph provides context for what follows.

## Getting Started

Welcome to the getting started guide.

To begin, install the prerequisites:

1. Install Rust toolchain
2. Install Python 3.10+
3. Clone the repository

After installation, run the following:

```bash
cargo build --release
```

Next, configure your environment variables.

You can use `TagRead()` to read data from the historian.
Also see `Module.func()` for advanced usage.

For more details, see [Configuration Guide](config.md).
Also check [API Reference](api.md) for the full API.

Do NOT follow this image link: ![architecture diagram](arch.png)

Additional setup steps include verifying connectivity,
running the test suite, and checking log output.
Make sure all services are running before proceeding.
Verify the database connection string is correct.

## API Reference

The API provides several endpoints for data access.

### TagRead Function

The `TagRead()` function reads historian tags by name.

Parameters:
- **tagName**: The name of the tag to read
- **startTime**: Start of the time range
- **endTime**: End of the time range

Returns a list of timestamped values.

```python
# This heading inside a code block should NOT be parsed
## Also not a heading
### Still not a heading
def example():
    result = TagRead("Temperature.PV")
    return result
```

After the code block, additional notes on usage.

### TagWrite Function

Use `Class::method(arg)` to write tag values.

The write operation is atomic and transactional.
Partial writes are not supported.

| Parameter | Type   | Description          |
|-----------|--------|----------------------|
| tagName   | string | Target tag name      |
| value     | float  | Value to write       |
| timestamp | date   | Optional timestamp   |

Writes are buffered and flushed every 5 seconds.
Error handling is managed by the pipeline.

## Advanced Topics

This section covers advanced configuration scenarios
including high availability, clustering, and replication.

Historians can be configured in active-passive mode
for failover. The primary historian handles all writes
while the secondary maintains a synchronized copy.

When the primary fails, the secondary promotes itself
automatically. No manual intervention is required.

Network partitions are handled using a quorum-based
approach. At least two of three nodes must agree
on the cluster state before accepting writes.

For performance tuning, consider adjusting the buffer
size, write interval, and compression settings.
Each parameter affects throughput and latency differently.

See [Clustering Guide](clustering.md) for setup instructions.

Monitoring is available through the dashboard at
the `/metrics` endpoint. Key metrics include
write throughput, read latency, and disk usage.

Log verbosity can be controlled via the `LOG_LEVEL`
environment variable. Valid levels are DEBUG, INFO,
WARN, and ERROR.

Backup strategies should include both full and
incremental snapshots. Full backups capture the
entire dataset while incremental backups only
record changes since the last snapshot.

Retention policies determine how long historical
data is kept. Expired data is automatically purged
during the nightly maintenance window.
