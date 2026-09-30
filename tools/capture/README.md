# capture

Record/replay TCP proxy that produces the golden packet corpus (task T-007).

Sits between a real client and the legacy cluster and writes both directions of
every session to disk byte for byte. The recordings are the acceptance corpus
that later proves the Rust services behave identically to the legacy ones, so
**the recordings contain exactly the bytes that crossed the sockets** — in
order, unmodified, with nothing dropped or reordered.

It deliberately does not parse, decrypt or frame anything. The front link is
plaintext and the game link is ciphered; both are recorded as opaque bytes.
Understanding the stream is `gz-net`'s job.

## Build

```
cargo build -p capture
```

The crate has no third-party dependencies — just `std` — so it builds with a
vanilla toolchain and nothing to vendor.

## Usage

```
capture --listen <ADDR> --target <ADDR> --out <STEM>
capture --verify <STEM>
capture --help
```

`--out` is a file *stem*, e.g. `sessions/front-login`. Each session writes four
files:

| file                   | contents                                                       |
| ---------------------- | -------------------------------------------------------------- |
| `<STEM>.c2s.bin`       | every byte the client sent (client → server)                   |
| `<STEM>.s2c.bin`       | every byte the server sent (server → client)                   |
| `<STEM>.meta.json`     | endpoints, timestamps, duration, end reason, per-direction     |
|                        | byte/chunk counts                                              |
| `<STEM>.timing.jsonl`  | one line per read chunk: direction, time since start, size     |

Sessions are accepted one at a time. If the stem is taken a `-2`, `-3`, …
suffix is appended rather than overwriting, because a golden corpus artifact
that silently vanishes is worse than an awkward filename.

### Recording a real session

Point the real client at the proxy instead of the cluster:

```
capture --listen 127.0.0.1:5750 --target 127.0.0.1:5700 --out sessions/front-login
```

then connect the client to `127.0.0.1:5750`. The proxy logs one line per
session to stderr. Stop it with Ctrl-C.

### Verifying a capture

```
capture --verify sessions/front-login
```

In CI the corpus can be asserted intact without any network:

```
capture --verify sessions/*  # (per captured stem)
```

The verifier re-reads the timing sidecar and checks, without a JSON parser,
that the per-chunk lengths sum exactly to each direction file's on-disk length
and that timestamps never go backwards within a direction.

## Guarantees

* **Byte-exact records.** Everything is sunk with `write_all` and recorded
  before it is forwarded. A short write would put the forwarded stream out of
  order relative to the capture, so it is never tolerated.
* **Half-close, never hard-close.** When one direction reaches EOF the peer's
  *write* half is shut down so the far end sees end-of-stream while the other
  direction keeps running. A proxy that closes both sockets on the first EOF
  truncates sessions and silently corrupts the corpus.
* **A late transport error still writes the metadata** with `end_reason` set,
  so nothing recorded is discarded just because the session ended badly.
* **No secrets, no parsing.** `--verify` reads only counts; the binaries are
  never interpreted.

## Testing

```
cargo test -p capture
```

Two loopback tests cover the acceptance property directly:

* `captures_exact_bytes_in_both_directions` — 1 MiB of client data and a
  different 234 KiB server response are proxied live and the files must equal
  the wire bytes exactly, then `--verify` must agree with them.
* `provokes_and_propagates_half_close` — the client half-closes after a short
  message and keeps reading; the target must observe the FIN *and* still get
  the full answer delivered, which a hard-closing proxy would truncate.

## Files

* `src/lib.rs` — module wiring and re-exports
* `src/json.rs` — minimal flat JSON writer (no nesting, no dependency)
* `src/meta.rs` — `.meta.json` document, UTC formatter
* `src/proxy.rs` — accept/connect/pump, half-close handling
* `src/recorder.rs` — on-disk session files, shared timing sidecar
* `src/verify.rs` — `--verify` without a JSON parser
* `src/main.rs` — hand-parsed CLI
* `tests/loopback.rs` — byte-exactness and half-close acceptance tests