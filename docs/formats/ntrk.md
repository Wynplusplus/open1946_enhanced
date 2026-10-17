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
here; reading the channel's messages is the network-protocol work tracked in the roadmap.
