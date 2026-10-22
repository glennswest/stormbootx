//! Commands to stormbootx without a BMC (#167): the signed text, the body
//! they come in, the verbs, and the HTTP the API speaks.
//!
//! A command reaches the machine two ways, in one shape: posted to
//! stormbootx's own API (`POST /api/v1/command`, `api.rs`), or read from the
//! engine's host record (`GET …/boothost/<name>/command`, stormblock#494)
//! when the API can't be reached. Either way it is a JSON object
//!
//! ```text
//! {"host":"server8","id":1792000000123,"command":"reboot","args":"",
//!  "issued":"2026-10-22T03:00:00Z","by":"stormcentral","sig":"<128 hex>"}
//! ```
//!
//! and only the signature counts (the owner's choice on #167): an Ed25519
//! signature, by a key compiled in (`manifest::RELEASE_KEYS`, stormcentral's)
//! or named on the media (`command_key =`, the laptop app's), over
//!
//! ```text
//! stormbootx-command v1\n<host>\n<id>\n<command>\n<args>\n<issued>\n
//! ```
//!
//! The first line keeps a signed manifest from ever reading as a command.
//! `host` must be one of this machine's names, and `id` must be above the
//! highest this machine has taken (`StormBootCommandId`, non-volatile): a
//! command is done once, on one machine. `by` is not signed; it is said on
//! the console.
//!
//! `core` and `alloc` only, and nothing from the rest of the crate, so the
//! host tests run with `rustc --edition 2021 --test src/command.rs`. The
//! signature check itself is `api.rs`'s (ed25519-compact).

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The signed text's first line.
pub const MAGIC: &str = "stormbootx-command v1";

/// The longest body a command may have.
pub const BODY_MAX: usize = 4096;

/// A command as it came, before its signature is checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub host: String,
    pub id: u64,
    pub command: String,
    pub args: String,
    pub issued: String,
    pub by: String,
    pub sig: [u8; 64],
}

/// What a command asks.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    /// `ResetSystem(EfiResetCold)`.
    Reboot,
    /// `ResetSystem(EfiResetShutdown)`: ACPI S5.
    Poweroff,
    /// `BootNext = NNNN`, then a reset.
    BootNext(u16),
    /// Claim and boot, whatever the intent says (this boot only).
    Install,
    /// Boot the local disk, nothing claimed (this boot only).
    Local,
    /// Run the boot again from the engine.
    Retry,
    /// Stay in stormbootx, the API up, until another command.
    Hold,
    /// Collect the inventory again.
    Inventory,
    /// The fast wipe of the drive with this serial: its first and last MiB.
    Format(String),
    /// Boot the maintenance image (baremetalservices).
    Maint,
}

impl Verb {
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Reboot => "reboot",
            Verb::Poweroff => "poweroff",
            Verb::BootNext(_) => "boot-next",
            Verb::Install => "install",
            Verb::Local => "local",
            Verb::Retry => "retry",
            Verb::Hold => "hold",
            Verb::Inventory => "inventory",
            Verb::Format(_) => "format",
            Verb::Maint => "maint",
        }
    }
}

/// The bytes the signature is over.
pub fn signed_text(s: &Signed) -> String {
    format!("{MAGIC}\n{}\n{}\n{}\n{}\n{}\n", s.host, s.id, s.command, s.args, s.issued)
}

/// Read a command's body. Every field but `args`, `issued` and `by` must be
/// there; a field with a newline in it is refused (it would let one signed
/// text read as another).
pub fn parse_body(body: &str) -> Result<Signed, String> {
    if body.len() > BODY_MAX {
        return Err(format!("the body is over {BODY_MAX} bytes"));
    }
    let need = |k: &str| json_str(body, k).ok_or_else(|| format!("no \"{k}\""));
    let host = need("host")?;
    let command = need("command")?;
    let sig_hex = need("sig")?;
    let id = json_u64(body, "id").ok_or("no \"id\" (a number)")?;
    let args = json_str(body, "args").unwrap_or_default();
    let issued = json_str(body, "issued").unwrap_or_default();
    let by = json_str(body, "by").unwrap_or_default();
    for (k, v) in [("host", &host), ("command", &command), ("args", &args), ("issued", &issued)] {
        if v.contains('\n') || v.contains('\r') {
            return Err(format!("\"{k}\" has a line break in it"));
        }
    }
    if host.is_empty() {
        return Err("\"host\" is empty".into());
    }
    let mut sig = [0u8; 64];
    if !hex_into(&sig_hex, &mut sig) {
        return Err("\"sig\" is not 128 hex digits".into());
    }
    Ok(Signed { host, id, command, args, issued, by, sig })
}

/// The verb and its arguments.
pub fn verb(command: &str, args: &str) -> Result<Verb, String> {
    let args = args.trim();
    let none = |v: Verb| {
        if args.is_empty() {
            Ok(v)
        } else {
            Err(format!("{command} takes no arguments"))
        }
    };
    match command {
        "reboot" => none(Verb::Reboot),
        "poweroff" => none(Verb::Poweroff),
        "install" => none(Verb::Install),
        "local" => none(Verb::Local),
        "retry" => none(Verb::Retry),
        "hold" => none(Verb::Hold),
        "inventory" => none(Verb::Inventory),
        "maint" => none(Verb::Maint),
        "boot-next" => {
            let ok = args.len() == 4 && args.bytes().all(|c| c.is_ascii_hexdigit());
            match u16::from_str_radix(args, 16) {
                Ok(n) if ok => Ok(Verb::BootNext(n)),
                _ => Err(format!("boot-next takes a Boot#### number (4 hex digits), not {args:?}")),
            }
        }
        "format" => {
            let mut w = args.split_whitespace();
            let serial = w.next().ok_or("format takes the drive's serial")?;
            match (w.next(), w.next()) {
                (None | Some("fast"), None) => Ok(Verb::Format(String::from(serial))),
                (Some(m @ ("full" | "secure")), None) => Err(format!(
                    "format {m} is the maintenance image's (baremetalservices /disks/wipe, \
                     /disks/secure-erase); stormbootx does the fast wipe only"
                )),
                _ => Err(format!("format takes `<serial> [fast]`, not {args:?}")),
            }
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// Whether a command whose signature verified is this machine's, now: its
/// host one of `names` (any case), its id above `floor`.
pub fn for_me(s: &Signed, names: &[String], floor: u64) -> Result<(), String> {
    if !names.iter().any(|n| n.eq_ignore_ascii_case(&s.host)) {
        return Err(format!("it is for {}, and this machine is {}", s.host, names.join(", ")));
    }
    if s.id <= floor {
        return Err(format!("id {} is not above {floor}, the last this machine took", s.id));
    }
    Ok(())
}

// --------------------------------------------------------------------- JSON

/// The string value of a top-level `"key"` (escapes undone), else `None`.
/// Enough for the bodies above: flat objects, no key repeated.
pub fn json_str(body: &str, key: &str) -> Option<String> {
    let v = value_of(body, key)?;
    let b = v.as_bytes();
    if b.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = v[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let h: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}

/// The unsigned integer value of a top-level `"key"`.
pub fn json_u64(body: &str, key: &str) -> Option<u64> {
    let v = value_of(body, key)?;
    let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    v[..end].parse().ok()
}

/// What follows `"key":`, whitespace skipped. Only at depth 1 of the object,
/// outside strings, so a key's name inside a value never matches.
fn value_of<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let b = body.as_bytes();
    let (mut depth, mut i) = (0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let name = body.get(start..j.min(b.len()))?;
                i = j + 1;
                let rest = body.get(i..)?.trim_start();
                if depth == 1 && name == key {
                    if let Some(v) = rest.strip_prefix(':') {
                        return Some(v.trim_start());
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `s` as JSON string contents (no quotes).
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn hex_into(s: &str, out: &mut [u8]) -> bool {
    let b = s.as_bytes();
    if b.len() != out.len() * 2 {
        return false;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (b[2 * i] as char).to_digit(16);
        let lo = (b[2 * i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => *o = (h * 16 + l) as u8,
            _ => return false,
        }
    }
    true
}

/// An Ed25519 public key as 64 hex digits (`command_key =`).
pub fn key_hex(s: &str) -> Option<[u8; 32]> {
    let mut k = [0u8; 32];
    hex_into(s.trim(), &mut k).then_some(k)
}

// --------------------------------------------------------------------- HTTP

/// The most a request may be, headers and body.
pub const REQUEST_MAX: usize = 8192;

/// A request read so far.
#[derive(Debug, PartialEq)]
pub enum Request<'a> {
    /// Not all of it yet.
    Partial,
    /// Not HTTP this serves: answer 400 with this.
    Bad(&'static str),
    Done { method: &'a str, path: &'a str, body: &'a [u8] },
}

/// Read an HTTP/1.x request from `buf` (everything received on the
/// connection). The body is `Content-Length` bytes, none when absent.
pub fn request(buf: &[u8]) -> Request<'_> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() >= REQUEST_MAX { Request::Bad("headers too long") } else { Request::Partial };
    };
    let Ok(head) = core::str::from_utf8(&buf[..end]) else { return Request::Bad("headers not UTF-8") };
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path), Some(version)) = (first.next(), first.next(), first.next()) else {
        return Request::Bad("no request line");
    };
    if !version.starts_with("HTTP/1.") {
        return Request::Bad("not HTTP/1.x");
    }
    let mut length = 0usize;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                match v.trim().parse() {
                    Ok(n) => length = n,
                    Err(_) => return Request::Bad("bad Content-Length"),
                }
            }
        }
    }
    let start = end + 4;
    if start + length > REQUEST_MAX {
        return Request::Bad("request too long");
    }
    if buf.len() < start + length {
        return Request::Partial;
    }
    Request::Done { method, path, body: &buf[start..start + length] }
}

/// A whole response; the connection closes after it.
pub fn response(status: u16, content_type: &str, body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// `{"error":"…"}` with that status.
pub fn error(status: u16, why: &str) -> Vec<u8> {
    response(status, "application/json", format!("{{\"error\":\"{}\"}}", escape(why)).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    const SIG: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\
                       00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn body(host: &str, id: u64, command: &str, args: &str) -> String {
        format!(
            "{{\"host\":\"{host}\",\"id\":{id},\"command\":\"{command}\",\"args\":\"{args}\",\
             \"issued\":\"2026-10-22T03:00:00Z\",\"by\":\"stormcentral\",\"sig\":\"{SIG}\"}}"
        )
    }

    #[test]
    fn the_signed_text_is_the_documents() {
        let s = parse_body(&body("server8", 17, "format", "S3Z9 fast")).unwrap();
        assert_eq!(
            signed_text(&s),
            "stormbootx-command v1\nserver8\n17\nformat\nS3Z9 fast\n2026-10-22T03:00:00Z\n"
        );
        assert_eq!(s.by, "stormcentral");
        assert_eq!(s.sig[1], 0x11);
    }

    #[test]
    fn the_engines_record_reads_the_same() {
        // stormblock#494's record: sorted keys, spaces, other fields.
        let rec = format!(
            "{{ \"args\": \"\", \"by\": \"laptop\", \"command\": \"reboot\", \"host\": \"server8\", \
             \"id\": 1792000000123, \"issued\": \"\", \"sig\": \"{SIG}\", \"state\": {{\"host\": \"x\"}} }}"
        );
        let s = parse_body(&rec).unwrap();
        assert_eq!((s.host.as_str(), s.id, s.command.as_str()), ("server8", 1_792_000_000_123, "reboot"));
    }

    #[test]
    fn a_nested_key_is_not_the_objects() {
        let b = "{\"inner\":{\"host\":\"evil\"},\"note\":\"\\\"host\\\":\\\"evil\\\"\",\"host\":\"good\"}";
        assert_eq!(json_str(b, "host").as_deref(), Some("good"));
        assert_eq!(json_str("{\"a\":\"x\\u0041\\n\"}", "a").as_deref(), Some("xA\n"));
    }

    #[test]
    fn what_a_body_must_carry() {
        assert!(parse_body(&body("", 1, "reboot", "")).is_err());
        assert!(parse_body("{\"host\":\"a\",\"command\":\"reboot\",\"sig\":\"00\"}").is_err());
        let no_id = body("a", 1, "reboot", "").replace("\"id\":1,", "");
        assert_eq!(parse_body(&no_id).unwrap_err(), "no \"id\" (a number)");
        let short_sig = body("a", 1, "reboot", "").replace(SIG, "abcd");
        assert!(parse_body(&short_sig).unwrap_err().contains("128 hex"));
        // A line break would let one signed text read as another.
        let split = body("a\\nb", 1, "reboot", "");
        assert!(parse_body(&split).unwrap_err().contains("line break"));
        assert!(parse_body(&"x".repeat(BODY_MAX + 1)).is_err());
    }

    #[test]
    fn the_verbs() {
        assert_eq!(verb("reboot", ""), Ok(Verb::Reboot));
        assert_eq!(verb("poweroff", " "), Ok(Verb::Poweroff));
        assert!(verb("reboot", "now").is_err());
        assert_eq!(verb("boot-next", "000A"), Ok(Verb::BootNext(10)));
        assert!(verb("boot-next", "A").is_err());
        assert!(verb("boot-next", "+00A").is_err());
        assert_eq!(verb("format", "S3Z9NB0K"), Ok(Verb::Format("S3Z9NB0K".into())));
        assert_eq!(verb("format", "S3Z9NB0K fast"), Ok(Verb::Format("S3Z9NB0K".into())));
        assert!(verb("format", "S3Z9NB0K secure").unwrap_err().contains("baremetalservices"));
        assert!(verb("format", "").is_err());
        assert!(verb("format", "a b c").is_err());
        assert!(verb("rm", "").is_err());
        for v in ["install", "local", "retry", "hold", "inventory", "maint"] {
            assert_eq!(verb(v, "").unwrap().name(), v);
        }
    }

    #[test]
    fn a_command_is_for_one_machine_once() {
        let s = parse_body(&body("Server8", 17, "reboot", "")).unwrap();
        let names = vec!["server8".to_string(), "mac-525400123456".to_string()];
        assert_eq!(for_me(&s, &names, 16), Ok(()));
        assert!(for_me(&s, &names, 17).unwrap_err().contains("not above 17"));
        assert!(for_me(&s, &["server1".to_string()], 0).unwrap_err().contains("for Server8"));
    }

    #[test]
    fn http_requests() {
        assert_eq!(request(b"GET /api/v1/status HTTP/1.1\r\nHost: x\r\n"), Request::Partial);
        assert_eq!(
            request(b"GET /api/v1/status HTTP/1.1\r\nHost: x\r\n\r\n"),
            Request::Done { method: "GET", path: "/api/v1/status", body: b"" }
        );
        let post = b"POST /api/v1/command HTTP/1.1\r\ncontent-length: 4\r\n\r\nab";
        assert_eq!(request(post), Request::Partial);
        let post = b"POST /api/v1/command HTTP/1.1\r\ncontent-length: 4\r\n\r\nabcd";
        assert_eq!(request(post), Request::Done { method: "POST", path: "/api/v1/command", body: b"abcd" });
        assert_eq!(request(b"HELLO\r\n\r\n"), Request::Bad("no request line"));
        assert_eq!(request(b"GET / SPDY\r\n\r\n"), Request::Bad("not HTTP/1.x"));
        assert_eq!(request(&[b'x'; REQUEST_MAX]), Request::Bad("headers too long"));
        let big = b"POST / HTTP/1.1\r\nContent-Length: 99999\r\n\r\n";
        assert_eq!(request(big), Request::Bad("request too long"));
        let r = response(403, "application/json", b"{}");
        assert!(r.starts_with(b"HTTP/1.1 403 Forbidden\r\n"));
        assert!(r.ends_with(b"Content-Length: 2\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}"));
        assert_eq!(error(400, "a \"b\""), response(400, "application/json", b"{\"error\":\"a \\\"b\\\"\"}"));
    }

    #[test]
    fn command_keys() {
        assert_eq!(key_hex(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(key_hex(" 00 "), None);
        assert_eq!(key_hex(&"zz".repeat(32)), None);
    }
}
