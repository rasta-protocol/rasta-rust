# Glossary

Terms and abbreviations used in RaSTA-Rust's code and documentation. Names of
the corresponding Rust items are given where they exist.

## Protocol layers and units

**RaSTA** — Rail Safe Transport Application: a protocol for safety-related
communication in railway signalling over IP networks.

**SRL, SR layer** — Safety and Retransmission Layer. Manages connections and
protects messages with sequence numbers, timestamps, timeliness supervision,
retransmission, flow control and a safety code (`engine::connection`,
`codec::sr`).

**RL** — Redundancy Layer. Sends every SRL message over one or two transport
channels, removes duplicates and briefly reorders (`redundancy`).

**PDU** — Protocol data unit: one encoded message of a layer. An *SR PDU* is
an SRL message; an *RL frame* is an RL header, an SR PDU and a check code.

**Application message** — One message of the application. RaSTA-Rust
supports 1 to 1055 bytes (`MAX_APPLICATION_PAYLOAD`, `ApplicationPayload`) and
does not fragment larger messages.

**Application-data field** — The body of a Data or RetrData message: one or
more application messages, each preceded by its length as a little-endian
`u16`. RaSTA-Rust's field holds at most 1057 bytes (`MAX_DATA_FIELD_LEN`,
`ApplicationData`): one maximal message, or several smaller ones.

**Packetization, `N_maxPacket`** — Sending several application messages in
one Data message. `N_maxPacket` is the most a Data message may hold; both
endpoints of a connection use the same value. RaSTA-Rust defaults to 1 and
closes a connection whose peer sends more
(`ConnectionConfig::with_max_packet`, `ConnectionEvent::SendPacked`).

**RaSTA network** — The endpoints that can reach each other at transport level
and use the same safety code. Each network has an identifier that maps to its
own MD4 initial value (`Md4InitialValue`); the identifier itself is not sent.
Endpoint identifiers need to be unique only within a network. Without a
safety code (`SafetyCodeLength::None`), messages from different networks
cannot be told apart.

**Transport channel** — One network path, here a UDP socket pair.

**Redundancy channel** — The RL channel of one connection, made up of one or
two transport channels.

## Connections and messages

**Client, server** — The endpoint that sends the connection request, and the
endpoint that waits for it and answers; the client completes the handshake
with a heartbeat. These are protocol roles for the handshake, not functions in
the railway system. They follow from the identifiers: the endpoint with the
lower identifier is the client, the one with the higher identifier the server
(`ConnectionConfig::role`, `Role`). Every message carries the sender's and
receiver's identifiers.

**ConnReq, ConnResp** — Connection request and connection response. They open
an SRL connection and exchange the protocol version and `N_SENDMAX`.

**HB** — Heartbeat: a message without payload, sent when nothing else has been
sent for `T_h`, so that the peer can supervise the connection.

**Data, RetrData** — Application data, and application data sent again during
retransmission.

**RetrReq, RetrResp** — Retransmission request (the receiver detected a gap in
the sequence numbers) and retransmission response (the sender starts sending
the missing messages again).

**DiscReq** — Disconnection request. Closes a connection and carries a reason:
user request, unexpected message, sequence-number error, timeout, service not
allowed, protocol-version error, retransmission failed, or protocol-sequence
error (`DisconnectReason`).

**Protocol version** — Four ASCII digits exchanged during connection setup,
for example `0303` (`ProtocolVersion`).

## Sequence numbers and confirmations

**SN** — Sequence number of a message, counted per connection and direction,
with wrap-around.

**CS** — Confirmed sequence number: the peer's latest sequence number the
sender has received. It releases messages from the peer's send buffer.

**TS** — Timestamp: the sender's local time when the message was sent, in
milliseconds with wrap-around. The two endpoints' clocks are independent.

**CTS** — Confirmed timestamp: the peer's latest timestamp the sender has
received. Comparing it with the local clock measures the round-trip delay.

**`N_SENDMAX`** — How many messages an endpoint accepts without having
confirmed them; the peer may not send more unconfirmed messages than that.
2 to 20.

**`M_W_A`** — How many received messages may stay unconfirmed before an
endpoint sends a confirmation even without other traffic.

## Timing

**`T_max`** — The maximum accepted age of the peer's most recent confirmation.
A connection whose confirmations become older is closed.

**`T_rtd`** — Round-trip delay of a received message:
`local time + timer granularity - CTS`.

**Timeliness** — A message is timely if `T_rtd <= T_max`; an untimely message
closes the connection.

**`T_i`** — Adaptive timer: `T_max - T_rtd`, started when a timely message is
accepted and replaced by the next one. The connection times out when it
expires ([ADR 0004](adr/0004-adaptive-timer.md)).

**`T_h`** — Heartbeat period. An idle connection stays up while the two
endpoints' heartbeat periods and the latency together stay below `T_max`.

**`T_alive`** — Time between successive confirmations from the peer; recorded
for diagnostics.

**Timer granularity** — Resolution of the local clock in milliseconds, added
to `T_rtd` so that it is never underestimated. It is a property of the host
platform: 1 ms by default (`DEFAULT_TIMER_GRANULARITY`, also the resolution of
`ProtocolClock`), set with `ConnectionConfig::with_timer_granularity` for a
coarser clock.

**`T_seq`** — How long the RL holds a frame that arrived early while waiting
for a missing predecessor, 50 to 500 ms.

**Defer queue** — The RL's holding area for such frames, 4 to 10 entries.

**Receive queue** — Received SR PDUs waiting until the SRL has room, 20
entries. While it lacks room, datagrams stay in the transport.

**Diagnostic window** — The number of accepted timely messages (SRL) or
distinct frames (RL, `N_diagnosis`) after which diagnostics are reported and
reset.

**`N_missed`, `T_drift`** — RL diagnostics per transport channel: frames that
did not arrive on the channel within `T_seq` of their first arrival, and the
summed delay behind the first arrival (`T_drift2` sums the squares).

## Integrity

**Safety code** — Code over every SR PDU that detects corruption: none, the
first 8 bytes of an MD4 digest, or the full 16 bytes (`SafetyCodeLength`). It
is computed with configurable initial values (`Md4InitialValue`) and is not
a cryptographic protection.

**Check code** — Code over every RL frame: none or one of four CRC profiles
(`CheckCodeType` A to E).

## Safety context

**EN 50159** — European standard for safety-related communication in
transmission systems. It names the threats to messages in transit
(repetition, deletion, insertion, resequencing, corruption, delay,
masquerade) that protocols such as RaSTA counter.

**SIL** — Safety integrity level, 1 to 4. SIL 4 is the highest, required for
example for interlockings.

**EULYNX** — A European initiative that standardises interfaces in railway
signalling; RaSTA is used as the transport for its interfaces.

**Interlocking** — The signalling system that sets and locks routes so that
train movements cannot conflict.

## In this repository

**`StackEngine`** — The composed SRL and RL stack without I/O; the host feeds
it commands, datagrams and time, and executes the actions it returns.

**`ConnectionEngine`, `RedundancyEngine`** — The SRL and RL state machines on
their own.

**Observation (`OBS-*`)** — Behaviour recorded from a pinned public
implementation, with its evidence (`requirements/observations.md`). Not a
normative requirement.

**ADR** — Architecture decision record (`adr/`).

**Reference implementations** — The public RaSTA implementations used as
behavioural references and interoperability peers (`provenance.md`).
