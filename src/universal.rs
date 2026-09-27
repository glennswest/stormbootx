//! Universal boot: one boot medium for every machine, none of them named on it
//! (#15, stormblock#200).
//!
//! A machine that has not been told who it is (no `tag =` on the media) claims
//! `boothost/default` and says which machine it is with its MAC:
//!
//! ```text
//! POST /api/v1/synonyms/boothost/default/claim  {"mac":"aa:bb:cc:dd:ee:ff", "serial":"…"}
//! ```
//!
//! The engine gives it a copy-on-write clone of the default release **of its
//! own**, recorded as host `mac-<hex>` until someone names it, and a second
//! claim from the same MAC gets the same host back. A MAC and not the SMBIOS
//! serial, because serials are not unique: seven of the eight Supermicro
//! MicroCloud nodes report the chassis serial, so a serial-keyed claim would
//! boot all seven as one machine.
//!
//! Two decisions live here, both pure, and the one piece of the claim reply
//! the console and the host NQN need (`claimed_host`):
//!
//! - **Whether the engine can take that claim at all** (`supports_default_claim`).
//!   The shape is stormblock's #200, which landed after v19.3.0. An engine from
//!   v17 to v19.3.0 reads `default` as one more tag: every machine would claim
//!   the same boot clone name, and each claim releases the clone the last
//!   machine is running on. Older engines have no fallback at all. So the
//!   default is claimed only from an engine whose public `/api/v1/health`
//!   reports a version strictly after 19.3.0, and anything unreadable is "no".
//! - **Which MAC is the machine's** (`better_mac`). The owner's words are "the
//!   first NIC's MAC". Handle order is not stable enough to mean that: SNP
//!   handles appear as drivers bind, and `tcp4::ensure_available` may bind
//!   some of them on demand. The lowest valid unicast MAC across every NIC is
//!   the same answer on every boot of the same hardware.
//!
//! Same constraints as `intent.rs` and `sha256.rs`: `core` only and no
//! `crate::` item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/universal.rs -o t/universal-test
//! ```

/// The last stormblock release without the #200 default claim.
const BEFORE_DEFAULT_CLAIM: (u32, u32, u32) = (19, 3, 0);

/// Whether an engine reporting `version` gives a `boothost/default` claim
/// carrying a MAC its own clone per machine.
///
/// `version` is the `version` field of `/api/v1/health`, as it arrived. A
/// leading `v` and a pre-release or build suffix are tolerated; anything else
/// that does not read as `MAJOR.MINOR.PATCH` is `false`, because the wrong
/// "yes" is the shared-clone hazard and the wrong "no" is today's behaviour.
pub fn supports_default_claim(version: &str) -> bool {
    parse_version(version).is_some_and(|v| v > BEFORE_DEFAULT_CLAIM)
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    // `19.4.0-rc1` and `19.4.0+abc` are 19.4.0 for this purpose.
    let core_part = v.split(['-', '+']).next()?;
    let mut parts = core_part.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

/// Whether six bytes can identify a machine: not all zeros, not broadcast, and
/// not a group address (the I/G bit, the low bit of the first octet).
pub fn usable_mac(mac: &[u8; 6]) -> bool {
    *mac != [0; 6] && *mac != [0xff; 6] && mac[0] & 1 == 0
}

/// Of the machine's MAC so far and one more NIC's, the one that identifies it:
/// the lowest usable one. Unusable candidates never win.
pub fn better_mac(best: Option<[u8; 6]>, candidate: [u8; 6]) -> Option<[u8; 6]> {
    if !usable_mac(&candidate) {
        return best;
    }
    match best {
        Some(b) if b <= candidate => Some(b),
        _ => Some(candidate),
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `aa:bb:cc:dd:ee:ff`, the form the engine stores a MAC alias in.
pub fn mac_colon(mac: &[u8; 6]) -> [u8; 17] {
    let mut out = [b':'; 17];
    for (i, b) in mac.iter().enumerate() {
        out[i * 3] = HEX[(b >> 4) as usize];
        out[i * 3 + 1] = HEX[(b & 0xf) as usize];
    }
    out
}

/// `mac-aabbccddeeff`, the provisional host the engine names a machine that
/// booted the default and has not been named (stormblock
/// `provisional_host_name`).
pub fn provisional_name(mac: &[u8; 6]) -> [u8; 16] {
    let mut out = *b"mac-000000000000";
    for (i, b) in mac.iter().enumerate() {
        out[4 + i * 2] = HEX[(b >> 4) as usize];
        out[4 + i * 2 + 1] = HEX[(b & 0xf) as usize];
    }
    out
}

/// The engine's name for the machine, and whether it is still provisional,
/// from a boothost claim reply (stormblock#199/#200):
///
/// ```text
/// "host": {"aliases": […], "claimed_as": …, "mac": …, "name": "mac-…", "new": …, "provisional": true}
/// ```
///
/// Read inside that object only, so a `"name"` elsewhere in the reply (the
/// volume's) is never taken for the host's. The aliases array holds strings
/// and no braces, so the first `}` ends the object. `None` from an engine that
/// sends no `host`.
pub fn claimed_host(body: &str) -> Option<(&str, bool)> {
    let at = body.find("\"host\"")? + "\"host\"".len();
    let rest = body[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('{')?;
    let obj = &rest[..rest.find('}')?];
    let name = value(obj, "name")?.strip_prefix('"')?;
    let name = &name[..name.find('"')?];
    let provisional = value(obj, "provisional").is_some_and(|v| v.starts_with("true"));
    Some((name, provisional))
}

/// What follows `"key":` in a flat object, untrimmed at the end.
fn value<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = obj[from..].find(key) {
        let start = from + i;
        let end = start + key.len();
        from = end;
        let quoted = start > 0
            && obj.as_bytes()[start - 1] == b'"'
            && obj.as_bytes().get(end) == Some(&b'"');
        if quoted {
            if let Some(v) = obj[end + 1..].trim_start().strip_prefix(':') {
                return Some(v.trim_start());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_engines_after_19_3_0_take_the_default_claim() {
        assert!(!supports_default_claim("13.7.0")); // forge, 2026-09-27
        assert!(!supports_default_claim("17.0.0")); // the shared-tag range
        assert!(!supports_default_claim("19.3.0"));
        assert!(supports_default_claim("19.3.1"));
        assert!(supports_default_claim("19.4.0"));
        assert!(supports_default_claim("20.0.0"));
        assert!(supports_default_claim("v19.4.0"));
        assert!(supports_default_claim("19.4.0-rc1"));
    }

    #[test]
    fn an_unreadable_version_is_no() {
        for v in ["", "19", "19.4", "19.4.0.1", "latest", "19.x.0", " "] {
            assert!(!supports_default_claim(v), "{v:?}");
        }
    }

    #[test]
    fn unusable_macs_never_identify() {
        assert!(!usable_mac(&[0; 6]));
        assert!(!usable_mac(&[0xff; 6]));
        assert!(!usable_mac(&[0x01, 0x00, 0x5e, 0, 0, 1])); // multicast
        assert!(usable_mac(&[0x02, 0, 0, 0, 0, 1])); // locally administered is fine
        assert_eq!(better_mac(None, [0; 6]), None);
        assert_eq!(better_mac(Some([0x10; 6]), [0xff; 6]), Some([0x10; 6]));
    }

    #[test]
    fn the_lowest_mac_wins_whatever_the_order() {
        let a = [0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x02];
        let b = [0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];
        let c = [0xec, 0x0d, 0x9a, 0x11, 0x22, 0x33];
        let fold = |order: &[[u8; 6]]| order.iter().fold(None, |m, n| better_mac(m, *n));
        assert_eq!(fold(&[a, b, c]), Some(b));
        assert_eq!(fold(&[c, a, b]), Some(b));
        assert_eq!(fold(&[b, c, a]), Some(b));
    }

    // The shape stormblock's claim_boothost sends: serde_json without
    // preserve_order, so keys are sorted and `provisional` ends the object.
    const REPLY: &str = r#"{"attach":{"address":"192.168.31.202","nqn":"nqn.x","nsid":7,"port":4420},"claimed_from":{"release":1,"synonym":"boothost/mac-ac1f6b8aa79c","version":1,"volume":2},"host":{"aliases":["ac:1f:6b:8a:a7:9c"],"claimed_as":"default","mac":"ac:1f:6b:8a:a7:9c","name":"mac-ac1f6b8aa79c","new":true,"provisional":true},"host_golden":{"collected":[],"minted":true,"synonym":"hostgolden/mac-ac1f6b8aa79c","volume":2},"kept":[],"released":[],"volume":{"access":"rw","id":3,"name":"boothost-mac-ac1f6b8aa79c","sealed":false}}"#;

    #[test]
    fn the_host_is_read_from_its_own_object() {
        assert_eq!(claimed_host(REPLY), Some(("mac-ac1f6b8aa79c", true)));
        let named = REPLY
            .replace(r#""name":"mac-ac1f6b8aa79c""#, r#""name":"server3""#)
            .replace(r#""provisional":true"#, r#""provisional":false"#);
        assert_eq!(claimed_host(&named), Some(("server3", false)));
        // Pretty-printed, and an engine from before #199 that sends no host.
        let pretty = "{\n  \"host\": {\n    \"name\": \"C2NR0Q2\",\n    \"provisional\": false\n  }\n}";
        assert_eq!(claimed_host(pretty), Some(("C2NR0Q2", false)));
        assert_eq!(claimed_host(r#"{"attach":{"nqn":"x"},"volume":{"name":"v"}}"#), None);
    }

    #[test]
    fn names_match_the_engine() {
        let m = [0xac, 0x1f, 0x6b, 0x8a, 0xa7, 0x9c];
        assert_eq!(&mac_colon(&m), b"ac:1f:6b:8a:a7:9c");
        assert_eq!(&provisional_name(&m), b"mac-ac1f6b8aa79c");
    }
}
