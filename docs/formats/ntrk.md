# Track files (`.ntrk`, `InOutStreams` container)

The original's recorded tracks are `records/*.ntrk`. They are multi-stream archives
written by `com.maddox.rts.InOutStreams`, the same container the net code uses for
file payloads. The recording itself is the runtime's network replication stream
(the `traffic` stream below); the container around it is straightforward and
unencrypted.

All integers are big-endian (`java.io.DataOutput`).

## File layout

```
[ header 16 B ][ compressed blocks ... ][ index ][ entries ][ tailer 8 B ]
```

### Header (16 B)

| field | type | value |
|---|---|---|
| signature | int32 | `1245391901` (`0x4A3B2C1D`) |
| version | int32 | `101` |
| compressMethod | int32 | `0` none, `1` LZSS, `2` ZIP (zlib raw deflate) |
| blockSize | int32 | `32768` in the shipped files (range 2048–131072) |

### Tailer (8 B, last 8 bytes of the file)

| field | type | meaning |
|---|---|---|
| indexesSizeInFile | int32 | total bytes of the index region = `4 × blockCount` |
| entrysSizeInFile | int32 | total bytes of the entries region |

So, reading backwards from the end:

```
tailer      = last 8 bytes
entries     = [fileSize - 8 - entrysSizeInFile, fileSize - 8)
index       = [fileSize - 8 - entrysSizeInFile - indexesSizeInFile, fileSize - 8 - entrysSizeInFile)
blocks      = [16, fileSize - 8 - entrysSizeInFile - indexesSizeInFile)
```

### Index

`blockCount = indexesSizeInFile / 4` entries, each a single int32 `lenInFile` — the
on-disk byte length of that compressed block. Block `i`'s offset is
`16 + Σ lenInFile[0..i]`. Every block (except possibly the last of a stream) decompresses
to `blockSize` bytes.

### Entries (named streams)

Repeated until the region is exhausted:

| field | type |
|---|---|
| name | UTF (modified): `uint16 byteLen` then UTF-8 bytes |
| len | int32 — decompressed length of the stream |
| blockCount | int32 |
| blockIndex[blockCount] | int32 each — indices into the index list (0-based) |

The block list is the order the stream's data is split across blocks; concatenate and
decompress them to recover `len` bytes.

### Compression

`compressMethod 0` stores blocks verbatim. `compressMethod 2` (ZIP) is the same per-block
envelope `il2-sfs` uses:

- if the block's stored length is already at least its decompressed length (the block
  didn't shrink, or is the trailing partial block of a stored stream), it is stored
  verbatim;
- otherwise the block starts with a `u16` little-endian **method** (`8` = raw deflate,
  `0` = stored), followed by the payload. Method 8 is raw deflate (`zlib` `wbits = -15`),
  not the zlib-wrapped form.

`compressMethod 1` (LZSS) is native and not used by the shipped `.ntrk` files; OpenIL2
reads but does not write it.

The `traffic` stream's blocks are all deflate; the small `speed`, `version`, `states2`
streams are stored verbatim; `states0` and `states1` are single deflate blocks.

## Streams a track contains

`NetMissionTrack.startRecording` creates, in this order, the streams a playback needs:

| stream | contents |
|---|---|
| `speed` | one line: the record speed as a percentage float, e.g. `105.0`. |
| `version` | two lines: `playingVersion` then `playingOriginalVersion`. Single-player tracks write `109` for both; the shipped 4.13 tracks write `102`. |
| `traffic` | the replication stream: `NetChannelOutStream` messages (spawns, state, damage, …). This is the bulk of the file and the part that needs the network protocol to read. |
| `states0` | `Main3D.saveRecordedStates0` — the mission hash and a few seeds; starts with a modified-UTF string (an 8-hex-digit mission key). |
| `states1` | `Main3D.saveRecordedStates1` — camera / view state. |
| `states2` | `Main3D.saveRecordedStates2` — game settings; two lines, e.g. `70.0\r\n19`. |
| `NetFilesDir` | optional sub-directory for attached net files (skins, nose art); empty in single-player tracks. |

The `traffic` stream is the recorded network channel. OpenIL2 decodes the container
and the packet/message framing; interpreting each message type (spawn, state, damage)
for playback is the network-protocol work still tracked in the roadmap.

## `traffic` packet framing

`NetChannelOutStream.sendPacket` writes one packet per send. All integers big-endian.

```
u16  deltaTime          ms since the previous packet (0 if the gap exceeded 65535,
                       which forces a packet with at least 20 bytes)
u16  payloadLen         the bytes that follow = 3 + filtered + guaranted
u16  crc16              CRC16 over the bytes from here to the packet end; the writer
                       first writes 7361 here and then overwrites it with the checksum
u8   guarantedCount     number of guaranteed messages that follow
     guarantedCount ×    guaranteed messages
     …filtered messages  state updates
```

A guaranteed message (`NetChannel.putMessage`, 3-arg):

```
u16  objIndex           the sending object's id, or a spawn/destroy tag
u8   msgSize            body length, not counting the `iObjects` that follow
msgSize bytes           the message body
u16 × N                 referenced object ids (`iObjects`) — no count in the framing;
                       N is known only from the message type
```

A filtered message (`NetChannel.putMessage`, 5-arg) begins with `u16 objIndex`, then a
variable-length size/time byte (bit 5 = "size in next byte", the top two bits = the
time-delta width: none / 1 / 2 / 3 bytes), then `size` body bytes and the trailing
`iObjects`. The width encodings:

| top 2 bits | size < 32 (bit 5 clear) | size ≥ 32 (bit 5 set) |
|---|---|---|
| `00` | size = low 5 bits, no time (1 B) | size in next byte, no time (2 B) |
| `01` | size = low 5 bits, ±7-bit time (2 B) | size in next byte, 4-bit time (2 B) |
| `10` | size = low 5 bits, ±15-bit time (3 B) | size in next byte, 12-bit time (3 B) |
| `11` | size = low 5 bits, ±23-bit time (4 B) | size in next byte, 20-bit time (4 B) |

OpenIL2 verifies the packet layer against all 46 shipped tracks: every `traffic` stream
walks cleanly as a run of packets with no trailing bytes (Intro3b alone is 23 061 packets,
18 323 guaranteed messages).

## Guaranteed-message semantics

The channel tags every guaranteed message with the meaning in `objIndex`
(`NetChannel.setGMsg`):

| `objIndex` | meaning | body | trailing `iObjects` |
|---|---|---|---|
| `3` | **spawn** (`NetMsgSpawn`) | `u32 finger` (class hash, `Finger.Int` of the class name — `superObj`'s class first, else the object's own), then `u16 idLocal & 0x7fff` | appended by the spawned class's `netReplicate`/`netSpawn` data |
| `4` | **destroy** (`NetMsgDestroy`) | empty | exactly one ref: the destroyed object |
| other | a regular message from that object (bit 15 = mirrored on this channel) | class-specific | class-specific |

`iObjects` (`writeNetObj`) are `u16` references to other objects — bit 15 set means
"object mirrored on this channel" (looked up in the channel's own table), clear means
"global object id". They travel **after** the body with no count in the framing, but the
`u8 size` field counts them: `NetMsgOutput.size() = body + 2 × iObjects.len()`, and
`readNetObj` pops them from the *end* of the message.

Class-finger resolution: `Finger.incInt` hashes a class name char-by-char through the
256-entry `FPaTable` (rolling `h = (h << 8 \| byte) ^ FPaTable[h >> 24]` twice per char,
once for the low byte, once for the high byte of the UTF-16 code unit). OpenIL2's
`il2-formats::finger` module ports the algorithm; the class registry is supplied at
runtime, nothing is embedded.

Replay state: spawn creates an object (`idLocal` → class), destroy removes it. Verified
against Intro3b: 23 061 packets, 18 323 guaranteed messages, **all 6 spawns resolve** —
`NetUser` (the recording pilot), `NetServerParams`, `GameTrack` (the recorder),
`Mission`, `FlightModelTrack` (per-aircraft FM replay data, one per aircraft, holding
`readNetObj` → its `Aircraft`), and `Paratrooper`. The guaranteed-message count in the
packet header bounds the message walk — everything after it is filtered (state) traffic.

Per-class message bodies (what `NetUser.netInput`, `NetServerParams.netInput`, … read)
are the next layer: spawn bodies already decode for the protocol objects (e.g.
`NetUser`: `read255` name, `u8` bornPlace, `u8` place (255 = −1), `u16` idChannelFirst,
`bool` bTrackWriter).
