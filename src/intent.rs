//! What this machine has been told to do on this boot: install, boot local, or
//! decide for itself.
//!
//! The intent is stored on the engine with the machine's boot host and read
//! **before** the claim, because a claim mints a clone and `local` exists so
//! that nothing gets minted. Setting a state and then power-cycling the machine
//! gets you that state. The contract is stormblock#148 (0e3c47b):
//!
//! ```text
//! GET /api/v1/synonyms/boothost/<name>/intent
//!     -> 200 {"host":"server1","intent":"local","updated_at":…}   (open)
//!     -> 404 for a name that is no host and no alias
//! ```
//!
//! `run()` asks under the same names the claim tries, in the same order (DNS
//! name, MAC, tag), and moves on only on a 404. The engine resets `install`
//! to `local` when the node's OS reports the install done
//! (`POST …/installed`); this binary never writes the intent.
//!
//! | intent    | this binary                                              |
//! |-----------|----------------------------------------------------------|
//! | `install` | claims and boots the image                               |
//! | `local`   | falls through to the disk; no claim, no clone            |
//! | `auto`    | claims; with `local_when_bootable`, a bootable disk first |
//!
//! **Any doubt reads as `auto`.** A 404, an engine without the route, an
//! unreachable engine, a body with no intent or one this binary does not know:
//! each is reported and then treated as `auto`.
//!
//! **`auto` claims by default.** The owner's override of 2026-09-30: until
//! intents work on forge (stormblock#148), every boot claims, because a stale
//! OS on a local disk would otherwise win. `local_when_bootable = true` in
//! `stormboot.conf` turns on the rule below; `run()` then passes whether a
//! disk can boot, and otherwise always passes `false`.
//!
//! **With it on, `auto` boots the local disk when there is one to boot** (#3,
//! the owner's first answer of 2026-09-30). A machine boots local unless an install was
//! requested; but a bare machine, or one the engine does not know yet, has
//! nobody who could have requested one, so "nothing to boot locally" claims
//! as before. "Something to boot" is a local, non-removable disk other than
//! the boot media whose GPT has an ESP carrying `\EFI\BOOT\BOOTX64.EFI`,
//! found by `blockio::local_bootloader` with no network. That is `decide`.
//! Only `install` claims over a bootable disk, and only `local` skips the
//! claim on a machine with nothing to boot.
//!
//! This module has the same constraints as `sha256.rs`, for the same reason: it
//! uses only `core` and names no `crate::` item, so its tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/intent.rs -o $CARGO_TARGET_DIR/intent-test
//! ```
//!
//! The HTTP exchange lives in `registry.rs`. This module parses and decides.

/// A boot intent the engine can state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Take the network image. `install` is one-shot, but resetting it to
    /// `local` after the flow-over is the engine's job, not this binary's.
    Install,
    /// Boot what is on the disk. No claim, so no clone.
    Local,
    /// Decide for itself: the local disk if it has a bootloader, else claim.
    Auto,
}

/// What this boot does, given the intent and whether a local disk carries a
/// bootloader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Claim an image from the engine and boot it.
    Claim,
    /// Fall through to the local disk; nothing is claimed.
    Local,
}

/// The owner's rule (#3, 2026-09-30): an install is taken only when asked
/// for, and a machine with nothing of its own to boot claims.
pub fn decide(intent: Intent, local_bootable: bool) -> Action {
    match intent {
        Intent::Install => Action::Claim,
        Intent::Local => Action::Local,
        Intent::Auto if local_bootable => Action::Local,
        Intent::Auto => Action::Claim,
    }
}

impl Intent {
    pub fn name(self) -> &'static str {
        match self {
            Intent::Install => "install",
            Intent::Local => "local",
            Intent::Auto => "auto",
        }
    }

    /// Whether `decide` needs to know about the local disk at all. Only
    /// `auto` does, so `install` and `local` never touch a local disk.
    pub fn asks_the_disk(self) -> bool {
        matches!(self, Intent::Auto)
    }

    fn from_word(w: &str) -> Option<Intent> {
        let w = w.trim();
        if w.eq_ignore_ascii_case("install") {
            Some(Intent::Install)
        } else if w.eq_ignore_ascii_case("local") {
            Some(Intent::Local)
        } else if w.eq_ignore_ascii_case("auto") {
            Some(Intent::Auto)
        } else {
            None
        }
    }
}

/// What the engine's answer amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply<'a> {
    /// The engine stated an intent.
    Stated(Intent),
    /// 404: either this engine has no intent route yet (stormblock#148) or
    /// there is no `boothost/<tag>`. The claim reports the second case on its
    /// own, so the two are not told apart here.
    NotFound,
    /// Any other status.
    Status(u16),
    /// A 2xx whose body stated no intent, or a value this binary does not
    /// know. Carries the raw value when there was one, so the console can
    /// show what arrived.
    Unreadable(Option<&'a str>),
}

impl Reply<'_> {
    /// The intent this boot acts on. See the module header: any doubt is
    /// `auto`.
    pub fn intent(&self) -> Intent {
        match self {
            Reply::Stated(i) => *i,
            _ => Intent::Auto,
        }
    }
}

/// Read the engine's answer to `GET …/boothost/<tag>/intent`.
pub fn from_reply(status: u16, body: &str) -> Reply<'_> {
    if status == 404 {
        return Reply::NotFound;
    }
    if !(200..300).contains(&status) {
        return Reply::Status(status);
    }
    match string_field(body, "intent") {
        Some(v) => match Intent::from_word(v) {
            Some(i) => Reply::Stated(i),
            None => Reply::Unreadable(Some(v)),
        },
        None => Reply::Unreadable(None),
    }
}

/// Take one string field from a flat JSON object without a JSON parser. This
/// is `registry::field` restricted to strings and returning a borrow, since
/// this module has no allocator.
///
/// The needle includes both quotes, so `"intent"` does not match inside
/// `"intents"` or `"boot_intent"`.
fn string_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = body;
    loop {
        let at = rest.find('"')?;
        let after = &rest[at + 1..];
        let close = after.find('"')?;
        let word = &after[..close];
        let tail = &after[close + 1..];
        let t = tail.trim_start();
        if word == key {
            if let Some(v) = t.strip_prefix(':') {
                let v = v.trim_start().strip_prefix('"')?;
                return Some(&v[..v.find('"')?]);
            }
        }
        rest = tail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_stated_intent() {
        assert_eq!(from_reply(200, r#"{"intent":"install"}"#), Reply::Stated(Intent::Install));
        assert_eq!(from_reply(200, r#"{"intent":"local"}"#), Reply::Stated(Intent::Local));
        assert_eq!(from_reply(200, r#"{"intent":"auto"}"#), Reply::Stated(Intent::Auto));
    }

    #[test]
    fn the_owners_rule() {
        // #3, 2026-09-30: install claims, local stays, auto boots a local
        // bootloader when there is one and claims when there is not.
        assert_eq!(decide(Intent::Install, true), Action::Claim);
        assert_eq!(decide(Intent::Install, false), Action::Claim);
        assert_eq!(decide(Intent::Local, true), Action::Local);
        assert_eq!(decide(Intent::Local, false), Action::Local);
        assert_eq!(decide(Intent::Auto, true), Action::Local);
        assert_eq!(decide(Intent::Auto, false), Action::Claim);
        assert!(Intent::Auto.asks_the_disk());
        assert!(!Intent::Install.asks_the_disk());
        assert!(!Intent::Local.asks_the_disk());
    }

    #[test]
    fn doubt_boots_a_bootable_disk_and_claims_on_a_bare_one() {
        // A 404 (forge before #148, an unknown MAC), an engine error, a
        // reply with no intent: each is `auto`, so it follows the disk.
        for r in [from_reply(404, ""), from_reply(503, ""), from_reply(200, "{}")] {
            assert_eq!(decide(r.intent(), true), Action::Local, "{r:?}");
            assert_eq!(decide(r.intent(), false), Action::Claim, "{r:?}");
        }
    }

    #[test]
    fn whitespace_case_and_other_fields() {
        let body = "{\n  \"tag\": \"C2NR0Q2\",\n  \"intent\" : \"Local\",\n  \"updated_at\": 1790000000\n}";
        assert_eq!(from_reply(200, body), Reply::Stated(Intent::Local));
    }

    #[test]
    fn key_is_matched_whole() {
        // A value that happens to read "intent" is not the key, and neither is
        // a longer key that contains it.
        let body = r#"{"note":"intent","boot_intent":"local","intent":"install"}"#;
        assert_eq!(from_reply(200, body), Reply::Stated(Intent::Install));
        assert_eq!(from_reply(200, r#"{"intents":"local"}"#), Reply::Unreadable(None));
    }

    #[test]
    fn doubt_reads_as_auto() {
        let cases = [
            from_reply(404, r#"{"error":"no boothost/C2NR0Q2"}"#),
            from_reply(404, ""),
            from_reply(500, r#"{"intent":"local"}"#),
            from_reply(401, ""),
            from_reply(200, ""),
            from_reply(200, "{}"),
            from_reply(200, r#"{"intent":"reinstall"}"#),
            from_reply(200, r#"{"intent":null}"#),
            from_reply(200, r#"{"intent":"loc"#),
        ];
        for r in cases {
            assert_eq!(r.intent(), Intent::Auto, "{r:?}");
        }
    }

    #[test]
    fn the_engines_own_reply() {
        // stormblock 0e3c47b (#148), `intent_body`: serde_json with sorted
        // keys, `updated_at` a number, `resolved_from` when asked by an alias
        // (here the MAC a machine booted from the default reads under).
        let by_alias = r#"{"host":"server1","intent":"local","resolved_from":"ac1f6b8aa79c","updated_at":1790553600}"#;
        assert_eq!(from_reply(200, by_alias), Reply::Stated(Intent::Local));
        let by_name = r#"{"host":"stormblock1","intent":"install","updated_at":1790553600}"#;
        assert_eq!(from_reply(200, by_name), Reply::Stated(Intent::Install));
        // A host named like the key is still not the key.
        let named_intent = r#"{"host":"intent","intent":"auto","updated_at":0}"#;
        assert_eq!(from_reply(200, named_intent), Reply::Stated(Intent::Auto));
        // An unknown machine: the engine's SynonymError::NotFound.
        let unknown = r#"{"error":"synonym boothost/server9 not found"}"#;
        assert_eq!(from_reply(404, unknown), Reply::NotFound);
    }

    #[test]
    fn a_non_2xx_is_never_trusted() {
        // A 500 whose body names `local` must not keep a machine off its image.
        assert_eq!(from_reply(500, r#"{"intent":"local"}"#), Reply::Status(500));
    }

    #[test]
    fn unknown_values_are_reported() {
        assert_eq!(
            from_reply(200, r#"{"intent":"reinstall"}"#),
            Reply::Unreadable(Some("reinstall"))
        );
    }
}
