# Security policy

This repository is experimental and must not be used as a certified safety or
security component.

Please report vulnerabilities privately to the repository maintainers through
GitHub's private vulnerability reporting. Include affected versions, a minimal
reproducer when possible, and any expected impact. Do not include operational
railway secrets or personal data in a report.

## Threat model

RaSTA counters loss, repetition, reordering, delay, corruption, and
misdelivery of messages with sequence numbers, timestamps, a safety code, and
fixed endpoint identifiers. It does not protect against a deliberate
attacker:

- The MD4 safety code, with its configured initial value, and the RL check
  codes detect random errors but do not authenticate messages; MD4 is not a
  message authentication code. Anyone who can read and write traffic on the
  network can inject valid PDUs, for example a disconnection request that
  closes the connection.
- The UDP adapter accepts datagrams only from the configured peer address,
  but source addresses can be spoofed.
- On networks that are not physically protected, a separate security layer
  is required, for example IPsec, a VPN, or network segmentation, following
  EN 50159 and IEC 62443. It is outside the scope of this crate.

Malformed network input is part of the normal threat model. A panic, excessive
resource use, or state transition caused by malformed input is treated as a
security-relevant defect.
