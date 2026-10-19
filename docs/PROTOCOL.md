# Qianli / Sunshine SuperCam protocol

Applies to the LC-AD11 family, sold as QIANLI Super Cam LC-AD11, SUNSHINE
LC-AD11 and TB-03 V2.0. The wire protocol identifies itself as `LAUNCHDIGITAL`.
Reverse-engineered from a capture of the vendor Windows client; no vendor
documentation or code was used.

This note records only what the Rust driver relies on. The Python project's
`docs/PROTOCOL.md` remains the fuller record.

## Ports and sockets

| Port | Role |
|------|------|
| 3001 | control / registration |
| 3000 | media — each socket carries one value (0 = visible, 1 = secondary/thermal) |

One session needs **four** media sockets:

* two **probe/hold** sockets, one per value: they receive the flag and carry the
  keepalives;
* two **streaming** sockets, one per value: they get INIT + START and produce
  the picture.

Control sockets are short-lived and per-command. The handshake must be
sequential: the camera accepts out-of-order connections and then drops them.

## The per-session flag

There are no fixed magic values. Each probe reply is 56 bytes; the one whose
bytes **28..30 are `01 00`** carries a fresh 4-byte flag at **48..52**. Not
every reply carries it, so scan for the marker instead of assuming the first
reply.

The flag must be echoed in:

* the 8-byte START packet,
* the 16-byte CTRL packet.

A stale flag is accepted politely — the register ACK is byte-identical,
including the `Neptune` name and MAC — and then the media sockets are FINed
after 1–3 s with **zero frames**. Always verify that frames arrive.

## Handshake sequence used by the driver

```
3001  register(0x65, flags=0)      one connection each
3001  register(0x65, flags=1)
3001  register(0x66, flags=0)
3001  register(0x66, flags=1)

3000  probe(value=0) + serial      -> read flag0
3000  probe(value=1) + serial      -> read flag1

3000  INIT(value=0, channel=8)     on a new socket
3000  START(flag0, 1)
3000  INIT(value=1, channel=8)     on a new socket
3000  START(flag1, 1)

3001  register(0x78, flags=1) + CTRL(4, flag1, 3000)
3000  type14(value=1)              on probe1
3001  register(0x78, flags=0) + CTRL(2, flag0, 3000)
3000  type14(value=0)              on probe0
```

Then read both streaming sockets and send `keepalive(0)` / `keepalive(1)` on
the probe sockets every 10 s. Without keepalives the camera stops the stream.

## Frames

Both channels deliver fixed **39448-byte** wrapper blocks:

```
00 00 01 b3 <hdr> "LAUNCHDIGITAL" ... "RAW" | "H264" ...
```

* Visible: `H264` payload, ordinary Annex-B, 1920×1080, ~30 fps.
* Thermal: `RAW` payload — **160×120, 16-bit little-endian**, **38400 bytes at
  offset 88**; header `a0 00` = 160, `78 00` = 120, `10 00` = 16-bit.
  `celsius = raw * scale + offset`, factory scale `0.01`.

The stream is block-aligned but a capture may start mid-block, so locate frames
by the magic **with `RAW` at offset 48**, never by assuming the first byte is a
frame start. False positives (the visible wrapper) are skipped.

## Settings block

A control probe (`0x6c`) returns the camera's configuration: an 88-byte echo
followed by a fixed **376-byte** structure.

| offset | size | field |
|--------|------|-------|
| 0   | 24  | device name |
| 24  | 16  | IP address |
| 40  | 16  | netmask |
| 56  | 16  | gateway |
| 72  | 16  | DNS |
| 88  | 16  | factory/misc |
| 104 | 72  | opaque (ports, MAC, flags) — preserve verbatim |
| 176 | 96  | firmware |
| 276 | 100 | build date |

Write ritual: plain probe → write-mode probe → 376 bytes → `6a` COMMIT, which
the camera echoes. The camera reboots to apply a network change (~2 min).

## Known gaps

* The vendor app's write session sends two `6c` probes, a `d7` probe and an
  8-byte `0100000002000000` select before writing; our shorter sequence stores
  the configuration but does not restart the camera. Porting the full sequence
  is the next thing to try.
* Temperature offset is unverified; calibrate against a reference.
* The camera runs a Telnet server on port 23 with `root` and a blank password.
  That is a real exposure — restrict the port or set a password.
