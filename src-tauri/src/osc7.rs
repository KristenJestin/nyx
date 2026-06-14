//! OSC 7 (current working directory) parsing — the PORTABLE cwd source.
//!
//! On Linux nyx reads the live cwd from `/proc/<pid>/cwd` (see [`crate::proc`]).
//! That anchor does not exist on Windows/macOS, so the portable source is the
//! shell's **OSC 7** escape: a well-behaved shell emits, after every prompt,
//!
//! ```text
//! ESC ] 7 ; file://<host>/<path> BEL
//! ```
//!
//! (the terminator may be `BEL` = `0x07` or the 2-byte `ST` = `ESC \`). The
//! payload is a `file://` URI whose path is percent-encoded. We parse it on the
//! bridge side and feed the NORMALIZED cwd into the SAME resolver as `/proc`, so
//! auto-attach is platform-agnostic above the provider layer.
//!
//! This module is pure (no IO): it takes a byte/str slice and yields the decoded
//! filesystem path, so it is unit-tested without a terminal. The terminal/bridge
//! layer is responsible for spotting the OSC 7 sequence in the PTY byte stream
//! and handing the inner payload here.

/// Decode the PATH out of an OSC 7 `file://` payload (the part between
/// `ESC ] 7 ;` and the terminator). Returns the decoded, host-stripped
/// filesystem path, or `None` if the payload is not a usable `file://` cwd URI.
///
/// Handled:
/// - `file://host/path` and `file:///path` (empty/`localhost` host dropped).
/// - Percent-decoding of the path (`%20` → space, `%C3%A9` → `é`, …).
/// - Windows drive paths: `file:///C:/Users/...` → `C:\Users\...` style input
///   (we return `C:/Users/...`; the caller normalizes separators/case).
///
/// We DO NOT normalize here (that is [`crate::pathnorm`]'s job, applied uniformly
/// to every provider's output) — we only decode the URI to a raw path string.
pub fn parse_file_uri(payload: &str) -> Option<String> {
    let payload = payload.trim();
    let rest = payload.strip_prefix("file://")?;

    // Split off the authority (host) from the path: everything up to the first
    // `/` is the host. `file:///path` ⇒ empty host; `file://host/path` ⇒ host.
    let (_host, path_part) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        // `file://host` with no path is not a usable cwd.
        None => return None,
    };

    let decoded = percent_decode(path_part);

    // On a Windows-style URI the path is `/C:/Users/...`; strip the leading `/`
    // so it becomes a drive path `C:/Users/...`. Detect the `/<drive>:` shape.
    let cleaned = strip_windows_drive_slash(&decoded);

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// If `s` looks like `/<letter>:/...` (a URI-encoded Windows drive path), drop
/// the leading slash so it reads as `C:/...`. Otherwise return `s` unchanged.
fn strip_windows_drive_slash(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        s[1..].to_string()
    } else {
        s.to_string()
    }
}

/// Percent-decode a URI path component. Invalid/incomplete `%xx` escapes are
/// left verbatim (defensive: never panics on malformed input). Decodes UTF-8
/// byte sequences (`%C3%A9` → `é`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = hex_val(bytes[i + 1]);
            let l = hex_val(bytes[i + 2]);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Scan a raw PTY byte chunk for the LAST complete OSC 7 sequence and return its
/// decoded path. A shell emits OSC 7 after every prompt, so when a chunk carries
/// several we want the most recent (the current cwd). Returns `None` if the
/// chunk contains no complete OSC 7.
///
/// Recognized framing: `ESC ] 7 ;` … terminator, where the terminator is `BEL`
/// (`0x07`) or `ST` (`ESC \`, i.e. `0x1b 0x5c`). The payload between them is
/// handed to [`parse_file_uri`].
pub fn extract_last_cwd(chunk: &[u8]) -> Option<String> {
    const INTRO: &[u8] = b"\x1b]7;";
    let mut search_from = 0;
    let mut last: Option<String> = None;
    while let Some(rel) = find_subslice(&chunk[search_from..], INTRO) {
        let start = search_from + rel + INTRO.len();
        // Find the terminator: BEL or ST (ESC \).
        let mut end = None;
        let mut j = start;
        while j < chunk.len() {
            if chunk[j] == 0x07 {
                end = Some((j, j + 1));
                break;
            }
            if chunk[j] == 0x1b && j + 1 < chunk.len() && chunk[j + 1] == b'\\' {
                end = Some((j, j + 2));
                break;
            }
            j += 1;
        }
        match end {
            Some((payload_end, after)) => {
                let payload = &chunk[start..payload_end];
                if let Ok(s) = std::str::from_utf8(payload) {
                    if let Some(path) = parse_file_uri(s) {
                        last = Some(path);
                    }
                }
                search_from = after;
            }
            // Incomplete sequence (no terminator yet in this chunk): stop.
            None => break,
        }
    }
    last
}

/// First index of `needle` in `haystack`, or `None`. Small, allocation-free.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unix_file_uri_with_empty_host() {
        assert_eq!(
            parse_file_uri("file:///home/kris/work"),
            Some("/home/kris/work".to_string())
        );
    }

    #[test]
    fn parses_file_uri_with_localhost_host() {
        // `file://localhost/path` and `file://host/path` both keep the path.
        assert_eq!(
            parse_file_uri("file://localhost/home/kris/work"),
            Some("/home/kris/work".to_string())
        );
        assert_eq!(
            parse_file_uri("file://myhost/srv/data"),
            Some("/srv/data".to_string())
        );
    }

    #[test]
    fn percent_decodes_path() {
        assert_eq!(
            parse_file_uri("file:///home/kris/my%20work"),
            Some("/home/kris/my work".to_string())
        );
        // UTF-8 multibyte: %C3%A9 = é
        assert_eq!(
            parse_file_uri("file:///home/caf%C3%A9"),
            Some("/home/café".to_string())
        );
    }

    #[test]
    fn windows_drive_uri_strips_leading_slash() {
        // `file:///C:/Users/Kris/Work` → `C:/Users/Kris/Work` (caller normalizes).
        assert_eq!(
            parse_file_uri("file:///C:/Users/Kris/Work"),
            Some("C:/Users/Kris/Work".to_string())
        );
        // With a host part it still works.
        assert_eq!(
            parse_file_uri("file://localhost/C:/proj"),
            Some("C:/proj".to_string())
        );
    }

    #[test]
    fn rejects_non_file_uri_and_empty() {
        assert_eq!(parse_file_uri("http://example.com/x"), None);
        assert_eq!(parse_file_uri(""), None);
        assert_eq!(parse_file_uri("file://host"), None); // no path
        assert_eq!(parse_file_uri("not a uri"), None);
    }

    #[test]
    fn malformed_percent_escape_is_left_verbatim() {
        // A trailing `%` or non-hex escape must not panic; left as-is.
        assert_eq!(
            parse_file_uri("file:///home/a%"),
            Some("/home/a%".to_string())
        );
        assert_eq!(
            parse_file_uri("file:///home/a%zz"),
            Some("/home/a%zz".to_string())
        );
    }

    #[test]
    fn extracts_osc7_with_bel_terminator() {
        let chunk = b"prompt\x1b]7;file:///home/kris/work\x07$ ";
        assert_eq!(extract_last_cwd(chunk), Some("/home/kris/work".to_string()));
    }

    #[test]
    fn extracts_osc7_with_st_terminator() {
        let chunk = b"\x1b]7;file:///srv/data\x1b\\rest";
        assert_eq!(extract_last_cwd(chunk), Some("/srv/data".to_string()));
    }

    #[test]
    fn extracts_the_last_of_several_osc7() {
        // Two prompts in one chunk: the most recent cwd wins.
        let chunk = b"\x1b]7;file:///first\x07out\x1b]7;file:///second/dir\x07$ ";
        assert_eq!(extract_last_cwd(chunk), Some("/second/dir".to_string()));
    }

    #[test]
    fn ignores_incomplete_osc7() {
        // No terminator yet: nothing to extract (wait for more bytes).
        let chunk = b"\x1b]7;file:///home/kris/wo";
        assert_eq!(extract_last_cwd(chunk), None);
    }

    #[test]
    fn no_osc7_yields_none() {
        assert_eq!(extract_last_cwd(b"just some output\r\n"), None);
    }
}
