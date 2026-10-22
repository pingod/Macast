//! The live/IPTV renderer's channel list: how an m3u8 becomes menu rows.
//!
//! Rust counterpart of `macast/plugins/renderer/live.py`'s list handling (`:119`, `:170-194`).
//!
//! ## The split literal is the whole format
//!
//! The reference splits the file on the string `#EXTINF:-1 ,` — **with a space before the
//! comma**. That is not a typo to be tidied up: it is what the upstream list actually
//! contains, and "fixing" it to `#EXTINF:-1,` would leave the whole file as one chunk, i.e. a
//! menu with a single row. The first chunk carries the `#EXTM3U` header, which is why one row
//! is always "PlayList" (the file itself) rather than a channel.
//!
//! ## The crash this module exists to not repeat
//!
//! A chunk whose second line is empty used to raise `IndexError` **while the menu was being
//! built**, which took the entire settings menu down with it — a malformed entry in a
//! convenience list could make the app's menu unusable. So a chunk becomes a row only when it
//! has two lines *and* the second one is non-empty after trimming; everything else is
//! skipped with a warning.
//!
//! ## Nothing here blocks
//!
//! Reading the list is the caller's job (a file, a cached fetch). This module only decides
//! what the bytes mean, because the reference's expensive mistake was fetching on the UI
//! thread at import time.

/// The separator, space included. See the module docs: this is the upstream format.
pub const CHUNK_SEPARATOR: &str = "#EXTINF:-1 ,";

/// One entry in the channel list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Channel {
    /// The playlist itself, offered as a single row.
    PlayList,
    /// A channel: the label as written, and the address from the next line.
    Stream { label: String, url: String },
    /// A chunk we could not turn into a row. Carried so the caller can log it; the reference
    /// logs the first 60 characters.
    Malformed { preview: String },
}

/// Split the list text into chunks.
///
/// Empty text is no chunks at all — **not** one empty chunk. The reference gets this by
/// taking `[]` when the text is falsy, and the difference is visible: one empty chunk would
/// render an empty menu row.
pub fn chunks(text: &str) -> Vec<&str> {
    // `is_empty`, **not** `trim().is_empty()`: the reference tests `if not text`, and a
    // whitespace-only string is truthy in Python, so it yields one (malformed) chunk there.
    // Both answer the same menu — a malformed chunk is skipped — but matching the test exactly
    // keeps the corpus a record of the reference rather than of my reading of it.
    if text.is_empty() {
        return Vec::new();
    }
    text.split(CHUNK_SEPARATOR).collect()
}

/// Turn one chunk into a row.
///
/// The order of the two tests is the reference's: the `#EXTM3U` check comes first, so a file
/// whose header line is followed by a channel still yields exactly one PlayList row.
pub fn channel_of(chunk: &str) -> Channel {
    if chunk.starts_with("#EXTM3U") {
        return Channel::PlayList;
    }
    let mut lines = chunk.split('\n');
    let label = lines.next().unwrap_or("").trim_end_matches('\r');
    let url = lines.next().unwrap_or("").trim_end_matches('\r');
    if url.trim().is_empty() {
        // The `IndexError` that used to take the whole menu down. Keep a short preview so the
        // warning can name what was skipped.
        return Channel::Malformed {
            preview: chunk.chars().take(60).collect(),
        };
    }
    Channel::Stream {
        label: label.to_string(),
        url: url.to_string(),
    }
}

/// The whole list as rows.
pub fn channels(text: &str) -> Vec<Channel> {
    chunks(text).into_iter().map(channel_of).collect()
}

/// The rows that become menu items, in order.
pub fn menu_rows(text: &str) -> Vec<Channel> {
    channels(text)
        .into_iter()
        .filter(|channel| !matches!(channel, Channel::Malformed { .. }))
        .collect()
}

/// The address a row plays, for the caller that only needs that.
pub fn url_of(channel: &Channel) -> Option<&str> {
    match channel {
        Channel::Stream { url, .. } => Some(url),
        Channel::PlayList | Channel::Malformed { .. } => None,
    }
}

/// Labels for the two filler rows when there is nothing to show.
///
/// Both are needed: the first says why the menu is empty, the second is the only way out.
/// A menu with neither looks like the plugin failed to load.
pub const NO_CHANNELS_LABEL: &str = "No channels (refresh to fetch)";
pub const REFRESH_LABEL: &str = "Refresh channel list";
pub const PLAYLIST_LABEL: &str = "PlayList";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_separator_keeps_its_space_before_the_comma() {
        // Tidying this to `#EXTINF:-1,` would leave the file as one chunk — a menu with one
        // row — and nothing would report an error.
        assert_eq!(CHUNK_SEPARATOR, "#EXTINF:-1 ,");
        let text = "#EXTM3U\n#EXTINF:-1 ,CCTV1\nhttp://a/1.m3u8\n#EXTINF:-1 ,CCTV2\nhttp://a/2.m3u8";
        assert_eq!(chunks(text).len(), 3);
        // …and the tidied form really does fail to split, which is the point of the assertion.
        let tidied = text.replace("#EXTINF:-1 ,", "#EXTINF:-1,");
        assert_eq!(tidied.split("#EXTINF:-1 ,").count(), 1);
    }

    #[test]
    fn the_header_chunk_becomes_the_playlist_row() {
        let text = "#EXTM3U\n#EXTINF:-1 ,CCTV1\nhttp://a/1.m3u8";
        let rows = menu_rows(text);
        assert_eq!(rows[0], Channel::PlayList);
        assert_eq!(
            rows[1],
            Channel::Stream {
                label: "CCTV1".into(),
                url: "http://a/1.m3u8".into()
            }
        );
    }

    #[test]
    fn a_chunk_with_no_address_is_skipped_instead_of_crashing_the_menu() {
        // The defect this module is written around: `lines[1]` on a one-line chunk used to
        // raise IndexError while the menu was being built, taking the whole menu down.
        for bad in ["CCTV1", "CCTV1\n", "CCTV1\n   ", "CCTV1\n\n"] {
            let channel = channel_of(bad);
            assert!(
                matches!(channel, Channel::Malformed { .. }),
                "{bad:?} produced {channel:?} instead of being skipped"
            );
            // …and it must not reach the menu.
            assert!(!menu_rows(bad)
                .iter()
                .any(|row| matches!(row, Channel::Stream { .. })));
        }
    }

    #[test]
    fn the_malformed_preview_is_short_enough_for_a_log_line() {
        // Malformed *and* long: many lines, none of them an address.
        let long = format!("{}\n   ", "x".repeat(500));
        match channel_of(&long) {
            Channel::Malformed { preview } => {
                assert_eq!(preview.chars().count(), 60, "preview should be 60 characters");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn empty_text_yields_no_chunks_rather_than_one_empty_one() {
        // One empty chunk would render an empty menu row; the reference's falsy check is why
        // an unreadable list produces the two filler rows instead.
        assert!(chunks("").is_empty());
        assert!(menu_rows("").is_empty());
        // Whitespace-only text is *not* empty, and the reference splits it into one malformed
        // chunk. Same menu either way (malformed rows are skipped), but the corpus records the
        // reference, so this matches it rather than tidying it.
        assert_eq!(chunks("   \n  ").len(), 1);
        assert!(matches!(
            channel_of(chunks("   \n  ")[0]),
            Channel::Malformed { .. }
        ));
    }

    /// The CRLF `\r` is stripped here and **kept** by the reference — a deliberate fix.
    ///
    /// A trailing `\r` travels into the address handed to the player, which then fails to open
    /// it, and the failure reads as "this channel is dead" rather than "our parser kept a
    /// character". This is the one row of the corpus the port intentionally answers
    /// differently, so the replay below flips it explicitly instead of pretending the corpus
    /// agrees.
    #[test]
    fn carriage_returns_are_stripped_so_a_windows_list_does_not_carry_them_into_the_url() {
        // A `\r` left on the end of the address is passed to the player, which then fails to
        // open it — the kind of defect that only shows up on someone else's platform.
        let text = "#EXTM3U\r\n#EXTINF:-1 ,CCTV1\r\nhttp://a/1.m3u8\r\n";
        let row = menu_rows(text)
            .into_iter()
            .find(|row| matches!(row, Channel::Stream { .. }))
            .expect("a channel row");
        assert_eq!(url_of(&row), Some("http://a/1.m3u8"));
        match row {
            Channel::Stream { label, .. } => assert_eq!(label, "CCTV1"),
            other => panic!("expected a stream, got {other:?}"),
        }
    }

    #[test]
    fn the_playlist_row_has_no_url_because_it_plays_the_file() {
        // The reference passes LIVE_PATH as the data for that row, not an address from the
        // list, so asking this module for a url must answer "none".
        assert_eq!(url_of(&Channel::PlayList), None);
        assert_eq!(url_of(&Channel::Malformed { preview: String::new() }), None);
    }

    /// Every chunking and row decision the reference made, replayed against this port.
    ///
    /// Produced by executing the reference's `get_live_list` split and its menu loop over
    /// eleven list shapes: the two separators (correct and "tidied"), a missing header, a
    /// malformed entry, blank names and CRLF. Exactly one row is expected to disagree —
    /// `crlf`, where this port strips the carriage return on purpose.
    #[test]
    fn every_reference_list_shape_replays_identically_except_the_declared_fix() {
        let corpus = include_str!("../tests/golden/live_list.jsonl");
        let mut rows = 0usize;
        let mut declared = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let case = row["case"].as_str().expect("case");
            let text = row["text"].as_str().expect("text");
            let expected_chunks = row["chunks"].as_u64().expect("chunks") as usize;
            assert_eq!(
                chunks(text).len(),
                expected_chunks,
                "{case} chunked differently"
            );

            let expected: Vec<Vec<Option<String>>> = row["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|pair| {
                    pair.as_array()
                        .expect("pair")
                        .iter()
                        .map(|cell| cell.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .collect();

            let actual: Vec<Vec<Option<String>>> = channels(text)
                .into_iter()
                .map(|channel| match channel {
                    Channel::PlayList => vec![Some("playlist".to_string()), None],
                    Channel::Malformed { .. } => vec![Some("malformed".to_string()), None],
                    Channel::Stream { label, url } => vec![Some(label), Some(url)],
                })
                .collect();

            if case == "crlf" {
                // The declared fix: this port removes the `\r`, the reference keeps it.
                declared += 1;
                assert_ne!(actual, expected, "the crlf row no longer diverges — was the fix removed?");
                let url = actual
                    .iter()
                    .filter_map(|cells| cells.get(1).and_then(|c| c.clone()))
                    .find(|candidate| candidate.starts_with("http"))
                    .expect("the crlf row still has an address");
                assert!(
                    !url.ends_with('\r'),
                    "a carriage return must not reach the player: {url:?}"
                );
                assert!(expected
                    .iter()
                    .filter_map(|cells| cells.get(1).and_then(|c| c.clone()))
                    .any(|candidate| candidate.ends_with('\r')));
                rows += 1;
                continue;
            }

            assert_eq!(actual, expected, "{case} produced different rows");
            rows += 1;
        }
        assert_eq!(rows, 11, "the corpus lost rows");
        // A corpus where the declared fix vanished would mean someone "aligned" it away.
        assert_eq!(declared, 1, "the declared CRLF divergence is gone");
    }

    #[test]
    fn both_filler_labels_exist_because_one_alone_leaves_no_way_out() {
        // "No channels" without "Refresh" is a dead end; "Refresh" without the reason reads
        // like the plugin failed to load.
        assert!(NO_CHANNELS_LABEL.contains("refresh"));
        assert!(REFRESH_LABEL.to_lowercase().contains("refresh"));
        assert_eq!(PLAYLIST_LABEL, "PlayList");
    }
}
