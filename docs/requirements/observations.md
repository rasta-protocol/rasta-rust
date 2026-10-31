# Behavioural observations

These are **not normative RaSTA requirements**. They record behaviour observed
in the pinned public implementations listed in `../provenance.md`. Each item
must be checked against the official specification before it is promoted to a
normative project requirement.

| ID | Observation | Initial evidence |
|---|---|---|
| OBS-SBB-SR-001 | SR multibyte integer fields are encoded little-endian. | `srmsg_sr_messages.c` byte-wise writers/readers |
| OBS-SBB-SR-002 | The common SR header is 28 bytes: length and type as `u16`, followed by six `u32` fields. | `srtyp_sr_types.h`, `srmsg_sr_messages.c` |
| OBS-SBB-SR-003 | Message type values are 6200, 6201, 6212, 6213, 6216, 6220, 6240, and 6241. | `srtyp_sr_types.h` |
| OBS-SBB-SR-004 | Connection request/response bodies contain four ASCII version digits, `N_SENDMAX` as `u16`, and eight reserved bytes. | `srmsg_sr_messages.c` |
| OBS-SBB-SR-005 | Data PDUs contain a `u16` payload length followed by at most 1055 payload bytes in the observed profile. | `srmsg_sr_messages.c`, `radef_rasta_definitions.h` |
| OBS-SBB-SR-006 | Safety-code widths are 0, 8 (lower MD4), or 16 (full MD4) bytes and the code trails the PDU. | `srtyp_sr_types.h`, `srmsg_sr_messages.c` |
| OBS-SBB-SR-007 | The observed profile accepts `N_SENDMAX` in the inclusive range 2 through 20. | SBB configuration validation and tests |
| OBS-SBB-SR-008 | Disconnect reasons use values 0 through 8. | `srtyp_sr_types.h` |
| OBS-SBB-SR-009 | The MD4 input is the encoded PDU excluding the trailing safety code; its length field already includes that code. | `srmsg_sr_messages.c` safety-code update/check functions |
| OBS-SBB-SR-010 | MD4 uses four configurable 32-bit initial state words. | `srmd4_sr_md4.c`, `srcty_sr_config_types.h` |
| OBS-SBB-SR-011 | Lower MD4 consists of the first eight digest bytes; full MD4 consists of all sixteen bytes. | `srmsg_sr_messages.c`, `unit_test_srmd4.cc` |
| OBS-SBB-SR-012 | The send buffer is a 20-entry FIFO ring that separately counts queued messages and messages not yet sent; confirmations remove only the sent prefix through the confirmed sequence number, including across `u32` wrap. | `srsend_sr_send_buffer.c`, `radef_rasta_definitions.h` |
| OBS-SBB-SR-013 | The application receive buffer is a payload-only FIFO ring whose active capacity is the local `N_SENDMAX`, within a 20-entry backing array. | `srrece_sr_received_buffer.c` |
| OBS-SBB-SR-015 | SBB puts exactly one application message into each Data or RetrData message and accepts a received one only if its first length prefix covers the rest of the field. A message holding several length-prefixed application messages fails the size check and is discarded like a corrupted one. | `srmsg_CheckMessage`, `srmsg_GetDataMessagePayload`, `srcor_sr_core.c` (message check) |
| OBS-SBB-SM-001 | Connection states are Closed, Down, Start, Up, Retransmission Request, and Retransmission Running after initialization. | `srstm_sr_state_machine.c`, `sraty_sr_api_types.h` |
| OBS-SBB-SM-002 | Opening initializes both roles with an externally random outbound sequence number; a client sends ConnReq and enters Start, while a server enters Down. | `srcor_InitRaStaConnData`, `ProcessStateClosedEvents` |
| OBS-SBB-SM-012 | The connection role follows from the identifiers, not from configuration: an endpoint whose own identifier is lower than the peer's is the client, otherwise the server. | `srcor_IsConnRoleServer` |
| OBS-SBB-SM-003 | A server accepts ConnReq in Down, sends ConnResp, and enters Start; a client accepts ConnResp in Start, sends Heartbeat, and enters Up; the server enters Up after the in-sequence Heartbeat. | `ProcessStateDownEvents`, `ProcessStateStartEvents` |
| OBS-SBB-SM-004 | ConnReq uses confirmed sequence number and confirmed timestamp zero. Sequence counters use wrapping `u32` arithmetic. | `srcor_SendConnReqMessage`, `srcor_SendPendingMessages` |
| OBS-SBB-SM-005 | A received connection protocol version is accepted when it is lexicographically equal to or greater than the local four-digit version. | `srcor_IsProtocolVersionAccepted` |
| OBS-SBB-SM-006 | Establishment errors emit disconnection requests with distinct version, sequence, protocol-sequence, timeout, unexpected-message, or service-not-allowed reasons. | `ProcessStateDownEvents`, `ProcessStateStartEvents` |
| OBS-SBB-SM-007 | In Up, an in-sequence Heartbeat is processed without a state change; an admissible sequence gap discards the PDU, sends RetrReq, and enters Retransmission Request. | `ProcessStateUpEvents`, `srcor_SendRetrReqMessage` |
| OBS-SBB-SM-008 | Sending is permitted while the number of sent but unconfirmed messages is less than the peer's advertised `N_SENDMAX`; otherwise newly created messages remain queued. | `SendPendingMessagesWithFlowControlAllowed`, `srsend_ReadMessageToSend` |
| OBS-SBB-SM-009 | In Up, an in-sequence Data message passes the same sequence, confirmed-timestamp, and timeliness processing as Heartbeat; its payload enters the bounded receive buffer and triggers an application notification. A sequence gap instead discards the PDU, sends RetrReq, and enters Retransmission Request. | `ProcessStateUpEvents`, `srcor_ProcessReceivedMessage`, `srrece_AddToBuffer` |
| OBS-SBB-TIME-001 | Protocol timestamps and adapter timers use wrapping millisecond counters. | `srcor_sr_core.c`, SBB system adapter |
| OBS-SBB-TIME-002 | `T_rtd` is calculated from current time, timer granularity, and the peer-confirmed local timestamp. | `srcor_CalculateRtd`, timing unit tests |
| OBS-SBB-TIME-003 | `T_i` is `T_max - T_rtd` while `T_rtd <= T_max`, otherwise zero; timeout uses a strict elapsed-time comparison. | `srcor_CalculateTi`, timer processing |
| OBS-SBB-TIME-004 | `T_alive` records elapsed time since the preceding peer-confirmed timestamp and is diagnostic rather than the heartbeat period. | `srcor_CalculateRtd`, diagnostic data |
| OBS-SBB-TIME-005 | With the default configuration of the pinned revision (`T_max` 750 ms, `T_h` 300 ms), two SBB endpoints time out repeatedly on an idle link, first after about 0.45 s; the configuration validation rejects every `T_h` below 300 ms. Changing only the timeout comparison to measure `T_i` from acceptance keeps the idle link up. | Local two-endpoint bench of the pinned SR layer with an in-memory link, 1 ms and 10 ms polling, 1 to 20 ms latency (2026-10-09); `srcfg_sr_config.c`, `srcty_sr_config_types.c` |
| OBS-SBB-TIME-007 | On an idle link between an SBB endpoint (`T_max` 750 ms, `T_h` 300 ms) and RaSTA-Rust with the same `T_max`, the SBB side times out when RaSTA-Rust's `T_h` is 187 or 300 ms, and stays up for 20 s when it is 100 or 50 ms, in both client/server role assignments. | Two-process UDP interoperability test with the complete SBB stack (2026-10-10) |
| OBS-SBB-TIME-006 | The SBB design documentation describes `T_max` as the maximum accepted age of a message and the timeout check as elapsed time exceeding that age, but states the check's formula with `T_i` in place of `T_max`. | SBB-RaSTA-071 design specification: `srcor_IsMessageTimeout`, `srcty_SafetyRetransmissionConfiguration::t_max` |
| OBS-SBB-SM-010 | `M_W_A` triggers an acknowledgement after the configured number of newly received sequenced messages since the last transmitted confirmation. | `srcor_IsAckRequired`, message processing |
| OBS-SBB-SM-011 | RetrReq identifies the first missing sequence through its confirmed sequence, and RetrResp plus RetrData rebuilds a bounded replay stream from retained send-buffer entries. | retransmission handlers, send-buffer unit tests |
| OBS-SBB-SM-013 | Rebuilding the send buffer for a retransmission starts with RetrResp and gives every following message the next sequence number. Sent Data and RetrData become RetrData; other sent messages are dropped. Unsent RetrData is kept while the replay stream is open (SBB treats one after the stream has ended as an internal error). Unsent Data ends the stream and is kept. Unsent RetrReq ends an open stream with a Heartbeat and is then always kept. Unsent Heartbeats are dropped. A stream still open after the last message ends with a Heartbeat. | `srsend_PrepareBufferForRetr` |
| OBS-SBB-SM-014 | A received DiscReq closes the redundancy channel and reports the Closed state with the disconnect reason "not in use"; the reason and detailed reason in the PDU are not read. A sent DiscReq carries the detailed reason the application supplied with its close request, zero by default. | `ProcessState*Events` (DiscReq received), `UpdateConnectionState`, `srapi_CloseConnection`, `srcor_SendDiscReqMessage`; `srmsg_GetDiscMessageData` is called only from unit tests |
| OBS-SBB-RL-001 | The redundancy header is 8 bytes: total length, two reserved zero bytes, and a little-endian `u32` sequence number. It wraps the SR PDU plus a profile-dependent check code. | SBB redundancy-layer message implementation |
| OBS-SBB-RL-002 | RL uses a sequence space independent of SRL and sends one logical frame over every configured transport channel. | SBB redundancy state machine and transport interface |
| OBS-SBB-RL-003 | Older and duplicate frames are discarded; admissible future frames are held in a bounded defer queue. An admissible future frame that arrives while the queue is full is discarded without an error. | SBB receive/defer processing, `reddfq_AddMessage` |
| OBS-SBB-RL-004 | A missing sequence is skipped only after strict `T_seq` expiry, after which contiguous deferred frames are delivered in order. | SBB sequence timer processing |
| OBS-SBB-RL-005 | Observed check-code profiles include no code, polynomial `0xEE5B42FD`, CRC-32C, CRC-16/KERMIT, and CRC-16/ARC. | SBB redundancy check-code configuration and implementations |
| OBS-SBB-RL-006 | A received frame is discarded while both the receive and transmit sequence numbers are zero and the frame's sequence number is not. The rule has no other condition, so it also applies when both counters wrap to zero at the same time during a session. | `redcor_ProcessReceivedMessage` |
| OBS-SBB-RL-007 | Delivered SR PDUs wait in a 20-entry RL received buffer, which is reset when the redundancy channel is initialised. RL reads a frame from a transport channel only while that buffer has more free entries than the defer queue holds frames; otherwise the frame stays in the transport. Adding to a full buffer is a fatal error, which this condition prevents. SR takes PDUs from the buffer when RL reports a delivery and in its periodic timing check, but only while its receive buffer has at least 1 free entry and its send buffer at least 3; reading application data does not trigger it. The application may fill the send buffer until no entry is free, so from 18 used entries SR takes no PDU, not even confirmations. | `redrbf_red_received_buffer.c`, `ReceivedMessagesPolling`, `redtrn_MessageReceivedNotification`, `srcor_IsReceivedMsgPendingAndBuffersNotFull`, `srapi_CheckTimings`, `sradno_MessageReceivedNotification`, `srapi_SendData` |
| OBS-SBB-DIAG-001 | SR counts errors in received PDUs while checking them in this order: safety code (counted), message type (counted), size (not counted), sender and receiver identifiers (counted), sequence-number range for RetrReq, DiscReq, HB, Data and RetrData (not counted), confirmed sequence number (counted), timestamp order for HB, Data and RetrData (not counted). A PDU failing one of these checks is discarded. Finally a RetrReq, HB, Data or RetrData whose sequence number is not the expected one is counted, but still processed. | `GeneralMessageCheck`, `srmsg_CheckMessage`, `CheckSequenceNumberRange`, `CheckConfirmedSequenceNumber`, `CheckTimeStamp`, `CheckSequenceNumber`, `srcor_ReceiveMessage` |
| OBS-SBB-DIAG-002 | Each accepted timely HB, Data or RetrData adds its `T_rtd` and `T_alive` to two distributions of five intervals, bounded by four configured values `0 < T1 < T2 < T3 < T4 < T_max`; a value equal to a bound falls into the lower interval. After a configured number of such messages (100 to 10000, default 5000), and when the redundancy channel closes, the error counters and distributions are reported to the application and reset. They are also reset when a connection opens. | `srdia_sr_diagnostics.c`, `srcor_ProcessReceivedMessage`, `srcor_CloseRedundancyChannel`, `srcor_InitRaStaConnData`, `srcfg_sr_config.c` |
| OBS-SBB-DIAG-003 | RL diagnoses each transport channel over a window of distinct sequence numbers (`N_diagnosis`, 10 to 1000, default 200). The first arrival of a frame is recorded; a copy on another channel within `T_seq` adds its delay to that channel's `T_drift` and its square to `T_drift2`. When a new frame would exceed the window, each channel reports `N_diagnosis`, `N_missed` (frames not received on it within `T_seq`), `T_drift` and `T_drift2`, and the window is reset. Frames in sequence, older frames, and admissible future frames update the window; frames discarded by the start-up rule or beyond the admissible window do not. The window is reset when the redundancy channel is initialised. | `reddia_red_diagnostics.c`, `redcor_ProcessReceivedMessage`, `redcfg_red_config.c` |
| OBS-EULYNX-SR-001 | A current public implementation identifies protocol version `0303`. | `librasta` version definition |
| OBS-RCCS-SR-002 | Data messages carry one or more application messages, each preceded by its little-endian `u16` length. The sender packs up to `max_packet` queued messages into one Data message (default 3 in Railway-CCS's configuration loader and in the EULYNX Live examples). | Railway-CCS `rastafactory.c`, `rasta_new.c`, `config.c`; EULYNX `rastafactory.c` (`createDataMessage`), `transport/events.c`, `safety_retransmission.c` |
| OBS-RCCS-SM-001 | On a received DiscReq, the application receives the PDU's reason and detailed reason in a separate notification after the state-change notification. The EULYNX Live fork keeps the notification type but does not call it. In both, the application's disconnect call sends detailed reason zero. | Railway-CCS `rasta_new.c` (`handle_discreq`, `sr_disconnect`), `rastahandle.c` (`fire_on_discrequest_state_change`); EULYNX `rastahandle.c` (`on_discrequest_change_call`), `safety_retransmission.c` (`sr_disconnect`) |
| OBS-RCCS-DIAG-001 | Railway-CCS keeps the same five error counters (safety code, address, type, sequence number, confirmed sequence number), citing section 5.5.5 of the specification, and distributions of `T_rtd` and `T_alive` over intervals of a fixed 500 ms, citing section 5.5.6.4. Its redundancy layer reports `N_diagnosis`, `N_missed`, `T_drift` and `T_drift2` per transport channel. | Railway-CCS `rastalist.h` (`rasta_error_counters`, `diagnostic_interval`), `rasta_new.h` (`DIAGNOSTIC_INTERVAL_SIZE`), `rasta_red_multiplexer.c` |
| OBS-RCCS-TIME-001 | `T_i` is computed as `T_max - T_rtd` on receipt, but the connection timer restarted on receipt always runs `T_max`; `T_i` is not used for expiry. The EULYNX Live fork behaves the same. | Railway-CCS `rasta_new.c` (`updateTI`, `init_conn_expired_event`); EULYNX `safety_retransmission.c`, `rasta_init.c` |

The codec and safety-code implementation trace to OBS-SBB-SR-001 through
OBS-SBB-SR-011. The application-data field is packed as in OBS-RCCS-SR-002.
The number of application messages per Data message (`N_maxPacket`) is
configured per connection and defaults to one, as SBB requires
(OBS-SBB-SR-015). A received field is checked after
its safety code: a malformed field, or one with more messages than
`N_maxPacket`, closes the connection. MD4 padding boundaries are covered by
known-answer tests. Protocol version and MD4 initial state are deliberately
accepted as configuration data rather than declaring observed defaults
normative. The connection buffers trace to OBS-SBB-SR-012, OBS-SBB-SR-013 and
OBS-SBB-SM-013.
The connection engine traces to OBS-SBB-SM-001 through OBS-SBB-SM-012,
OBS-SBB-SM-014,
OBS-SBB-TIME-001, OBS-SBB-TIME-002 and OBS-SBB-TIME-004. It computes `T_i` as
in OBS-SBB-TIME-003 but deliberately runs it from message acceptance instead
of from the confirmed timestamp; see `../adr/0004-adaptive-timer.md` and
OBS-SBB-TIME-005, OBS-SBB-TIME-006 and OBS-RCCS-TIME-001. As in
OBS-SBB-SM-014, it does not report the reason of a received disconnection
request; Railway-CCS does (OBS-RCCS-SM-001). Unlike SBB, the application
cannot set the detailed reason of a sent disconnection request; it is always
zero. The RL codec and engine trace to
OBS-SBB-RL-001 through OBS-SBB-RL-006, and the composed stack's receive queue
to OBS-SBB-RL-007. Unlike SBB, which takes every queued PDU in one pass, the
stack processes at most eleven per operation and the rest at the next poll, so
that its fixed action bound holds. Diagnostics trace to OBS-SBB-DIAG-001
through OBS-SBB-DIAG-003; the timing intervals follow SBB rather than the fixed
intervals of OBS-RCCS-DIAG-001. The Rust decoder checks a PDU's structure
before its safety code, so the stack classifies an undecodable PDU afterwards
in the order of OBS-SBB-DIAG-001. Configuration enforces only the structure of
the timing intervals and the storage bound of 1000 frames for the RL window,
not SBB's other ranges (see Q-012 in `../open-questions.md`). The counters in
`RedundancyDiagnostics` have no counterpart in the references.
