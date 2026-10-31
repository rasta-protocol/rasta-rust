//! Deterministic adaptive timing and acknowledgement thresholds.

use core::fmt;

/// Validated timing and receive-acknowledgement configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingConfig {
    t_max: u32,
    t_heartbeat: u32,
    timer_granularity: u32,
    m_w_a: u16,
}

impl TimingConfig {
    /// Creates timing configuration for one SR connection.
    ///
    /// # Errors
    ///
    /// Returns an error when a duration is zero, the timer granularity exceeds
    /// `T_max`, or `M_W_A` is outside `1..N_SENDMAX`.
    pub const fn new(
        t_max: u32,
        t_heartbeat: u32,
        timer_granularity: u32,
        m_w_a: u16,
        n_send_max: u16,
    ) -> Result<Self, TimingConfigurationError> {
        if t_max == 0 {
            return Err(TimingConfigurationError::ZeroMaximumTime);
        }
        if t_heartbeat == 0 {
            return Err(TimingConfigurationError::ZeroHeartbeatTime);
        }
        if timer_granularity > t_max {
            return Err(TimingConfigurationError::GranularityExceedsMaximum);
        }
        if m_w_a == 0 || m_w_a >= n_send_max {
            return Err(TimingConfigurationError::InvalidAcknowledgementThreshold {
                threshold: m_w_a,
                n_send_max,
            });
        }

        Ok(Self {
            t_max,
            t_heartbeat,
            timer_granularity,
            m_w_a,
        })
    }

    /// Returns `T_max` in protocol milliseconds.
    #[must_use]
    pub const fn t_max(self) -> u32 {
        self.t_max
    }

    /// Returns heartbeat period `T_h` in protocol milliseconds.
    #[must_use]
    pub const fn t_heartbeat(self) -> u32 {
        self.t_heartbeat
    }

    /// Returns the adapter timer granularity in milliseconds.
    #[must_use]
    pub const fn timer_granularity(self) -> u32 {
        self.timer_granularity
    }

    /// Returns receive acknowledgement threshold `M_W_A`.
    #[must_use]
    pub const fn m_w_a(self) -> u16 {
        self.m_w_a
    }
}

/// Invalid adaptive timing configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingConfigurationError {
    /// `T_max` must be non-zero.
    ZeroMaximumTime,
    /// `T_h` must be non-zero.
    ZeroHeartbeatTime,
    /// Timer granularity must not exceed `T_max`.
    GranularityExceedsMaximum,
    /// `M_W_A` must be in `1..N_SENDMAX`.
    InvalidAcknowledgementThreshold {
        /// Supplied threshold.
        threshold: u16,
        /// Configured local receive window.
        n_send_max: u16,
    },
}

impl fmt::Display for TimingConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroMaximumTime => formatter.write_str("T_max must be non-zero"),
            Self::ZeroHeartbeatTime => formatter.write_str("T_h must be non-zero"),
            Self::GranularityExceedsMaximum => {
                formatter.write_str("timer granularity must not exceed T_max")
            }
            Self::InvalidAcknowledgementThreshold {
                threshold,
                n_send_max,
            } => write!(
                formatter,
                "M_W_A {threshold} must be in 1..N_SENDMAX ({n_send_max})"
            ),
        }
    }
}

impl core::error::Error for TimingConfigurationError {}

/// Result of processing a timeout-relevant message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timeliness {
    /// Round-trip delay `T_rtd`.
    pub t_rtd: u32,
    /// Alive time `T_alive`.
    pub t_alive: u32,
    /// Updated adaptive timer `T_i`.
    pub timer_t_i: u32,
    /// Whether the message remains within `T_max`.
    pub respected: bool,
}

/// Replayable adaptive timing state.
///
/// The adaptive timer `T_i` runs from the moment it is (re)started: when a
/// timeout-relevant message is accepted, with `T_i = T_max - T_rtd`, and when
/// a connection is being established, with `T_i = T_max`. Accepting the next
/// timeout-relevant message replaces it. The connection times out when `T_i`
/// expires, so the peer's latest confirmation of a local timestamp may be at
/// most `T_max` old (`docs/adr/0004-adaptive-timer.md`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingState {
    /// Current adaptive timer `T_i`.
    pub timer_t_i: u32,
    /// Local time at which `T_i` was last (re)started.
    pub timer_started_at: u32,
    /// Last calculated round-trip delay.
    pub t_rtd: u32,
    /// Last calculated alive time.
    pub t_alive: u32,
    /// Confirmed sequence number placed in the last transmitted PDU.
    pub last_sent_confirmed_sequence: u32,
    /// Timestamp placed in the last transmitted PDU.
    pub last_sent_timestamp: u32,
}

impl TimingState {
    /// Creates reset timing state.
    #[must_use]
    pub const fn new(config: TimingConfig) -> Self {
        Self {
            timer_t_i: config.t_max,
            timer_started_at: 0,
            t_rtd: 0,
            t_alive: 0,
            last_sent_confirmed_sequence: 0,
            last_sent_timestamp: 0,
        }
    }

    /// Evaluates and records one timeout-relevant message.
    #[must_use]
    pub fn evaluate(
        &mut self,
        config: TimingConfig,
        now: u32,
        confirmed_timestamp: u32,
        previous_confirmed_timestamp: u32,
    ) -> Timeliness {
        let t_rtd = now
            .wrapping_add(config.timer_granularity)
            .wrapping_sub(confirmed_timestamp);
        let t_alive = now.wrapping_sub(previous_confirmed_timestamp);
        let respected = t_rtd <= config.t_max;
        let timer_t_i = if respected { config.t_max - t_rtd } else { 0 };

        self.t_rtd = t_rtd;
        self.t_alive = t_alive;
        self.timer_t_i = timer_t_i;
        self.timer_started_at = now;
        Timeliness {
            t_rtd,
            t_alive,
            timer_t_i,
            respected,
        }
    }

    /// Starts `T_i` with the full `T_max` while a connection is being
    /// established, before any timeout-relevant message was accepted.
    pub const fn start_establishment(&mut self, config: TimingConfig, now: u32) {
        self.timer_t_i = config.t_max;
        self.timer_started_at = now;
    }

    /// Returns whether the adaptive timer `T_i` has expired.
    #[must_use]
    pub const fn timeout_expired(self, now: u32) -> bool {
        now.wrapping_sub(self.timer_started_at) > self.timer_t_i
    }

    /// Returns whether a heartbeat is due.
    #[must_use]
    pub const fn heartbeat_due(self, config: TimingConfig, now: u32) -> bool {
        now.wrapping_sub(self.last_sent_timestamp) >= config.t_heartbeat
    }

    /// Returns whether `M_W_A` requires an acknowledgement PDU.
    #[must_use]
    pub const fn acknowledgement_due(self, config: TimingConfig, confirmed_sequence: u32) -> bool {
        confirmed_sequence.wrapping_sub(self.last_sent_confirmed_sequence) >= config.m_w_a as u32
    }

    /// Records header values that were actually emitted by an adapter effect.
    pub const fn note_transmitted(&mut self, timestamp: u32, confirmed_sequence: u32) {
        self.last_sent_timestamp = timestamp;
        self.last_sent_confirmed_sequence = confirmed_sequence;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TimingConfig {
        TimingConfig::new(100, 25, 2, 3, 10).unwrap()
    }

    #[test]
    fn adaptive_values_follow_the_timing_formulas() {
        let config = config();
        let mut state = TimingState::new(config);

        assert_eq!(
            state.evaluate(config, 50, 12, 20),
            Timeliness {
                t_rtd: 40,
                t_alive: 30,
                timer_t_i: 60,
                respected: true,
            }
        );
        assert_eq!(
            state.evaluate(config, 200, 20, 50),
            Timeliness {
                t_rtd: 182,
                t_alive: 150,
                timer_t_i: 0,
                respected: false,
            }
        );
    }

    #[test]
    fn deadlines_and_thresholds_are_wrap_safe() {
        let config = config();
        let mut state = TimingState::new(config);
        state.timer_t_i = 5;
        state.timer_started_at = u32::MAX - 3;
        state.note_transmitted(u32::MAX - 10, u32::MAX - 1);

        assert!(state.heartbeat_due(config, 20));
        assert!(!state.timeout_expired(1));
        assert!(state.timeout_expired(2));
        assert!(state.acknowledgement_due(config, 2));
    }

    #[test]
    fn t_i_runs_from_acceptance_so_confirmations_may_be_t_max_old() {
        // T_max 100, granularity 2. A message confirming local time 12
        // arrives at 50: T_rtd = 40, so T_i = 60 runs until 110, which is
        // T_max after the confirmed timestamp (minus the granularity).
        let config = config();
        let mut state = TimingState::new(config);
        assert!(state.evaluate(config, 50, 12, 0).respected);
        assert!(!state.timeout_expired(110));
        assert!(state.timeout_expired(111));

        // A late but still timely message must not expire on acceptance.
        assert!(state.evaluate(config, 200, 120, 12).respected);
        assert_eq!(state.timer_t_i, 18);
        assert!(!state.timeout_expired(200));
        assert!(!state.timeout_expired(218));
        assert!(state.timeout_expired(219));
    }

    #[test]
    fn establishment_starts_t_i_with_t_max() {
        let config = config();
        let mut state = TimingState::new(config);
        state.start_establishment(config, 1_000);
        assert!(!state.timeout_expired(1_100));
        assert!(state.timeout_expired(1_101));
    }
}
