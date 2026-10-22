//! The local-file HTTP face: which name is served, and with which headers.
//!
//! Rust counterpart of `cast_local_file.py`'s `Store` / `Entry` / `do_GET` / `_headers`
//! (`:680-880`). [`crate::relay`] already owns `parse_range` and `serve_plan` for the *relay*
//! shape (a remote origin that may honour Range); this module is the **local file** shape, and
//! the two differ in exactly one place, which is why it is worth a separate table rather than a
//! reuse with flags.
//!
//! ## The three red lines, each measured on a real television
//!
//! 1. **`parse_range(None)` is not a partial answer.** Only a `Range` the client actually sent
//!    earns a 206; a client that sends none gets a 200 *without* `Content-Range`. The reference's
//!    comment calls this out because the shape `spec = parse_range(header)` — where `None` parses
//!    to `(0, None)` — makes a whole-file read answer 206. AGENTS.md §4.8 records Part 25 catching
//!    it with three tests.
//! 2. **A registered file that has since vanished must be refused, not served empty.** The entry
//!    is registered while the file exists; if it moved, `total == 0` and a 200 with no length
//!    leaves the renderer waiting for bytes that never come. That is a hang, not an error.
//! 3. **A start past the end is a 416 with `bytes */total`.** Not a 200: the renderer asked for a
//!    window that does not exist, and answering "here is the file" makes it seek into nothing.
//!
//! ## Headers that are not optional
//!
//! `Accept-Ranges: bytes` on **every** answer (a renderer decides whether to offer scrubbing by
//! looking for it), `Cache-Control: no-store` and `nosniff` (the registry name is random and the
//! content is a local file), and for a DLNA push only, `transferMode.dlna.org: Streaming` plus the
//! `contentFeatures.dlna.org` string — a television reads those **before** committing to a
//! stream, so their absence is "this file will not play" with no error anywhere.

use crate::relay::parse_range;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Everything served lives under one prefix, and the name is random: no listing, nothing to walk.
pub const MEDIA_PREFIX: &str = "/media/";

/// 64 KiB, as the reference's `CHUNK`. Big enough that a 1080p stream is not syscall-bound.
pub const CHUNK: usize = 64 * 1024;

/// The DLNA feature string. One literal, one meaning: a device parses this, so a typo is a
/// file that silently will not play.
pub const DLNA_FEATURES: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";

/// One registered thing to serve.
#[derive(Clone, Debug)]
pub struct Entry {
    /// `<name>.<suffix>`, the last path segment the renderer asks for.
    pub filename: String,
    pub path: PathBuf,
    pub content_type: String,
    /// The advertised length for a transcode (an estimate); `None` for a real file, whose length
    /// is read from disk at request time.
    pub job_total: Option<u64>,
    /// Only entries pushed at a television carry the DLNA headers.
    pub dlna: bool,
}

impl Entry {
    /// The length to advertise.
    ///
    /// A job's total is an **estimate** the renderer draws a progress bar with; a real file's is
    /// whatever is on disk *now*, which is why it is not cached: the file can be replaced between
    /// two requests and a stale length is a renderer seeking past the end.
    pub fn size(&self) -> u64 {
        match self.job_total {
            Some(total) => total,
            None => std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
        }
    }

    /// Read a window, for a real file.
    pub fn read(&self, offset: u64, length: usize) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut handle = std::fs::File::open(&self.path)?;
        handle.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; length];
        let mut filled = 0usize;
        while filled < length {
            match handle.read(&mut buffer[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        buffer.truncate(filled);
        Ok(buffer)
    }
}

/// The names this server answers for. An unknown name is always 404.
#[derive(Debug, Default)]
pub struct Store {
    entries: BTreeMap<String, Entry>,
}

impl Store {
    pub fn new() -> Store {
        Store::default()
    }

    /// Register a file and return the generated name.
    ///
    /// The name is random (`token_hex(8)` in the reference) and the caller supplies it: a store
    /// that generated its own names could not be tested, and one that used the file's own name
    /// would let a caller walk the filesystem by guessing.
    pub fn add(&mut self, name: String, entry: Entry) -> String {
        let filename = entry.filename.clone();
        self.entries.insert(name, entry);
        filename
    }

    pub fn lookup(&self, filename: &str) -> Option<&Entry> {
        self.entries.get(filename)
    }

    pub fn drop_name(&mut self, name: &str) {
        self.entries.remove(name);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What an HTTP answer must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The last path segment, when the request is under [`MEDIA_PREFIX`].
///
/// Returns `None` for anything else — including `..`, which cannot appear as a segment because
/// only the final one is used.
pub fn filename_of(path_and_query: &str) -> Option<&str> {
    let path = path_and_query.split('?').next().unwrap_or("");
    if !path.starts_with(MEDIA_PREFIX) {
        return None;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if last.is_empty() {
        None
    } else {
        Some(last)
    }
}

/// Decide the answer for a lookup.
///
/// `entry` is `None` for an unknown name. `range_header` is the raw `Range` value, if any.
/// The argument is `Option`-of-`&str` on purpose: passing `""` for "no header" would make
/// [`parse_range`] see an empty unit and answer a whole file, which happens to be right — but
/// stating "there was no header" separately is what keeps red line 1 legible.
pub fn respond(entry: Option<&Entry>, range_header: Option<&str>) -> Response {
    let entry = match entry {
        Some(entry) => entry,
        None => return refuse(404),
    };
    let total = entry.size();
    if total == 0 && entry.job_total.is_none() {
        // Registered while the file was there; it has since moved or gone. A 200 with no length
        // would leave a renderer waiting for bytes that never come.
        return refuse(404);
    }

    // **One owner for the Range grammar.** `crate::relay::parse_range` already implements the
    // reference's rules verbatim — including `stop < start` answering `None` (whole file), which a
    // hand-written "is this junk" test of mine got wrong on `bytes=9-0` and the corpus caught.
    //
    // The three outcomes land on two answers: a usable window (`Some((Some(a), …))`) is a 206; no
    // header, junk, and "start from the beginning" are all the whole file, 200, no Content-Range.
    let spec = match range_header {
        Some(header) if !header.is_empty() => match parse_range(Some(header)) {
            // A usable window, in either form:
            //   `bytes=2-5`   -> (Some(2), Some(5))
            //   `bytes=7-`    -> (Some(7), None)
            //   `bytes=-3`    -> (None,    Some(3))   <- the suffix form, resolved below
            //
            // What is **not** a window is `(Some(0), None)`, which `parse_range` answers for a
            // missing or empty header — "the whole file" wearing a Range's clothes. It cannot
            // arrive here (the guard above), but matching it explicitly keeps the two shapes
            // apart in the code as well as in the answer.
            Some((Some(0), None)) => None,
            Some(other) => Some(other),
            None => None,
        },
        _ => None,
    };

    let (start, stop) = match spec {
        None => (None, None),
        Some((start, stop)) => (start, stop),
    };

    // "The last n bytes": only the real length can resolve it.
    let (start, stop) = if start.is_none() && stop.is_some() {
        let tail = stop.unwrap_or(0);
        (total.saturating_sub(tail), None)
    } else {
        (start.unwrap_or(0), stop)
    };

    if start >= total {
        // **No DLNA headers on a 416.** The reference's refusal path sends only the code and the
        // two fields, and the corpus caught this port adding them: a 416 that also claims
        // `transferMode.dlna.org: Streaming` tells the device "this is a stream" about a window
        // that does not exist.
        return Response {
            status: 416,
            headers: vec![
                ("Content-Range".to_string(), format!("bytes */{total}")),
                ("Content-Length".to_string(), "0".to_string()),
            ],
        };
    }

    let end = match stop {
        None => total - 1,
        Some(stop) if stop >= total => total - 1,
        Some(stop) => stop.max(start),
    };
    // `spec`, not `asked`: only a Range the client sent **and** that parsed earns a 206.
    let status = if spec.is_some() { 206 } else { 200 };
    let mut headers = vec![
        ("Content-Type".to_string(), entry.content_type.clone()),
        ("Cache-Control".to_string(), "no-store".to_string()),
        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        ("Accept-Ranges".to_string(), "bytes".to_string()),
    ];
    if status == 206 {
        headers.push((
            "Content-Range".to_string(),
            format!("bytes {start}-{end}/{total}"),
        ));
        headers.push((
            "Content-Length".to_string(),
            (end - start + 1).to_string(),
        ));
    } else {
        headers.push(("Content-Length".to_string(), total.to_string()));
    }
    headers.extend(dlna_headers(entry));
    Response { status, headers }
}

fn refuse(code: u16) -> Response {
    Response {
        status: code,
        headers: vec![("Content-Length".to_string(), "0".to_string())],
    }
}

fn dlna_headers(entry: &Entry) -> Vec<(String, String)> {
    if !entry.dlna {
        return Vec::new();
    }
    vec![
        (
            "transferMode.dlna.org".to_string(),
            "Streaming".to_string(),
        ),
        (
            "contentFeatures.dlna.org".to_string(),
            DLNA_FEATURES.to_string(),
        ),
    ]
}

/// Where a sidecar subtitle goes, so a name cannot escape the settings directory.
///
/// `None` when the result would leave `base` — a `..` in a derived name is the only way this can
/// happen and it must not be served.
pub fn sidecar_path(base: &Path, stem: &str, suffix: &str) -> Option<PathBuf> {
    if stem.is_empty() || stem.contains('/') || stem.contains('\\') || stem.contains("..") {
        return None;
    }
    Some(base.join(format!("{stem}.{suffix}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut handle = std::fs::File::create(&path).expect("create");
        handle.write_all(bytes).expect("write");
        path
    }

    fn entry(path: PathBuf, dlna: bool) -> Entry {
        Entry {
            filename: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            path,
            content_type: "video/mp4".to_string(),
            job_total: None,
            dlna,
        }
    }

    #[test]
    fn only_the_final_segment_under_the_prefix_is_a_name() {
        crate::pin_test_dir();
        assert_eq!(filename_of("/media/abc.mp4"), Some("abc.mp4"));
        assert_eq!(filename_of("/media/abc.mp4?t=1"), Some("abc.mp4"));
        // A traversal attempt cannot escape: only the last segment is used, and `..` is not it.
        assert_eq!(filename_of("/media/../../etc/passwd"), Some("passwd"));
        assert_eq!(filename_of("/other/abc.mp4"), None);
        assert_eq!(filename_of("/media/"), None);
        assert_eq!(filename_of(""), None);
    }

    #[test]
    fn an_unknown_name_is_always_404() {
        crate::pin_test_dir();
        let response = respond(None, None);
        assert_eq!(response.status, 404);
        assert_eq!(response.header("Content-Length"), Some("0"));
        // Nothing else is promised about an answer we cannot make.
        assert_eq!(response.header("Accept-Ranges"), None);
    }

    #[test]
    fn a_vanished_file_is_refused_rather_than_served_empty() {
        crate::pin_test_dir();
        // Red line 2: registered while the file existed. A 200 with no length leaves the
        // renderer waiting forever, which reads as a hang, not an error.
        let missing = Entry {
            filename: "gone.mp4".to_string(),
            path: PathBuf::from("/nonexistent/pavia/gone.mp4"),
            content_type: "video/mp4".to_string(),
            job_total: None,
            dlna: false,
        };
        assert_eq!(respond(Some(&missing), None).status, 404);
        // …but a transcode's advertised length keeps it servable even though the file is still
        // growing and may not exist yet.
        let growing = Entry {
            job_total: Some(1_000),
            ..missing.clone()
        };
        assert_eq!(respond(Some(&growing), None).status, 200);
    }

    #[test]
    fn a_request_without_a_range_is_a_200_and_carries_no_content_range() {
        crate::pin_test_dir();
        // Red line 1, and the exact shape Part 25 caught: `spec = parse_range(header)` makes a
        // whole-file read answer 206, and a DLNA renderer reads that as a partial file.
        let dir = tempfile_dir();
        let path = temp_file(&dir, "a.mp4", b"0123456789");
        let response = respond(Some(&entry(path, false)), None);
        assert_eq!(response.status, 200);
        assert_eq!(response.header("Content-Range"), None);
        assert_eq!(response.header("Content-Length"), Some("10"));
        assert_eq!(response.header("Accept-Ranges"), Some("bytes"));
    }

    #[test]
    fn a_junk_range_is_ignored_and_answers_the_whole_file() {
        crate::pin_test_dir();
        // RFC 7233: an unsupported Range is ignored, **not** answered 416 — 416 is what a DLNA
        // renderer reads as a dead file.
        let dir = tempfile_dir();
        let path = temp_file(&dir, "b.mp4", b"0123456789");
        for junk in ["kilobytes=0-9", "bytes=9-0", "bytes=1-2,4-5", "bytes=", "garbage"] {
            let response = respond(Some(&entry(path.clone(), false)), Some(junk));
            assert_eq!(response.status, 200, "{junk} should be ignored");
            assert_eq!(response.header("Content-Range"), None, "{junk}");
        }
    }

    #[test]
    fn a_real_range_is_a_206_with_its_content_range() {
        crate::pin_test_dir();
        let dir = tempfile_dir();
        let path = temp_file(&dir, "c.mp4", b"0123456789");
        let response = respond(Some(&entry(path.clone(), false)), Some("bytes=2-5"));
        assert_eq!(response.status, 206);
        assert_eq!(response.header("Content-Range"), Some("bytes 2-5/10"));
        assert_eq!(response.header("Content-Length"), Some("4"));

        // Open-ended and suffix forms both resolve against the real length.
        let open = respond(Some(&entry(path.clone(), false)), Some("bytes=7-"));
        assert_eq!(open.header("Content-Range"), Some("bytes 7-9/10"));
        let tail = respond(Some(&entry(path, false)), Some("bytes=-3"));
        assert_eq!(tail.header("Content-Range"), Some("bytes 7-9/10"));
    }

    #[test]
    fn a_start_past_the_end_is_a_416_naming_the_real_length() {
        crate::pin_test_dir();
        // Red line 3: the renderer asked for a window that does not exist. Saying "here is the
        // file" makes it seek into nothing.
        let dir = tempfile_dir();
        let path = temp_file(&dir, "d.mp4", b"0123456789");
        let response = respond(Some(&entry(path, false)), Some("bytes=99-"));
        assert_eq!(response.status, 416);
        assert_eq!(response.header("Content-Range"), Some("bytes */10"));
        assert_eq!(response.header("Content-Length"), Some("0"));
    }

    #[test]
    fn a_dlna_entry_carries_the_two_headers_a_television_reads_first() {
        crate::pin_test_dir();
        let dir = tempfile_dir();
        let path = temp_file(&dir, "e.mp4", b"0123456789");
        let dlna = respond(Some(&entry(path.clone(), true)), None);
        assert_eq!(dlna.header("transferMode.dlna.org"), Some("Streaming"));
        assert_eq!(dlna.header("contentFeatures.dlna.org"), Some(DLNA_FEATURES));
        // …and a non-DLNA push must not claim to be one, or a Chromecast gets television headers.
        let plain = respond(Some(&entry(path, false)), None);
        assert_eq!(plain.header("transferMode.dlna.org"), None);
        assert_eq!(plain.header("contentFeatures.dlna.org"), None);
        // The feature string is a protocol payload, so pin it literally.
        assert!(DLNA_FEATURES.starts_with("DLNA.ORG_OP=01;"));
        assert!(DLNA_FEATURES.contains("DLNA.ORG_FLAGS=01700000000000000000000000000000"));
    }

    #[test]
    fn reads_return_the_requested_window_and_stop_at_the_end() {
        crate::pin_test_dir();
        let dir = tempfile_dir();
        let path = temp_file(&dir, "f.mp4", b"0123456789");
        let entry = entry(path, false);
        assert_eq!(entry.read(2, 3).expect("read"), b"234");
        // A read that runs past the end returns what is there, not an error: the caller loops
        // until it gets an empty answer.
        assert_eq!(entry.read(8, 100).expect("read"), b"89");
        assert!(entry.read(100, 10).expect("read").is_empty());
        assert_eq!(entry.size(), 10);
    }

    #[test]
    fn a_sidecar_name_cannot_escape_the_base_directory() {
        crate::pin_test_dir();
        let base = Path::new("/tmp/macast-sidecars");
        assert_eq!(
            sidecar_path(base, "abc", "vtt"),
            Some(base.join("abc.vtt"))
        );
        for bad in ["", "..", "../etc/passwd", "a/b", "a\\b"] {
            assert_eq!(sidecar_path(base, bad, "vtt"), None, "{bad:?} escaped");
        }
    }

    #[test]
    fn the_store_answers_only_for_names_it_was_given() {
        crate::pin_test_dir();
        let dir = tempfile_dir();
        let path = temp_file(&dir, "g.mp4", b"x");
        let mut store = Store::new();
        store.add("name1".to_string(), entry(path, false));
        assert!(store.lookup("name1").is_some());
        assert!(store.lookup("name2").is_none());
        assert_eq!(store.len(), 1);
        store.drop_name("name1");
        assert!(store.is_empty());
    }

    /// Every answer the reference's `do_GET` produced, replayed against this port.
    ///
    /// 49 cases from executing the reference's decision path (no socket) over two lengths, twelve
    /// `Range` values and both DLNA flags: the three red lines, the junk-ignored table, the suffix
    /// form, and the 416 that names the real length.
    #[test]
    fn every_reference_answer_replays_identically() {
        crate::pin_test_dir();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/local_serve.json"))
                .expect("corpus is JSON");
        let cases = corpus.as_array().expect("an array of cases");
        let mut rows = 0usize;
        for case in cases {
            let total = case["total"].as_u64().expect("total");
            let dlna = case["dlna"].as_bool().expect("dlna");
            let range = case["range"].as_str();
            let expected = &case["result"];

            // A real file of exactly `total` bytes, so `Entry::size()` answers what the corpus
            // recorded without this test reaching into the entry.
            let dir = tempfile_dir();
            let name = format!("corpus-{rows}-{total}.mp4");
            let path = temp_file(&dir, &name, &vec![b'x'; total as usize]);
            let entry = Entry {
                filename: name,
                path,
                content_type: "video/mp4".to_string(),
                job_total: None,
                dlna,
            };

            let response = if total == 0 {
                // The corpus's zero-length case is the "file vanished" shape, which a real
                // zero-byte file is not: it exists, so `size()` is 0 but the entry is present.
                // Reproduce the vanished case by pointing at a path that does not exist.
                let missing = Entry {
                    path: PathBuf::from("/nonexistent/local-serve/corpus.mp4"),
                    ..entry
                };
                respond(Some(&missing), range)
            } else {
                respond(Some(&entry), range)
            };

            assert_eq!(
                response.status as u64,
                expected["status"].as_u64().expect("status"),
                "case {rows} (total {total}, range {range:?}) answered the wrong status"
            );
            let expected_headers = expected["headers"].as_object().expect("headers");
            for (key, value) in expected_headers {
                assert_eq!(
                    response.header(key),
                    value.as_str(),
                    "case {rows}: header {key} differs"
                );
            }
            // …and no header the reference did not send, so an extra `Content-Range` on a 200
            // (red line 1 inverted) fails here.
            for (key, _) in &response.headers {
                assert!(
                    expected_headers.contains_key(key)
                        || expected_headers.keys().any(|k| k.eq_ignore_ascii_case(key)),
                    "case {rows}: unexpected header {key}"
                );
            }
            rows += 1;
        }
        assert_eq!(rows, 49, "the corpus lost cases");
    }

    /// A directory under the temp root, unique per test.
    fn tempfile_dir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "macast-local-file-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        base
    }
}
