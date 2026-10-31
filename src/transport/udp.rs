//! Datagram-preserving UDP adapter for Redundancy Layer frames.
//!
//! A host driving a `StackEngine` passes its datagrams through
//! [`UdpRedundancyAdapter::send_datagram`] and
//! [`UdpRedundancyAdapter::receive_datagram`]. [`UdpRedundancyAdapter::transmit`]
//! and [`UdpRedundancyAdapter::receive`] encode and decode frames for hosts that
//! drive the Redundancy Layer themselves.

use std::{
    fmt, io,
    net::{SocketAddr, UdpSocket},
    time::Instant,
};

use crate::{
    redundancy::codec::{
        decode, CheckCodeType, RedundancyCodecError, RedundancyFrame, MAX_REDUNDANCY_FRAME_LEN,
    },
    stack::WireDatagram,
};

const MAX_CHANNELS: usize = 2;
const RECEIVE_BUDGET: usize = 32;

/// Monotonic adapter clock converted to wrapping protocol milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct ProtocolClock {
    origin: Instant,
}

impl ProtocolClock {
    /// Resolution of [`Self::now`] in milliseconds, for
    /// `ConnectionConfig::with_timer_granularity`.
    pub const GRANULARITY: u32 = 1;

    /// Starts a clock at the current monotonic instant.
    #[must_use]
    pub fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    /// Returns wrapping milliseconds since this clock was started.
    #[must_use]
    pub fn now(&self) -> u32 {
        wrapping_millis(self.origin.elapsed().as_millis())
    }
}

fn wrapping_millis(millis: u128) -> u32 {
    let bytes = millis.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// One configured UDP transport channel.
#[derive(Debug)]
pub struct UdpChannel {
    socket: UdpSocket,
    peer: SocketAddr,
}

impl UdpChannel {
    /// Binds one non-blocking UDP socket and configures its fixed peer.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn bind(local: SocketAddr, peer: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local)?;
        Self::from_socket(socket, peer)
    }

    /// Configures an already-bound UDP socket and its fixed peer.
    ///
    /// # Errors
    ///
    /// Returns the underlying error if non-blocking mode cannot be enabled.
    pub fn from_socket(socket: UdpSocket, peer: SocketAddr) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self { socket, peer })
    }

    /// Returns the actual bound local endpoint.
    ///
    /// # Errors
    ///
    /// Returns the underlying socket error.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn send(&self, datagram: &[u8]) -> io::Result<()> {
        let sent = self.socket.send_to(datagram, self.peer)?;
        if sent == datagram.len() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "UDP adapter did not send the complete datagram",
            ))
        }
    }

    fn receive(&self, output: &mut [u8]) -> io::Result<Option<usize>> {
        // Bound the work so continuous noise cannot starve protocol timers.
        for _ in 0..RECEIVE_BUDGET {
            match self.socket.recv_from(output) {
                Ok((received, source)) if source == self.peer => return Ok(Some(received)),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                // Windows reports an ICMP port-unreachable reply to an earlier
                // send as a reset on a later receive; it says nothing about
                // the datagrams still waiting.
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }
}

/// Up to two independent UDP paths for one redundancy channel.
#[derive(Debug)]
pub struct UdpRedundancyAdapter {
    channels: [Option<UdpChannel>; MAX_CHANNELS],
    channel_count: u8,
    check_code: CheckCodeType,
}

impl UdpRedundancyAdapter {
    /// Creates an adapter from one or two configured channels.
    ///
    /// The arguments guarantee a valid channel count; construction is infallible.
    #[must_use]
    pub fn new(first: UdpChannel, second: Option<UdpChannel>, check_code: CheckCodeType) -> Self {
        let channel_count = if second.is_some() { 2 } else { 1 };
        Self {
            channels: [Some(first), second],
            channel_count,
            check_code,
        }
    }

    /// Returns configured channel count.
    #[must_use]
    pub const fn channel_count(&self) -> u8 {
        self.channel_count
    }

    /// Encodes and transmits one redundancy frame on the selected channel.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid channel, frame codec failure, or socket
    /// transmission failure.
    pub fn transmit(
        &self,
        channel: u8,
        sequence_number: u32,
        payload: &[u8],
    ) -> Result<(), UdpAdapterError> {
        let channel = self.channel(channel)?;
        let frame = RedundancyFrame {
            sequence_number,
            payload,
        };
        let mut datagram = [0_u8; MAX_REDUNDANCY_FRAME_LEN];
        let written = frame
            .encode_into(self.check_code, &mut datagram)
            .map_err(UdpAdapterError::Codec)?;
        channel
            .send(&datagram[..written])
            .map_err(UdpAdapterError::Io)
    }

    /// Transmits one encoded `StackEngine` datagram on its channel.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid channel or socket transmission failure.
    pub fn send_datagram(&self, datagram: &WireDatagram) -> Result<(), UdpAdapterError> {
        self.channel(datagram.channel())?
            .send(datagram.as_slice())
            .map_err(UdpAdapterError::Io)
    }

    /// Receives at most one datagram from a selected channel without decoding
    /// it, for `StackEngine::receive_datagram`.
    ///
    /// The returned bytes borrow `output`, preserving the UDP datagram
    /// boundary without allocation. Up to 32 datagrams from unexpected peers
    /// are skipped per call; exhausting that budget returns `Ok(None)` so the
    /// caller can service protocol timers before receiving again. A
    /// connection reset is skipped in the same way: Windows reports one on an
    /// unconnected UDP socket after an earlier datagram drew an ICMP
    /// port-unreachable reply, for example while the peer is restarting.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid channel, another socket error, or a
    /// datagram longer than [`MAX_REDUNDANCY_FRAME_LEN`].
    pub fn receive_datagram<'a>(
        &self,
        channel: u8,
        output: &'a mut [u8; MAX_REDUNDANCY_FRAME_LEN],
    ) -> Result<Option<&'a [u8]>, UdpAdapterError> {
        let socket = self.channel(channel)?;
        let mut datagram = [0_u8; MAX_REDUNDANCY_FRAME_LEN + 1];
        let Some(received) = socket.receive(&mut datagram).map_err(UdpAdapterError::Io)? else {
            return Ok(None);
        };
        if received > MAX_REDUNDANCY_FRAME_LEN {
            return Err(UdpAdapterError::DatagramTooLarge(received));
        }
        output[..received].copy_from_slice(&datagram[..received]);
        Ok(Some(&output[..received]))
    }

    /// Receives and validates at most one frame from a selected channel.
    ///
    /// The returned frame borrows `output`. Datagrams are received as by
    /// [`Self::receive_datagram`].
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid channel, socket error,
    /// or malformed frame.
    pub fn receive<'a>(
        &self,
        channel: u8,
        output: &'a mut [u8; MAX_REDUNDANCY_FRAME_LEN],
    ) -> Result<Option<RedundancyFrame<'a>>, UdpAdapterError> {
        let Some(datagram) = self.receive_datagram(channel, output)? else {
            return Ok(None);
        };
        decode(datagram, self.check_code)
            .map(Some)
            .map_err(UdpAdapterError::Codec)
    }

    fn channel(&self, channel: u8) -> Result<&UdpChannel, UdpAdapterError> {
        self.channels
            .get(usize::from(channel))
            .and_then(Option::as_ref)
            .ok_or(UdpAdapterError::InvalidChannel(channel))
    }
}

/// UDP redundancy adapter failure.
#[derive(Debug)]
pub enum UdpAdapterError {
    /// Requested transport channel is not configured.
    InvalidChannel(u8),
    /// Redundancy frame codec failure.
    Codec(RedundancyCodecError),
    /// Received datagram exceeded the configured frame maximum.
    DatagramTooLarge(usize),
    /// Operating-system UDP failure.
    Io(io::Error),
}

impl fmt::Display for UdpAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannel(channel) => write!(formatter, "UDP channel {channel} is invalid"),
            Self::Codec(error) => write!(formatter, "redundancy frame error: {error}"),
            Self::DatagramTooLarge(length) => {
                write!(
                    formatter,
                    "redundancy datagram length {length} is too large"
                )
            }
            Self::Io(error) => write!(formatter, "UDP error: {error}"),
        }
    }
}

impl std::error::Error for UdpAdapterError {}
