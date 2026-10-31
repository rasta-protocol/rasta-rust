# Implemented architecture boundary

This records the implemented alpha design. It is not an accepted normative ADR
and does not make a conformance or safety claim.

`ConnectionEngine` owns deterministic SRL state. The caller owns
`ConnectionBuffers`; input consists only of a wrapping millisecond timestamp and
an explicit event. Effects request transmission, RL lifecycle changes, state
notifications, or application notification.

`RedundancyEngine` is independent of SRL. It accepts complete encoded SR PDUs,
assigns its own sequence number, emits one transmission per configured channel,
and delivers only ordered, de-duplicated PDUs upward. Diagnostics remain RL
outputs and are not synthesized into SRL protocol events.

`StackEngine` atomically composes:

1. SRL state and bounded buffers;
2. SR encoding and MD4 safety code;
3. RL sequencing, duplication, and defer handling;
4. RL framing and check code.

It returns complete datagrams with a channel index. It does not perform I/O.
Decode or capacity failures leave both protocol engines unchanged.

The `std` UDP adapter owns sockets and monotonic-clock conversion; a host passes
the stack's datagrams through its `send_datagram` and `receive_datagram`, as
`examples/udp_two_node.rs` shows for two paths. The simulator
owns injected nondeterminism. Neither is part of the `no_std` protocol core.
Hosts with their own runtime, for example an asynchronous one, drive a
`StackEngine` from their event loop in the same way.

## Resource and backpressure limits

- SR application message: at most 1055 bytes (`MAX_APPLICATION_PAYLOAD`).
- SR application-data field: at most 1057 bytes (`MAX_DATA_FIELD_LEN`), with
  at most `N_maxPacket` application messages (1 by default, at most 352).
- SR application/send window: 20 entries. Three internal control slots are
  reserved only while reconstructing a maximum retransmission burst.
- RL payload: at most 1101 bytes.
- RL frame: at most 1113 bytes.
- RL receive queue: 20 SR PDUs between RL and SR (OBS-SBB-RL-007). RL takes a
  frame only while the queue has more free entries than the defer queue holds
  frames (`StackEngine::can_receive_datagram`); otherwise the host leaves the
  datagram in its transport. SR takes a queued PDU only while its receive
  buffer has 1 free entry and its send buffer 3, at most eleven per operation.
- RL defer queue: configured 4–10 entries.
- RL diagnosis window: configured 1–1000 frames, stored in a fixed 1000-entry
  array (about 12 kB per engine). A frame that finds the queue full is
  discarded and counted in `RedundancyDiagnostics::defer_queue_full`.
- Transport channels: one or two.
- Stack actions and effects: fixed-capacity arrays sized for a maximum ten-frame
  RL defer flush with two-channel acknowledgements, plus one SR and two RL
  diagnostic reports; capacity failure is typed and atomic.
- No implicit fragmentation or unbounded protocol allocation is performed.
