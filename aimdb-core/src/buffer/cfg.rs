//! Buffer configuration types
//!
//! Defines the configuration enum for selecting buffer behavior per record type.

use core::fmt;

#[cfg(not(feature = "std"))]
extern crate alloc;

/// Buffer configuration for a record type
///
/// Each record can choose a buffering strategy that matches its data flow characteristics.
/// The configuration is set during record registration and determines how values are
/// queued between producer and consumers.
///
/// # Buffer Types
///
/// ## SPMC Ring
/// Best for high-frequency data streams where you need bounded memory but can tolerate
/// some lag in slow consumers.
///
/// **Characteristics:**
/// - Bounded capacity with overflow handling
/// - Each consumer has independent read position
/// - Fast producer can outrun slow consumer (lag detection)
/// - Oldest messages dropped on overflow
///
/// **Memory:** `capacity × size_of::<T>()` bytes
///
/// ## SingleLatest
/// Best for state synchronization where only the newest value matters.
///
/// **Characteristics:**
/// - Only the most recent value is kept
/// - Consumers always see latest state
/// - Intermediate values are skipped
/// - No memory accumulation
///
/// **Memory:** `size_of::<Option<T>>()` bytes (constant)
///
/// ## Mailbox
/// Best for command processing where you need guaranteed delivery but can overwrite
/// pending commands.
///
/// **Characteristics:**
/// - Single value slot
/// - New value overwrites old (if not consumed)
/// - Notify-based wake-up
/// - At-least-once delivery guarantee
///
/// **Memory:** `size_of::<Option<T>>() + notify_overhead` (~32 bytes)
///
/// # Examples
///
/// ```rust
/// use aimdb_core::buffer::BufferCfg;
///
/// // High-frequency telemetry (1000+ Hz)
/// let telemetry = BufferCfg::SpmcRing { capacity: 2048 };
///
/// // Configuration state (occasional updates)
/// let config = BufferCfg::SingleLatest;
///
/// // Command processing (one-shot events)
/// let commands = BufferCfg::Mailbox;
/// ```
///
/// # Decision Guide
///
/// ```text
/// High-frequency data (>10 Hz)?
///   Yes → SPMC Ring (tune capacity)
///   No  ↓
///
/// Need full history?
///   Yes → SPMC Ring (large capacity)
///   No  ↓
///
/// Only care about latest?
///   Yes → SingleLatest
///   No  ↓
///
/// One-shot events?
///   Yes → Mailbox
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BufferCfg {
    /// SPMC (Single Producer, Multiple Consumer) ring buffer
    ///
    /// # Use Cases
    /// - High-frequency telemetry (sensor streams at 100-1000 Hz)
    /// - Event logs with bounded memory
    /// - Metrics collection
    /// - Any scenario where you need backlog but can tolerate lag
    ///
    /// # Overflow Behavior
    /// When buffer is full:
    /// - New values overwrite oldest
    /// - Lagging consumers get `RecvErr::Lagged(n)`
    /// - Fast consumers are unaffected
    ///
    /// # Performance
    /// - Push: O(1) lock-free
    /// - Recv: O(1) lock-free
    /// - Throughput: >1M msg/s (typical)
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// // Sensor data at 1000 Hz with ~1 second backlog
    /// let cfg = BufferCfg::SpmcRing { capacity: 1024 };
    ///
    /// // High-throughput logs with 4 second backlog
    /// let cfg = BufferCfg::SpmcRing { capacity: 4096 };
    /// ```
    SpmcRing {
        /// Maximum number of items in the buffer
        ///
        /// **Recommendation:** Use power-of-2 for optimal performance.
        ///
        /// **Sizing Guide:**
        /// - `capacity = data_rate_hz × acceptable_lag_seconds`
        /// - Example: 100 Hz × 2s = 200 items
        /// - Add margin for bursts: 200 × 1.5 = 300, round to 512
        capacity: usize,
    },

    /// Single latest value buffer (no backlog)
    ///
    /// # Use Cases
    /// - Configuration updates
    /// - Status synchronization
    /// - Latest sensor reading (when history doesn't matter)
    /// - Any scenario where only current state matters
    ///
    /// # Behavior
    /// - Only the most recent value is kept
    /// - Consumers always get latest state when they read
    /// - Intermediate updates are collapsed
    /// - No per-consumer tracking needed
    ///
    /// # Performance
    /// - Push: O(1) atomic
    /// - Recv: O(1) wait
    /// - Throughput: >10M msg/s (typical)
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// // Device configuration (only latest matters)
    /// let cfg = BufferCfg::SingleLatest;
    /// ```
    ///
    /// # Note
    /// If you need to process *every* update, use `SpmcRing` instead.
    SingleLatest,

    /// Single-slot mailbox with overwrite
    ///
    /// # Use Cases
    /// - Command processing (start/stop/reset)
    /// - One-shot triggers
    /// - State machine transitions
    /// - Any scenario where you need delivery guarantee but can overwrite pending
    ///
    /// # Behavior
    /// - Single value slot
    /// - New value overwrites old if not yet consumed
    /// - Notify-based wake-up (efficient)
    /// - At-least-once delivery (last value before consumption)
    ///
    /// # Performance
    /// - Push: O(1) lock
    /// - Recv: O(1) lock + wait
    /// - Throughput: ~100K msg/s (typical)
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// // Command processing (latest command wins)
    /// let cfg = BufferCfg::Mailbox;
    /// ```
    ///
    /// # Note
    /// If you need to queue multiple commands, use `SpmcRing` instead.
    Mailbox,
}

impl BufferCfg {
    /// Validates the buffer configuration
    ///
    /// # Returns
    /// - `Ok(())` if configuration is valid
    /// - `Err(&str)` with error description if invalid
    ///
    /// # Validation Rules
    /// - SPMC Ring: `capacity` must be > 0
    /// - SingleLatest: always valid
    /// - Mailbox: always valid
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// let cfg = BufferCfg::SpmcRing { capacity: 1024 };
    /// assert!(cfg.validate().is_ok());
    ///
    /// let invalid = BufferCfg::SpmcRing { capacity: 0 };
    /// assert!(invalid.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            BufferCfg::SpmcRing { capacity } => {
                if *capacity == 0 {
                    return Err("SPMC ring capacity must be > 0");
                }
                // Note: Non-power-of-2 is allowed but may have performance implications
                // This is documented but not enforced
                Ok(())
            }
            BufferCfg::SingleLatest | BufferCfg::Mailbox => Ok(()),
        }
    }

    /// Returns a human-readable name for this buffer type
    ///
    /// Useful for logging and debugging.
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// let cfg = BufferCfg::SpmcRing { capacity: 1024 };
    /// assert_eq!(cfg.name(), "spmc_ring");
    ///
    /// let cfg = BufferCfg::SingleLatest;
    /// assert_eq!(cfg.name(), "single_latest");
    /// ```
    pub fn name(&self) -> &'static str {
        match self {
            BufferCfg::SpmcRing { .. } => "spmc_ring",
            BufferCfg::SingleLatest => "single_latest",
            BufferCfg::Mailbox => "mailbox",
        }
    }

    /// Returns estimated memory overhead for this buffer type
    ///
    /// This is an approximation and may vary by implementation and platform.
    ///
    /// # Arguments
    /// * `item_size` - Size in bytes of a single item `size_of::<T>()`
    /// * `consumer_count` - Number of consumers for this record
    ///
    /// # Returns
    /// Estimated memory in bytes
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    /// use core::mem::size_of;
    ///
    /// struct SensorData {
    ///     temperature: f32,
    ///     timestamp: u64,
    /// }
    ///
    /// let cfg = BufferCfg::SpmcRing { capacity: 1024 };
    /// let memory = cfg.estimated_memory_bytes(size_of::<SensorData>(), 3);
    /// // ~1024 * 12 bytes + 3 * 64 bytes ≈ 12KB + 192 bytes
    /// ```
    pub fn estimated_memory_bytes(&self, item_size: usize, consumer_count: usize) -> usize {
        match self {
            BufferCfg::SpmcRing { capacity } => {
                // Buffer storage + per-consumer overhead
                let buffer_size = capacity * item_size;
                let consumer_overhead = consumer_count * 64; // Approximate
                buffer_size + consumer_overhead
            }
            BufferCfg::SingleLatest => {
                // Single slot + per-consumer overhead
                let buffer_size = item_size + 8; // Option<T> overhead
                let consumer_overhead = consumer_count * 16; // Watcher handles
                buffer_size + consumer_overhead
            }
            BufferCfg::Mailbox => {
                // Single slot + notify + per-consumer overhead
                let buffer_size = item_size + 8; // Option<T>
                let notify_overhead = 32; // Notify primitive
                let consumer_overhead = consumer_count * 16; // Notifier handles
                buffer_size + notify_overhead + consumer_overhead
            }
        }
    }
}

impl Default for BufferCfg {
    /// Returns the default buffer configuration
    ///
    /// Default is `SpmcRing { capacity: 1024 }` which provides:
    /// - Reasonable backlog for most use cases
    /// - ~1 second buffer at 1000 Hz
    /// - Backward compatibility with existing behavior
    ///
    /// # Example
    /// ```rust
    /// use aimdb_core::buffer::BufferCfg;
    ///
    /// let cfg = BufferCfg::default();
    /// assert_eq!(cfg, BufferCfg::SpmcRing { capacity: 1024 });
    /// ```
    fn default() -> Self {
        BufferCfg::SpmcRing { capacity: 1024 }
    }
}

impl fmt::Display for BufferCfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BufferCfg::SpmcRing { capacity } => write!(f, "SpmcRing(capacity={})", capacity),
            BufferCfg::SingleLatest => write!(f, "SingleLatest"),
            BufferCfg::Mailbox => write!(f, "Mailbox"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_cfg_validation() {
        // Valid configurations
        assert!(BufferCfg::SpmcRing { capacity: 1 }.validate().is_ok());
        assert!(BufferCfg::SpmcRing { capacity: 1024 }
            .validate()
            .is_ok());
        assert!(BufferCfg::SingleLatest.validate().is_ok());
        assert!(BufferCfg::Mailbox.validate().is_ok());

        // Invalid configuration
        assert!(BufferCfg::SpmcRing { capacity: 0 }.validate().is_err());
    }

    #[test]
    fn test_buffer_cfg_default() {
        let cfg = BufferCfg::default();
        assert_eq!(cfg, BufferCfg::SpmcRing { capacity: 1024 });
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_buffer_cfg_names() {
        assert_eq!(
            BufferCfg::SpmcRing { capacity: 100 }.name(),
            "spmc_ring"
        );
        assert_eq!(BufferCfg::SingleLatest.name(), "single_latest");
        assert_eq!(BufferCfg::Mailbox.name(), "mailbox");
    }

    #[test]
    fn test_buffer_cfg_display() {
        assert_eq!(
            format!("{}", BufferCfg::SpmcRing { capacity: 512 }),
            "SpmcRing(capacity=512)"
        );
        assert_eq!(format!("{}", BufferCfg::SingleLatest), "SingleLatest");
        assert_eq!(format!("{}", BufferCfg::Mailbox), "Mailbox");
    }

    #[test]
    fn test_estimated_memory() {
        // SPMC Ring with 100-byte items, 1024 capacity, 3 consumers
        let cfg = BufferCfg::SpmcRing { capacity: 1024 };
        let mem = cfg.estimated_memory_bytes(100, 3);
        // Should be roughly 1024*100 + 3*64 = 102,400 + 192 = 102,592
        assert!(mem > 102_000 && mem < 103_000);

        // SingleLatest with 100-byte items, 3 consumers
        let cfg = BufferCfg::SingleLatest;
        let mem = cfg.estimated_memory_bytes(100, 3);
        // Should be roughly 100 + 8 + 3*16 = 156
        assert!(mem > 100 && mem < 200);

        // Mailbox with 100-byte items, 3 consumers
        let cfg = BufferCfg::Mailbox;
        let mem = cfg.estimated_memory_bytes(100, 3);
        // Should be roughly 100 + 8 + 32 + 3*16 = 188
        assert!(mem > 140 && mem < 250);
    }

    #[test]
    fn test_clone_and_equality() {
        let cfg1 = BufferCfg::SpmcRing { capacity: 512 };
        let cfg2 = cfg1.clone();
        assert_eq!(cfg1, cfg2);

        let cfg3 = BufferCfg::SingleLatest;
        assert_ne!(cfg1, cfg3);
    }
}
