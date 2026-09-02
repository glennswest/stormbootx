# dns-wire

The only part of `stormbootx` that can be tested without a machine to boot.

`src/dns.rs` is `no_std` and firmware-facing, so there is no `cargo test` for
it — but its message parser is pure byte handling, and the wire format is
exactly the kind of thing this project has already been bitten by once (see the
NVMe PSDT and CC.EN notes in `nvme.rs`). A DNS parser that mis-walks a message
is a bug that only shows on hardware, in the boot path, with no console.

So `run.sh` **extracts the parsing half of `src/dns.rs` verbatim** — same text,
`alloc::` rewritten to `std::` — and exercises it two ways:

- against a **real resolver over TCP**, using `build_query` to ask and the real
  parser to read the answer back, so the framing is proven end to end;
- against **synthetic answers** in the shape microdns serves, including a
  compressed owner name, the target's A record in the additional section,
  priority/weight selection, a `.` target meaning "not offered here", a
  compression-pointer loop, and every truncation of a valid answer.

The last two matter most. This code parses bytes off the network before any OS
exists, so a hang or a panic is a machine that does not boot, and every prefix
of a valid answer is an input it can really see.

```bash
./run.sh                 # against 1.1.1.1
./run.sh 192.168.8.252   # against a specific resolver
```

Runs on `dev.g8.lo` like everything else.
