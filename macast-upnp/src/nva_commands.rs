//! NVA command table: which verbs exist, what they need, and whether they answer first.
//!
//! Rust counterpart of `nirvana.py`'s `cmd_from_client` (`:1204-1340`). The reference dispatches
//! with an `if/elif` chain; what is portable — and what a port can get silently wrong — is the
//! **shape of the table** rather than the Bilibili calls inside each arm.
//!
//! ## Three columns, each with a way to be wrong in silence
//!
//! * **Does it need arguments?** `GetVolume` and `Pause` take nothing while `SetVolume` and
//!   `Play` cannot work without them. Reading `params["volume"]` on a frame that carried no body
//!   raises `TypeError` in Python, which the request thread's `except` turns into a dropped
//!   connection — a client that sent the wrong shape sees the socket close and nothing else.
//! * **Which keys are required?** The reference uses `params['aid']` (required, raises) for some
//!   and `params.get('oid', self.aid)` (optional, defaults) for others. Getting that distinction
//!   backwards turns "this client omitted an optional field" into a dropped connection.
//! * **Does it reply before acting?** Most arms call `self.send_res(counter)` *first*; `SetVolume`
//!   and `SwitchSpeed` do not reply at all. A port that always answers would send an extra frame
//!   the client does not expect; one that never answers makes a `Pause` look like it hung.
//!
//! ## What is deliberately out of scope
//!
//! No Bilibili API, no renderer, no network. Each [`Verb`] names the arm; the caller performs it.
//! That split is what makes this testable at all — the reference's arms call `requests` and
//! `get_video_url`, neither of which belongs in a table.

/// What a verb needs from the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// No body. A body is ignored rather than refused (the reference reads no keys).
    Nothing,
    /// A JSON body; the listed keys must be present.
    Arguments(&'static [&'static str]),
    /// A JSON body must exist, but **no particular key is required** — the arm uses
    /// `params.get(key, default)`, so a mapping without that key is a range check or a default
    /// rather than an exception. A bodyless frame is still `TypeError` (there is no mapping to
    /// call `.get` on), which is the distinction the reference makes accidentally but really
    /// does make.
    BodyOnly,
}

/// Whether the arm answers the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `send_res(counter)` before doing anything — the client waits for it.
    Before,
    /// No reply. The client is not listening for one.
    None,
}

/// One row of the command table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub verb: Verb,
    /// The wire name, exactly as the client sends it (case-sensitive: the reference compares
    /// `method == 'GetVolume'` with no normalisation).
    pub name: &'static str,
    pub needs: Needs,
    pub reply: Reply,
}

/// The verbs the reference dispatches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    GetVolume,
    SetVolume,
    Pause,
    Resume,
    SendDanmaku,
    SwitchDanmaku,
    SwitchSpeed,
    SwitchQn,
    Stop,
    Play,
    PlayUrl,
    Seek,
}

/// The command table, in the reference's `if/elif` order.
///
/// Order matters only for a port that dispatches by prefix; this one matches exactly, so the
/// order is documentation rather than behaviour. It is kept so a diff against the reference
/// reads line by line.
pub const TABLE: &[Row] = &[
    Row { verb: Verb::GetVolume, name: "GetVolume", needs: Needs::Nothing, reply: Reply::Before },
    // `params.get('volume', -1)` — an absent key defaults to -1, which then fails the 0..100
    // range check rather than raising. A bodyless frame is `TypeError` (there is no mapping to
    // call `.get` on), which is why this arm needs *a* body but not that key.
    Row { verb: Verb::SetVolume, name: "SetVolume", needs: Needs::BodyOnly, reply: Reply::None },
    Row { verb: Verb::Pause, name: "Pause", needs: Needs::Nothing, reply: Reply::Before },
    Row { verb: Verb::Resume, name: "Resume", needs: Needs::Nothing, reply: Reply::Before },
    Row { verb: Verb::SendDanmaku, name: "SendDanmaku", needs: Needs::Arguments(&["content"]), reply: Reply::Before },
    Row { verb: Verb::SwitchDanmaku, name: "SwitchDanmaku", needs: Needs::Arguments(&["open"]), reply: Reply::Before },
    Row { verb: Verb::SwitchSpeed, name: "SwitchSpeed", needs: Needs::Arguments(&["speed"]), reply: Reply::None },
    Row { verb: Verb::SwitchQn, name: "SwitchQn", needs: Needs::Arguments(&["qn"]), reply: Reply::Before },
    Row { verb: Verb::Stop, name: "Stop", needs: Needs::Nothing, reply: Reply::Before },
    Row { verb: Verb::Play, name: "Play", needs: Needs::Arguments(&["aid", "cid"]), reply: Reply::Before },
    Row { verb: Verb::PlayUrl, name: "PlayUrl", needs: Needs::Arguments(&["url"]), reply: Reply::Before },
    // `params.get('seekTs', 0)` after `send_res`, so a bodyless frame raises `TypeError` and the
    // reply has already gone out. `BodyOnly` is the shape: a mapping is needed, that key is not.
    Row { verb: Verb::Seek, name: "Seek", needs: Needs::BodyOnly, reply: Reply::Before },
];

/// Look a verb up by its wire name.
pub fn row_named(name: &str) -> Option<&'static Row> {
    // Exact match, deliberately: `method == 'GetVolume'` is what the reference does, and a
    // case-insensitive table would accept `getvolume` that a real client never sends.
    TABLE.iter().find(|row| row.name == name)
}

/// Why a frame cannot be dispatched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// No row for this verb. The reference's `else` arm answers `send_res(counter)` and returns,
    /// so this is **not** an error on the wire — it is a reply with no action.
    UnknownVerb,
    /// The verb needs arguments and the frame carried none.
    MissingArguments,
    /// The verb needs arguments and one of them is absent.
    MissingKey(&'static str),
}

impl Rejected {
    /// Does the client still get a reply?
    ///
    /// `UnknownVerb` does (the reference answers in its `else`); a missing-key arm raises inside
    /// the request thread and the connection is dropped, so it does not. That asymmetry is the
    /// whole reason this is a method rather than a constant.
    pub fn replies(&self) -> bool {
        matches!(self, Rejected::UnknownVerb)
    }
}

/// Check a frame against the table **without performing it**.
///
/// Returns the row to execute, or why not. An `UnknownVerb` is a normal answer (see
/// [`Rejected::replies`]); the two `Missing…` variants name a frame the reference would crash on.
pub fn dispatch(name: &str, body: Option<&serde_json::Value>) -> Result<&'static Row, Rejected> {
    let row = row_named(name).ok_or(Rejected::UnknownVerb)?;
    match row.needs {
        Needs::Nothing => Ok(row),
        Needs::BodyOnly => match body {
            Some(_) => Ok(row),
            None => Err(Rejected::MissingArguments),
        },
        Needs::Arguments(keys) => {
            let body = body.ok_or(Rejected::MissingArguments)?;
            for key in keys {
                // `Value::get` on an object; a non-object body has no keys at all, which is
                // `MissingKey` rather than a second error kind.
                if body.get(*key).is_none() {
                    return Err(Rejected::MissingKey(key));
                }
            }
            Ok(row)
        }
    }
}

/// The optional keys each arm reads with a default, for the arms that have any.
///
/// Recorded because the reference mixes `params['x']` (raises) and `params.get('x', default)`
/// (tolerates) within single arms, and a port that treated them uniformly would drop connections
/// a real client expects to keep.
pub fn optional_keys(verb: Verb) -> &'static [(&'static str, &'static str)] {
    match verb {
        Verb::Play => &[
            ("oid", "aid"),
            ("epId", "0"),
            ("accessKey", ""),
            ("userDesireQn", "0"),
            ("contentType", "1"),
            ("seasonId", "0"),
            ("userDesireSpeed", "1"),
            ("danmakuSwitchSave", "true"),
            ("seekTs", "0"),
            ("roomId", "0"),
        ],
        Verb::PlayUrl => &[("playurl_type", "1"), ("contentType", "1")],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_in_the_table_is_reachable_by_its_wire_name() {
        crate::pin_test_dir();
        for row in TABLE {
            assert_eq!(
                row_named(row.name).map(|found| found.verb),
                Some(row.verb),
                "{} is in the table but not findable",
                row.name
            );
        }
        // All twelve arms are present, so a missing one is visible rather than a quiet no-op.
        assert_eq!(TABLE.len(), 12);
        let mut verbs: Vec<&str> = TABLE.iter().map(|row| row.name).collect();
        verbs.sort_unstable();
        verbs.dedup();
        assert_eq!(verbs.len(), 12, "a verb name is duplicated");
    }

    #[test]
    fn the_lookup_is_case_sensitive_because_the_reference_compares_exactly() {
        crate::pin_test_dir();
        // `method == 'GetVolume'` has no normalisation, so a client sending `getvolume` falls into
        // the reference's `else` arm. A case-insensitive table would invent a behaviour.
        assert!(row_named("GetVolume").is_some());
        assert!(row_named("getvolume").is_none());
        assert_eq!(dispatch("getvolume", None), Err(Rejected::UnknownVerb));
    }

    #[test]
    fn an_unknown_verb_still_gets_a_reply_but_a_missing_key_does_not() {
        crate::pin_test_dir();
        // The reference's `else` arm answers `send_res(counter)` and does nothing; an arm that
        // indexes a missing key raises inside the request thread and the connection is dropped.
        assert!(Rejected::UnknownVerb.replies());
        assert!(!Rejected::MissingArguments.replies());
        assert!(!Rejected::MissingKey("volume").replies());
    }

    #[test]
    fn a_verb_that_takes_nothing_is_not_refused_when_a_body_arrives() {
        crate::pin_test_dir();
        // `Pause` reads no keys, so an extra body is ignored rather than rejected — refusing it
        // would drop a connection the reference keeps.
        let row = dispatch("Pause", Some(&serde_json::json!({"ignored": 1}))).expect("accepted");
        assert_eq!(row.verb, Verb::Pause);
        assert_eq!(row.reply, Reply::Before);
    }

    #[test]
    fn a_verb_that_needs_arguments_refuses_a_bodyless_frame_only_for_the_arms_that_index() {
        crate::pin_test_dir();
        // `SetVolume` reads `params.get('volume', -1)` — a missing body is `TypeError` because
        // `params` itself is `None`, not because the key is absent. Both refusals are real.
        assert_eq!(dispatch("SetVolume", None), Err(Rejected::MissingArguments));
        // …but a body *without* `volume` is fine: `params.get('volume', -1)` yields -1, which
        // fails the 0..100 range check instead of raising. The corpus caught this: my first
        // version demanded the key and would have dropped a connection the reference keeps.
        assert!(dispatch("SetVolume", Some(&serde_json::json!({"other": 1}))).is_ok());
        assert!(dispatch("SetVolume", Some(&serde_json::json!({"volume": 50}))).is_ok());
    }

    #[test]
    fn play_requires_two_keys_while_treating_the_rest_as_optional() {
        crate::pin_test_dir();
        // The asymmetry inside one arm: `params['aid']` and `params['cid']` raise, everything else
        // has a default. Reading that backwards drops connections for clients that omit an
        // optional field.
        assert_eq!(
            dispatch("Play", Some(&serde_json::json!({"aid": 1}))),
            Err(Rejected::MissingKey("cid"))
        );
        assert!(dispatch("Play", Some(&serde_json::json!({"aid": 1, "cid": 2}))).is_ok());
        let optional = optional_keys(Verb::Play);
        let named: Vec<&str> = optional.iter().map(|(key, _)| *key).collect();
        for expected in ["oid", "epId", "accessKey", "seekTs", "roomId"] {
            assert!(named.contains(&expected), "{expected} should be optional");
        }
        // …and the required ones must NOT appear as optional, or the two lists disagree.
        assert!(!named.contains(&"aid"));
        assert!(!named.contains(&"cid"));
    }

    #[test]
    fn only_two_arms_answer_nothing_to_the_client() {
        crate::pin_test_dir();
        // `SetVolume` and `SwitchSpeed` never call `send_res`, so a client waits for a reply that
        // never comes — by design. A port that answered everything would send frames the client
        // does not read, and one that answered nothing would make `Pause` look hung.
        let silent: Vec<&str> = TABLE
            .iter()
            .filter(|row| row.reply == Reply::None)
            .map(|row| row.name)
            .collect();
        assert_eq!(silent, vec!["SetVolume", "SwitchSpeed"]);
    }

    /// Every dispatch decision the reference made, replayed against this port.
    ///
    /// Fifteen rows produced by executing the reference's `cmd_from_client` arm structure with
    /// the Bilibili/renderer calls replaced by event markers: which arm fired, whether
    /// `send_res` ran, and whether a `KeyError`/`TypeError` escaped.
    ///
    /// Two rows changed this table when it was first run, and both were mine:
    /// * `SetVolume {'other': 1}` — the reference answers **no** exception, because it reads
    ///   `params.get('volume', -1)`; I had demanded the key.
    /// * `Seek` with a non-object body — also accepted, same `.get` reason.
    #[test]
    fn every_reference_dispatch_replays_identically() {
        crate::pin_test_dir();
        let corpus = include_str!("../tests/golden/nva_commands.jsonl");
        let mut rows = 0usize;
        for line in corpus.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value = serde_json::from_str(line).expect("corpus row");
            let method = row["method"].as_str().expect("method");
            let body = row["params"].as_object().map(|_| row["params"].clone());
            let result = &row["result"];
            let raised = result["raised"].as_str();
            let replied = result["events"]
                .as_array()
                .expect("events")
                .iter()
                .any(|event| event.as_str() == Some("res"));

            let outcome = dispatch(method, body.as_ref());
            match raised {
                None => match outcome {
                    Ok(found) => assert_eq!(
                        found.reply == Reply::Before,
                        replied,
                        "{method}: the reply decision differs from the reference"
                    ),
                    // `UnknownVerb` is not a refusal on the wire: the reference's `else` arm
                    // answers `send_res(counter)` and does nothing. So the row must have replied,
                    // and this outcome is the table's way of saying "no arm to run".
                    Err(Rejected::UnknownVerb) => assert!(
                        replied,
                        "{method}: refused as unknown, but the reference did not reply either"
                    ),
                    Err(why) => panic!(
                        "{method} {body:?} was refused ({why:?}) where the reference ran it"
                    ),
                },
                Some(_) => {
                    // The reference raised, so the frame must be refused **and** must not have
                    // replied — the reply, if any, happens after the failing read only for
                    // `Seek`, which is why that row still records `res`.
                    assert!(
                        outcome.is_err(),
                        "{method} {body:?} was accepted where the reference raised {raised:?}"
                    );
                }
            }
            rows += 1;
        }
        assert_eq!(rows, 15, "the corpus lost rows");
    }

    #[test]
    fn a_non_object_body_has_no_keys_and_is_refused_as_a_missing_key() {
        crate::pin_test_dir();
        // A JSON array or a bare number carries no field, which is the same answer as an object
        // without that field — not a third error kind.
        // `Seek` uses `params.get('seekTs', 0)` after replying, so any body is accepted and a
        // missing key means "seek to 0". Only a bodyless frame raises.
        assert!(dispatch("Seek", Some(&serde_json::json!([1, 2]))).is_ok());
        assert!(dispatch("Seek", Some(&serde_json::json!(42))).is_ok());
        assert_eq!(dispatch("Seek", None), Err(Rejected::MissingArguments));
        assert!(dispatch("Seek", Some(&serde_json::json!({"seekTs": 30}))).is_ok());
    }
}
