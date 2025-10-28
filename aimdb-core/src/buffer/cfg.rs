//! Buffer configuration types
//!
//! Defines the configuration enum for selecting buffer behavior per record type.

use core::fmt;

#[cfg(not(feature = "std"))]
extern crate alloc;

/// Buffer configuration for a record type
///
/// Selects buffering strategy: SPMC Ring (backlog), SingleLatest (state sync), or Mailbox (commands).
///
/// # Quick Selection Guide
/// - **High-frequency data (>10 Hz)**: `SpmcRing` with tuned capacity
/// - **State/config updates**: `SingleLatest` (only latest matters)
/// - **Commands/one-shot events**: `Mailbox`
///
/// # Examples
/// ```rust
/// use aimdb_core::buffer::BufferCfg;
///
/// let telemetry = BufferCfg::SpmcRing { capacity: 2048 };  // High-freq data
/// let config = BufferCfg::SingleLatest;                     // State sync
/// let commands = BufferCfg::Mailbox;                        // Commands
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BufferCfg {
    /// SPMC (Single Producer, Multiple Consumer) ring buffer
    ///
    /// Best for high-frequency data streams with bounded memory. Fast producers
    /// can outrun slow consumers (lag detection). Oldest messages dropped on overflow.
    ///
    /// **Sizing:** `capacity = data_rate_hz × lag_seconds` (use power-of-2)
    SpmcRing {
        /// Maximum number of items in the buffer
        capacity: usize,
    },

    /// Single latest value buffer (no backlog)
    ///
    /// Only most recent value is kept. Consumers always get latest state.
    /// Intermediate updates are collapsed. Use when history doesn't matter.
    SingleLatest,

    /// Single-slot mailbox with overwrite
    ///
    /// New value overwrites old if not consumed. At-least-once delivery.
    /// Use for commands where latest command wins.
    Mailbox,
}

impl BufferCfg {
    /// Validates the buffer configuration
    ///
    /// Returns `Err` if SPMC Ring capacity is 0.
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
    pub fn name(&self) -> &'static str {
        match self {
            BufferCfg::SpmcRing { .. } => "spmc_ring",
            BufferCfg::SingleLatest => "single_latest",
            BufferCfg::Mailbox => "mailbox",
        }
    }

    /// Returns estimated memory overhead for this buffer type
    ///
    /// Approximation; varies by implementation and platform.
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
    /// Returns the default buffer configuration: `SpmcRing { capacity: 1024 }`
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
        assert!(BufferCfg::SpmcRing { capacity: 1024 }.validate().is_ok());
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
        assert_eq!(BufferCfg::SpmcRing { capacity: 100 }.name(), "spmc_ring");
        assert_eq!(BufferCfg::SingleLatest.name(), "single_latest");
        assert_eq!(BufferCfg::Mailbox.name(), "mailbox");
    }

    #[test]
    #[cfg(feature = "std")]
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
