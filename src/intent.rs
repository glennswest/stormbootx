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
//! | intent    | this binary                                   |
//! |-----------|-----------------------------------------------|
//! | `install` | claims and boots the image                    |
//! | `local`   | falls through to the disk; no claim, no clone |
//! | `auto`    | today's behaviour: claims and boots           |
//!
//! **Any doubt reads as `auto`.** A 404, an engine without the route, an
//! unreachable engine, a body with no intent or one this binary does not know:
//! each is reported and then treated as `auto`, which is what every boot did
//! before intents existed. Only an explicit `local` skips the network image.
//! Reading it the other way round would let a failure keep a machine off an
//! install it was asked for, or keep an uninstalled one off the only image it
//! can boot.
//!
//! `auto` means what it meant before intents existed. The owner's rule is that
//! an installed node boots local unless there is a new golden **and** an
//! install was requested (#11, 2026-09-24). Applying that needs #3 to tell an
//! installed, current disk apart, and #3 waits on stormcos#30. Until then
//! `auto` keeps claiming.
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
    /// Decide for itself; today, that means claim and boot.
    Auto,
}

impl Intent {
    pub fn name(self) -> &'static str {
        match self {
            Intent::Install => "install",
            Intent::Local => "local",
            Intent::Auto => "auto",
        }
    }

    /// Does this boot claim an image from the engine?
    pub fn claims(self) -> bool {
        !matches!(self, Intent::Local)
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
    fn only_local_skips_the_claim() {
        assert!(Intent::Install.claims());
        assert!(Intent::Auto.claims());
        assert!(!Intent::Local.claims());
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
