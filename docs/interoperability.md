# Interoperability with the reference stacks

These issues were found in the pinned public implementations (see
`provenance.md`). They are not defects of RaSTA-Rust, but they affect links
between RaSTA-Rust and those stacks.

## SBB RaSTA stack

- **Idle connections time out unless RaSTA-Rust heartbeats often.** The SBB
  stack measures its adaptive timer from the confirmed timestamp
  (OBS-SBB-TIME-003). With its own default configuration (`T_max` 750 ms,
  `T_h` 300 ms), an idle connection between two SBB endpoints times out
  repeatedly (OBS-SBB-TIME-005). On a link with RaSTA-Rust, the SBB side needs
  roughly `2 * T_h(SBB) + T_h(RaSTA-Rust) + latency < T_max`
  (OBS-SBB-TIME-007). Against an SBB endpoint at 750/300, configure RaSTA-Rust
  with `T_max` 750 ms and `T_h` 100 ms or less through
  `ConnectionConfig::new_with_timing`; the derived default (`T_max / 4`,
  187 ms) is not short enough.
- **One application message per Data message.** SBB discards a Data message
  that holds several application messages (OBS-SBB-SR-015). Keep
  `N_maxPacket` at its default of one on links to SBB.

The tested interoperability scenarios with the SBB stack are described in
`differential-testing.md`.

## Railway-CCS rasta-protocol and EULYNX Live librasta

- **Packed Data messages by default.** These stacks put up to `max_packet`
  application messages into one Data message, with a default of 3 in
  Railway-CCS's configuration loader and the EULYNX Live examples
  (OBS-RCCS-SR-002). RaSTA-Rust closes a connection whose peer sends more
  messages than its own `N_maxPacket`, so configure the same value on both
  sides (`ConnectionConfig::with_max_packet`).
- **No adaptive timeout.** The connection timer always runs `T_max` from the
  last accepted message (OBS-RCCS-TIME-001), so these stacks tolerate older
  confirmations than RaSTA-Rust does.
