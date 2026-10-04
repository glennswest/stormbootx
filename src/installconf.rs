//! `install-config.yaml` from the boot media to the node (#79, stormcos#82).
//!
//! storminstall writes the file into the boot ISO itself, at
//! `\stormboot\install-config.yaml` on the media's ESP (storminstall
//! `docs/config-slot.md`). stormbootx reads it there and hands it down to
//! Linux in **volatile** EFI variables under #76's vendor GUID, the way
//! `StormBootTag` goes down; the initramfs copies it to
//! `/state/config/install-config.yaml` on a first boot.
//!
//! One variable cannot carry it: EDK2's default `PcdMaxVariableSize` is
//! 1 KiB, header and name included, and the file may be 256 KiB. So:
//!
//! | variable | value |
//! |---|---|
//! | `StormBootInstallConfig0` … `StormBootInstallConfig<N-1>` | the file's bytes, [`CHUNK`] each (the last one shorter) |
//! | `StormBootInstallConfig` | `v1:<length>:<N>:<sha256, lower-case hex>` |
//!
//! The header is set **last**, and a chunk that fails takes the ones already
//! set with it, so a header means every chunk is there. A reader concatenates
//! the `N` chunks, and uses the result only if its length and SHA-256 match.
//!
//! Same constraints as `sntp.rs` and the rest: `core` only and no `crate::`
//! item, so the tests run on the host:
//!
//! ```text
//! rustc --edition 2021 --test src/installconf.rs -o t/installconf-test
//! ```

/// Where storminstall writes the file on the media's ESP.
pub const PATH: &str = r"\stormboot\install-config.yaml";

/// storminstall's cap. A real file is about 1 KiB.
pub const MAX: usize = 256 * 1024;

/// Bytes per chunk variable: under EDK2's 1 KiB default variable size once
/// its header (~60 bytes) and the UTF-16 name (up to 52 bytes) are counted.
pub const CHUNK: usize = 768;

/// The header variable's name, and the chunks' prefix.
pub const NAME: &str = "StormBootInstallConfig";

/// How many chunk variables `len` bytes take.
pub fn chunks(len: usize) -> usize {
    len.div_ceil(CHUNK)
}

/// Why a file on the media is not handed down.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    Empty,
    TooLarge(usize),
}

/// Whether a file of `len` bytes is handed down at all.
pub fn accept(len: usize) -> Result<(), Refused> {
    match len {
        0 => Err(Refused::Empty),
        n if n > MAX => Err(Refused::TooLarge(n)),
        _ => Ok(()),
    }
}

/// A `core::fmt::Write` over a fixed buffer.
struct Buf<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl core::fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.at + s.len();
        if end > self.out.len() {
            return Err(core::fmt::Error);
        }
        self.out[self.at..end].copy_from_slice(s.as_bytes());
        self.at = end;
        Ok(())
    }
}

fn format<'a>(out: &'a mut [u8], args: core::fmt::Arguments) -> Option<&'a str> {
    let mut b = Buf { out, at: 0 };
    core::fmt::write(&mut b, args).ok()?;
    let at = b.at;
    core::str::from_utf8(&b.out[..at]).ok()
}

/// The name of chunk `i`: `StormBootInstallConfig<i>`.
pub fn chunk_name(i: usize, out: &mut [u8; 40]) -> &str {
    format(out, format_args!("{NAME}{i}")).unwrap_or(NAME)
}

/// The header's value for a file of `len` bytes with SHA-256 `sha256_hex`.
pub fn header<'a>(len: usize, sha256_hex: &str, out: &'a mut [u8; 128]) -> Option<&'a str> {
    format(out, format_args!("v1:{len}:{}:{sha256_hex}", chunks(len)))
}

/// A header read back. The reader's half: tcp4probe and the tests use it,
/// and it is the initramfs's contract; stormbootx itself only writes.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub struct Header<'a> {
    pub len: usize,
    pub chunks: usize,
    pub sha256: &'a str,
}

/// Read a header back. Anything but a `v1` header whose numbers agree with
/// each other and with [`MAX`] is `None`.
#[allow(dead_code)]
pub fn parse_header(s: &str) -> Option<Header<'_>> {
    let mut parts = s.trim_end_matches(['\0', '\n']).split(':');
    if parts.next()? != "v1" {
        return None;
    }
    let len: usize = parts.next()?.parse().ok()?;
    let n: usize = parts.next()?.parse().ok()?;
    let sha256 = parts.next()?;
    if parts.next().is_some() || accept(len).is_err() || n != chunks(len) {
        return None;
    }
    let hex = sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    hex.then_some(Header { len, chunks: n, sha256 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b";

    #[test]
    fn chunk_counts() {
        assert_eq!(chunks(0), 0);
        assert_eq!(chunks(1), 1);
        assert_eq!(chunks(CHUNK), 1);
        assert_eq!(chunks(CHUNK + 1), 2);
        assert_eq!(chunks(MAX), 342);
    }

    #[test]
    fn names() {
        let mut b = [0u8; 40];
        assert_eq!(chunk_name(0, &mut b), "StormBootInstallConfig0");
        let mut b = [0u8; 40];
        assert_eq!(chunk_name(341, &mut b), "StormBootInstallConfig341");
    }

    #[test]
    fn the_chunk_variable_fits_edk2s_default() {
        // VARIABLE_HEADER (authenticated, 60 bytes) + the longest UTF-16
        // name with its NUL + a full chunk, against PcdMaxVariableSize 0x400.
        let mut b = [0u8; 40];
        let longest = chunk_name(chunks(MAX) - 1, &mut b).len();
        assert!(60 + 2 * (longest + 1) + CHUNK <= 0x400);
    }

    #[test]
    fn the_header_round_trips() {
        let mut b = [0u8; 128];
        let h = header(1500, D, &mut b).unwrap();
        assert_eq!(h, "v1:1500:2:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b");
        assert_eq!(parse_header(h), Some(Header { len: 1500, chunks: 2, sha256: D }));
    }

    #[test]
    fn the_header_only_says_what_stormbootx_does() {
        // A value made with the other names is still handoff-safe: every
        // character is in [A-Za-z0-9._:-].
        let mut b = [0u8; 128];
        let h = header(MAX, D, &mut b).unwrap();
        assert!(h.bytes().all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)));
        // efivarfs readers may see a trailing NUL or newline.
        let mut b2 = [0u8; 128];
        let h2 = header(10, D, &mut b2).unwrap();
        let mut with_nl = [0u8; 128];
        with_nl[..h2.len()].copy_from_slice(h2.as_bytes());
        with_nl[h2.len()] = b'\n';
        let s = core::str::from_utf8(&with_nl[..h2.len() + 1]).unwrap();
        assert!(parse_header(s).is_some());
    }

    #[test]
    fn a_header_that_disagrees_is_refused() {
        let bad = [
            "",
            "v2:10:1:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
            "v1:10:2:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
            "v1:0:0:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
            "v1:262145:342:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
            "v1:10:1:6B86B273FF34FCE19D6B804EFF5A3F5747ADA4EAA22F1D49C01E52DDB7875B4B",
            "v1:10:1:6b86b273",
            "v1:10:1:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b:x",
            "v1:-1:1:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
        ];
        for h in bad {
            assert_eq!(parse_header(h), None, "{h}");
        }
    }

    #[test]
    fn what_is_handed_down() {
        assert_eq!(accept(0), Err(Refused::Empty));
        assert_eq!(accept(1), Ok(()));
        assert_eq!(accept(MAX), Ok(()));
        assert_eq!(accept(MAX + 1), Err(Refused::TooLarge(MAX + 1)));
    }
}
