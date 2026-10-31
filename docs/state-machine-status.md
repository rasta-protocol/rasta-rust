# State-machine implementation status

This table is a development boundary, not a conformance claim.

| State | Implemented behaviour | Open points |
|---|---|---|
| Closed | Role-dependent open, explicit entropy input, channel-open effect, ignored inactive events | None |
| Down | ConnReq validation, version negotiation, ConnResp, invalid service closure | Normative review when the standard is available |
| Start | Client ConnResp/HB transition to Up, server HB transition to Up, closure, adaptive timeliness, and timeout paths | Normative review when the standard is available |
| Up | User/service closure, timer-driven Heartbeat and timeout, sequence/timestamp monitoring, `M_W_A`, Data flow control, confirmation release, RetrReq handling, bounded replay reconstruction, and application delivery | Normative review and differential expansion |
| Retransmission Request | RetrResp validation and transition to Retransmission Running; the peer's RetrReq answered with RetrResp and a rebuilt replay; Heartbeat, Data and RetrData ignored while waiting; timer-driven Heartbeat, timeout and closure. The own RetrReq is not repeated: without a RetrResp the connection times out | Normative review and differential expansion |
| Retransmission Running | RetrData stream processing, ordinary Data boundary handling, nested RetrReq handling, timeout, disconnect, and return to Up | Normative review and differential expansion |

The engine validates endpoint addresses, connection confirmed-sequence rules,
protocol-version ordering, heartbeat sequence ranges, timestamps, and
confirmed timestamps for the implemented establishment path. Wire decoding and
safety-code verification happen before events reach the engine.

As a project design decision, every SR closure also closes and resets the RL
channel. A locally initiated closure emits the disconnection request before
`CloseRedundancyChannel` and the SR state-change notification. This preserves
the `Closed` invariant and lets both endpoints reopen their existing
`StackEngine` after a timeout, protocol error, or user closure. Keeping the
local RL channel open used to strand the next connection request: the peer
discarded it as out of window or as a duplicate, depending on which endpoint
closed first. This lifecycle decision still awaits normative review.

`ConnectionBuffers` is separately owned by the caller and passed explicitly to
each engine step. Both rings are allocation-free. The send ring retains sent
messages until peer confirmation and holds newer messages when the peer window
is exhausted. Accepted Data payloads enter the bounded receive ring and emit
`ApplicationDataAvailable`; the application retrieves them through
`pop_received`. A full receive ring rejects the transition atomically; in the
composed stack, received PDUs instead wait in the receive queue until the ring
has room (OBS-SBB-RL-007).

The separate RL engine has Closed/Up lifecycle states, its own wrapping sequence
space, duplicate suppression, a bounded defer queue, strict `T_seq` expiry, and
diagnostic counters. The composed stack commits neither SRL nor RL state if
decoding or translation fails.
