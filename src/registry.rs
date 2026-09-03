//! Claiming an image from sbregistry, keyed on the service tag.
//!
//! HTTP is spoken directly over TCP4 rather than through EFI_HTTP_PROTOCOL.
//! One request is a hundred lines; EFI_HTTP is a whole driver stack that
//! firmware may not carry, and the NVMe path needs TCP4 regardless — so this
//! keeps the extension to exactly one protocol dependency instead of two.
//!
//! The response parser is deliberately not a JSON parser. The claim reply is a
//! small flat object from a service we control, and a boot-critical binary is
//! the wrong place to grow a parser for it.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::tcp4::Tcp4Socket;

/// Everything needed to attach, as sbregistry reports it.
///
/// Taken from the **response**, never assumed from the request: older
/// stormblockmk ignores the requested protocol and exports iSCSI regardless,
/// so a client that assumed its own request had been honoured would attach
/// nothing and blame the network.
#[derive(Debug, Clone)]
pub struct Attach {
    pub address: [u8; 4],
    pub port: u16,
    pub nqn: String,
    pub nsid: u32,
}

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.trim().split('.');
    for slot in out.iter_mut() {
        *slot = parts.next()?.parse::<u8>().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Pull one field out of a flat JSON object.
fn field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = body.find(&needle)? + needle.len();
    let rest = body[at..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    if let Some(s) = rest.strip_prefix('"') {
        let end = s.find('"')?;
        Some(s[..end].to_string())
    } else {
        let end = rest.find([',', '}', '\n'])?;
        let v = rest[..end].trim();
        (!v.is_empty()).then(|| v.to_string())
    }
}

fn request(
    server: [u8; 4],
    port: u16,
    host: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let mut sock = Tcp4Socket::connect(server, port)?;

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    req.push_str("User-Agent: stormbootx\r\nAccept: application/json\r\nConnection: close\r\n");
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }

    sock.send(req.as_bytes())?;
    let raw = sock.read_to_end(256 * 1024)?;
    String::from_utf8(raw).map_err(|_| "response was not UTF-8".to_string())
}

fn split_response(response: &str) -> Result<(u16, &str), String> {
    let status = response
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("no HTTP status line")?;
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
    Ok((status, body))
}

/// The namespace holding one synonym per machine, keyed on its service tag.
///
/// Not a per-network or per-cluster name: the service tag identifies the
/// chassis, so `boothost/<tag>` means the same machine wherever it is plugged
/// in. See `docs/BOOT.md` in stormcos.
pub const BOOTHOST_NS: &str = "boothost";

/// Claim this machine's image from the storage engine, keyed on its service tag.
///
/// `POST /api/v1/synonyms/boothost/<tag>/claim`. **One request, and the answer
/// is bootable.** The reply carries a copy-on-write clone of whatever golden
/// the fleet says this box runs *and* the tuple that reaches it — address,
/// port, NQN and NSID. A claim that returned only a volume id would leave
/// firmware knowing a volume exists and still having to ask where, which is a
/// second round trip from a client whose whole state machine is "get an
/// address, attach it, boot", and a window in which the claim is held and
/// nothing is being served.
///
/// The body is `{}` rather than omitted: the endpoint takes options this client
/// has no use for, and a POST with no body reads as a malformed request to more
/// than one HTTP stack.
///
/// Which image that is stays a fleet decision made next to the images — moving
/// this machine is a `PUT` on its synonym, not a visit to the machine.
pub fn claim_boothost(
    server: [u8; 4],
    port: u16,
    host: &str,
    service_tag: &str,
) -> Result<Attach, String> {
    let path = format!("/api/v1/synonyms/{BOOTHOST_NS}/{service_tag}/claim");
    let response = request(server, port, host, "POST", &path, Some("{}"))?;
    let (status, body) = split_response(&response)?;
    if status == 404 {
        // Worth separating from every other failure: it is not a fault, it is
        // this machine having no image assigned yet, and the console line that
        // says so is the one that tells an operator what to do about it.
        return Err(format!("no {BOOTHOST_NS}/{service_tag} synonym on this engine"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("claim returned HTTP {status}: {}", body.trim()));
    }
    attach_from(body)
}

/// Claim an image for this machine.
///
/// The service tag is the `consumer`, which is exactly what that field is for
/// — sbregistry sets it late precisely so a warm clone can be bound to whoever
/// turns out to need it. It also makes `GET /v1/clones?consumer=<tag>` the
/// answer to "what is this machine booting?".
pub fn claim(
    server: [u8; 4],
    port: u16,
    host: &str,
    golden: &str,
    service_tag: &str,
) -> Result<Attach, String> {
    let body = format!("{{\"golden\":\"{golden}\",\"consumer\":\"{service_tag}\"}}");
    let response = request(server, port, host, "POST", "/v1/clones/claim", Some(&body))?;
    let (status, body) = split_response(&response)?;
    if !(200..300).contains(&status) {
        return Err(format!("claim returned HTTP {status}: {}", body.trim()));
    }
    attach_from(body)
}

/// Look for a clone this machine already holds, so a reboot reattaches the
/// same volume instead of minting another.
pub fn existing(
    server: [u8; 4],
    port: u16,
    host: &str,
    service_tag: &str,
) -> Result<Option<Attach>, String> {
    let path = format!("/v1/clones?consumer={service_tag}");
    let response = request(server, port, host, "GET", &path, None)?;
    let (status, body) = split_response(&response)?;
    if !(200..300).contains(&status) {
        return Err(format!("lookup returned HTTP {status}"));
    }
    if body.trim() == "[]" || body.trim().is_empty() {
        return Ok(None);
    }
    attach_from(body).map(Some)
}

/// Read an attach out of a response, in either spelling.
///
/// sbregistry answers `address`/`port`; stormblock answers `traddr`/`trsvcid`
/// inside an `addresses` array (`mgmt/api/v1.rs`, `AttachInfo::NvmeTcp`). Both
/// are accepted rather than one being chosen, because the alternative is a
/// boot path that fails on a field name — and the two ends of this are moving
/// independently right now.
///
/// The nesting costs nothing: `field` scans for `"key"` with its closing quote,
/// so it reads `traddr` out of the array without a JSON parser, and `"address"`
/// does not match inside `"addresses"`.
fn attach_from(body: &str) -> Result<Attach, String> {
    let address = field(body, "address")
        .or_else(|| field(body, "traddr"))
        .and_then(|a| parse_ipv4(&a))
        .ok_or("no usable \"address\" or \"traddr\" in the response")?;
    let nqn = field(body, "nqn").ok_or("no \"nqn\" in the response")?;
    let port = field(body, "port")
        .or_else(|| field(body, "trsvcid"))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(4420);
    let nsid = field(body, "nsid")
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(1);
    Ok(Attach {
        address,
        port,
        nqn,
        nsid,
    })
}

/// Everything after the last `/`, for logging a digest without the noise.
// Unused until there are digests to log — see #2 and #4.
#[allow(dead_code)]
pub fn short(s: &str) -> &str {
    s.rsplit(['/', ':']).next().unwrap_or(s)
}

#[allow(dead_code)]
pub fn to_vec(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}
