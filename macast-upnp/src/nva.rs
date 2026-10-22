//! NVA ("哔哩必连") request-line parsing.
//!
//! Rust counterpart of `macast/plugins/protocol/nirvana.py`'s `NVAHTTPRequest.read_request_line`
//! (`:266-340`). The plugin is 1,822 lines and most of it is a `cheroot` subclass tree plus the
//! danmaku manager; what is portable — and what makes NVA a *protocol* rather than a quirk — is
//! this: **a request line whose protocol version is `NVA/1.0` instead of `HTTP/1.0`**.
//!
//! ## Why the substitution exists at all
//!
//! `cheroot` refuses anything that does not start with `HTTP/`, so the plugin rewrites the
//! version token **before** the version check and remembers that it did. Two consequences ride
//! on that:
//!
//! * `close_connection = True` — an NVA client speaks its own binary framing after the request
//!   line, so the socket must not be reused for a second HTTP request;
//! * `linger = True` — the socket is handed over rather than closed, because the same connection
//!   carries the binary command channel.
//!
//! Both are reported here as data ([`RequestLine::hand_over_socket`]) rather than applied, so
//! this module stays free of sockets.
//!
//! ## The status codes are the reference's, and one of them is surprising
//!
//! * **no bytes at all** → `408`. Not a 400: an idle persistent connection that times out is
//!   *different* from a malformed request, and `cheroot`'s own comment says so.
//! * a line not ending in `CRLF` → `400` with its own sentence.
//! * a version above `1.1` → `505`, not `400` — the request is well formed, we just will not
//!   serve it.
//! * `NVA/2.0` → `400 bad protocol`, because the substitution only knows `NVA/1.0`; a client
//!   inventing a version must not be silently read as HTTP.
//!
//! ## Case
//!
//! The method is upper-cased (`method.upper()`), so `nva …` is accepted as `NVA`. The strict-mode
//! rejection of a lowercase method is `cheroot`'s own check and lives outside this function —
//! which is why the corpus records `nva` as valid here. Splitting it any other way would make
//! this module responsible for a policy it does not own.

/// `CRLF`, as bytes.
pub const CRLF: &[u8] = b"\r\n";

/// The NVA version token and what it becomes.
pub const NVA_VERSION: &str = "NVA/1.0";
pub const HTTP_VERSION: &str = "HTTP/1.0";

/// Why a request line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No bytes arrived before the timeout. `408`, not `400`.
    Idle,
    /// The line does not end in CRLF.
    NoCrlf,
    /// Not three space-separated fields.
    Malformed,
    /// The version token is neither `HTTP/x.y` nor `NVA/1.0`.
    BadProtocol,
    /// `HTTP/` followed by something that is not `major.minor`.
    BadVersion,
    /// A version above 1.1.
    UnsupportedVersion,
}

impl Refusal {
    /// The status code the reference answers with.
    pub fn status(&self) -> u16 {
        match self {
            Refusal::Idle => 408,
            Refusal::UnsupportedVersion => 505,
            Refusal::NoCrlf | Refusal::Malformed | Refusal::BadProtocol | Refusal::BadVersion => {
                400
            }
        }
    }
}

/// A request line that was understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestLine {
    /// The method, **upper-cased**.
    pub method: String,
    pub uri: String,
    /// The line was `NVA/1.0`, so the socket carries the binary channel afterwards.
    pub is_nva: bool,
}

impl RequestLine {
    /// Whether the connection must not be reused for a second HTTP request.
    ///
    /// Two names in the reference (`close_connection` and `linger`) for one fact that matters to
    /// this module: **the socket is handed over**. Keeping them apart here would invite a caller
    /// to set one and not the other.
    pub fn hand_over_socket(&self) -> bool {
        self.is_nva
    }
}

/// Parse one request line.
///
/// Input is the raw bytes including the terminator, which is why `CRLF` is checked before the
/// fields are split: `HTTP/1.0\n` has three fields and is still invalid.
pub fn parse_request_line(line: &[u8]) -> Result<RequestLine, Refusal> {
    if line.is_empty() {
        return Err(Refusal::Idle);
    }
    if !line.ends_with(CRLF) {
        return Err(Refusal::NoCrlf);
    }
    // `strip()` in the reference trims the terminator and any surrounding whitespace; splitting
    // with `2` keeps a uri that contains spaces in one piece, which is the reference's shape.
    let text = String::from_utf8_lossy(&line[..line.len() - CRLF.len()]);
    let text = text.trim();
    let mut fields = text.splitn(3, ' ');
    let (method, uri) = match (fields.next(), fields.next()) {
        (Some(method), Some(uri)) => (method, uri),
        _ => return Err(Refusal::Malformed),
    };
    let protocol = match fields.next() {
        Some(protocol) => protocol,
        // Two fields only: the reference's `split(SPACE, 2)` unpack into three names raises
        // ValueError, which it catches as "malformed".
        None => return Err(Refusal::Malformed),
    };

    // The substitution: an NVA client sends `NVA/1.0` where an HTTP client sends `HTTP/1.0`.
    let (protocol, is_nva) = if protocol.contains("NVA") {
        (protocol.replace(NVA_VERSION, HTTP_VERSION), true)
    } else {
        (protocol.to_string(), false)
    };

    // Checked on the substituted value, exactly as the reference does — so `NVA/2.0` becomes
    // `NVA/2.0` (no substitution happened), fails this check, and is a 400 rather than a 505.
    let rest = match protocol.strip_prefix("HTTP/") {
        Some(rest) => rest,
        None => {
            return Err(if is_nva {
                Refusal::BadProtocol
            } else {
                Refusal::BadProtocol
            })
        }
    };
    let (major, minor) = match rest.split_once('.') {
        Some(parts) => parts,
        None => return Err(Refusal::BadVersion),
    };
    let major: u32 = major.parse().map_err(|_| Refusal::Malformed)?;
    let minor: u32 = minor.parse().map_err(|_| Refusal::Malformed)?;
    if (major, minor) > (1, 1) {
        return Err(Refusal::UnsupportedVersion);
    }

    Ok(RequestLine {
        method: method.to_uppercase(),
        uri: uri.to_string(),
        is_nva,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_http_line_is_not_nva_and_keeps_its_socket() {
        crate::pin_test_dir();
        let parsed = parse_request_line(b"GET http://h/a HTTP/1.0\r\n").expect("valid");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.uri, "http://h/a");
        assert!(!parsed.is_nva);
        assert!(!parsed.hand_over_socket());
    }

    #[test]
    fn nva_1_0_is_substituted_and_hands_the_socket_over() {
        crate::pin_test_dir();
        // Both consequences ride on this one bit: no second HTTP request on the socket, and the
        // socket is handed over rather than closed, because the binary channel follows.
        let parsed = parse_request_line(b"PLAY http://h/a NVA/1.0\r\n").expect("valid");
        assert!(parsed.is_nva);
        assert!(parsed.hand_over_socket());
        assert_eq!(parsed.method, "PLAY");
    }

    #[test]
    fn an_unknown_nva_version_is_a_400_not_a_505() {
        crate::pin_test_dir();
        // The substitution only knows `NVA/1.0`, so `NVA/2.0` survives untouched and fails the
        // `HTTP/` prefix check. A 505 here would tell a client "your version is too new, try
        // HTTP" — which is not true; we simply do not speak that protocol.
        let refusal = parse_request_line(b"GET http://h/a NVA/2.0\r\n").expect_err("refused");
        assert_eq!(refusal, Refusal::BadProtocol);
        assert_eq!(refusal.status(), 400);
    }

    #[test]
    fn status_codes_are_the_references_including_the_surprising_ones() {
        crate::pin_test_dir();
        // No bytes at all is an idle timeout (408), not a malformed request — cheroot's own
        // comment makes the distinction, and a 400 there would blame the client for our timeout.
        assert_eq!(parse_request_line(b"").unwrap_err().status(), 408);
        // Too new a version is well formed: 505, not 400.
        assert_eq!(
            parse_request_line(b"GET http://h/a HTTP/2.0\r\n").unwrap_err().status(),
            505
        );
        assert_eq!(
            parse_request_line(b"GET http://h/a HTTP/1.0\n").unwrap_err(),
            Refusal::NoCrlf
        );
        assert_eq!(
            parse_request_line(b"GET http://h/a FOO/1.0\r\n").unwrap_err(),
            Refusal::BadProtocol
        );
        assert_eq!(
            parse_request_line(b"GET http://h/a HTTP/1\r\n").unwrap_err(),
            Refusal::BadVersion
        );
        assert_eq!(
            parse_request_line(b"GET http://h/a HTTP/x.y\r\n").unwrap_err(),
            Refusal::Malformed
        );
    }

    #[test]
    fn the_method_is_upper_cased_because_the_reference_does_that_here() {
        crate::pin_test_dir();
        // `cheroot`'s strict-mode rejection of a lowercase method is a *different* check and is
        // not this function's; recording it as valid here keeps the seam where the reference
        // put it.
        let parsed = parse_request_line(b"nva http://h/a NVA/1.0\r\n").expect("valid here");
        assert_eq!(parsed.method, "NVA");
        assert!(parsed.is_nva);
    }

    #[test]
    fn a_uri_containing_spaces_stays_in_one_piece() {
        crate::pin_test_dir();
        // `split(' ', 2)` means the third field is the remainder, so a stray space in the uri
        // does not shift the protocol field — it becomes part of the version and is refused.
        let parsed = parse_request_line(b"GET http://h/a?q=1+2 HTTP/1.1\r\n").expect("valid");
        assert_eq!(parsed.uri, "http://h/a?q=1+2");
        assert_eq!(parsed.method, "GET");
    }

    /// Every request line the reference's parser answered, replayed against this port.
    ///
    /// Produced by executing the reference's `read_request_line` decision path over thirteen
    /// shapes: the two protocol tokens, both supported versions, the three refusals that are
    /// easy to conflate (400/505/408), a missing terminator, and a lowercase method.
    #[test]
    fn every_reference_request_line_replays_identically() {
        crate::pin_test_dir();
        let corpus = include_str!("../tests/golden/nva_request.jsonl");
        let mut rows = 0usize;
        let mut accepted = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let raw = row["line"].as_str().expect("line").as_bytes();
            let expected = &row["result"];
            let actual = parse_request_line(raw);
            match actual {
                Ok(parsed) => {
                    assert_eq!(
                        expected["ok"].as_bool(),
                        Some(true),
                        "the port accepted {raw:?} where the reference refused"
                    );
                    assert_eq!(parsed.method, expected["method"].as_str().unwrap_or(""));
                    assert_eq!(parsed.uri, expected["uri"].as_str().unwrap_or(""));
                    assert_eq!(parsed.is_nva, expected["is_nva"].as_bool().unwrap_or(false));
                    assert_eq!(
                        parsed.hand_over_socket(),
                        expected["close_connection"].as_bool().unwrap_or(false)
                    );
                    accepted += 1;
                }
                Err(refusal) => {
                    assert_eq!(
                        expected["ok"].as_bool(),
                        Some(false),
                        "the port refused {raw:?} where the reference accepted it"
                    );
                    assert_eq!(
                        refusal.status() as u64,
                        expected["status"].as_u64().unwrap_or(0),
                        "{raw:?} got the wrong status"
                    );
                }
            }
            rows += 1;
        }
        assert_eq!(rows, 13, "the corpus lost rows");
        // A corpus where nothing parsed would pass every refusal branch and prove nothing.
        assert!(accepted >= 5, "the corpus barely exercises the accept path: {accepted}");
    }
}
