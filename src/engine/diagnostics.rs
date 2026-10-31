//! SR connection diagnostics: error counters and timing distributions
//! (OBS-SBB-DIAG-001, OBS-SBB-DIAG-002).

use core::fmt;

/// Number of intervals in each timing distribution.
pub const TIMING_INTERVALS: usize = 5;

/// Validated diagnostic window and timing-distribution bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticsConfig {
    window: u32,
    interval_bounds: [u32; TIMING_INTERVALS - 1],
}

impl DiagnosticsConfig {
    /// Creates a diagnostics configuration.
    ///
    /// `window` is the number of accepted timeout-relevant messages
    /// (Heartbeat, Data, Retransmitted Data) after which the diagnostics are
    /// reported and reset. `interval_bounds` are the upper bounds
    /// `T1 < T2 < T3 < T4` of the first four timing intervals; the fifth
    /// interval reaches `T_max`. The connection configuration checks that
    /// `T4` is below `T_max`.
    ///
    /// # Errors
    ///
    /// Returns an error if `window` or `T1` is zero, or the bounds do not
    /// increase strictly.
    pub const fn new(
        window: u32,
        interval_bounds: [u32; TIMING_INTERVALS - 1],
    ) -> Result<Self, DiagnosticsConfigurationError> {
        if window == 0 {
            return Err(DiagnosticsConfigurationError::ZeroWindow);
        }
        if interval_bounds[0] == 0 {
            return Err(DiagnosticsConfigurationError::ZeroFirstBound);
        }
        let mut index = 1;
        while index < interval_bounds.len() {
            if interval_bounds[index - 1] >= interval_bounds[index] {
                return Err(DiagnosticsConfigurationError::BoundsNotIncreasing);
            }
            index += 1;
        }
        Ok(Self {
            window,
            interval_bounds,
        })
    }

    /// Returns the diagnostic window in accepted timeout-relevant messages.
    #[must_use]
    pub const fn window(self) -> u32 {
        self.window
    }

    /// Returns the upper bounds of the first four timing intervals.
    #[must_use]
    pub const fn interval_bounds(self) -> [u32; TIMING_INTERVALS - 1] {
        self.interval_bounds
    }

    fn interval(self, value: u32) -> usize {
        self.interval_bounds
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(TIMING_INTERVALS - 1)
    }
}

/// Invalid diagnostics configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsConfigurationError {
    /// The diagnostic window must be non-zero.
    ZeroWindow,
    /// The first interval bound must be non-zero.
    ZeroFirstBound,
    /// The interval bounds must increase strictly.
    BoundsNotIncreasing,
    /// The last interval bound must be below `T_max`.
    LastBoundNotBelowMaximum,
}

impl fmt::Display for DiagnosticsConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroWindow => formatter.write_str("diagnostic window must be non-zero"),
            Self::ZeroFirstBound => formatter.write_str("first interval bound must be non-zero"),
            Self::BoundsNotIncreasing => {
                formatter.write_str("interval bounds must increase strictly")
            }
            Self::LastBoundNotBelowMaximum => {
                formatter.write_str("last interval bound must be below T_max")
            }
        }
    }
}

impl core::error::Error for DiagnosticsConfigurationError {}

/// SR diagnostics collected over one diagnostic window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConnectionDiagnostics {
    /// Received PDUs whose safety code did not match.
    pub safety_code_errors: u32,
    /// Received PDUs with a wrong sender or receiver identifier.
    pub address_errors: u32,
    /// Received PDUs with an undefined message type.
    pub type_errors: u32,
    /// Received PDUs whose sequence number was not the expected one.
    pub sequence_errors: u32,
    /// Received PDUs with an implausible confirmed sequence number.
    pub confirmed_sequence_errors: u32,
    /// Accepted timeout-relevant messages per round-trip-delay interval.
    pub round_trip_delays: [u32; TIMING_INTERVALS],
    /// Accepted timeout-relevant messages per alive-time interval.
    pub alive_times: [u32; TIMING_INTERVALS],
}

impl ConnectionDiagnostics {
    pub(crate) const EMPTY: Self = Self {
        safety_code_errors: 0,
        address_errors: 0,
        type_errors: 0,
        sequence_errors: 0,
        confirmed_sequence_errors: 0,
        round_trip_delays: [0; TIMING_INTERVALS],
        alive_times: [0; TIMING_INTERVALS],
    };

    /// Returns the number of accepted timeout-relevant messages in the window.
    #[must_use]
    pub fn messages(&self) -> u32 {
        self.round_trip_delays
            .iter()
            .fold(0, |sum, count| sum.wrapping_add(*count))
    }

    pub(crate) fn record_timing(
        &mut self,
        config: DiagnosticsConfig,
        round_trip_delay: u32,
        alive_time: u32,
    ) {
        let delay = &mut self.round_trip_delays[config.interval(round_trip_delay)];
        *delay = delay.wrapping_add(1);
        let alive = &mut self.alive_times[config.interval(alive_time)];
        *alive = alive.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_requires_a_window_and_strictly_increasing_bounds() {
        assert!(DiagnosticsConfig::new(1, [1, 2, 3, 4]).is_ok());
        assert_eq!(
            DiagnosticsConfig::new(0, [1, 2, 3, 4]),
            Err(DiagnosticsConfigurationError::ZeroWindow)
        );
        assert_eq!(
            DiagnosticsConfig::new(1, [0, 2, 3, 4]),
            Err(DiagnosticsConfigurationError::ZeroFirstBound)
        );
        for bounds in [[1, 1, 3, 4], [1, 2, 2, 4], [1, 2, 3, 3], [4, 3, 2, 1]] {
            assert_eq!(
                DiagnosticsConfig::new(1, bounds),
                Err(DiagnosticsConfigurationError::BoundsNotIncreasing)
            );
        }
    }

    #[test]
    fn timing_values_fall_into_inclusive_upper_bounded_intervals() {
        let config = DiagnosticsConfig::new(10, [150, 300, 450, 600]).unwrap();
        let mut diagnostics = ConnectionDiagnostics::default();
        for (value, alive) in [(0, 0), (150, 151), (300, 450), (451, 600), (601, 750)] {
            diagnostics.record_timing(config, value, alive);
        }
        assert_eq!(diagnostics.round_trip_delays, [2, 1, 0, 1, 1]);
        assert_eq!(diagnostics.alive_times, [1, 1, 1, 1, 1]);
        assert_eq!(diagnostics.messages(), 5);
    }
}
