# Trying it out

`examples/udp_peer.rs` is an interactive node over one or two UDP paths. Each
line typed on standard input is sent as one application message, newline
included. `/close` closes the connection, `/open` opens it again, and `/quit`
or the end of input closes it and exits. The node prints received messages
(`<`), state changes and diagnostics (`*`), and problems (`!`). Run it without
arguments to see all options.

## Two nodes

Start the server (the higher identifier) in one terminal:

```sh
cargo run --example udp_peer -- --local-id 2 --remote-id 1 \
  --bind 127.0.0.1:47001,127.0.0.1:47002 --peer 127.0.0.1:47011,127.0.0.1:47012
```

and the client in another:

```sh
cargo run --example udp_peer -- --local-id 1 --remote-id 2 \
  --bind 127.0.0.1:47011,127.0.0.1:47012 --peer 127.0.0.1:47001,127.0.0.1:47002
```

Both print `SR Start -> Up` once connected. Lines typed in one terminal appear
in the other. Diagnostics are reported every 20 accepted messages; idle nodes
exchange a heartbeat every quarter of `T_max` (250 ms by default).

To send many messages, pipe them into the client instead:

```sh
(for i in $(seq 1 200); do echo "message $i"; sleep 0.02; done; sleep 3; echo /quit) |
  cargo run -q --example udp_peer -- --local-id 1 --remote-id 2 \
    --bind 127.0.0.1:47011,127.0.0.1:47012 --peer 127.0.0.1:47001,127.0.0.1:47002
```

## Against EULYNX Live librasta

`rcat_udp` from the pinned EULYNX Live revision (see `provenance.md`) works
like `udp_peer`: it sends the lines it reads and prints the messages it
receives. Build it outside this repository:

```sh
cmake -S /path/to/eulynx-librasta -B /tmp/rasta-ref-eulynx \
  -DCMAKE_BUILD_TYPE=Debug -DBUILD_TESTING=OFF -DBUILD_LOCAL_EXAMPLES=ON
cmake --build /tmp/rasta-ref-eulynx --target rcat_udp
```

It reads `rasta_server_local.cfg` (`r`, receiver) or `rasta_client_local.cfg`
(`s`, sender) from the current directory; the build copies both into
`/tmp/rasta-ref-eulynx/examples`. They configure identifiers 0x61 (server, on
ports 8888 and 8889) and 0x60 (client, on 9998 and 9999), no safety code,
check code A, `T_max` 10000 ms, `N_SENDMAX` 10 and `N_maxPacket` 3. Start
`rcat_udp` as server:

```sh
cd /tmp/rasta-ref-eulynx/examples && ../rcat_udp r
```

and `udp_peer` as client with matching settings:

```sh
cargo run --example udp_peer -- --local-id 0x60 --remote-id 0x61 \
  --bind 127.0.0.1:9998,127.0.0.1:9999 --peer 127.0.0.1:8888,127.0.0.1:8889 \
  --safety-code none --check-code a --t-max 10000 --n-send-max 10 --max-packet 3
```

For the opposite roles, start `udp_peer` with `--local-id 0x61 --remote-id
0x60 --bind 127.0.0.1:8888,127.0.0.1:8889 --peer 127.0.0.1:9998,127.0.0.1:9999`
and the same remaining options, then `../rcat_udp s`. `rcat_udp` logs at debug
level; received lines appear between its log lines.

To use the lower MD4 safety code and CRC-32C, copy the two configuration files
to another directory, change `RASTA_SR_CHECKSUM_LEN = NONE` to `HALF` and
`RASTA_CRC_TYPE = TYPE_A` to `TYPE_C`, run `rcat_udp` from that directory, and
pass `--safety-code lower --check-code c` to `udp_peer`.

## Over a lossy network

On Linux, `tc netem` can drop and delay the datagrams to selected ports on the
loopback interface. The commands need root and affect only the filtered
ports. Run the two nodes from the first section, then set up a priority
queue whose third band applies netem:

```sh
sudo tc qdisc add dev lo root handle 1: prio
sudo tc qdisc add dev lo parent 1:3 handle 30: netem loss 30%
```

Send the datagrams for the server's first path through it:

```sh
sudo tc filter add dev lo parent 1: protocol ip prio 1 u32 \
  match ip dport 47001 0xffff flowid 1:3
```

The server's RL diagnostics now report missed frames on path 0 but not on
path 1, and messages arrive without SR sequence errors: the second path
delivers what the first loses. Add the second path as well:

```sh
sudo tc filter add dev lo parent 1: protocol ip prio 1 u32 \
  match ip dport 47002 0xffff flowid 1:3
```

Some frames are now lost on both paths. After `T_seq` the server's SR sees the
gap: its diagnostics count sequence errors, and it prints `Up ->
RetransmissionRequest -> RetransmissionRunning -> Up` while the client
retransmits. Messages still arrive complete and in order.

Delay instead of loss shows up as drift. With only the first filter:

```sh
sudo tc qdisc change dev lo parent 1:3 handle 30: netem delay 30ms
```

path 0 reports `T_drift` of about 30 ms per frame. A delay longer than
`T_seq` (50 ms) counts as missed instead. Remove everything with:

```sh
sudo tc qdisc del dev lo root
```
