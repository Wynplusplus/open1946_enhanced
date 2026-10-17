//! Per-class interpretation of `.ntrk` traffic messages.
//!
//! Everything here is verified against Intro3b's recorded bytes (see
//! `docs/formats/ntrk.md`). The message layer ([`crate::ntrk_msg`]) says *who* speaks;
//! this module says *what they say*:
//!
//! - [`KeyRecordMsg`]: the pilot's hot-key events (`KeyRecord.NetKeyRecord.post`) —
//!   `i32 id` followed by 1..9 `i32` args, the count implied by the message size
//!   (4 B each, no sentinel on the wire; the -1 sentinel lives in the struct fields).
//! - [`FmTrackMsg`]: `FlightModelTrack.netWrite` — `i32 flags`, then the quantized
//!   control/engine/gear/weapon fields each flag bit marks present.
//! - [`AircraftGuaranted`]: `NetAircraft.netGetGMsg` — the first byte is the message
//!   id (0 = aircraft-state update, 11 = full first state, 6/7/8 = hits, 9 = explode,
//!   10 = dead …).
//! - [`sectfile_from_chunks`]: the mission a track replays is streamed inside the
//!   `Mission` object's case-1 messages as `SectFile` list data; this reassembles it.

use crate::ntrk_traffic::{GuarantedMsg, Packet};

/// The pilot's hot-key event (`KeyRecord.NetKeyRecord.post`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecordMsg {
    pub id: i32,
    pub args: Vec<i32>,
}

impl KeyRecordMsg {
    /// Parses a key-record message body: `i32 id`, then as many `i32` args as fit.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 || !data.len().is_multiple_of(4) {
            return None;
        }
        let id = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let mut args = Vec::with_capacity(data.len() / 4 - 1);
        for chunk in data[4..].as_chunks::<4>().0 {
            args.push(i32::from_be_bytes(*chunk));
        }
        Some(Self { id, args })
    }
}

/// `FlightModelTrack` state flags (`netWrite`/`netInput`), bits as the runtime sets them.
pub mod fm_flags {
    pub const GEAR: u32 = 1;
    pub const FLAPS: u32 = 2;
    pub const BRAKE: u32 = 4;
    pub const AILERON: u32 = 8;
    pub const ELEVATOR: u32 = 16;
    pub const RUDDER: u32 = 32;
    pub const TRIM_AILERON: u32 = 64;
    pub const TRIM_ELEVATOR: u32 = 128;
    pub const TRIM_RUDDER: u32 = 256;
    pub const FUEL: u32 = 512;
    pub const COCKPIT_AZIMUTH: u32 = 1024;
    pub const SAVE_DEEP: u32 = 2048;
    pub const SHAKE_LEVEL: u32 = 4096;
    pub const AM_X: u32 = 8192;
    /// bit 14 + engine index: engine `m` block follows (per-engine fields, u16 mask).
    pub const ENGINE_BASE: u32 = 1 << 14;
    pub const BITS: u32 = 1 << 31; // radiator/l-gear/r-gear/hydro/on-ground/air-brake
    pub const WEAPONS: u32 = 1 << 30; // u32 WeaponControl[0..20] bitmap
    pub const GUNS_COUNT: u32 = 1 << 28; // u16 bullet counts per gun follow
    pub const SENSITIVITY: u32 = 1 << 29;
}

/// One quantized field's decoding rule: `value = signed(byte) / mul` (or
/// `unsigned(short) * scale + offset` where the runtime uses that form).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quant {
    /// Decode: signed byte divided by this, for `mul > 0` byte fields.
    pub div: f32,
}

/// The decoded flight-model state of one `FlightModelTrack` message.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FmTrackMsg {
    pub flags: u32,
    pub gear: Option<f32>,
    pub flaps: Option<f32>,
    pub brake: Option<f32>,
    pub aileron: Option<f32>,
    pub elevator: Option<f32>,
    pub rudder: Option<f32>,
    pub trim_aileron: Option<f32>,
    pub trim_elevator: Option<f32>,
    pub trim_rudder: Option<f32>,
    pub fuel: Option<f32>,
    pub cockpit_azimuth_speed: Option<f32>,
    pub save_deep: Option<f32>,
    pub shake_level: Option<f32>,
    pub am_x: Option<f32>,
    pub sensitivity: Option<f32>,
    /// Per-engine state: `(mask, fields)` where the mask selects which of
    /// oil-in/oil-out/water/prop-phi/manifold/rpm/throttle/prop/mix/magneto/compressor
    /// are present (bits 1/2/4/8/16/32/64/128/256/512/1024).
    pub engines: Vec<(u16, Vec<f32>)>,
    /// The `bits` byte: bit0 radiator, bit1 l-gear, bit2 r-gear, bit3 hydro,
    /// bit4 on-ground, bit5 air-brake.
    pub bits: Option<u8>,
    pub weapons: Option<u32>,
    /// Gun bullet counts (present when the `GUNS_COUNT` flag is set).
    pub gun_counts: Vec<u16>,
}

impl FmTrackMsg {
    /// Parses a `FlightModelTrack` update (`netWrite` layout), reading only the
    /// fields the flags mark present. Returns `None` when the body is too short.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let flags = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let mut m = FmTrackMsg { flags, ..Default::default() };
        let mut q = 4usize;
        let byte = |q: &mut usize| -> Option<f32> {
            let b = *data.get(*q)? as i8;
            *q += 1;
            Some(b as f32)
        };
        let signed = |v: f32, div: f32| v / div;
        let f = |bit: u32, q: &mut usize, div: f32| -> Option<f32> {
            if flags & bit != 0 {
                Some(signed(byte(q)?, div))
            } else {
                None
            }
        };
        m.gear = f(fm_flags::GEAR, &mut q, 32.0);
        m.flaps = f(fm_flags::FLAPS, &mut q, 32.0);
        m.brake = f(fm_flags::BRAKE, &mut q, 32.0);
        m.aileron = f(fm_flags::AILERON, &mut q, 32.0);
        m.elevator = f(fm_flags::ELEVATOR, &mut q, 32.0);
        m.rudder = f(fm_flags::RUDDER, &mut q, 32.0);
        m.trim_aileron = f(fm_flags::TRIM_AILERON, &mut q, 32.0);
        m.trim_elevator = f(fm_flags::TRIM_ELEVATOR, &mut q, 32.0);
        m.trim_rudder = f(fm_flags::TRIM_RUDDER, &mut q, 32.0);
        if flags & fm_flags::FUEL != 0 {
            let v = u16::from_be_bytes([*data.get(q)?, *data.get(q + 1)?]);
            q += 2;
            m.fuel = Some(v as f32 * 0.5);
        }
        m.cockpit_azimuth_speed = f(fm_flags::COCKPIT_AZIMUTH, &mut q, 2.0);
        if flags & fm_flags::SAVE_DEEP != 0 {
            let v = u16::from_be_bytes([*data.get(q)?, *data.get(q + 1)?]);
            q += 2;
            m.save_deep = Some(v as f32 / 200.0 - 3.0);
        }
        m.shake_level = f(fm_flags::SHAKE_LEVEL, &mut q, 128.0);
        m.am_x = f(fm_flags::AM_X, &mut q, 10_000.0);
        // Engine blocks: bit 14+index set → u16 mask + that mask's fields.
        for e in 0..8usize {
            if flags & (fm_flags::ENGINE_BASE << e) == 0 {
                continue;
            }
            let mask = u16::from_be_bytes([*data.get(q)?, *data.get(q + 1)?]);
            q += 2;
            let mut vals = Vec::new();
            for bit in [1u16, 2, 4, 16, 64, 128, 256] {
                if mask & bit != 0 {
                    vals.push(signed(byte(&mut q)?, match bit {
                        1 | 2 | 4 => 0.25,
                        16 => 40.0,
                        64 => 100.0,
                        128 | 256 => 64.0,
                        _ => 1.0,
                    }));
                }
            }
            if mask & 8 != 0 {
                let v = u16::from_be_bytes([*data.get(q)?, *data.get(q + 1)?]);
                q += 2;
                vals.push(v as f32 / 625.0); // prop phi 0..1.6
            }
            if mask & 32 != 0 {
                let v = u16::from_be_bytes([*data.get(q)?, *data.get(q + 1)?]);
                q += 2;
                vals.push(v as f32 / 51.18125); // rpm
            }
            if mask & 512 != 0 {
                vals.push(*data.get(q)? as f32);
                q += 1;
            }
            if mask & 1024 != 0 {
                vals.push(*data.get(q)? as f32);
                q += 1;
            }
            m.engines.push((mask, vals));
        }
        if flags & fm_flags::BITS != 0 {
            m.bits = Some(*data.get(q)?);
            q += 1;
        }
        if flags & fm_flags::WEAPONS != 0 {
            let v = u32::from_be_bytes([
                *data.get(q)?,
                *data.get(q + 1)?,
                *data.get(q + 2)?,
                *data.get(q + 3)?,
            ]);
            q += 4;
            m.weapons = Some(v);
        }
        m.sensitivity = f(fm_flags::SENSITIVITY, &mut q, 100.0);
        if flags & fm_flags::GUNS_COUNT != 0 {
            // u16 per gun until the body ends; the runtime knows the gun count.
            while q + 2 <= data.len() {
                let v = u16::from_be_bytes([data[q], data[q + 1]]);
                q += 2;
                m.gun_counts.push(v);
            }
        }
        Some(m)
    }
}

/// `NetAircraft` guaranteed-message ids (`netGetGMsg` switch).
pub mod aircraft_msg {
    pub const AS_UPDATE: u8 = 0;
    pub const UPDATE_WAYPOINT: u8 = 1;
    pub const WEAPONS_BIT_STATES: u8 = 2;
    pub const GUN_PODS_ON: u8 = 3;
    pub const GUN_PODS_OFF: u8 = 4;
    pub const DROP_FUEL_TANKS: u8 = 5;
    pub const HITS: u8 = 6;
    pub const HIT_PROP: u8 = 7;
    pub const CUT: u8 = 8;
    pub const EXPLODE: u8 = 9;
    pub const DEAD: u8 = 10;
    pub const FIRST_UPDATE: u8 = 11;
}

/// A decoded `NetAircraft` guaranteed message (first byte = id, bit 7 = ask flag).
#[derive(Debug, Clone, PartialEq)]
pub enum AircraftGuaranted {
    /// Full initial state (`netGetFirstUpdate` → `netSpawnCommon`):
    /// 3×f32 position, 3×f32 orientation (azimuth/tangage/kren), 3×f32 speed,
    /// then the damage/CUT/weapon/AS fields (kept raw here — they need the
    /// aircraft's part-name table to interpret).
    FirstUpdate {
        pos: [f32; 3],
        orient: [f32; 3],
        speed: [f32; 3],
        rest: Vec<u8>,
    },
    /// Aircraft-state update (`FM.AS.netUpdate`): the leading bytes are the flag
    /// byte (bit 7 = ask, low nibble = engine count when bit 0 set …), the rest is
    /// class/AS-specific and kept raw.
    AsUpdate { raw: Vec<u8> },
    /// Any other guaranteed message: `(id, raw body)`.
    Other { id: u8, raw: Vec<u8> },
}

impl AircraftGuaranted {
    pub fn parse(data: &[u8]) -> Option<Self> {
        let id = *data.first()?;
        let rest = &data[1..];
        match id & 0x7f {
            aircraft_msg::FIRST_UPDATE => {
                if rest.len() < 36 {
                    return None;
                }
                let f32x3 = |q: usize| -> [f32; 3] {
                    [
                        f32::from_be_bytes([rest[q], rest[q + 1], rest[q + 2], rest[q + 3]]),
                        f32::from_be_bytes([rest[q + 4], rest[q + 5], rest[q + 6], rest[q + 7]]),
                        f32::from_be_bytes([rest[q + 8], rest[q + 9], rest[q + 10], rest[q + 11]]),
                    ]
                };
                let pos = f32x3(0);
                let orient = f32x3(12);
                let speed = f32x3(24);
                Some(AircraftGuaranted::FirstUpdate {
                    pos,
                    orient,
                    speed,
                    rest: rest[36..].to_vec(),
                })
            }
            aircraft_msg::AS_UPDATE => Some(AircraftGuaranted::AsUpdate { raw: rest.to_vec() }),
            id => Some(AircraftGuaranted::Other {
                id,
                raw: rest.to_vec(),
            }),
        }
    }
}

/// A reassembled streamed mission: section name + entries as `(block, key, value)`
/// triples, exactly what `SectFile` list-data encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamedSection {
    pub name: String,
    pub entries: Vec<(u32, String, String)>,
}

/// Reassembles the streamed mission from `Mission` case-1 guaranteed messages
/// and decodes the `doSendMission` record framing. Each message body (after the
/// 1-byte case id) is a run of records:
///
/// ```text
/// i32  section      −1 = a new section follows; ≥ 0 = an entry of the current one
/// u8   len + bytes  section name (for −1) / key (for ≥ 0)
/// u8   len + bytes  value (entries only)
/// ```
///
/// The runtime chunks the stream at ~128/250 bytes purely for message size; records
/// never straddle messages. `mission_sender` is the id of the `Mission` net object
/// (resolved from its spawn) — other senders' messages are ignored.
pub fn sectfile_from_messages<'a, I>(msgs: I, mission_sender: u16) -> Option<Vec<StreamedSection>>
where
    I: IntoIterator<Item = &'a GuarantedMsg<'a>>,
{
    let mut sections: Vec<StreamedSection> = Vec::new();
    for msg in msgs {
        if msg.obj_index & 0x7fff != mission_sender {
            continue;
        }
        let data = msg.data;
        if data.len() < 2 || data[0] != 1 {
            continue;
        }
        let mut q = 1usize;
        while q < data.len() {
            let sect = i32::from_be_bytes([
                *data.get(q)?,
                *data.get(q + 1)?,
                *data.get(q + 2)?,
                *data.get(q + 3)?,
            ]);
            q += 4;
            let nlen = *data.get(q)? as usize;
            q += 1;
            let name = String::from_utf8_lossy(data.get(q..q + nlen)?).into_owned();
            q += nlen;
            if sect < 0 {
                sections.push(StreamedSection { name, entries: Vec::new() });
            } else {
                let vlen = *data.get(q)? as usize;
                q += 1;
                let val = String::from_utf8_lossy(data.get(q..q + vlen)?).into_owned();
                q += vlen;
                sections.last_mut()?.entries.push((sect as u32, name, val));
            }
        }
    }
    Some(sections)
}

/// Walks a whole traffic stream, collecting the streamed mission chunks
/// (guaranteed messages whose body starts with case byte `1` from any sender)
/// and the key-record events. The caller still resolves senders via
/// [`crate::ntrk_msg::ObjectWorld`] if it wants names.
/// One aircraft's initial state as recorded at its first update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FirstUpdate {
    pub sender: u16,
    pub pos: [f32; 3],
    pub orient: [f32; 3],
    pub speed: [f32; 3],
}

pub struct TrafficSummary {
    pub key_events: Vec<KeyRecordMsg>,
    pub fm_updates: Vec<FmTrackMsg>,
    pub aircraft_first_updates: Vec<FirstUpdate>,
    /// Everything else (AS updates, hits, cuts, …) as `(sender, body)` for the
    /// caller's per-class interpretation.
    pub other_guaranted: Vec<(u16, Vec<u8>)>,
}

/// Scans all guaranteed messages of a traffic stream and classifies them.
/// `fm_sender` is the sender id whose messages are `FlightModelTrack` updates
/// (the object spawned as `com.maddox.il2.fm.FlightModelTrack`).
pub fn summarize<'a, I>(packets: I, fm_sender: u16) -> TrafficSummary
where
    I: IntoIterator<Item = Result<Packet<'a>, crate::ntrk::NtrkError>>,
{
    let mut out = TrafficSummary {
        key_events: Vec::new(),
        fm_updates: Vec::new(),
        aircraft_first_updates: Vec::new(),
        other_guaranted: Vec::new(),
    };
    for pkt in packets {
        let pkt = match pkt {
            Ok(p) => p,
            Err(_) => break,
        };
        for msg in crate::ntrk_traffic::guaranted_messages(pkt.body()).take(pkt.guaranted_count as usize)
        {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            let sender = msg.obj_index & 0x7fff;
            match msg.obj_index {
                crate::ntrk_msg::MSG_TAG_SPAWN | crate::ntrk_msg::MSG_TAG_DESTROY => {}
                _ => {
                    if sender == fm_sender {
                        if let Some(fm) = FmTrackMsg::parse(msg.data) {
                            out.fm_updates.push(fm);
                        }
                    } else if msg.data.len() >= 37 {
                        // Full first state: 36 bytes of pos/orient/speed after the id.
                        if let Some(AircraftGuaranted::FirstUpdate { pos, orient, speed, rest: _ }) =
                            AircraftGuaranted::parse(msg.data)
                        {
                            out.aircraft_first_updates.push(FirstUpdate { sender, pos, orient, speed });
                        }
                    } else if msg.data.len() <= 16 {
                        // Key-record events are 1..4 ints; aircraft guaranteed
                        // messages with engine/turret data are longer (verified
                        // against Intro3b: key events are 8/12/16 B, AS updates
                        // 4 B + engines, first updates 92/94 B).
                        if let Some(k) = KeyRecordMsg::parse(msg.data) {
                            out.key_events.push(k);
                        }
                    } else {
                        out.other_guaranted.push((sender, msg.data.to_vec()));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntrk_traffic::GuarantedMsg;

    #[test]
    fn key_record_parses_1_and_3_arg_messages() {
        // 8-byte body from Intro3b: id=27, one arg 0.
        let m = KeyRecordMsg::parse(&[0, 0, 0, 27, 0, 0, 0, 0]).unwrap();
        assert_eq!(m.id, 27);
        assert_eq!(m.args, vec![0]);
        // 16-byte body: id=203, args 1, 1001, 1002.
        let mut d = Vec::new();
        d.extend_from_slice(&203i32.to_be_bytes());
        d.extend_from_slice(&1i32.to_be_bytes());
        d.extend_from_slice(&1001i32.to_be_bytes());
        d.extend_from_slice(&1002i32.to_be_bytes());
        let m = KeyRecordMsg::parse(&d).unwrap();
        assert_eq!(m.id, 203);
        assert_eq!(m.args, vec![1, 1001, 1002]);
        // 4-byte body: id only.
        let m = KeyRecordMsg::parse(&[0, 0, 0, 1]).unwrap();
        assert!(m.args.is_empty());
    }

    #[test]
    fn fm_track_parses_control_flags() {
        // flags = ail|elev|rud (0x38), bytes: ail=-4/32, elev=-5/32, rud=-24/32.
        let mut d = Vec::new();
        d.extend_from_slice(&0x38u32.to_be_bytes());
        d.extend_from_slice(&[(-4i8) as u8, (-5i8) as u8, (-24i8) as u8]);
        let fm = FmTrackMsg::parse(&d).unwrap();
        assert_eq!(fm.flags, 0x38);
        assert_eq!(fm.aileron, Some(-0.125));
        assert_eq!(fm.elevator, Some(-0.15625));
        assert_eq!(fm.rudder, Some(-0.75));
        assert!(fm.gear.is_none() && fm.bits.is_none() && fm.weapons.is_none());
    }

    #[test]
    fn fm_track_parses_bits_and_weapons() {
        // flags = BITS|WEAPONS (0xc0000000); bits byte 0x06 (l+rgear), weapons 0.
        let mut d = Vec::new();
        d.extend_from_slice(&0xc000_0000u32.to_be_bytes());
        d.push(0x06);
        d.extend_from_slice(&0u32.to_be_bytes());
        let fm = FmTrackMsg::parse(&d).unwrap();
        assert_eq!(fm.bits, Some(0x06));
        assert_eq!(fm.weapons, Some(0));
    }

    #[test]
    fn fm_track_parses_engine_block() {
        // flags = ENGINE_BASE (m0) + mask 0x0020 (rpm short) → w = value/51.18125.
        let mut d = Vec::new();
        d.extend_from_slice(&fm_flags::ENGINE_BASE.to_be_bytes());
        d.extend_from_slice(&0x0020u16.to_be_bytes());
        d.extend_from_slice(&14470u16.to_be_bytes()); // 14470/51.18125 ≈ 282.7
        let fm = FmTrackMsg::parse(&d).unwrap();
        assert_eq!(fm.engines.len(), 1);
        let (mask, vals) = &fm.engines[0];
        assert_eq!(*mask, 0x0020);
        assert!((vals[0] - 282.7).abs() < 0.1);
    }

    #[test]
    fn aircraft_first_update_decodes() {
        // id 11 + 9 floats (pos/orient/speed).
        let mut d = vec![11u8];
        for v in [-402183.2f32, -263262.3, 2010.4, 389.8, 1.4, 0.0, 79.9, -45.8, -0.0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        let a = AircraftGuaranted::parse(&d).unwrap();
        match a {
            AircraftGuaranted::FirstUpdate { pos, orient, speed, rest } => {
                assert_eq!(pos, [-402183.2, -263262.3, 2010.4]);
                assert_eq!(orient[0], 389.8);
                assert_eq!(speed, [79.9, -45.8, -0.0]);
                assert!(rest.is_empty());
            }
            other => panic!("expected FirstUpdate, got {other:?}"),
        }
    }

    #[test]
    fn sectfile_decodes_stream_framing() {
        // One section "MAIN" with MAP=... and TIME=6.0 (Intro3b's real first bytes):
        // records are (i32 section, u8-len name [, u8-len value]).
        let mut p = Vec::new();
        p.extend_from_slice(&(-1i32).to_be_bytes());
        p.push(4);
        p.extend_from_slice(b"MAIN");
        p.extend_from_slice(&0i32.to_be_bytes());
        p.push(3);
        p.extend_from_slice(b"MAP");
        p.push(17);
        p.extend_from_slice(b"CoralSea/load.ini");
        p.extend_from_slice(&0i32.to_be_bytes());
        p.push(4);
        p.extend_from_slice(b"TIME");
        p.push(3);
        p.extend_from_slice(b"6.0");
        // Wrap into one case-1 message from sender 991.
        let mut d = vec![1u8];
        d.extend_from_slice(&p);
        let msgs = vec![GuarantedMsg { obj_index: 991, data: &d }];
        let s = sectfile_from_messages(&msgs, 991).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "MAIN");
        assert_eq!(s[0].entries[0], (0, "MAP".to_string(), "CoralSea/load.ini".to_string()));
        assert_eq!(s[0].entries[1].1, "TIME");
        assert_eq!(s[0].entries[1].2, "6.0");
    }

    #[test]
    fn sectfile_spans_multiple_messages() {
        // The runtime chunks the record stream at ~128/250 bytes; a record never
        // straddles messages, so decoding is a straight concatenation of bodies.
        let head = {
            let mut d = vec![1u8]; // case byte
            d.extend_from_slice(&(-1i32).to_be_bytes());
            d.push(4);
            d.extend_from_slice(b"MAIN");
            d
        };
        let tail = {
            let mut d = vec![1u8];
            d.extend_from_slice(&7i32.to_be_bytes()); // section index 7 (unused)
            d.push(4);
            d.extend_from_slice(b"TIME");
            d.push(3);
            d.extend_from_slice(b"6.0");
            d
        };
        // A non-mission sender's message must be ignored.
        let noise = vec![1u8, 0, 0, 0, 9, 5];
        let msgs = vec![
            GuarantedMsg { obj_index: 991, data: &head },
            GuarantedMsg { obj_index: 1002, data: &noise },
            GuarantedMsg { obj_index: 991, data: &tail },
        ];
        let s = sectfile_from_messages(&msgs, 991).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "MAIN");
        assert_eq!(s[0].entries[0].1, "TIME");
        assert_eq!(s[0].entries[0].2, "6.0");
    }

    /// End-to-end on the real Intro3b track (needs the install): the FM-track
    /// messages decode, the key-record events parse, the mission reassembles to
    /// the CoralSea intro with its F4F3/A6M2 wings, and the five aircraft's
    /// first updates carry CoralSea-frame positions.
    #[test]
    fn intro3b_end_to_end() {
        use crate::ntrk::Container;
        use crate::ntrk_msg::ObjectWorld;
        use crate::ntrk_traffic::Packet;
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
        let r = crate::ntrk_msg::resolver(&names);
        let world = ObjectWorld::replay(Packet::iter(&traffic), r);
        let fm_sender = world
            .objects
            .iter()
            .find(|(_, n)| n.as_str() == "com.maddox.il2.fm.FlightModelTrack")
            .map(|(id, _)| *id)
            .expect("FM track spawned");
        let summary = summarize(Packet::iter(&traffic), fm_sender);
        eprintln!(
            "intro3b: {} key events, {} fm updates, {} first updates",
            summary.key_events.len(),
            summary.fm_updates.len(),
            summary.aircraft_first_updates.len()
        );
        assert_eq!(summary.fm_updates.len(), 5376);
        // 13 aircraft = the four wings' Planes values (3+4+3+3); each mirror sends
        // exactly one first-update.
        assert_eq!(summary.aircraft_first_updates.len(), 13);
        // All FM updates parse with fields consistent with their flags.
        for fm in &summary.fm_updates {
            let engine_count = (0..8)
                .filter(|e| fm.flags & (fm_flags::ENGINE_BASE << e) != 0)
                .count();
            assert_eq!(fm.engines.len(), engine_count);
            if fm.flags & fm_flags::AILERON != 0 {
                let a = fm.aileron.unwrap();
                assert!((-1.0..=1.0).contains(&a), "aileron {a} out of range");
            }
        }
        // The streamed mission: sections Wing/Class appear with the intro's aircraft.
        let mission_msgs: Vec<GuarantedMsg> = Packet::iter(&traffic)
            .filter_map(|p| {
                let p = p.ok()?;
                Some(
                    crate::ntrk_traffic::guaranted_messages(p.body())
                        .take(p.guaranted_count as usize)
                        .filter_map(|m| m.ok()),
                )
            })
            .flatten()
            .collect();
        let mission_sender = world
            .objects
            .iter()
            .find(|(_, n)| n.as_str() == "com.maddox.il2.game.Mission")
            .map(|(id, _)| *id)
            .expect("Mission spawned");
        let mission = sectfile_from_messages(&mission_msgs, mission_sender).expect("mission streams");
        let class: Vec<_> = mission
            .iter()
            .flat_map(|s| &s.entries)
            .filter(|(_, k, _)| k == "Class")
            .map(|(_, _, v)| v.as_str())
            .collect();
        assert!(class.contains(&"air.F4F3"), "F4F3 in {class:?}");
        assert!(class.contains(&"air.A6M2_21"), "A6M2_21 in {class:?}");
        let map: Vec<_> = mission
            .iter()
            .flat_map(|s| &s.entries)
            .filter(|(_, k, _)| k == "MAP")
            .map(|(_, _, v)| v.as_str())
            .collect();
        assert_eq!(map, vec!["CoralSea/load.ini"]);
    }
}
