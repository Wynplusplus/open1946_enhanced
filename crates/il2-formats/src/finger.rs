//! The runtime's `Finger.Int` / `Finger.incInt` algorithm.
//!
//! Ported directly from `com.maddox.rts.Finger`. The algorithm is a per-character
//! rolling hash over `FPaTable[256]`; the Java implementation is pure (the native
//! overloads just feed primitive arrays into it). Used to resolve class-name hashes
//! embedded in `.ntrk` spawn messages and in SFS archive paths.
//!
//! Hash values are signed 32-bit integers, identical to the Java output.

/// The 256-entry permutation table (`Finger.FPaTable`). The constant is duplicated here
/// rather than read from the install, since the table is part of the runtime's public
/// algorithm and the same on every install of the same major version.
const FP_A: [u32; 256] = [
    0, 595103070, 1190206140, 1703516130, 232928632, 781891622, 1259548612, 1751884442,
    951904174, 465857264, 2119040274, 1563783244, 895406806, 371613576, 1940899946, 1356285236,
    1903808348, 1375814146, 931714528, 351295678, 2090596900, 1608742778, 980082840, 420638150,
    1237657842, 1790813612, 255078990, 743227152, 1143414154, 1734316244, 47580982, 565086824,
    1660133048, 1099633638, 604144644, 125438298, 1863429056, 1281959582, 702591356, 177749026,
    1515157782, 2033710152, 482239402, 1070001908, 1470975086, 1960165680, 291264210, 841276300,
    327832036, 821222586, 1434143576, 1979958790, 510157980, 1024518594, 1486454304, 2078405502,
    725005898, 139344660, 1841802486, 1321148840, 651461426, 95161964, 1613076878, 1130173648,
    1721741358, 1172782448, 544123538, 51783628, 1803388246, 1208289288, 764190698, 250876596,
    1579374464, 2103171806, 416435516, 1001046114, 1405182712, 1891233702, 355498052, 910751002,
    400981874, 882831916, 1360487886, 1919936656, 436488714, 964478804, 1559580854, 2140003816,
    794466524, 203560322, 1772847712, 1255345982, 582528420, 29368570, 1682552600, 1194408518,
    74198678, 655664072, 1117598762, 1642445172, 160308206, 720803504, 1333723474, 1812433932,
    1020315960, 531121254, 2049037188, 1499029210, 825424960, 306868510, 2009327356, 1421568930,
    1964368330, 1450011796, 870644598, 278689320, 2029507762, 1536121324, 1040633358, 494814032,
    1302922852, 1859226426, 190323928, 673223046, 1078670108, 1664335426, 112863648, 633513214,
    1850207490, 1295999068, 683079614, 198081248, 1671813242, 1088247076, 626311878, 103567256,
    1459292844, 1971554290, 269094928, 863149390, 1528381396, 2019668618, 501753192, 1049667638,
    524165726, 1011265280, 1506752738, 2058859964, 316413734, 832871032, 1412238746, 2002092228,
    662881776, 83511470, 1634983756, 1108038162, 710996104, 152600022, 1821502004, 1340696426,
    213152698, 801963748, 1246062854, 1765663832, 22431426, 573492124, 1202150526, 1692389664,
    872977428, 393222474, 1928957608, 1367409654, 971678060, 445787186, 2132523984, 1550005902,
    2110635238, 1588933048, 993830490, 407120644, 1882163614, 1398211776, 920556322, 363208316,
    1165056840, 1711920662, 58737140, 553176234, 1217621552, 1810621294, 241333388, 756742610,
    148397356, 731959410, 1311328144, 1834076878, 87713876, 641918218, 1137406696, 1622409142,
    811907714, 320616412, 1989517374, 1441607008, 1032228858, 519963300, 2071434566, 1477384216,
    2040631920, 1524178734, 1062242508, 472384914, 1950590728, 1463495254, 850574772, 298463466,
    1092449758, 1650849920, 132935522, 613737020, 1291796646, 1871171064, 168712730, 695654212,
    1781253012, 1230196426, 752539944, 262296694, 1741289196, 1152482226, 557378640, 37773582,
    1385636922, 1911531876, 342244998, 924759000, 1601507650, 2081266716, 428084222, 989628064,
    458362056, 942309782, 1570969204, 2128321322, 380647856, 902345966, 1346446092, 1933160018,
    602860390, 9856568, 1696592346, 1181187204, 772595230, 225727296, 1761461410, 1267026428,
];

/// `Finger.incInt(seed, s)` — the Java algorithm fed char-by-char; `seed` is `i32`
/// (high bit treated as a sign bit in Java). Returns the signed `i32` hash.
pub fn inc_int(seed: i32, s: &str) -> i32 {
    let mut h: u32 = seed as u32;
    for ch in s.chars() {
        let code = ch as u32;
        let low = code & 0xff;
        let v = (h >> 24) & 0xff;
        h = (h << 8) | low;
        h ^= FP_A[v as usize];
        let hi = (code >> 8) & 0xff;
        let v2 = (h >> 24) & 0xff;
        h = (h << 8) | hi;
        h ^= FP_A[v2 as usize];
    }
    h as i32
}

/// `Finger.Int(s) = incInt(0, s)`.
pub fn int(s: &str) -> i32 {
    inc_int(0, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Java reference values computed from `com.maddox.rts.Finger` on these inputs.
    /// These don't depend on the install — the table is the algorithm.
    #[test]
    fn matches_java_known_hashes() {
        // Reference values from Finger.java run on the same input strings.
        let cases = [
            ("com.maddox.rts.NetChannel", 0x9f786ba8u32 as i32),
            ("com.maddox.il2.net.NetServerParams", 0xe2909192u32 as i32),
            ("com.maddox.il2.game.GameTrack", 0x894b0ef2u32 as i32),
            ("com.maddox.il2.net.NetUser", 0xbbf10be6u32 as i32),
            ("com.maddox.il2.game.Mission", 0x84191422u32 as i32),
        ];
        for (s, want) in cases {
            assert_eq!(int(s), want, "Finger.Int({s})");
        }
    }

    #[test]
    fn empty_string_is_zero() {
        assert_eq!(int(""), 0);
    }

    #[test]
    fn ascii_only_round_trip() {
        // Re-running incInt on the same string twice produces the same hash.
        let a = int("com.maddox.il2.objects.air.NetAircraft");
        let b = int("com.maddox.il2.objects.air.NetAircraft");
        assert_eq!(a, b);
    }
}
