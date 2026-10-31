//! Deterministic bounded datagram network with fault injection.

use std::{collections::VecDeque, fmt, vec::Vec};

use crate::redundancy::codec::MAX_REDUNDANCY_FRAME_LEN;

const MAX_CHANNELS: usize = 2;

/// One owned datagram in the deterministic network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulatedDatagram {
    /// Source endpoint index.
    pub source: u8,
    /// Destination endpoint index.
    pub destination: u8,
    /// Transport channel.
    pub channel: u8,
    /// Scheduled delivery time in wrapping milliseconds.
    pub deliver_at: u32,
    /// Datagram bytes.
    pub bytes: Vec<u8>,
}

/// Fault applied to the next send on a transport channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NextFault {
    /// Drop the next datagram.
    Drop,
    /// Deliver the next datagram twice.
    Duplicate,
    /// Flip one bit in the next datagram.
    Corrupt,
    /// Add a deterministic delay.
    Delay(u32),
    /// Hold the next datagram until a later datagram is queued.
    Reorder,
}

/// Bounded deterministic network used by end-to-end tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeterministicNetwork {
    pending: VecDeque<SimulatedDatagram>,
    held: [Option<SimulatedDatagram>; MAX_CHANNELS],
    next_fault: [Option<NextFault>; MAX_CHANNELS],
    link_available: [[[bool; MAX_CHANNELS]; 2]; 2],
    capacity: usize,
}

impl DeterministicNetwork {
    /// Creates a network with a fixed maximum number of queued datagrams.
    ///
    /// # Errors
    ///
    /// Returns an error if capacity is zero.
    pub fn new(capacity: usize) -> Result<Self, SimulationError> {
        if capacity == 0 {
            return Err(SimulationError::ZeroCapacity);
        }
        Ok(Self {
            pending: VecDeque::with_capacity(capacity),
            held: [None, None],
            next_fault: [None, None],
            link_available: [[[true; MAX_CHANNELS]; 2]; 2],
            capacity,
        })
    }

    /// Enables or disables one directional channel between the two endpoints.
    ///
    /// Disabled links drop datagrams, matching an unavailable network path.
    ///
    /// # Errors
    ///
    /// Returns an error for endpoint IDs outside `0..=1` or an invalid channel.
    pub fn set_link_available(
        &mut self,
        source: u8,
        destination: u8,
        channel: u8,
        available: bool,
    ) -> Result<(), SimulationError> {
        let link = self
            .link_available
            .get_mut(usize::from(source))
            .and_then(|destinations| destinations.get_mut(usize::from(destination)))
            .and_then(|channels| channels.get_mut(usize::from(channel)))
            .ok_or(SimulationError::InvalidRoute {
                source,
                destination,
                channel,
            })?;
        *link = available;
        Ok(())
    }

    /// Simulates an endpoint restart by discarding all packets queued to or from it.
    ///
    /// # Errors
    ///
    /// Returns an error for endpoint IDs outside `0..=1`.
    pub fn restart_endpoint(&mut self, endpoint: u8) -> Result<(), SimulationError> {
        if endpoint > 1 {
            return Err(SimulationError::InvalidEndpoint(endpoint));
        }
        self.pending
            .retain(|entry| entry.source != endpoint && entry.destination != endpoint);
        for held in &mut self.held {
            if held
                .as_ref()
                .is_some_and(|entry| entry.source == endpoint || entry.destination == endpoint)
            {
                *held = None;
            }
        }
        Ok(())
    }

    /// Configures a fault for the next send on `channel`.
    ///
    /// # Errors
    ///
    /// Returns an error for channels outside `0..=1`.
    pub fn inject(&mut self, channel: u8, fault: NextFault) -> Result<(), SimulationError> {
        let slot = self
            .next_fault
            .get_mut(usize::from(channel))
            .ok_or(SimulationError::InvalidChannel(channel))?;
        *slot = Some(fault);
        Ok(())
    }

    /// Queues one complete datagram.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid channel, oversize datagram, or exhausted
    /// configured capacity.
    pub fn send(
        &mut self,
        source: u8,
        destination: u8,
        channel: u8,
        now: u32,
        bytes: &[u8],
    ) -> Result<(), SimulationError> {
        if bytes.len() > MAX_REDUNDANCY_FRAME_LEN {
            return Err(SimulationError::DatagramTooLarge(bytes.len()));
        }
        let channel_index = usize::from(channel);
        let route_available = self
            .link_available
            .get(usize::from(source))
            .and_then(|destinations| destinations.get(usize::from(destination)))
            .and_then(|channels| channels.get(channel_index))
            .copied()
            .ok_or(SimulationError::InvalidRoute {
                source,
                destination,
                channel,
            })?;
        let fault = self
            .next_fault
            .get_mut(channel_index)
            .ok_or(SimulationError::InvalidChannel(channel))?
            .take();
        if !route_available || fault == Some(NextFault::Drop) {
            return Ok(());
        }

        let mut datagram = SimulatedDatagram {
            source,
            destination,
            channel,
            deliver_at: now,
            bytes: bytes.to_vec(),
        };
        match fault {
            Some(NextFault::Corrupt) if !datagram.bytes.is_empty() => datagram.bytes[0] ^= 1,
            Some(NextFault::Delay(delay)) => {
                datagram.deliver_at = datagram.deliver_at.wrapping_add(delay);
            }
            Some(NextFault::Reorder) => {
                self.held[channel_index] = Some(datagram);
                return Ok(());
            }
            _ => {}
        }

        self.push(datagram.clone())?;
        if fault == Some(NextFault::Duplicate) {
            self.push(datagram)?;
        }
        if let Some(held) = self.held[channel_index].take() {
            self.push(held)?;
        }
        Ok(())
    }

    /// Removes the first due datagram for a destination.
    #[must_use]
    pub fn receive(&mut self, destination: u8, now: u32) -> Option<SimulatedDatagram> {
        let index = self.pending.iter().position(|datagram| {
            datagram.destination == destination
                && now.wrapping_sub(datagram.deliver_at) < u32::MAX / 2
        })?;
        self.pending.remove(index)
    }

    /// Returns queued and held datagram count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len() + self.held.iter().filter(|entry| entry.is_some()).count()
    }

    /// Returns whether no datagram is queued or held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn push(&mut self, datagram: SimulatedDatagram) -> Result<(), SimulationError> {
        if self.len() >= self.capacity {
            return Err(SimulationError::CapacityExceeded);
        }
        self.pending.push_back(datagram);
        Ok(())
    }
}

/// Deterministic network configuration or send failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationError {
    /// Capacity must be non-zero.
    ZeroCapacity,
    /// Transport channel is outside `0..=1`.
    InvalidChannel(u8),
    /// Endpoint is outside the two-node simulation.
    InvalidEndpoint(u8),
    /// Source, destination, or channel is outside the simulated topology.
    InvalidRoute {
        /// Source endpoint.
        source: u8,
        /// Destination endpoint.
        destination: u8,
        /// Transport channel.
        channel: u8,
    },
    /// Datagram exceeds the Redundancy Layer maximum.
    DatagramTooLarge(usize),
    /// Pending datagram capacity is exhausted.
    CapacityExceeded,
}

impl fmt::Display for SimulationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => formatter.write_str("simulation capacity must be non-zero"),
            Self::InvalidChannel(channel) => {
                write!(formatter, "simulation channel {channel} is invalid")
            }
            Self::InvalidEndpoint(endpoint) => {
                write!(formatter, "simulation endpoint {endpoint} is invalid")
            }
            Self::InvalidRoute {
                source,
                destination,
                channel,
            } => write!(
                formatter,
                "simulation route {source}->{destination} on channel {channel} is invalid"
            ),
            Self::DatagramTooLarge(length) => {
                write!(formatter, "simulated datagram length {length} is too large")
            }
            Self::CapacityExceeded => formatter.write_str("simulation capacity exceeded"),
        }
    }
}

impl std::error::Error for SimulationError {}
