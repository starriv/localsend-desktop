//! Device identity verification.
//!
//! LocalSend's "Verify" page lets two devices compare their certificate
//! fingerprints: both sides show the same combined fingerprint, as a text and
//! as a sequence of Material icons. Both representations come from the
//! official implementation, so they can be compared with the other device.

/// The combined fingerprint two devices compare.
///
/// Both certificate fingerprints (uppercase hex SHA-256), sorted and
/// concatenated — exactly the string the official clients show under
/// "Verify → Text". Sorting makes it identical on both devices; the uppercase
/// normalization keeps it identical even if a peer reported lowercase hex.
pub fn combined_fingerprint(own: &str, peer: &str) -> String {
    let mut parts = [
        own.trim().to_ascii_uppercase(),
        peer.trim().to_ascii_uppercase(),
    ];
    parts.sort();
    parts.concat()
}

/// The Material icon alphabet LocalSend's icon view uses, in order: the
/// `Icons.*` list in the official app's `fingerprint_alphabet.dart` (260
/// entries), mapped to the codepoints in Flutter's `icons.dart`.
///
/// The order is part of the display: two devices must map the same digit to
/// the same icon. Changing the list means regenerating this table from both
/// upstream files, never sorting or deduplicating it.
const ICON_ALPHABET: [u32; 260] = [
    0xe037, 0xe03c, 0xe063, 0xe072, 0xe076, 0xe084, 0xe085, 0xe089, 0xf04be, 0xe090, 0xe0b1,
    0xe0b2, 0xe0b6, 0xf04c5, 0xe0c4, 0xe0c8, 0xe0c9, 0xf04c6, 0xe0cf, 0xe0d2, 0xe0dd, 0xe0df,
    0xe0e0, 0xe0e4, 0xe0ee, 0xe0f1, 0xe113, 0xe115, 0xe116, 0xe11e, 0xe120, 0xe121, 0xe122, 0xe138,
    0xe143, 0xe144, 0xf04cb, 0xe149, 0xf04cc, 0xe14d, 0xe151, 0xe153, 0xe15d, 0xf04cd, 0xe163,
    0xe16f, 0xe179, 0xe17d, 0xe185, 0xe186, 0xe191, 0xf0858, 0xf04d9, 0xe199, 0xe19b, 0xe19f,
    0xf04da, 0xf0797, 0xe1b9, 0xf04ed, 0xe1d2, 0xe1d3, 0xe1d5, 0xe1d7, 0xe1fc, 0xe214, 0xe217,
    0xe21a, 0xf04f8, 0xe229, 0xe22a, 0xe22c, 0xe22f, 0xe234, 0xe248, 0xe250, 0xe252, 0xf04fd,
    0xe25a, 0xe25b, 0xe262, 0xe263, 0xe287, 0xe288, 0xe28a, 0xe28d, 0xe28e, 0xe295, 0xe297, 0xe2a0,
    0xf0866, 0xe2bb, 0xe2d3, 0xe2e4, 0xe2ee, 0xf06be, 0xe2f3, 0xe2ff, 0xe304, 0xe306, 0xf0517,
    0xe315, 0xf0518, 0xe318, 0xe325, 0xe32a, 0xf051d, 0xe330, 0xe331, 0xe34a, 0xe34e, 0xf052a,
    0xf052b, 0xe351, 0xe35d, 0xe35e, 0xe365, 0xe36d, 0xe379, 0xe37b, 0xe380, 0xe383, 0xe38c,
    0xe38d, 0xe391, 0xe393, 0xe394, 0xe398, 0xe39a, 0xe3a0, 0xe3a6, 0xe3a7, 0xe3a8, 0xe3ae, 0xe3c1,
    0xe3c8, 0xe3d2, 0xe3d3, 0xe3d9, 0xe3db, 0xe3dc, 0xe3dd, 0xe3e1, 0xe3e6, 0xe3e7, 0xf053d,
    0xe3fe, 0xf053e, 0xe40a, 0xe40b, 0xe40d, 0xe414, 0xf0541, 0xe42e, 0xe44f, 0xf07b7, 0xe463,
    0xe465, 0xe46b, 0xe478, 0xf0546, 0xe491, 0xe4a1, 0xf0548, 0xe4a2, 0xe4b6, 0xe4be, 0xe4c3,
    0xe4c9, 0xe4dc, 0xe4e0, 0xe4e4, 0xe4ea, 0xf07ba, 0xe4ef, 0xe4f0, 0xe4f4, 0xe4f5, 0xe501,
    0xe502, 0xe506, 0xe50c, 0xf055a, 0xe511, 0xe514, 0xe532, 0xf055c, 0xe53d, 0xf0561, 0xe542,
    0xe54d, 0xf0562, 0xe553, 0xe559, 0xe55a, 0xe567, 0xe57f, 0xf0877, 0xe596, 0xe59b, 0xe59c,
    0xe5a0, 0xf0569, 0xe5b7, 0xe5c5, 0xe5c6, 0xe5c8, 0xe5d0, 0xf07c6, 0xe5db, 0xe5e8, 0xe5e9,
    0xe5ea, 0xe5ec, 0xe5ef, 0xe5f2, 0xe5f3, 0xf0570, 0xf0572, 0xe5f8, 0xe5f9, 0xe60a, 0xe60e,
    0xe612, 0xf0585, 0xe640, 0xf0587, 0xe654, 0xe655, 0xe656, 0xe65b, 0xe66c, 0xe66d, 0xf07cc,
    0xe672, 0xe674, 0xe675, 0xe683, 0xe68a, 0xe697, 0xf0597, 0xe6a8, 0xe6b4, 0xe6bd, 0xe6c1,
    0xf07d3, 0xf07d4, 0xf05a0, 0xe6cb, 0xe6ce, 0xf05a2, 0xe6d3, 0xe6d9, 0xe6e1, 0xe6e3, 0xe6e7,
    0xf07da, 0xe6f0, 0xe6f1, 0xe6f2,
];

/// How many icons a fingerprint renders to: the official clients show
/// `ceil(128 * log2 / log(260))` = 16 base-260 digits of the hash.
const ICON_DIGITS: usize = 16;

/// The icon codepoints for `combined`, exactly as LocalSend's
/// `fingerprintToIcons`: SHA-256 of the combined text, its first 128 bits read
/// as a big-endian base-260 number, each digit indexing [`ICON_ALPHABET`].
///
/// The hash input is the combined text as shown (uppercase hex, sorted) — the
/// official clients hash the string they display, not its characters' bytes
/// separately, so this matches the icon view on every device.
pub fn fingerprint_icons(combined: &str) -> [u32; ICON_DIGITS] {
    let digest = localsend::crypto::hash::sha256(combined.as_bytes());
    let mut value = 0_u128;
    for byte in &digest[..16] {
        value = (value << 8) | u128::from(*byte);
    }
    let base = ICON_ALPHABET.len() as u128;
    let mut icons = [ICON_ALPHABET[0]; ICON_DIGITS];
    for slot in icons.iter_mut().rev() {
        *slot = ICON_ALPHABET[(value % base) as usize];
        value /= base;
    }
    icons
}

#[cfg(test)]
mod tests {
    use super::{ICON_ALPHABET, ICON_DIGITS, combined_fingerprint, fingerprint_icons};

    const A: &str = "AA11BB22CC33DD44EE55FF660011223344556677889900AABBCCDDEEFF00112233";
    const B: &str = "00112233445566778899AABBCCDDEEFF00112233445566778899AABBCCDDEEFF00";

    #[test]
    fn both_devices_compute_the_same_text() {
        assert_eq!(combined_fingerprint(A, B), combined_fingerprint(B, A));
        assert_eq!(combined_fingerprint(A, B).len(), A.len() + B.len());
    }

    #[test]
    fn fingerprints_are_normalized_to_uppercase() {
        let lower = A.to_ascii_lowercase();
        assert_eq!(combined_fingerprint(&lower, B), combined_fingerprint(A, B));
        assert_eq!(
            combined_fingerprint("  ", B),
            combined_fingerprint("", B),
            "surrounding whitespace is ignored"
        );
    }

    #[test]
    fn the_sorted_order_is_lexicographic() {
        let combined = combined_fingerprint(A, B);
        assert!(
            combined.starts_with(B),
            "the smaller fingerprint comes first: {combined}"
        );
    }

    #[test]
    fn icons_match_the_official_icon_view() {
        // Reference computed with the official Dart implementation
        // (`fingerprintToIcons`) over the alphabet and the bundled font.
        let combined = "00112233445566778899AABBCCDDEEFF00112233445566778899AABBCCDDEEFF00AA11BB22CC33DD44EE55FF660011223344556677889900AABBCCDDEEFF00112233";
        let icons = fingerprint_icons(combined);
        assert_eq!(
            icons,
            [
                0xe42e, 0xf05a2, 0xf0858, 0xe330, 0xe6e3, 0xe4dc, 0xe252, 0xe2ee, 0xe40b, 0xe1b9,
                0xe37b, 0xe5f3, 0xe11e, 0xe38d, 0xe5f3, 0xe502
            ],
            "the icon sequence must equal LocalSend's"
        );
    }

    #[test]
    fn the_digit_count_follows_the_official_formula() {
        // `convertFingerprintToAlphabet`: ceil(32 hex chars * 4 bits /
        // log2(alphabet)), i.e. 16 icons for 260 entries.
        assert_eq!(ICON_ALPHABET.len(), 260);
        let digits = (128.0 / (ICON_ALPHABET.len() as f64).log2()).ceil() as usize;
        assert_eq!(digits, ICON_DIGITS);
    }

    #[test]
    fn known_alphabet_entries_keep_their_material_codepoints() {
        // First and last of LocalSend's list, from Flutter's `icons.dart`:
        // ac_unit, accessibility, ... work.
        assert_eq!(ICON_ALPHABET[0], 0xe037);
        assert_eq!(ICON_ALPHABET[1], 0xe03c);
        assert_eq!(ICON_ALPHABET[259], 0xe6f2);
    }

    #[test]
    fn icons_are_a_fixed_number_of_alphabet_entries() {
        let combined = combined_fingerprint("AABB", "CCDD");
        let icons = fingerprint_icons(&combined);
        assert_eq!(icons.len(), ICON_DIGITS);
        assert!(icons.iter().all(|icon| ICON_ALPHABET.contains(icon)));
    }
}
