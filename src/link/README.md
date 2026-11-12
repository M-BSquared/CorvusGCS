# corvus_link

MAVLink for Corvus GCS, in plain Rust. The program and its backend
(`corvus_core`) use it through `corvus_link::plain`. It needs no Python to
build, to test or to run.

| | |
| --- | --- |
| `plain::defs` | the message, enum and constant tables of the `all` dialect, generated from `message_definitions/v1.0/` by `build.rs`, in a MAVLink 1 view (ids up to 255, no extension fields) and a MAVLink 2 view |
| `plain::Message`, `Value` | a message as data, one value per declared field |
| `plain::codec` | `pack_payload` (MAVLink 2 trims trailing zeros) and `unpack_payload` (a short payload reads zero filled) |
| `plain::frame` | `encode` (v1 and v2, CRC_EXTRA), the stream `Parser` (resyncs on a bad checksum), `Signing` (SHA-256) |
| `plain::crc` | the x25 checksum |
| `plain::connection` | `open("udpin:...")` from a connection string (udp, udpin, udpout, udpbcast, tcp, tcpin, mcast, uds, serial, `device,baud`), `recv(timeout)` frames and messages with a signature check, `send`, the systems heard, counters; no thread of its own, and a lost stream is an error for the caller's reconnect loop |
| `plain::transport`, `plain::websocket` | the links, bytes in and out |
| `plain::param_value`, `msp`, `shell_text`, `time` | parameter value encoding, MSP for Betaflight, NSH console text, MAVLink time stamps |

## Layout

```
lib.rs              the crate root
plain/              the API above
wire/defs.rs        the generated tables and their two views
link/               the transports: UDP, TCP, Unix sockets, multicast, serial, WebSocket
build.rs            the generator (follows pymavlink's mavparse and mavgen step for step)
message_definitions the MAVLink XML
tests/              the tests against pymavlink's frozen answers
```

The `tls` feature adds `wss:` (rustls with the `ring` provider, which needs a C
compiler); the program builds without it.

## Held to pymavlink

pymavlink was the reference while Corvus was a Python program. Its answers
were frozen into `tests/data` when Python left the project, so the tests need
no Python:

- `tests/pymavlink_frozen.rs` reads `tests/data/pymavlink.json.gz`, frozen from
  pymavlink 2.4.50: every message table of both wire versions (fields, wire
  order, types, array lengths, CRC_EXTRA, payload length, instance field,
  units, enums, display), every enum entry with its description, location
  flag, parameters and labels, every enum constant, the framing constants,
  one frame of every message in both versions produced byte for byte and read
  back to its values, the messages pymavlink recovers from a damaged stream
  (found here too, in order, in any piece size), signed frames and x25
  checksums.
- `tests/plain_frames.rs` reads `tests/data/plain_frames.json`: frames with
  chosen values, a stream with noise, a bad checksum, a signed frame,
  out-of-range values, a MAVLink 2 payload that lost its zeros, and connection
  strings that are not links.

The fixtures are the reference and are not regenerated. A change to the XML
that moves a table shows up in these tests and is accepted by checking it
against the MAVLink definitions, never by rewriting a fixture to match the code.
