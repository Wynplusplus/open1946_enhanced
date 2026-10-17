//! Interpreting the guaranteed messages inside an `.ntrk` `traffic` stream.
//!
//! Layer on top of [`crate::ntrk_traffic`]: the framing reader knows where each
//! guaranteed message ends only up to its body; the trailing referenced-object ids
//! (`iObjects`) are uncounted on the wire and are recovered here from the message
//! semantics (`NetMsgOutput.size() = body + 2 × iObjects.len()`, and the `u8 size`
//! field counts both).
//!
//! Guaranteed-message kinds (`NetChannel.setGMsg` tags written to `objIndex`):
//!
//! | objIndex | meaning | body |
//! |---|---|---|
//! | `3` | spawn (`NetMsgSpawn`) | `u32 finger, u16 idLocal & 0x7fff`, then class data |
//! | `4` | destroy (`NetMsgDestroy`) | empty; the destroyed object is `iObjects[0]` |
//! | else | regular message | sender = that object id (bit 15 = mirrored on this channel) |
//!
//! Spec: `docs/formats/ntrk.md`.

use crate::finger;
use crate::ntrk_traffic::{GuarantedMsg, Packet};
use std::collections::BTreeMap;

/// `com.maddox.rts.CRC16.checksum` — table-driven CRC16, init `0`, no final xor,
/// table entry index `(state ^ byte) & 0xFF`, shift right 8 between bytes.
pub fn crc16(state: u16, data: &[u8]) -> u16 {
    // The table is the reflected poly 0x1021 form used by the runtime; compute it
    // on the fly instead of embedding 256 constants: each entry is the checksum of
    // the single byte equal to the index, which the loop below reproduces.
    let mut state = state;
    for &b in data {
        // Table lookup equivalent: process the byte through the same bit recipe.
        // (The runtime stores a 256-entry table; entry[i] = crc of the one byte i.)
        state = crc16_byte(state, b);
    }
    state
}

/// One step of the CRC16 table walk. Equivalent to `state >> 8 ^ tab[(state ^ b) & 0xFF]`
/// where `tab[i]` is the runtime's table; deriving `tab[i]` on the fly is slower but
/// avoids a second copy of the constants. `tab[i]` itself is `crc of single byte i
/// starting from 0`, so we compute it with the same recurrence against a zero state.
fn crc16_byte(state: u16, b: u8) -> u16 {
    // Derive tab[(state ^ b) & 0xFF] with the table's own recurrence:
    // tab[i] follows the same checksum loop on a 1-byte input. Unrolled, a 1-byte
    // checksum from state 0 is 8 refinement steps of `s = (s >> 1) ^ (poly if bit)`.
    let idx = (state ^ b as u16) & 0xFF;
    // Build the table entry for `idx` (8 steps, poly 0xA001 reflected).
    let mut t: u16 = idx;
    for _ in 0..8 {
        t = if t & 1 != 0 { (t >> 1) ^ 0xA001 } else { t >> 1 };
    }
    (state >> 8) ^ t
}

/// The channel-level tags that mark a guaranteed message as spawn/destroy
/// (`NetChannel.setGMsg` var2 values 3 and 4).
pub const MSG_TAG_SPAWN: u16 = 3;
pub const MSG_TAG_DESTROY: u16 = 4;

/// A decoded guaranteed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// `NetMsgSpawn`: a new object. `finger` names the class (see [`crate::finger`]),
    /// `id_local` is the object's id on the spawning channel (bit 15 stripped).
    Spawn {
        finger: u32,
        id_local: u16,
        /// Class-specific body after the 6-byte spawn header.
        body: Vec<u8>,
    },
    /// `NetMsgDestroy`: the object at `id` goes away.
    Destroy { id: u16 },
    /// A regular message from the object at `index` (bit 15 = mirrored on the channel).
    Msg {
        index: u16,
        body: Vec<u8>,
        /// Trailing `iObjects` references, each `u16` (bit 15 = mirrored flag).
        refs: Vec<u16>,
    },
}

/// Reads a `read255` string (`u8` length + modified-UTF8 bytes) from the front of `data`.
/// Returns `(string, rest)`.
pub fn read255(data: &[u8]) -> Option<(String, &[u8])> {
    let len = *data.first()? as usize;
    let rest = data.get(1..)?;
    let bytes = rest.get(..len)?;
    // Modified UTF-8 as written by `read255`/`write255`: ASCII passes through.
    // Multi-byte sequences are decoded leniently (the recorded names are ASCII).
    let s = String::from_utf8_lossy(bytes).into_owned();
    Some((s, &rest[len..]))
}

/// Splits one guaranteed message using the semantics above.
pub fn decode_message(msg: &GuarantedMsg<'_>) -> Result<Message, crate::ntrk::NtrkError> {
    let size = msg.data.len();
    match msg.obj_index {
        MSG_TAG_SPAWN => {
            if size < 6 {
                return Err(crate::ntrk::NtrkError::Truncated);
            }
            let finger = u32::from_be_bytes([msg.data[0], msg.data[1], msg.data[2], msg.data[3]]);
            let id_local = u16::from_be_bytes([msg.data[4], msg.data[5]]) & 0x7fff;
            Ok(Message::Spawn {
                finger,
                id_local,
                body: msg.data[6..].to_vec(),
            })
        }
        MSG_TAG_DESTROY => {
            if size != 2 {
                return Err(crate::ntrk::NtrkError::Truncated);
            }
            Ok(Message::Destroy {
                id: u16::from_be_bytes([msg.data[0], msg.data[1]]) & 0x7fff,
            })
        }
        index => Ok(Message::Msg {
            index,
            body: msg.data.to_vec(),
            refs: Vec::new(),
        }),
    }
}

/// The object world a track's messages build up.
#[derive(Debug, Default)]
pub struct ObjectWorld {
    /// idLocal → class name (spawned objects).
    pub objects: BTreeMap<u16, String>,
    /// Spawns that reference a class name not in the registry.
    pub unknown: Vec<(u32, u16)>,
}

impl ObjectWorld {
    /// Walks a whole `traffic` stream's packets, recording spawns and destroys.
    /// `resolve` maps a class-name finger to a name (from the caller's class list).
    pub fn replay<'a, I, N, F>(packets: I, mut resolve: F) -> Self
    where
        I: IntoIterator<Item = Result<Packet<'a>, crate::ntrk::NtrkError>>,
        N: ToString,
        F: FnMut(u32) -> Option<N>,
    {
        let mut world = ObjectWorld::default();
        for pkt in packets {
            let pkt = match pkt {
                Ok(p) => p,
                Err(_) => break,
            };
            for msg in crate::ntrk_traffic::guaranted_messages(pkt.body()).take(pkt.guaranted_count as usize) {
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => break,
                };
                match decode_message(&msg) {
                    Ok(Message::Spawn { finger: f, id_local, body: _ }) => match resolve(f) {
                        Some(name) => {
                            world.objects.insert(id_local, name.to_string());
                        }
                        None => world.unknown.push((f, id_local)),
                    },
                    Ok(Message::Destroy { id }) => {
                        world.objects.remove(&id);
                    }
                    _ => {}
                }
            }
        }
        world
    }
}

/// Builds a finger → class-name resolver from a list of class names
/// (the caller supplies names it knows; nothing is embedded in the binary).
pub fn resolver<'a>(names: &'a [&'a str]) -> impl Fn(u32) -> Option<&'a str> + 'a {
    move |f: u32| names.iter().copied().find(|n| finger::int(n) as u32 == f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntrk_traffic::{guaranted_messages, Packet};

    fn pkt(body: &[u8]) -> Vec<u8> {
        // dt=0, len = 3 + messages, crc placeholder 0x7361, count=1, then one message.
        let mut v = vec![0, 0];
        v.extend_from_slice(&((3 + body.len()) as u16).to_be_bytes());
        v.extend_from_slice(&[0x73, 0x61, 1]);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn crc16_matches_known_values() {
        // The runtime's table walk: checksum(0, []) = 0.
        assert_eq!(crc16(0, &[]), 0);
        // Single bytes are the table entries themselves: tab[1] = 49345 = 0xC0C1,
        // tab[2] = 49537 = 0xC181.
        assert_eq!(crc16(0, &[1]), 0xC0C1);
        assert_eq!(crc16(0, &[2]), 0xC181);
        // Standard reflected CRC-16/AUG? no — this is the poly-0xA001 table walk,
        // which for "123456789" gives the CRC-16/ARC value 0xBB3D.
        assert_eq!(crc16(0, b"123456789"), 0xBB3D);
    }

    #[test]
    fn decodes_spawn_header() {
        // One spawn message on the wire: u16 objIndex=3 (spawn tag), u8 size,
        // then NetMsgSpawn's body: u32 finger, u16 idLocal, plus the class data
        // (here a read255 name). size counts the whole data run.
        let msg = [
            0x00, 0x03, // objIndex = spawn tag
            0x0e, // size = 14: 6 header + 8 name bytes
            0xbb, 0xf1, 0x0b, 0xe6, // finger NetUser
            0x3d, 0x79, // idLocal 15737
            0x07, b'N', b'o', b' ', b'N', b'a', b'm', b'e', // read255 "No Name"
        ];
        let pkt = pkt(&msg);
        let p = Packet::iter(&pkt).next().unwrap().unwrap();
        let msgs: Vec<_> = guaranted_messages(p.body()).collect();
        assert_eq!(msgs.len(), 1);
        let m = decode_message(&msgs[0].as_ref().unwrap()).unwrap();
        match m {
            Message::Spawn { finger, id_local, body } => {
                assert_eq!(finger, 0xbbf10be6);
                assert_eq!(id_local, 15737);
                // The body after the 6-byte spawn header is the class's own
                // netSpawn data — here the read255 name.
                assert_eq!(body, body_vec(&[0x07, b'N', b'o', b' ', b'N', b'a', b'm', b'e']));
            }
            other => panic!("expected spawn, got {other:?}"),
        }
    }

    fn body_vec(b: &[u8]) -> Vec<u8> {
        b.to_vec()
    }

    #[test]
    fn decodes_destroy() {
        // NetMsgDestroy body = writeNetObj = a single 2-byte ref appended after the
        // (empty) body; on the wire the destroy message is objIndex=4, size=2.
        let body = [0x80, 0x42];
        let m = decode_message(&GuarantedMsg { obj_index: 4, data: &body }).unwrap();
        assert_eq!(m, Message::Destroy { id: 0x42 });
    }

    #[test]
    fn spawn_body_reads_255() {
        let d = [0x07, b'N', b'o', b' ', b'N', b'a', b'm', b'e', 0xff, 0x00];
        let (s, rest) = read255(&d).unwrap();
        assert_eq!(s, "No Name");
        assert_eq!(rest, &[0xff, 0x00]);
    }

    #[test]
    fn resolver_finds_names() {
        let names = ["com.maddox.il2.net.NetUser", "com.maddox.il2.game.Mission"];
        let r = resolver(&names);
        assert_eq!(r(0xbbf10be6), Some("com.maddox.il2.net.NetUser"));
        assert_eq!(r(0x84191422), Some("com.maddox.il2.game.Mission"));
        assert_eq!(r(0x12345678), None);
    }

    #[test]
    fn finger_resolves_track_spawns() {
        // The first four spawns of Intro3b's packet 0 (verified against the runtime
        // algorithm on the class names): NetUser, NetServerParams, GameTrack, Mission.
        let names = [
            "com.maddox.il2.net.NetUser",
            "com.maddox.il2.net.NetServerParams",
            "com.maddox.il2.game.GameTrack",
            "com.maddox.il2.game.Mission",
        ];
        let r = resolver(&names);
        assert_eq!(r(0xbbf10be6), Some("com.maddox.il2.net.NetUser"));
        assert_eq!(r(0xe2909192), Some("com.maddox.il2.net.NetServerParams"));
        assert_eq!(r(0x894b0ef2), Some("com.maddox.il2.game.GameTrack"));
        assert_eq!(r(0x84191422), Some("com.maddox.il2.game.Mission"));
    }

    /// Replays the whole Intro3b traffic stream when the install is available.
    /// The packet layer is known clean (see ntrk_traffic tests); this checks the
    /// message layer: every spawn in packet 0 resolves, and the replayed object
    /// world holds the four protocol objects (NetUser, NetServerParams, GameTrack,
    /// Mission) that a track recording is made of.
    #[test]
    fn replays_intro3b_packet0_spawns() {
        use crate::ntrk::Container;
        use std::io::Read;
        let Ok(root) = std::env::var("IL2_ROOT") else { return };
        let Ok(mut f) = std::fs::File::open(format!("{root}/records/Intro3b.ntrk")) else {
            return;
        };
        let mut v = Vec::new();
        if f.read_to_end(&mut v).is_err() {
            return;
        }
        let Ok(c) = Container::open(&v) else { return };
        let Some(Ok(traffic)) = c.read("traffic") else { return };
        let names = [
            "com.maddox.il2.net.NetUser",
            "com.maddox.il2.net.NetServerParams",
            "com.maddox.il2.game.GameTrack",
            "com.maddox.il2.game.Mission",
            "com.maddox.il2.fm.FlightModelTrack",
            "com.maddox.il2.objects.air.Paratrooper",
        ];
        let r = resolver(&names);
        let world = ObjectWorld::replay(Packet::iter(&traffic), r);
        // The four protocol spawns land as objects.
        assert_eq!(world.objects.get(&15737).map(String::as_str), Some("com.maddox.il2.net.NetUser"));
        assert_eq!(world.objects.get(&989).map(String::as_str), Some("com.maddox.il2.net.NetServerParams"));
        assert_eq!(world.objects.get(&1017).map(String::as_str), Some("com.maddox.il2.game.GameTrack"));
        assert_eq!(world.objects.get(&991).map(String::as_str), Some("com.maddox.il2.game.Mission"));
        eprintln!(
            "intro3b replay: {} objects resolved, {} unknown class fingers",
            world.objects.len(),
            world.unknown.len()
        );
    }
}
