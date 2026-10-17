//! The `.ntrk` track container: `com.maddox.rts.InOutStreams`.
//!
//! A multi-stream archive: a 16-byte header, a run of compressed blocks, a block
//! index, the named-stream table and an 8-byte tailer. All integers are big-endian.
//! The recording itself lives in the `traffic` stream (the net replication channel);
//! this module only reads the container, not the channel messages. Spec: `docs/formats/ntrk.md`.

use thiserror::Error;

/// Magic written by `InOutStreams`: `0x4A3B2C1D`.
pub const SIGNATURE: u32 = 1_245_391_901;
/// `InOutStreams.VERSION`. The shipped `.ntrk` files are all version 101.
pub const VERSION: u32 = 101;

#[derive(Debug, Error)]
pub enum NtrkError {
    #[error("bad signature {0:#x}")]
    BadSignature(u32),
    #[error("unsupported container version {0}")]
    BadVersion(u32),
    #[error("unsupported compression method {0}")]
    BadCompress(u32),
    #[error("block size {0} out of range")]
    BadBlockSize(u32),
    #[error("truncated index or entry table")]
    Truncated,
    #[error("entry {0:?} references unknown block {1}")]
    UnknownBlock(String, u32),
    #[error("zlib decode failed for block {0}: {1}")]
    Inflate(usize, String),
}

/// How a block is stored. Method 1 (LZSS) is native and unused by shipped tracks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compress {
    None,
    Lzss,
    Zip,
}

impl Compress {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Lzss),
            2 => Some(Self::Zip),
            _ => None,
        }
    }
}

/// One named stream in the archive. `blocks` are indices into the block list.
#[derive(Clone, Debug)]
pub struct Stream<'a> {
    pub name: String,
    pub len: u32,
    pub blocks: Vec<u32>,
    data: &'a [u8],
    block_offsets: &'a [usize],
    block_lens: &'a [u32],
    compress: Compress,
    block_size: u32,
}

impl<'a> Stream<'a> {
    /// Decompresses and concatenates the stream's blocks into `len` bytes.
    pub fn decode(&self) -> Result<Vec<u8>, NtrkError> {
        let mut out = Vec::with_capacity(self.len as usize);
        for (n, &bi) in self.blocks.iter().enumerate() {
            let bi = bi as usize;
            let (&off, &ln) = self
                .block_offsets
                .get(bi)
                .zip(self.block_lens.get(bi))
                .ok_or(NtrkError::UnknownBlock(self.name.clone(), bi as u32))?;
            let end = off.checked_add(ln as usize).ok_or(NtrkError::Truncated)?;
            let raw = self.data.get(off..end).ok_or(NtrkError::Truncated)?;
            // `InOutStreams.readBlock` inflates only when the stored length is below the
            // decompressed block size; otherwise the block is stored verbatim. The last
            // block of a stream may decompress to less than `block_size`.
            let expected = if n + 1 == self.blocks.len() {
                self.len as usize - n * self.block_size as usize
            } else {
                self.block_size as usize
            };
            out.extend(decode_block(self.compress, raw, expected)?);
        }
        out.truncate(self.len as usize);
        Ok(out)
    }
}

/// Decoded container. Holds a borrow on the file bytes; use [`Container::open`] to parse.
pub struct Container<'a> {
    data: &'a [u8],
    compress: Compress,
    block_size: u32,
    block_offsets: Vec<usize>,
    block_lens: Vec<u32>,
    entries: Vec<(String, u32, Vec<u32>)>,
}

impl<'a> Container<'a> {
    /// Parses the container from file bytes.
    pub fn open(data: &'a [u8]) -> Result<Self, NtrkError> {
        let h = data.get(..16).ok_or(NtrkError::Truncated)?;
        let u32_be = |i: usize| u32::from_be_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let signature = u32_be(0);
        if signature != SIGNATURE {
            return Err(NtrkError::BadSignature(signature));
        }
        if u32_be(4) != VERSION {
            return Err(NtrkError::BadVersion(u32_be(4)));
        }
        let compress = Compress::from_u32(u32_be(8)).ok_or(NtrkError::BadCompress(u32_be(8)))?;
        let block_size = u32_be(12);
        if !(2048..=131072).contains(&block_size) {
            return Err(NtrkError::BadBlockSize(block_size));
        }

        let tail = data.get(data.len().saturating_sub(8)..).ok_or(NtrkError::Truncated)?;
        let t = |i: usize| u32::from_be_bytes([tail[i], tail[i + 1], tail[i + 2], tail[i + 3]]);
        let index_sz = t(0) as usize;
        let entry_sz = t(4) as usize;
        let entries_off = data.len() - 8 - entry_sz;
        let index_off = entries_off - index_sz;

        let n_blk = index_sz / 4;
        let mut block_lens = Vec::with_capacity(n_blk);
        let mut block_offsets = Vec::with_capacity(n_blk + 1);
        let mut acc = 16usize;
        for i in 0..n_blk {
            let o = index_off + i * 4;
            let ln = u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
            block_lens.push(ln);
            block_offsets.push(acc);
            acc = acc.saturating_add(ln as usize);
        }
        if acc != index_off {
            return Err(NtrkError::Truncated);
        }

        let mut entries = Vec::new();
        let mut rd = Reader { data, pos: entries_off };
        let end = data.len() - 8;
        while rd.pos < end {
            let name = rd.utf()?;
            let len = rd.u32()?;
            let cnt = rd.u32()? as usize;
            let mut blocks = Vec::with_capacity(cnt);
            for _ in 0..cnt {
                blocks.push(rd.u32()?);
            }
            entries.push((name, len, blocks));
        }

        Ok(Self { data, compress, block_size, block_offsets, block_lens, entries })
    }

    pub fn compress(&self) -> Compress {
        self.compress
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// All named streams, in archive order.
    pub fn streams(&self) -> impl Iterator<Item = Stream<'_>> + '_ {
        self.entries.iter().map(|(name, len, blocks)| Stream {
            name: name.clone(),
            len: *len,
            blocks: blocks.clone(),
            data: self.data,
            block_offsets: self.block_offsets.as_slice(),
            block_lens: self.block_lens.as_slice(),
            compress: self.compress,
            block_size: self.block_size,
        })
    }

    /// A named stream, if present.
    pub fn stream(&self, name: &str) -> Option<Stream<'_>> {
        self.streams().find(|s| s.name == name)
    }

    /// Decoded bytes of a named stream.
    pub fn read(&self, name: &str) -> Option<Result<Vec<u8>, NtrkError>> {
        self.stream(name).map(|s| s.decode())
    }

    pub fn stream_names(&self) -> Vec<&str> {
        self.entries.iter().map(|(n, _, _)| n.as_str()).collect()
    }
}

/// Decodes one block. `expected` is the block's decompressed length (`block_size`, or the
/// stream's trailing remainder for the last block). Matching `InOutStreams.readBlock`, a
/// block whose stored length is already at least `expected` is stored verbatim; a shorter
/// block carries a `u16` little-endian method prefix (the same envelope `il2-sfs` uses):
/// `8` = raw deflate. Method 1 (LZSS) is native and unused by shipped tracks.
fn decode_block(compress: Compress, raw: &[u8], expected: usize) -> Result<Vec<u8>, NtrkError> {
    match compress {
        Compress::None => Ok(raw.to_vec()),
        Compress::Zip | Compress::Lzss => {
            if raw.len() >= expected {
                return Ok(raw.to_vec());
            }
            let method = raw
                .get(..2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .ok_or(NtrkError::Truncated)?;
            let payload = raw.get(2..).ok_or(NtrkError::Truncated)?;
            match (compress, method) {
                (Compress::Zip, 8) => miniz_oxide::inflate::decompress_to_vec_with_limit(
                    payload, expected,
                )
                .map_err(|e| NtrkError::Inflate(0, format!("{e:?}"))),
                (Compress::Zip, 0) => Ok(payload.to_vec()),
                (Compress::Lzss, _) => Ok(payload.to_vec()),
                _ => Err(NtrkError::Inflate(0, format!("unknown method {method}"))),
            }
        }
    }
}

/// Cursor over the big-endian entry table.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Result<u32, NtrkError> {
        let b = self.data.get(self.pos..self.pos + 4).ok_or(NtrkError::Truncated)?;
        self.pos += 4;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `java.io.DataOutput.writeUTF`: a `u16` byte length, then modified-UTF-8 bytes.
    fn utf(&mut self) -> Result<String, NtrkError> {
        let lb = self.data.get(self.pos..self.pos + 2).ok_or(NtrkError::Truncated)?;
        let ulen = u16::from_be_bytes([lb[0], lb[1]]) as usize;
        self.pos += 2;
        let b = self.data.get(self.pos..self.pos + ulen).ok_or(NtrkError::Truncated)?;
        self.pos += ulen;
        std::str::from_utf8(b).map_err(|_| NtrkError::Truncated).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn intro3b() -> Option<Vec<u8>> {
        let root = std::env::var("IL2_ROOT").ok()?;
        let p = std::path::Path::new(&root).join("records/Intro3b.ntrk");
        let mut f = std::fs::File::open(&p).ok()?;
        let mut v = Vec::new();
        f.read_to_end(&mut v).ok()?;
        Some(v)
    }

    #[test]
    fn opens_intro3b_container() {
        let Some(data) = intro3b() else { return };
        let c = Container::open(&data).expect("open");
        assert_eq!(c.compress(), Compress::Zip);
        assert_eq!(c.block_size(), 32768);
        let names = c.stream_names();
        assert!(names.contains(&"speed"));
        assert!(names.contains(&"version"));
        assert!(names.contains(&"traffic"));
        assert!(names.contains(&"states0"));
    }

    #[test]
    fn decodes_speed_and_version() {
        let Some(data) = intro3b() else { return };
        let c = Container::open(&data).expect("open");
        let speed = String::from_utf8(c.read("speed").unwrap().unwrap()).unwrap();
        assert!(speed.trim_end_matches("\r\n").parse::<f32>().is_ok());
        // version is two lines; shipped 4.13 tracks record 102/102.
        let version = String::from_utf8(c.read("version").unwrap().unwrap()).unwrap();
        let mut lines = version.lines();
        let a = lines.next().and_then(|l| l.parse::<u32>().ok());
        let b = lines.next().and_then(|l| l.parse::<u32>().ok());
        assert!(a.is_some() && b.is_some());
    }

    #[test]
    fn decodes_traffic_to_declared_length() {
        let Some(data) = intro3b() else { return };
        let c = Container::open(&data).expect("open");
        let stream = c.stream("traffic").expect("traffic stream");
        let declared = stream.len as usize;
        let decoded = stream.decode().expect("decode");
        assert_eq!(decoded.len(), declared);
    }

    #[test]
    fn rejects_bad_signature() {
        let mut bad = vec![0u8; 16];
        bad[0..4].copy_from_slice(&0xDEADBEEFu32.to_be_bytes());
        let tail = vec![0u8; 8];
        let buf = [bad, tail].concat();
        let c = Container::open(&buf);
        assert!(matches!(c, Err(NtrkError::BadSignature(_))));
    }
}
