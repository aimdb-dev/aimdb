//! Integration tests for connection state management

#[cfg(feature = "tokio-runtime")]
mod tests {
    #[test]
    fn test_channel_state_sequence_management() {
        let mut state = ChannelState::new(42);

        assert_eq!(state.channel_id, 42);
        assert_eq!(state.outbound_seq, 0);

        // Sequence should increment
        assert_eq!(state.next_outbound_seq(), 0);
        assert_eq!(state.next_outbound_seq(), 1);
        assert_eq!(state.next_outbound_seq(), 2);

        // Should wrap at 256
        state.outbound_seq = 255;
        assert_eq!(state.next_outbound_seq(), 255);
        assert_eq!(state.next_outbound_seq(), 0); // Wrapped
    }

    #[test]
    fn test_pending_ack_tracking() {
        let mut state = ChannelState::new(1);

        // Add pending ACK
        state.add_pending_ack(5, None);
        assert_eq!(state.pending_acks.len(), 1);

        // Complete ACK
        assert!(state.complete_ack(5));
        assert_eq!(state.pending_acks.len(), 0);

        // Complete non-existent ACK
        assert!(!state.complete_ack(99));
    }

    #[test]
    fn test_ack_timeout_detection() {
        use std::time::{Duration, Instant};

        let mut state = ChannelState::new(1);

        // Add an ACK that's already old
        let old_ack = PendingAck {
            sent_at: Instant::now() - Duration::from_secs(5), // 5 seconds ago
            response_tx: None,
        };
        state.pending_acks.insert(10, old_ack);

        // Add a fresh ACK
        state.add_pending_ack(11, None);

        // Check timeouts
        let timed_out = state.check_ack_timeouts();

        // Old ACK should timeout, fresh one should remain
        assert!(timed_out.contains(&10), "Old ACK should timeout");
        assert!(!timed_out.contains(&11), "Fresh ACK should not timeout");
        assert_eq!(state.pending_acks.len(), 1, "Only fresh ACK should remain");
    }

    #[test]
    fn test_multiple_pending_acks() {
        let mut state = ChannelState::new(1);

        // Add multiple pending ACKs
        for seq in 0..10 {
            state.add_pending_ack(seq, None);
        }

        assert_eq!(state.pending_acks.len(), 10);

        // Complete some
        state.complete_ack(2);
        state.complete_ack(5);
        state.complete_ack(8);

        assert_eq!(state.pending_acks.len(), 7);

        // Verify remaining
        assert!(state.pending_acks.contains_key(&0));
        assert!(!state.pending_acks.contains_key(&2));
        assert!(state.pending_acks.contains_key(&4));
        assert!(!state.pending_acks.contains_key(&5));
    }

    // Simplified ChannelState for testing
    struct ChannelState {
        channel_id: u8,
        outbound_seq: u8,
        pending_acks: std::collections::HashMap<u8, PendingAck>,
    }

    struct PendingAck {
        sent_at: std::time::Instant,
        #[allow(dead_code)]
        response_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    }

    impl ChannelState {
        fn new(channel_id: u8) -> Self {
            Self {
                channel_id,
                outbound_seq: 0,
                pending_acks: std::collections::HashMap::new(),
            }
        }

        fn next_outbound_seq(&mut self) -> u8 {
            let seq = self.outbound_seq;
            self.outbound_seq = self.outbound_seq.wrapping_add(1);
            seq
        }

        fn add_pending_ack(
            &mut self,
            seq: u8,
            response_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
        ) {
            self.pending_acks.insert(
                seq,
                PendingAck {
                    sent_at: std::time::Instant::now(),
                    response_tx,
                },
            );
        }

        fn complete_ack(&mut self, seq: u8) -> bool {
            self.pending_acks.remove(&seq).is_some()
        }

        fn check_ack_timeouts(&mut self) -> Vec<u8> {
            let now = std::time::Instant::now();
            let mut timed_out = Vec::new();

            self.pending_acks.retain(|&seq, pending| {
                if now.duration_since(pending.sent_at) > std::time::Duration::from_secs(3) {
                    timed_out.push(seq);
                    false
                } else {
                    true
                }
            });

            timed_out
        }
    }
}
