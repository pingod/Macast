//! SOAP: what a control point says when it presses play.
//!
//! This module owns **the wire for remote procedure calls** and nothing else.
//! It reads the bytes a control point POSTs to `/AVTransport/action` (or
//! `/RenderingControl/action`), decides what the *document* said, writes the
//! state table through [`store::StateStore`], hands the media to
//! [`store::Renderer`], and builds the SOAP envelope that goes back. What the
//! player actually does, and what "this stream is live" costs downstream, are
//! not this module's decision — that is the layering stated in
//! [`crate::lib`], and this file is the reason it needs four seams rather than
//! three.
//!
//! # Where this came from
//!
//! `macast/protocol.py`: `DLNAHandler.call` at `:908-995` (the envelope, the
//! parameter bag, the branch, the response builder), the nine handler methods
//! (`RenderingControl_SetVolume:1063`, `RenderingControl_SetMute:1068`,
//! `AVTransport_SetAVTransportURI:1077`, `AVTransport_Next:1130`,
//! `AVTransport_Previous:1133`, `AVTransport_Play:1227`,
//! `AVTransport_Pause:1233`, `AVTransport_Seek:1238`, `AVTransport_Stop:1245`),
//! `_playlist_step:1136-1161`, `set_state` in the protocol class,
//! `normalize_soap_body`, `soap_fault:75-93`, and the two DIDL readers
//! (`protocol_info_from_didl` / `live_from_protocol_info`, and the
//! `.//{{dc}}title` read inside `AVTransport_SetAVTransportURI`).
//!
//! There is a census worth stating, because it decides half of this file's
//! shape: **zero of the nine handlers is a `ConnectionManager_*` method**. So
//! every `Get*` a control point calls — `GetPositionInfo`, `GetTransportInfo`,
//! `GetMediaInfo`, `GetProtocolInfo`, `GetVolume`, `GetMute`, … — takes the
//! *read* branch, and only those nine names take the write branch. Python
//! decides with `hasattr(self, method)`; we decide with [`HandlerKind`], which
//! is one lookup that drives **both** the branch choice and the dispatch, so
//! the two cannot drift apart into "we took the read branch for an action we
//! also implement".
//!
//! # Liveness: why there is no cell here
//!
//! Python stores the sender's answer in `self._media_live`. This module does
//! not keep one. `Cell<Option<bool>>` is not `Sync`, and a
//! `Mutex<Option<bool>>` would be rejected for a better reason: **nothing in
//! this crate ever reads it back**, and a second copy of "is this live" is
//! exactly the shape AGENTS.md §4.2 keeps biting us on. The value travels to
//! the only place that consumes it at the moment the address changes —
//! [`store::Renderer::note_media_live`] — which is the Rust form of that
//! assignment. `src-tauri`'s `Renderer` impl owns the record; we own the
//! reading of the sender's field and the ordering of the hand-over, which is
//! the part that was ever wrong (`PlaybackGuard`'s ordering, §4.2).
//!
//! # Deviations, all of them
//!
//! A list of places where a Python reader would see something else. Every one
//! is a decision, not an oversight, and the two that change what a *control
//! point sees* are marked as such.
//!
//! * **Every refusal is a real SOAP fault, with HTTP 500.** Python's `call()`
//!   wraps **only the parse** in `try/except etree.XMLSyntaxError`; that one
//!   path sets `cherrypy.response.status = 500` and returns
//!   `soap_fault(402, 'Invalid Args')`. Everything else **raises**, and
//!   CherryPy answers a raised exception with an HTML 500 page, not an
//!   envelope: `ValueError('malformed SOAP request: missing Body/action')`,
//!   `ValueError('malformed SOAP request: no action element')`, `KeyError` on
//!   `service_type.actions[action]`, `KeyError` on `state_list[name]`, and the
//!   `ValueError` that `etree.QName(comment.tag)` throws if a comment is among
//!   an action's argument children. Here all of those answer with a SOAP fault
//!   and HTTP 500, because "the control point gets an HTML page and cannot
//!   parse it" is not a behaviour worth porting. The messages Python logged are
//!   preserved verbatim in our log lines so that anyone comparing a Python and
//!   a Rust transcript reads the same words.
//! * **`402 'Invalid Args'`** — parse failure (lxml's equivalent), a body that
//!   is not UTF-8, and a missing/empty `<s:Body>`; plus, new here, a
//!   `SetAVTransportURI` with no usable `CurrentURI` (see the next bullet).
//! * **`401 'Invalid Action'`** — an action element with no `{namespace}local`
//!   form, a service URN too short to name a service, an unknown service, an
//!   action the service does not declare. Python raised (⇒ HTML 500) for the
//!   last two; `protocol.py` has **one** `soap_fault` call site in this path,
//!   `:930`, and it is the `402`.
//! * **`501 'Action Failed'`** — a state-variable name that the SCPD does not
//!   declare, on the write path, and a state name the store does not hold, on
//!   the read path. Both are internal inconsistencies, and 501 is UPnP Device
//!   Architecture's own category for them: *the action failed because of an
//!   error in the action (e.g. invalid state variables or variable values)*.
//!   Two readings were considered and rejected: 500, which loses the
//!   distinction between "the action ran and the device broke" and "the
//!   envelope was nonsense"; and "501 means not implemented", which is HTTP
//!   501's meaning, not SOAP's — and would be a false statement about an action
//!   that did run. Note for anyone grepping this repo for `501`: the ones in
//!   `macast/protocol_airplay.py:66`/`:290` are AirPlay's own error table, not
//!   this one.
//!   The offending name goes in the **log**, not in `errorDescription`,
//!   because that element's text is a standard string a control point may be
//!   matching on.
//! * **`SetAVTransportURI` without a usable `CurrentURI` is refused before
//!   anything reaches the player** (402). Python takes
//!   `data['CurrentURI'].value` — which is `None` both when the argument is
//!   absent and when the element has no text — and passes that `None` straight
//!   to `renderer.set_media_url(None)`, which fails somewhere below this module
//!   with a `TypeError` and an HTML 500. Refusing is the deviation; it is also
//!   the one that keeps a control point from being told "success" about a cast
//!   that never started. It does **not** roll anything back: the SCPD
//!   pre-writes for the arguments that *were* sent (`InstanceID`, and
//!   `CurrentURIMetaData` when present) have already landed by the time the
//!   refusal happens, exactly as they would have in Python.
//! * **The service name comes out of `service_name_from_urn`'s `.nth(3)`**,
//!   where Python does `qn.namespace.split(':')[-2]`. For
//!   `urn::::Name:1` Python reads `Name` and we read `''`. Both then refuse the
//!   request (there is no service named `Name` in the table either way, and no
//!   service named `''`), and ours produces a readable 401 rather than
//!   accidentally matching a service. `[-2]` is deliberately **not**
//!   re-implemented.
//! * **A comment inside `<s:Body>` or inside the action element creates no
//!   node here.** quick-xml reports comments and processing instructions, and
//!   [`Dom`] keeps elements only. So a comment-only `<s:Body>` reads as
//!   "missing Body/action", whereas Python's loop — which skips comments to
//!   find the action element — may reach `no action element` instead, depending
//!   on whether lxml's `len()` counts comment nodes. **I have not measured
//!   that**, and the two sentences differ only in a log line while both make
//!   Python raise, so the collapse is recorded instead of researched. Same for
//!   a comment among an action's argument children: Python raises (`etree.
//!   QName` on a comment's tag); we simply do not see the node.
//! * **`CurrentTrackMetaData` stores the sender's own text, not lxml's
//!   re-serialization.** Python's success branch does
//!   `etree.tostring(meta, encoding="UTF-8", xml_declaration=False)`, which
//!   normalises self-closing tags, quote style, attribute order and whitespace
//!   — that is a *different string* from what arrived. [`Dom`] is a flat arena
//!   that keeps no source bytes, so inventing a serializer here would be a
//!   second XML writer in the crate, worse than the deviation. Why this is
//!   invisible on our wire: `CurrentTrackMetaData` is **not** in
//!   [`store::OBSERVED`] (`store.rs:60-70`), so it is never enqueued for GENA,
//!   and `GetPositionInfo`'s `TrackMetaData` is the only place it could leave —
//!   which means it reaches a control point only if that control point reads it
//!   back *through us*, where it will get the bytes the sender wrote rather
//!   than their canonical form. `store.rs:97-99` is the other half of that
//!   argument: this module is the only thing that decides who hears about a
//!   change.
//! * **`Seek` with no `Target` sends `""` to the player, not `null`.**
//!   `macast_renderer/mpv.py:227` does `send_command(['seek', data, 'absolute'])`
//!   with `data = None`, which JSON-encodes as `null`. Measured (`store.rs:180-206`,
//!   from `macast-mpv/src/player.rs:533` and `/tmp/seek_arg_probe.py`):
//!   `["seek","not-a-time",…]` and `["seek",null,…]` get
//!   `invalid parameter` — mpv rejecting the *shape* — while
//!   `["seek","00:01:23","absolute"]` and `["seek",83,…]` are accepted against
//!   an idle player. **`["seek","",…]` has never been measured.** It is
//!   written here as the unmeasured neighbour of the measured `null`, not as
//!   equivalent to it, because [`store::Renderer::set_media_position`] takes a
//!   `&str` and there is no way to say `null` through it.
//! * **The `Allow:` response header is not reproduced**, and it would be wrong
//!   to do so. The shipped receiver answers
//!   `COOKIE_BROWSERS, COOKIE_JAR_NAME, FILE_GATE_DEFAULT, FILE_ROUTES, GET,
//!   HEAD, POST, POST_HELPER_PARAMS, POST_ROUTES, RESOLVE_JOBS_MAX,
//!   RESOLVE_JOB_TTL, SUBSCRIBE, UNSUBSCRIBE, _CODE_EXECUTION_PARAMS,
//!   _MANAGEMENT_PARAMS` (measured, `/tmp/dlna-http-witnesses.json`) — those are
//!   CherryPy's `Handler` **attribute names**, not HTTP methods. That header is
//!   `http.rs`'s business; it belongs in this doc because the witness file is
//!   also where this module's two byte-exact envelopes came from.
//!
//! # Log text is not the wire contract
//!
//! The two repr helpers below ([`py_str_repr`], [`bytes_repr`]) exist so that
//! a Rust transcript reads like a Python one, and they are **copies**. Three
//! separate reasons a copy is required rather than an import: the two in
//! `ssdp.rs` (`py_repr:1040`, `bytes_repr:1100`) are private to that module;
//! `macast-mpv`'s `pyops.rs:307` is public but lives in the *player* crate, and
//! a wire crate reaching into it would be the wrong direction of dependency;
//! `store.rs`'s `py_float:296` is private and, in any case, [`store::wire`]
//! already answers the only question this module has about numbers. Nothing
//! that is asserted about the wire format is asserted about log text — if a
//! repr is ever wrong, the fix must not be able to break a control point.
//!
//! [`crate::lib`]: crate

use std::sync::Arc;

use macast_core::logging::Logger;
use macast_core::setting::Setting;
use serde_json::Value;

use crate::scpd::{clark, service_name_from_urn, Dom, Scpd};
use crate::store::{is_observed, wire, SharedBroadcast, SharedRenderer, SharedStore};

/// The SOAP envelope namespace, verbatim from `macast/protocol.py`'s `soap_ns`
/// and the `SOAP_ENV_NS` constant the fault builder uses.
pub const SOAP_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";

/// `s:encodingStyle`, which the shipped receiver puts on every envelope it
/// builds, responses and faults alike.
pub const SOAP_ENCODING_NS: &str = "http://schemas.xmlsoap.org/soap/encoding/";

/// The namespace of the `<UPnPError>` element inside a fault's `<detail>`.
///
/// lxml serialises it with a generated prefix — the measured bytes say `ns0`,
/// and that is what a control point that bothers to look has seen from us. It
/// is a *content* choice, not an accident: nothing in the SOAP reading code
/// depends on the prefix, and `urn:schemas-upnp-org:control-1-0` is what the
/// element means.
pub const UPNP_CONTROL_NS: &str = "urn:schemas-upnp-org:control-1-0";

/// The `Content-Type` every SOAP answer carries, faults included. Measured:
/// `text/xml;charset=utf-8`, no space, no `+`-of-another-spelling.
pub const SOAP_CONTENT_TYPE: &str = "text/xml;charset=utf-8";

/// The `<detail>` element's prefix as the shipped receiver spells it.
const UPNP_ERROR_PREFIX: &str = "ns0";

/// Indentation and byte-order-mark tolerant SOAP parsing.
///
/// ```text
/// if isinstance(rawbody, str):
///     rawbody = rawbody.encode('utf-8')
/// return rawbody.lstrip(b'\xef\xbb\xbf \t\r\n')
/// ```
///
/// That is `macast/protocol.py`'s `normalize_soap_body`, and the reason it
/// exists is BiliPai/okhttp, which indents its envelope by twelve spaces
/// before the `<?xml …?>` declaration. An XML declaration that is not at the
/// very first byte is not a declaration, so lxml refuses the whole document,
/// and every `SetAVTransportURI` from that app was an HTTP 500.
///
/// It is a **byte set**, not a prefix rule: Python's `lstrip(b'\xef\xbb\xbf
/// \t\r\n')` strips any of those seven bytes in any order, so a body that
/// begins ` \n\xef…` loses all three. That is what the loop below does, and it
/// is why this takes and returns bytes.
///
/// It always returns bytes — including for a body that decoded fine — because
/// lxml refuses a `str` that carries an encoding declaration, so a decoded body
/// must never be handed back to it. The Rust reader decodes after normalizing
/// instead (`str::from_utf8` below), which is the same order in everything but
/// the type: normalize first, then decide whether the bytes are text.
pub fn normalize_soap_body(raw: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < raw.len() && matches!(raw[i], 0xEF | 0xBB | 0xBF | b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    &raw[i..]
}

/// `&` `<` `>` only, which is what a text node needs. `"` and `'` stay raw —
/// in text they are just characters, and Python's serializer leaves them alone.
///
/// `pub` because `eventing.rs` uses it for the `val="…"` attributes of a
/// GENA property set, where `"` would otherwise end the attribute.
pub fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// [`escape_text`] plus `"` → `&quot;`, for attribute values.
pub fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// A `<s:Fault>` envelope: the 408-byte measured shape, byte for byte.
///
/// ```text
/// <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
///             s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
///   <s:Body><s:Fault>
///     <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>
///     <detail><ns0:UPnPError xmlns:ns0="urn:schemas-upnp-org:control-1-0">
///       <errorCode>402</errorCode><errorDescription>Invalid Args</errorDescription>
///     </ns0:UPnPError></detail>
///   </s:Fault></s:Body></s:Envelope>
/// ```
///
/// Built as a string, not through [`Dom`], because this is a writer and `Dom`
/// is a reader with no serializer (see the `CurrentTrackMetaData` deviation in
/// the module doc). Python's `soap_fault` takes a third parameter,
/// `fault_code='s:Client'`, which every call site in the repo leaves at its
/// default; that knob is not modelled here, and `faultcode` is fixed — the one
/// place a caller could have changed it is a call site that does not exist.
pub fn soap_fault(code: i64, description: &str) -> Vec<u8> {
    let mut out = String::from("<s:Envelope xmlns:s=\"");
    out.push_str(SOAP_ENV_NS);
    out.push_str("\" s:encodingStyle=\"");
    out.push_str(SOAP_ENCODING_NS);
    out.push_str("\"><s:Body><s:Fault><faultcode>s:Client</faultcode>");
    out.push_str("<faultstring>UPnPError</faultstring><detail><");
    out.push_str(UPNP_ERROR_PREFIX);
    out.push_str(":UPnPError xmlns:");
    out.push_str(UPNP_ERROR_PREFIX);
    out.push_str("=\"");
    out.push_str(UPNP_CONTROL_NS);
    out.push_str("\"><errorCode>");
    out.push_str(&code.to_string());
    out.push_str("</errorCode><errorDescription>");
    out.push_str(&escape_text(description));
    out.push_str("</errorDescription></");
    out.push_str(UPNP_ERROR_PREFIX);
    out.push_str(":UPnPError></detail></s:Fault></s:Body></s:Envelope>");
    out.into_bytes()
}

/// What the SOAP layer decided, for `http.rs` to put on the wire.
///
/// Deliberately has no `ext` field: `Ext:` is not a SOAP decision. The shipped
/// receiver sends `Ext: ''` on the two SOAP routes and **not at all** on
/// `/description.xml` (measured, in that order: `Content-Type, Server, Date,
/// Allow, Ext, Content-Length` for the action routes; `Content-Type, Server,
/// Date, Allow, Content-Length` for the description), so it is a property of
/// the route. `http.rs` owns it.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

/// A refusal, before it knows it is an HTTP response.
struct Failure {
    code: i64,
    description: String,
}

impl Failure {
    fn new(code: i64, description: impl Into<String>) -> Self {
        Self {
            code,
            description: description.into(),
        }
    }

    fn into_reply(self) -> Reply {
        Reply {
            status: 500,
            content_type: SOAP_CONTENT_TYPE,
            body: soap_fault(self.code, &self.description),
        }
    }
}

/// The bag `call()` builds into `param`, with Python's dict semantics.
///
/// Two Python facts decide this representation. First, the **key's presence**
/// is a real question — `arg.name in param` gates both the SCPD pre-writes and
/// the `Argument(…, param[arg.name] if arg.name in param else None)` default,
/// and they are different questions: an absent key means "the control point did
/// not say anything about this", a present key with no text means "it said
/// nothing". Second, Python's `param[node.tag] = node.text` is an **assignment
/// into a dict**, so a repeated element keeps its original position and
/// overwrites its value; [`Params::insert` keeps that, and the insertion order
/// is observable because `logger.info("{} {}".format(method, param))` prints
/// the dict.
///
/// `Option<String>` is where two Python shapes collapse into one: lxml gives
/// `None` for `<CurrentURI></CurrentURI>` (`node.text` is `None` when there is
/// no text before the first child element), and `call()` gives `None` for a key
/// that is absent (`param[arg.name] if arg.name in param else None`). Python
/// cannot tell them apart either — both reach `Argument.value` as `None`.
#[derive(Debug, Default)]
struct Params {
    items: Vec<(String, Option<String>)>,
}

impl Params {
    /// Assignment into a dict: an existing key keeps its position.
    fn insert(&mut self, key: String, value: Option<String>) {
        for (k, v) in self.items.iter_mut() {
            if *k == key {
                *v = value;
                return;
            }
        }
        self.items.push((key, value));
    }

    /// `arg.name in param` — **true for a text-less child too**, which is what
    /// makes `set_state(arg.state, None)` a real write rather than a no-op.
    fn holds(&self, name: &str) -> bool {
        self.items.iter().any(|(k, _)| k == name)
    }

    /// `Argument.value`: the text, or `null` for absent-or-textless.
    fn value(&self, name: &str) -> Value {
        match self.items.iter().find(|(k, _)| k == name) {
            Some((_, Some(text))) => Value::String(text.clone()),
            _ => Value::Null,
        }
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }

    /// The `&str` form for the seams that cannot express "nothing", with the
    /// cost stated where it is paid: see the `Seek` deviation in the module doc
    /// for what `""` does and does not prove.
    fn text_or_empty(&self, name: &str) -> &str {
        self.text(name).unwrap_or("")
    }

    /// What Python prints for this dict, in Python's spelling: single quotes
    /// unless that would need escaping and double quotes would not, `None` for
    /// a value the control point left unsaid.
    fn py_dict_repr(&self) -> String {
        let mut out = String::from("{");
        for (i, (key, value)) in self.items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&py_str_repr(key));
            out.push_str(": ");
            match value {
                Some(text) => out.push_str(&py_str_repr(text)),
                None => out.push_str("None"),
            }
        }
        out.push('}');
        out
    }
}

/// The `res` dict, which becomes the response element's children in this order.
///
/// `Vec` rather than a map because Python's dict order is the wire order here:
/// `for key in res: prop = SubElement(response, key)` walks the insertion
/// order, so `GetPositionInfo`'s eight outputs come out in the SCPD's order
/// and a control point that reads by position — there are such readers, they
/// are why `lib.rs` says an ordered `Vec` twice — depends on it.
#[derive(Debug, Default)]
struct Res {
    items: Vec<(String, Value)>,
}

impl Res {
    fn insert(&mut self, key: String, value: Value) {
        for (k, v) in self.items.iter_mut() {
            if *k == key {
                *v = value;
                return;
            }
        }
        self.items.push((key, value));
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn py_dict_repr(&self) -> String {
        let mut out = String::from("{");
        for (i, (key, value)) in self.items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&py_str_repr(key));
            out.push_str(": ");
            out.push_str(&py_value_repr(value));
        }
        out.push('}');
        out
    }
}

/// The nine names `call()` can dispatch to. See the census in the module doc:
/// this list is complete, and its completeness is what makes every `Get*` a
/// read.
#[derive(Clone, Copy)]
enum HandlerKind {
    SetUri,
    Play,
    Pause,
    Seek,
    Stop,
    Next,
    Previous,
    SetVolume,
    SetMute,
}

impl HandlerKind {
    fn for_names(service: &str, action: &str) -> Option<Self> {
        match (service, action) {
            ("AVTransport", "SetAVTransportURI") => Some(Self::SetUri),
            ("AVTransport", "Play") => Some(Self::Play),
            ("AVTransport", "Pause") => Some(Self::Pause),
            ("AVTransport", "Seek") => Some(Self::Seek),
            ("AVTransport", "Stop") => Some(Self::Stop),
            ("AVTransport", "Next") => Some(Self::Next),
            ("AVTransport", "Previous") => Some(Self::Previous),
            ("RenderingControl", "SetVolume") => Some(Self::SetVolume),
            ("RenderingControl", "SetMute") => Some(Self::SetMute),
            _ => None,
        }
    }
}

/// The actions whose reply is big enough, and frequent enough, that printing it
/// would be noise. For these the log line prints the **method name** instead of
/// the dict — the line keeps its twenty asterisks and its `res: `, so a
/// transcript still shows that the call happened and answered.
///
/// `macast/protocol.py:979-983`. These lines are announced, not suppressed: a
/// gap in the log would read as "the control point never asked".
const NOISY_RESPONSES: [&str; 2] = [
    "ConnectionManager_GetProtocolInfo",
    "AVTransport_GetPositionInfo",
];

/// The three polling names whose *request* line is dropped
/// (`macast/protocol.py:953-957`). A control point polls `GetPositionInfo` and
/// `GetTransportInfo` about once a second while a phone screen is open, so the
/// request half of that transcript is this receiver's version of §4.2's "92%
/// of the log is saying nothing changed".
fn logs_request(method: &str) -> bool {
    !matches!(
        method,
        "AVTransport_GetPositionInfo" | "AVTransport_GetTransportInfo" | "RenderingControl_GetVolume"
    )
}

/// The response log line, factored out so the `NOISY_RESPONSES` substitution
/// cannot change the line's shape.
fn response_line(payload: &str) -> String {
    format!("{}res: {}", "*".repeat(20), payload)
}

/// `{namespace}local` — the shape every element in a SOAP envelope has, and the
/// only way [`Dom`] spells a name.
///
/// The `strip_prefix` is not decoration: a tag that contains `}` but does not
/// *begin* with `{` is not a Clark notation name, and guessing at it would make
/// `{}`-prefixed and unprefixed names read the same.
fn split_clark(tag: &str) -> Option<(&str, &str)> {
    let (head, local) = tag.rsplit_once('}')?;
    Some((head.strip_prefix('{')?, local))
}

/// The local part of a Clark notation tag, and the tag itself when it has no
/// namespace at all — which is `macast/protocol.py:157`'s `_local_name`, kept
/// because a real DIDL-Lite document arrives both ways. Its only caller in
/// Python is the DIDL walk at `protocol.py:149`.
fn local_name(tag: &str) -> &str {
    match tag.rsplit_once('}') {
        Some((_, local)) => local,
        None => tag,
    }
}

/// The first `protocolInfo` attribute in a DIDL-Lite document, or `""`.
///
/// `macast/protocol.py:143-151`:
///
/// ```python
/// def protocol_info_from_didl(meta):
///     if meta is None: return ''
///     for element in meta.iter():            # <-- INCLUDES self
///         tag = _local_name(element.tag)
///         if tag == 'res':
///             info = element.get('protocolInfo')
///             if info: return info
///     return ''
/// ```
///
/// Two details are load-bearing and both are easy to "tidy" into a bug. The
/// walk **includes the root** — hence [`Dom::iter_all`] rather than
/// `iter_subtree` over the children — because lxml's `.iter()` is self-inclusive
/// and a sender whose root element *is* the `res` would otherwise read as "no
/// protocol info". And the name is compared as a **local name**, not against a
/// namespace: the same element arrives as `res`, `{urn:schemas-upnp-org:DIDL-Lite:1}res`,
/// and with the namespace spelled differently by different firmware, which is
/// why this cannot be a `find(clark(DIDL_NS, "res"))`.
fn protocol_info_from_didl(dom: Option<&Dom>) -> String {
    let Some(dom) = dom else {
        return String::new();
    };
    for node in dom.iter_all(dom.root()) {
        if local_name(dom.tag(node)) == "res" {
            if let Some(info) = dom.attr(node, "protocolInfo") {
                if !info.is_empty() {
                    return info.to_string();
                }
            }
        }
    }
    String::new()
}

/// `DLNA.ORG_OP=`'s two hex digits, read as the sender's statement about
/// seeking: `00` means "this stream may not be range-requested", so **it is a
/// live stream**.
///
/// ```python
/// _ORG_OP_RE = re.compile(r'DLNA\.ORG_OP=([0-9a-fA-F]{2})')
///
/// def live_from_protocol_info(text):
///     if not isinstance(text, str): return None
///     match = _ORG_OP_RE.search(text)
///     if match is None: return None
///     try: value = int(match.group(1), 16)
///     except ValueError: return None
///     return value == 0
/// ```
///
/// **`None` is not `False`** — it means "keep today's behaviour". That is the
/// whole reason this is a three-state read and not a boolean, and AGENTS.md
/// §4.1 spells out the consequence of getting it backwards: our own two shapes
/// that *look* like files (screen_mirror's "disguised as a file" target and the
/// web-casting `/relay/<id>/media`) would be judged seekable and handed into a
/// read-ahead, which is exactly the bug that decision was made to avoid.
///
/// Read as a byte scan, not a regex: this crate has no regex dependency
/// (`Cargo.toml`), and the pattern is a fixed literal followed by two hex
/// digits. The scan walks every occurrence so that a `DLNA.ORG_OP=` **not**
/// followed by two hex digits finds a later one, which is what `re.search`'s
/// backtracking does; a one-pass "first literal only" scan would answer `None`
/// where Python answers `True`/`False`. The `int(…, 16)` failure branch is
/// unreachable by construction — the regex already required two hex digits — so
/// it is not reproduced, and nothing is claimed about it.
fn live_from_protocol_info(text: &str) -> Option<bool> {
    const MARKER: &[u8] = b"DLNA.ORG_OP=";
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(offset) = find(bytes, from, MARKER) {
        let start = offset + MARKER.len();
        if let (Some(&hi), Some(&lo)) = (bytes.get(start), bytes.get(start + 1)) {
            if let (Some(h), Some(l)) = (hex(hi), hex(lo)) {
                return Some(h * 16 + l == 0);
            }
        }
        from = offset + MARKER.len();
    }
    None
}

/// `memchr` without the dependency: the first occurrence of `needle` at or
/// after `from`.
fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Python's `repr()` of a `str`, for log lines.
///
/// The rules, each of which is why this is not `format!("{:?}")`: the quote
/// flips to `"` **only** when the text contains `'` and does not contain `"`;
/// `\\`, `\n`, `\r`, `\t` and the active quote are escaped; every other
/// non-printable code point becomes `\xNN` below 0x100, `\uXXXX` below 0x10000,
/// `\UXXXXXXXX` above.
///
/// Two honest limits. [`is_printable`] is a range rule, while CPython asks
/// `unicodedata`: so an exotic unprintable scalar (a format character above
/// U+00A0, say) prints as itself here where Python would escape it. And
/// `{:?}` is not a shortcut anyway — Rust escapes every non-ASCII printable the
/// same way it escapes the unprintable ones, which would turn a Chinese cast
/// title into a wall of `\uXXXX`, the opposite of what a person reading this
/// log needs. This is log text; see the module doc's "Log text is not the wire
/// contract".
fn py_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

fn is_printable(ch: char) -> bool {
    if ch.is_ascii() {
        return !ch.is_ascii_control();
    }
    let code = ch as u32;
    // C1 controls (0x80-0x9f) and no-break space (0xa0) are not printable;
    // everything above that is treated as printable. See py_str_repr.
    code > 0xa0
}

/// Python's `repr()` of `bytes`, which is what `call()`'s parse-failure line
/// prints for `rawbody[:300]` (`head={!r}`, against the caller's **original**
/// bytes — see [`Soap::parse_failed`]).
///
/// `b'…'` with the printable range `0x20..=0x7e` shown as themselves and
/// everything else as `\xNN`; the same quote-flipping rule as [`py_str_repr`].
fn bytes_repr(data: &[u8]) -> String {
    let quote = if data.contains(&b'\'') && !data.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::with_capacity(data.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &byte in data {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c as char);
            }
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push(quote as char);
    out
}

/// Python's `repr()` of a value, for the two log lines that print a dict.
///
/// A Python dict prints its **values with `repr()`**, so a string in `res`
/// shows up quoted in the log and unquoted on the wire — see
/// [`py_value_text`], which is the other half of this pair and the one
/// `build_response` calls.
///
/// The shape is worth stating because it is the whole difference between
/// `str()` and `repr()` in this range: for `None`, `True`, `False`, an integer
/// and a float the two are identical; for `None` Python prints the four
/// letters, for booleans `True`/`False` with a capital, and for numbers both go
/// through `repr`, so [`wire`] already answers all of them. Only `str` differs
/// — `repr()` adds the quotes `str()` drops. Hence: strings repr'd, everything
/// else handed to the function that is the wire contract.
///
/// One honest deviation inherited from [`wire`], which documents it at
/// `store.rs:265-288`: a list or a dict prints as **JSON**, not as Python's
/// repr (`wire`'s own words — "the closest honest answer is what Python prints
/// for it, and JSON is that for the shapes we could ever receive"). Our writers
/// do not produce those shapes; the module doc for the store trait says so, so
/// it is not repeated into an assertion here.
fn py_value_repr(value: &Value) -> String {
    match value {
        Value::String(text) => py_str_repr(text),
        other => wire(other),
    }
}

/// Python's `str()` of a value that is going into a SOAP text node — the exact
/// expression at `protocol.py:995`, `prop.text = str(res[key])`.
///
/// This is **not** [`py_value_repr`], and the difference is on the wire: a
/// `String` answers with its own text, not with that text in quotes. Two
/// independent witnesses, both read this round:
///
/// * the source line above — `str()`, not `repr()`;
/// * the measured 399-byte `GetTransportInfo` reply
///   (`/tmp/dlna-http-witnesses.json`, asserted verbatim in
///   `GetTransportInfo_is_the_measured_399_bytes`), whose first child is
///   `<CurrentTransportState>STOPPED</CurrentTransportState>`. A repr would put
///   `'STOPPED'` between the tags, and every control point that parses the
///   value as an enum name would reject it.
///
/// Everything that is not a string is unchanged from `repr()` — for `None`,
/// `True`, `False`, an int and a float, `str(x) == repr(x)` — so those still go
/// through [`wire`], and a container keeps Python's `str(list) == repr(list)`
/// shape via [`py_value_repr`].
fn py_value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_value_repr(other),
    }
}

/// The SOAP endpoint of a DLNA renderer: `POST /<service>/action`.
///
/// Cheap to construct and thread-safe by construction — everything mutable
/// lives behind the four seams it was given, so `http.rs` may hand one of these
/// to as many worker threads as it likes, which is what CherryPy does with the
/// Python one (and what AGENTS.md §4.2's copy-on-write lesson is about for the
/// GENA table next door).
pub struct Soap {
    log: Arc<Logger>,
    scpd: Arc<Scpd>,
    store: SharedStore,
    renderer: SharedRenderer,
    broadcast: SharedBroadcast,
    setting: Arc<Setting>,
}

impl Soap {
    /// The logger is named `Protocol` because that is the name Python's
    /// `protocol.py:30` uses for this same code, and log-module routing
    /// (`macast/logsplit.py`, AGENTS.md §4.10b) is keyed on it.
    pub fn new(
        scpd: Arc<Scpd>,
        store: SharedStore,
        renderer: SharedRenderer,
        broadcast: SharedBroadcast,
        setting: Arc<Setting>,
    ) -> Self {
        Self {
            log: Logger::named("Protocol"),
            scpd,
            store,
            renderer,
            broadcast,
            setting,
        }
    }

    /// Answer one SOAP request. `rawbody` is the **un-normalized** body,
    /// exactly as Python's `call()` receives it.
    pub fn handle(&self, rawbody: &[u8]) -> Reply {
        match self.dispatch(rawbody) {
            Ok(body) => Reply {
                status: 200,
                content_type: SOAP_CONTENT_TYPE,
                body,
            },
            Err(failure) => failure.into_reply(),
        }
    }

    /// The refused-parse log line, verbatim in structure from
    /// `macast/protocol.py:921-926`:
    ///
    /// ```python
    /// logger.error('SOAP parse failed: {} ({} bytes, head={!r})'.format(
    ///     e, len(rawbody), rawbody[:300]))
    /// ```
    ///
    /// The detail that took me by surprise when reading it, and that the Rust
    /// signature therefore enforces: `normalize_soap_body(rawbody)` is called
    /// **inside** the expression being tried, and its result is never bound, so
    /// the `len()` and the `head=` in that line describe the caller's **original**
    /// bytes — BOM and indentation included. A log line that reports the
    /// normalized body would report a length nobody sent. Hence this takes
    /// `rawbody` and not the slice [`Self::dispatch`] is working on.
    fn parse_failed(&self, rawbody: &[u8], why: &str) -> Failure {
        let head = &rawbody[..rawbody.len().min(300)];
        self.log.error(&format!(
            "SOAP parse failed: {why} ({} bytes, head={})",
            rawbody.len(),
            bytes_repr(head)
        ));
        Failure::new(402, "Invalid Args")
    }

    /// The ordered reading of `call()` (`:908-960`). Each step names the Python
    /// line it mirrors, in the order it happens there, because the order *is*
    /// the contract: the request log comes after the parameters are read and
    /// before the schema is consulted, and the schema is consulted **once**,
    /// before the branch, so an action we implement but the document does not
    /// declare cannot reach a handler.
    fn dispatch(&self, rawbody: &[u8]) -> Result<Vec<u8>, Failure> {
        let normalized = normalize_soap_body(rawbody);
        let text = std::str::from_utf8(normalized)
            .map_err(|e| self.parse_failed(rawbody, &e.to_string()))?;
        let dom = Dom::parse(text).map_err(|e| self.parse_failed(rawbody, &e))?;

        // `body = envelope.find('{soap_ns}Body')` / `if body is None or len(body) == 0`
        let Some(body) = dom.find(dom.root(), &clark(SOAP_ENV_NS, "Body")) else {
            self.log
                .error("malformed SOAP request: missing Body/action");
            return Err(Failure::new(402, "Invalid Args"));
        };
        let Some(el) = dom.children(body).first().copied() else {
            self.log
                .error("malformed SOAP request: missing Body/action");
            return Err(Failure::new(402, "Invalid Args"));
        };

        // `qn = etree.QName(action_el.tag); action = qn.localname;
        //  service = qn.namespace.split(':')[-2]`
        let Some((namespace, action)) = split_clark(dom.tag(el)) else {
            self.log.error(&format!(
                "malformed SOAP request: action element without a namespace: {}",
                dom.tag(el)
            ));
            return Err(Failure::new(401, "Invalid Action"));
        };
        let name = match service_name_from_urn(namespace) {
            Ok(name) => name,
            Err(why) => {
                self.log.error(&format!("unknown service type: {why}"));
                return Err(Failure::new(401, "Invalid Action"));
            }
        };

        // `param = {}; for node in action_el: param[QName(node.tag).localname] = node.text`
        let mut params = Params::default();
        for &child in dom.children(el) {
            params.insert(
                local_name(dom.tag(child)).to_string(),
                dom.text(child).map(str::to_string),
            );
        }

        let method = format!("{name}_{action}");
        let kind = HandlerKind::for_names(name, action);

        if logs_request(&method) {
            self.log
                .info(&format!("{} {}", method, params.py_dict_repr()));
        }

        // `service_type = Service.get(service)` and
        // `service_type.actions[action]` — one lookup, before the branch.
        let Some(service_type) = self.scpd.service(name) else {
            self.log.error(&format!("unknown service: {name}"));
            return Err(Failure::new(401, "Invalid Action"));
        };
        let Some(action_schema) = service_type.action(action) else {
            self.log
                .error(&format!("action not declared by {}: {action}", service_type.name));
            return Err(Failure::new(401, "Invalid Action"));
        };

        let mut res = Res::default();
        if let Some(kind) = kind {
            // `for arg in service_type.actions[action].input:` — the SCPD
            // pre-writes. Note what is **not** here: `is_observed`. That gate
            // lives inside `set_state`, because the name's *observability* is
            // not this loop's question; whether the value changed is, and only
            // the store can answer that.
            for arg in &action_schema.input {
                if params.holds(&arg.name) {
                    self.set_state(&arg.state, params.value(&arg.name))?;
                }
            }
            self.run_handler(kind, &params)?;
        } else {
            // `for arg in service_type.actions[action].output:
            //     res[arg.name] = self.state_list[arg.state].value`
            for arg in &action_schema.output {
                match self.store.read(&arg.state) {
                    Some(value) => res.insert(arg.name.clone(), value),
                    None => {
                        self.log.error(&format!(
                            "state variable missing from the table: {} ({})",
                            arg.state, method
                        ));
                        return Err(Failure::new(501, "Action Failed"));
                    }
                }
            }
        }

        let payload = if NOISY_RESPONSES.contains(&method.as_str()) {
            method.clone()
        } else {
            res.py_dict_repr()
        };
        self.log.info(&response_line(&payload));

        // The response builder (`:984-993`). `u:` is a *prefixed* binding, so
        // the default namespace stays unbound and every child of the response
        // element is emitted **unprefixed with no `xmlns=""`** — that is what
        // the measured 399-byte envelope shows, and it is the single most
        // common way a hand-built SOAP response is wrong on the wire.
        Ok(self.build_response(action, &service_type.namespace, &res))
    }

    /// Write one state variable, and decide — here, at the only call site that
    /// has both the boolean and the observed table in hand — whether anybody
    /// hears about it.
    ///
    /// ```python
    /// changed = self.state_list[name].value != value
    /// if changed and (name in SERVICE_STATE_OBSERVED['AVTransport'] or
    ///                 name in SERVICE_STATE_OBSERVED['RenderingControl']):
    ///     logger.debug("setState: {} {}".format(name, value))
    ///     self.state_queue.put((name, value)); self._state_event.set()
    /// if changed:
    ///     self.state_list[name].value = value
    /// ```
    ///
    /// Four things this shape is guarding, in the order the code does them:
    ///
    /// 1. **The name must exist in the SCPD.** Python's `state_list[name]` is a
    ///    `KeyError` **on purpose** — an undeclared name is our bug, not a
    ///    control point's, and the dict has no default. `store.write` is
    ///    therefore never reached for such a name. Note that the test is
    ///    `Scpd::state_index` and **not** `store.read(name).is_none()`: a table
    ///    may legitimately *hold* `null` (that is [`Value::Null`], which
    ///    `StateStore::read` distinguishes from "no such name" — see its doc at
    ///    `store.rs:104`), and conflating the two would refuse a valid write.
    /// 2. **The store answers whether it changed.** `Value::Null` is written
    ///    even for an absent element, so "did this change?" is the store's
    ///    question, not ours.
    /// 3. **Only a change that is *observed* is announced.** That is
    ///    [`store::Broadcast`]'s contractual precondition, spelled out at
    ///    `store.rs:240-248`: an implementation that enqueued on every write
    ///    would reintroduce the 92% of a real log that AGENTS.md §4.2 measured
    ///    — 464 of 505 duration reports were the same number. The `is_observed`
    ///    list is `store.rs:60-70`, and it is deliberately narrower than the
    ///    names this module writes: `CurrentTrackURI`, `CurrentTrackMetaData`,
    ///    `CurrentTrackTitle`, `RelativeTimePosition` and `AbsoluteTimePosition`
    ///    all change on a normal cast and announce nothing. Both halves of this
    ///    gate have a witness in the test
    ///    `next_and_previous_walk_the_queue_and_stop_at_both_ends`: a step that
    ///    *does* change `TransportStatus` must reach the queue, and the identical
    ///    repeat one step later must not. A suite that only proves the first half
    ///    passes when the gate is deleted.
    /// 4. **The value the store keeps is the value that was written**, so
    ///    `clone()` feeds the store and the `&value` borrow feeds the bus.
    ///
    /// The debug line comes **before** the enqueue because that is Python's
    /// order, and the order shows up in a transcript: a reader who sees
    /// `setState: TransportState PLAYING` and no NOTIFY can tell "we did not
    /// announce it" from "we announced it and the socket was dead".
    fn set_state(&self, name: &str, value: Value) -> Result<(), Failure> {
        if self.scpd.state_index(name).is_none() {
            self.log.error(&format!(
                "state variable not declared by the SCPD: {name} — this is our table, not the control point's"
            ));
            return Err(Failure::new(501, "Action Failed"));
        }
        let changed = self.store.write(name, value.clone());
        if changed && is_observed(name) {
            self.log.debug(&format!("setState: {} {}", name, wire(&value)));
            self.broadcast.enqueue(name, &value);
        }
        Ok(())
    }

    /// `self.set_state('CurrentTrackURI', uri)`, which appears at three sites
    /// in Python (`protocol.py:1321-1322` among them) and is the one write that
    /// every address change shares. Factored so the *name* has one spelling.
    ///
    /// `CurrentTrackURI` is **not** in [`store::OBSERVED`]: it changes the table
    /// and enqueues nothing.
    fn set_state_url(&self, uri: &str) -> Result<(), Failure> {
        self.set_state("CurrentTrackURI", Value::String(uri.to_string()))
    }

    fn run_handler(&self, kind: HandlerKind, params: &Params) -> Result<(), Failure> {
        match kind {
            HandlerKind::SetUri => self.av_transport_set_uri(params),
            HandlerKind::Play => {
                self.renderer.set_media_resume();
                self.set_state("TransportState", Value::String("PLAYING".into()))?;
                self.set_state("TransportStatus", Value::String("OK".into()))?;
                Ok(())
            }
            HandlerKind::Pause => {
                self.renderer.set_media_pause();
                self.set_state("TransportState", Value::String("PAUSED_PLAYBACK".into()))
            }
            HandlerKind::Seek => {
                // `target.value` twice: once for the player, where the seam is a
                // `&str` and an absent Target therefore says `""` (unmeasured —
                // module doc), and once for the state table, where an absent
                // Target is a real `null` write, exactly as Python stores it.
                let target = params.value("Target");
                self.renderer.set_media_position(params.text_or_empty("Target"));
                self.set_state("RelativeTimePosition", target.clone())?;
                self.set_state("AbsoluteTimePosition", target)
            }
            HandlerKind::Stop => {
                self.renderer.set_media_stop();
                self.set_state("TransportState", Value::String("STOPPED".into()))
            }
            HandlerKind::Next => self.playlist_step(1),
            HandlerKind::Previous => self.playlist_step(-1),
            HandlerKind::SetVolume => {
                // The WIRE value, i.e. a string: `mpv.py:139` does
                // `send_command(['set_property','volume', data])` with whatever
                // arrived, and also `set_media_text(f'Volume: {data}')`. Python
                // never coerces it, so neither do we.
                let volume = params.value("DesiredVolume");
                self.renderer.set_media_volume(volume);
                Ok(())
            }
            HandlerKind::SetMute => {
                // ```python
                // if mute.value == 0 or mute.value == '0': mute = False
                // else: mute = True
                // ```
                // A real `bool` reaches the renderer (`mpv.py:145` turns it into
                // `"yes"`/`"no"`). The `== 0` arm is **dead on this path**: the
                // value came from `node.text`, so it is a `str` or `None`, and
                // neither compares equal to the integer `0`. It is kept as an
                // arm of the `match` below rather than deleted, so the two live
                // cases (`"0"` and everything else) are the ones with names.
                let muted = match params.value("DesiredMute") {
                    Value::String(ref text) => text != "0",
                    _ => true,
                };
                self.renderer.set_media_mute(Value::Bool(muted));
                Ok(())
            }
        }
    }

    /// `AVTransport_SetAVTransportURI` (`macast/protocol.py:1077-1128`) — the
    /// one handler that does a dozen things, in an order that was changed on
    /// purpose.
    ///
    /// The sequence below is Python's, and the reason for the ordering is
    /// Python's own comment, transcribed because it is the part that must not be
    /// "tidied" back:
    ///
    /// > The sender's own declaration about this stream has to be in place
    /// > **before** the address reaches the player, because the player reads it
    /// > when it decides how much to buffer. That is the only reason the parse
    /// > moved up here: the previous order handed over the URL first and read
    /// > the metadata afterwards, so the declaration could never have steered
    /// > the load it describes. The state writes below keep their old sequence;
    /// > only the two error lines about unreadable metadata now appear one step
    /// > earlier in the log.
    ///
    /// The other comment that belongs to this method, above the parse:
    ///
    /// > Same tolerance as the SOAP body: a client that indents its envelope
    /// > indents the embedded DIDL-Lite metadata too, and a leading newline
    /// > before `<?xml...?>` would cost us the real title.
    ///
    /// That is [`normalize_soap_body`] being used a second time, on the embedded
    /// document, and it is why the title of an indented cast is not silently the
    /// friendly name.
    fn av_transport_set_uri(&self, params: &Params) -> Result<(), Failure> {
        let Some(uri) = params.text("CurrentURI") else {
            self.log.error(&format!(
                "SetAVTransportURI with no CurrentURI to cast: {}",
                params.py_dict_repr()
            ));
            return Err(Failure::new(402, "Invalid Args"));
        };
        let uri = uri.to_string();

        // `logger.info(uri)` — the address on its own line, which is the line
        // AGENTS.md §5 tells you to grep for when a cast "did nothing".
        self.log.info(&uri);

        let mut title = self.setting.friendly_name();
        let raw_meta = params.text("CurrentURIMetaData");
        let parsed = self.parse_meta(raw_meta);

        // Step 1 of the ordering argument: the liveness read, before the swap.
        // A `meta` that parsed but had no readable `dc` binding is still a
        // `meta` — see [`Soap::parse_meta`] for why that case returns a document
        // *and* sets `failed`.
        let live = live_from_protocol_info(&protocol_info_from_didl(parsed.dom.as_ref()));
        self.renderer.note_media_live(live);

        self.set_state_url(&uri)?;
        // `None`, not `"0"`: `SetAVTransportURI` has no start position argument,
        // and a sender that wants one sends `Seek` after this.
        self.renderer.set_media_url(&uri, None);

        if parsed.failed {
            // Python writes `raw_meta`, **unnormalized**, which is `None` when
            // the argument was absent or said nothing. Hence `params.value`, not
            // the normalized text: absent metadata is a `null` write, not `""`.
            self.set_state("CurrentTrackMetaData", params.value("CurrentURIMetaData"))?;
        } else {
            self.set_state(
                "CurrentTrackMetaData",
                Value::String(parsed.normalized.unwrap_or_default()),
            )?;
        }

        if let Some(found) = parsed.title {
            title = found;
        }
        self.renderer.set_media_title(&title);
        self.renderer.set_media_resume();

        self.set_state("CurrentTrackTitle", Value::String(title.clone()))?;
        // Python repeats the address here, after the title. It is a no-op
        // through `set_state`'s `changed` (step 5 wrote the identical value),
        // which is precisely why the second write announces nothing — and why
        // `CurrentTrackURI` being outside `OBSERVED` is not what makes this
        // quiet. Kept as a write so the table is updated if step 5's value ever
        // diverges.
        self.set_state("CurrentTrackURI", Value::String(uri.clone()))?;

        // ```python
        // # Maintain a simple cast playlist so AVTransport Next/Previous work.
        // if uri not in self.playlist:
        //     self.playlist.append(uri)
        // self.current_index = self.playlist.index(uri)
        // ```
        // Two Python statements, **two** seam calls. `playlist_push` appends only
        // if absent and returns `(CurrentTrack, NumberOfTracks)` as the store saw
        // them (`protocol.py:1118-1120`, `store.rs:113`); it deliberately does
        // **not** touch the index — its doc names only the pair it returns, and
        // [`StateStore::set_playlist_index`]'s doc names this site as one of the
        // three that must call it. So the second Python line is its own call
        // here, and the first draft of this function omitted it: `Next` then
        // stepped from `-1` and the queue never advanced. `playlist_push` gives
        // back a **1-based** position, hence the `- 1`, which is exactly Python's
        // `self.playlist.index(uri)` for the URI it just returned.
        //
        // Both state writes are **ints**, matching Python's `len(self.playlist)`
        // and `self.current_index + 1`.
        let (track, total) = self.store.playlist_push(&uri);
        self.store.set_playlist_index(track - 1);

        self.set_state("NumberOfTracks", Value::from(total))?;
        self.set_state("CurrentTrack", Value::from(track))?;
        self.set_state("RelativeTimePosition", Value::String("00:00:00".into()))?;
        self.set_state("AbsoluteTimePosition", Value::String("00:00:00".into()))?;
        self.set_state("TransportState", Value::String("PAUSED_PLAYBACK".into()))?;
        self.set_state("TransportStatus", Value::String("OK".into()))?;

        // `_add_history(uri, title)`. The store's seam is
        // `add_history(title, uri)` — **the argument order flips**, and it is
        // spelled out here because a reader comparing the two would otherwise
        // suspect a transposition bug. `store.rs:149`'s doc gives the other
        // half: dedupe by URI, newest 50, never raise.
        self.store.add_history(&title, &uri);
        Ok(())
    }

    /// `_playlist_step(delta)` (`macast/protocol.py:1136-1161`), which is what
    /// `Next` and `Previous` are.
    ///
    /// ```python
    /// if not self.playlist: return {}
    /// idx = self.current_index + delta
    /// if idx < 0 or idx >= len(self.playlist): return {}
    /// ```
    ///
    /// Out of range is **not** an error and **not** a wrap — it is silence, and
    /// the control point gets an empty `<u:NextResponse/>`. That is the correct
    /// shape for a DLNA remote's next-track button at the end of a queue, and it
    /// is why the two early returns below answer `Ok(())` rather than a fault.
    ///
    /// The writes are a strict subset of a cast's: there is **no** resume (the
    /// player is already playing and will be told by the address change), **no**
    /// history entry, **no** `NumberOfTracks`, and the title comes from the
    /// address rather than from DIDL-Lite. `self._media_live = None` is
    /// Python's, with its comment "Stepping the playlist carries no DIDL… Clear
    /// the record" — here that is [`store::Renderer::note_media_live(None)`],
    /// which is the three-state read's "keep today's behaviour", not "this is a
    /// file".
    fn playlist_step(&self, delta: i64) -> Result<(), Failure> {
        let playlist = self.store.playlist();
        if playlist.is_empty() {
            return Ok(());
        }
        let index = self.store.playlist_index() + delta;
        if index < 0 || index as usize >= playlist.len() {
            return Ok(());
        }
        self.store.set_playlist_index(index);
        let uri = playlist[index as usize].clone();

        // `title = media_resolve.title_for_uri(uri)` (`:1148`). Python calls the
        // resolver module directly; our deviation is the
        // [`store::Renderer::title_for_uri`] seam, whose `src-tauri`
        // implementation may delegate wherever it likes. The reason a seam
        // exists at all is AGENTS.md §4.2's "change the player's address and you
        // must change the player's name": whoever swaps the address names the
        // player, and this is one of the twelve sites that audit counts.
        let title = self.renderer.title_for_uri(&uri);

        self.renderer.note_media_live(None);
        self.set_state_url(&uri)?;
        // A playlist step starts the next item at its beginning: Python calls
        // `set_media_url(uri)` and lets `start` take its own `"0"` default.
        self.renderer.set_media_url(&uri, None);
        self.renderer.set_media_title(&title);
        self.set_state("CurrentTrack", Value::from(index + 1))?;
        self.set_state("CurrentTrackURI", Value::String(uri.clone()))?;
        self.set_state("CurrentTrackTitle", Value::String(title))?;
        self.set_state("TransportState", Value::String("PLAYING".into()))?;
        self.set_state("TransportStatus", Value::String("OK".into()))
    }

    /// Read the embedded DIDL-Lite, the way `call()`'s handler does it inside
    /// one `try`.
    ///
    /// Returns a document, the normalized text, a title, and whether
    /// something about the metadata was unreadable. The interesting case is the
    /// one where `failed` is set **and `dom` is still `Some`**: Python binds
    /// `meta = etree.fromstring(...)` successfully and *then* takes
    /// `meta.nsmap['dc']`, which is a `KeyError` when the declaration sits on a
    /// descendant rather than the root. By then `meta` exists, so the liveness
    /// read that follows still works, and only the title and the re-serialized
    /// metadata are lost. Collapsing that into `dom = None` would silently turn
    /// a live stream into "no opinion", which is the §4.1 bug class.
    ///
    /// The two failure log lines are Python's, in Python's order:
    /// `logger.error(str(e))` then `logger.error(raw_meta)`. For metadata that
    /// was absent altogether the second line reads `None`, which looks like a
    /// bug in the log and is not — it is what Python prints, and this code path
    /// is reached for an absent `CurrentURIMetaData` because
    /// `normalize_soap_body(None)` raises `AttributeError`, which the
    /// `except Exception` catches. Reproduced as a `debug`-free `error` line for
    /// fidelity, with the honest note that it fires on the most common request
    /// a renderer ever receives.
    fn parse_meta(&self, raw_meta: Option<&str>) -> ParsedMeta {
        let Some(raw) = raw_meta else {
            self.log.error("CurrentURIMetaData is absent or unreadable: None");
            return ParsedMeta {
                dom: None,
                normalized: None,
                title: None,
                failed: true,
            };
        };

        let normalized = normalize_soap_body(raw.as_bytes());
        let Ok(text) = std::str::from_utf8(normalized) else {
            self.log.error("CurrentURIMetaData is not valid UTF-8");
            self.log.error(raw);
            return ParsedMeta {
                dom: None,
                normalized: None,
                title: None,
                failed: true,
            };
        };
        let normalized = text.to_string();

        let dom = match Dom::parse(text) {
            Ok(dom) => dom,
            Err(why) => {
                self.log.error(&why);
                self.log.error(raw);
                return ParsedMeta {
                    dom: None,
                    normalized: None,
                    title: None,
                    failed: true,
                };
            }
        };

        // `meta.nsmap['dc']` — see `Dom::binding`'s doc at `scpd.rs:679` for how
        // a descendant-declared prefix differs between the two languages, and
        // why the answer here is still "this document never told me where dc is".
        let Some(dc) = dom.binding(dom.root(), "dc").map(str::to_string) else {
            self.log.error("DIDL-Lite metadata declares no `dc` namespace at its root");
            self.log.error(raw);
            return ParsedMeta {
                dom: Some(dom),
                normalized: Some(normalized),
                title: None,
                failed: true,
            };
        };

        // `meta.find('.//{{{}}}title'.format(meta.nsmap['dc']))` — `.//` is
        // descendants only, so `root` is filtered out of `iter_subtree`, which
        // is self-inclusive by design.
        let title_tag = clark(&dc, "title");
        let title = dom
            .iter_subtree(dom.root(), &title_tag)
            .into_iter()
            .find(|node| *node != dom.root())
            .and_then(|node| dom.text(node))
            .map(str::to_string);
        // `if title_xml is not None and title_xml.text is not None: title = …`
        // An empty `<dc:title></dc:title>` is `Some("")` in lxml's `.text`? No:
        // lxml gives `None` for an element with no text, so this code treats
        // `Some("")` as "found and genuinely empty", which is what a sender that
        // wrote `<dc:title> </dc:title>`… no: that is a one-character string in
        // both languages, because neither trims (see `Dom::text`). A title that
        // comes back as `None` means no element was found, and the friendly name
        // stands.
        ParsedMeta {
            dom: Some(dom),
            normalized: Some(normalized),
            title,
            failed: false,
        }
    }

    /// The success envelope, in the measured shape: `<s:Envelope
    /// xmlns:s="…" s:encodingStyle="…"><s:Body><u:XResponse xmlns:u="…">…`.
    ///
    /// Empty `res` is **self-closing** (`<u:StopResponse xmlns:u="…"/>`), which
    /// is what lxml does with a childless element and what a control point that
    /// string-compares anything will notice.
    fn build_response(&self, action: &str, namespace: &str, res: &Res) -> Vec<u8> {
        let name = format!("{action}Response");
        let mut out = String::from("<s:Envelope xmlns:s=\"");
        out.push_str(SOAP_ENV_NS);
        out.push_str("\" s:encodingStyle=\"");
        out.push_str(SOAP_ENCODING_NS);
        out.push_str("\"><s:Body>");
        if res.is_empty() {
            out.push_str(&format!("<u:{name} xmlns:u=\"{namespace}\"/>"));
        } else {
            out.push_str(&format!("<u:{name} xmlns:u=\"{namespace}\">"));
            for (key, value) in &res.items {
                // `prop.text = str(res[key])` at `protocol.py:995` — hence
                // [`py_value_text`], not [`py_value_repr`]: a string goes on the
                // wire as its own text, so `<CurrentTransportState>STOPPED</...>`
                // and not `<CurrentTransportState>'STOPPED'</...>`. The measured
                // 399-byte `GetTransportInfo` reply asserts exactly that.
                // Also hence that the element name is **unprefixed with no
                // `xmlns=""`**.
                let text = py_value_text(value);
                out.push_str(&format!("<{key}>{}</{key}>", escape_text(&text)));
            }
            out.push_str(&format!("</u:{name}>"));
        }
        out.push_str("</s:Body></s:Envelope>");
        out.into_bytes()
    }
}

/// What [`Soap::parse_meta`] decided about one embedded DIDL-Lite document.
struct ParsedMeta {
    dom: Option<Dom>,
    normalized: Option<String>,
    title: Option<String>,
    failed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Broadcast, Renderer, StateStore};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// The embedded SCPD with the init state applied. `""` for
    /// `apply_init_state`'s argument because **this crate must not become a
    /// second owner of that file**: `src-tauri/src/host.rs` already reads
    /// `sink_protocol_info` from disk at runtime through `paths::base_path`, and
    /// a test that read it here would pass on a machine where the file moved.
    fn scpd() -> Arc<Scpd> {
        let mut table = Scpd::embedded().expect("embedded SCPD parses");
        table.apply_init_state("").expect("init state applies");
        Arc::new(table)
    }

    struct MapStore {
        table: Mutex<BTreeMap<String, Value>>,
        writes: Mutex<Vec<String>>,
        playlist: Mutex<Vec<String>>,
        index: Mutex<i64>,
        history: Mutex<Vec<(String, String)>>,
    }

    /// Hand-written, not `#[derive(Default)]`, for one field: `index`.
    ///
    /// Python opens a DLNA session with **no** current track —
    /// `protocol.py:607` is `self.current_index = -1     # current playlist
    /// position`. A derived default would be `0`, which is a real playlist
    /// position: the first `Next` on a one-item queue would then be read as
    /// "walked off the end" instead of "not on the queue yet", and an out of
    /// range step would silently change the index on a store that was never
    /// addressed.
    impl Default for MapStore {
        fn default() -> Self {
            Self {
                table: Mutex::new(BTreeMap::new()),
                writes: Mutex::new(Vec::new()),
                playlist: Mutex::new(Vec::new()),
                index: Mutex::new(-1),
                history: Mutex::new(Vec::new()),
            }
        }
    }

    impl MapStore {
        /// A poisoned lock still holds the data, and a test that panics while
        /// holding one must not take the next test down with it.
        fn table(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Value>> {
            self.table.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn reads(&self, name: &str) -> Option<Value> {
            self.table().get(name).cloned()
        }

        fn written(&self, name: &str) -> bool {
            self.writes().iter().any(|n| n == name)
        }

        fn writes(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
            self.writes.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn urls(&self) -> Vec<String> {
            self.playlist.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn history(&self) -> Vec<(String, String)> {
            self.history.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn index(&self) -> i64 {
            *self.index.lock().unwrap_or_else(|e| e.into_inner())
        }
    }

    impl StateStore for MapStore {
        fn read(&self, name: &str) -> Option<Value> {
            self.table().get(name).cloned()
        }

        fn write(&self, name: &str, value: Value) -> bool {
            let mut table = self.table();
            let changed = table.get(name) != Some(&value);
            if changed {
                table.insert(name.to_string(), value);
            }
            drop(table);
            self.writes().push(name.to_string());
            changed
        }

        fn playlist_push(&self, uri: &str) -> (i64, i64) {
            let mut playlist = self.playlist.lock().unwrap_or_else(|e| e.into_inner());
            let position = match playlist.iter().position(|u| u == uri) {
                Some(i) => i,
                None => {
                    playlist.push(uri.to_string());
                    playlist.len() - 1
                }
            };
            let total = playlist.len() as i64;
            (position as i64 + 1, total)
        }

        fn playlist(&self) -> Vec<String> {
            self.playlist.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn playlist_index(&self) -> i64 {
            *self.index.lock().unwrap_or_else(|e| e.into_inner())
        }

        fn set_playlist_index(&self, index: i64) {
            *self.index.lock().unwrap_or_else(|e| e.into_inner()) = index;
        }

        fn add_history(&self, title: &str, uri: &str) {
            self.history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((title.to_string(), uri.to_string()));
        }

        /// The trait's three-field row, derived from the pairs this double keeps.
        /// SOAP-side tests assert on [`MapStore::history`] (the two-field inherent
        /// read) because what they ask is "which title and address reached the
        /// seam"; the timestamp belongs to whoever owns the table, and `MapStore`
        /// is not that owner — `src-tauri`'s `StateTable` stamps it (`host.rs`).
        fn play_history(&self) -> Vec<(String, String, i64)> {
            self.history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(title, uri)| (title.clone(), uri.clone(), 0))
                .collect()
        }
    }

    #[derive(Default)]
    struct Calls {
        lines: Mutex<Vec<String>>,
    }

    impl Calls {
        fn push(&self, line: String) {
            self.lines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line);
        }

        /// Hand back everything the player was told **and empty the log**.
        ///
        /// The name is a promise: `take` takes. The event recorder beside it is
        /// deliberately the opposite (its `seen` clones, so a test measures from
        /// a mark) because that one's contract is "which names were announced,
        /// in order, over the whole run", while this one's is "what did *this*
        /// request do to the player". A test that loads two items and then steps
        /// backwards needs the second answer without the first, and a
        /// clone-everything reader would let a stray `set_media_resume` hide in
        /// the setup and still satisfy `contains`.
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.lines.lock().unwrap_or_else(|e| e.into_inner()))
        }
    }

    impl Renderer for Calls {
        fn note_media_live(&self, live: Option<bool>) {
            let answer = match live {
                Some(true) => "True",
                Some(false) => "False",
                None => "None",
            };
            self.push(format!("note_media_live={answer}"));
        }

        /// `None` for `start` leaves the recorded line exactly as it was before
        /// this parameter existed, so the assertions in this module still ask
        /// about the address alone — which is the point: DLNA never has a start
        /// to hand over. A `Some` appends it, because then the line is the
        /// witness that the sender's position reached the player.
        fn set_media_url(&self, uri: &str, start: Option<&str>) {
            match start {
                None => self.push(format!("set_media_url={uri}")),
                Some(at) => self.push(format!("set_media_url={uri} start={at}")),
            }
        }

        fn set_media_title(&self, title: &str) {
            self.push(format!("set_media_title={title}"));
        }

        fn set_media_position(&self, target: &str) {
            self.push(format!("set_media_position={target:?}"));
        }

        fn set_media_volume(&self, value: Value) {
            self.push(format!("set_media_volume={}", wire(&value)));
        }

        fn set_media_mute(&self, value: Value) {
            self.push(format!("set_media_mute={}", wire(&value)));
        }

        fn set_media_pause(&self) {
            self.push("set_media_pause".into());
        }

        fn set_media_resume(&self) {
            self.push("set_media_resume".into());
        }

        fn set_media_stop(&self) {
            self.push("set_media_stop".into());
        }

        fn title_for_uri(&self, uri: &str) -> String {
            format!("title-of {uri}")
        }
    }

    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(String, Value)>>,
    }

    impl Recorder {
        fn seen(&self) -> Vec<(String, Value)> {
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    impl Broadcast for Recorder {
        fn enqueue(&self, name: &str, value: &Value) {
            self.seen
                .lock()
                // `enqueue` takes `&Value`, so this is the only shape the
                // witness can be recorded in; the clone is the test paying for
                // a trait that must stay object-safe.
                .unwrap_or_else(|e| e.into_inner())
                .push((name.to_string(), value.clone()));
        }
    }

    fn setting(name: &str) -> Arc<Setting> {
        let path = std::env::temp_dir().join(format!(
            "macast-upnp-soap-{}-{name}.json",
            std::process::id()
        ));
        Arc::new(Setting::new(path))
    }

    /// The one thing worth noting: `Broadcast` is a trait object **by
    /// construction**, and the coercion below is the point rather than boilerplate.
    fn pair() -> (Soap, Arc<MapStore>, Arc<Calls>, Arc<Recorder>) {
        let store = Arc::new(MapStore::default());
        let renderer = Arc::new(Calls::default());
        let recorder = Arc::new(Recorder::default());
        let table = scpd();
        let bus: SharedBroadcast = recorder.clone();
        let soap = Soap::new(
            table,
            store.clone(),
            renderer.clone(),
            bus,
            setting("pair"),
        );
        (soap, store, renderer, recorder)
    }

    const AVT_URN: &str = "urn:schemas-upnp-org:service:AVTransport:1";
    const RC_URN: &str = "urn:schemas-upnp-org:service:RenderingControl:1";

    fn envelope(service_urn: &str, action: &str, inner: &str) -> String {
        format!(
            "<s:Envelope xmlns:s=\"{SOAP_ENV_NS}\" s:encodingStyle=\"{SOAP_ENCODING_NS}\">\
             <s:Body><u:{action} xmlns:u=\"{service_urn}\">{inner}</u:{action}></s:Body></s:Envelope>"
        )
    }

    /// A `CurrentURIMetaData` argument carrying `meta` the way a real sender
    /// does: **escaped, as text**. Python reads every argument with
    /// `param[{localname}] = node.text` (`protocol.py:951`), so a fixture that
    /// embeds raw DIDL as a *child element* hands the handler `None` — the parse
    /// never runs, `meta_failed` is set, and the liveness answer collapses to
    /// "no opinion". Such a test then measures the refusal branch while claiming
    /// to measure the parse, which is how three of this module's fixtures first
    /// went red.
    fn metadata_field(meta: &str) -> String {
        format!("<CurrentURIMetaData>{}</CurrentURIMetaData>", escape_text(meta))
    }

    #[test]
    fn normalize_eats_the_indent_a_real_sender_uses() {
        crate::pin_test_dir();
        let declared = "     \n<?xml version=\"1.0\"?><r/>";
        // BiliPai/okhttp's shape: indentation before the declaration, which
        // makes the declaration not a declaration.
        assert_eq!(
            normalize_soap_body(declared.as_bytes()),
            b"<?xml version=\"1.0\"?><r/>"
        );
        // A BOM, and the byte SET the Python one-liner actually is: any of the
        // seven, in any order, not "a BOM then spaces".
        assert_eq!(
            normalize_soap_body("\u{feff}\r\n \t<r/>".as_bytes()),
            b"<r/>"
        );
        assert_eq!(normalize_soap_body(b"\n\n\n<x/>"), b"<x/>");
        assert_eq!(normalize_soap_body(b""), b"");
    }

    #[test]
    fn normalize_returns_bytes_so_a_declaration_still_parses() {
        crate::pin_test_dir();
        // The claim in the doc: a decoded body must never be handed back to a
        // parser that reads an encoding declaration. In Rust the decode happens
        // after normalizing, so the test is that this order accepts the body an
        // indented-declaration sender sends.
        let body = "        <?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string()
            + &envelope(AVT_URN, "Stop", "");
        let soap = pair().0;
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
    }

    #[test]
    fn escape_tables_are_text_plus_one_for_attributes() {
        crate::pin_test_dir();
        assert_eq!(escape_text("a & b < c > d \"e\" 'f'"), "a &amp; b &lt; c &gt; d \"e\" 'f'");
        assert_eq!(
            escape_attr("a & b < c > d \"e\" 'f'"),
            "a &amp; b &lt; c &gt; d &quot;e&quot; 'f'"
        );
        // A `"` is just a character in a text node, and Python's serializer
        // leaves it alone.
        assert_eq!(escape_text("\""), "\"");
    }

    #[test]
    fn the_fault_envelope_is_the_measured_408_bytes() {
        crate::pin_test_dir();
        // Verbatim from /tmp/dlna-http-witnesses.json's `soap_broken_xml.body`,
        // against the shipped receiver answering a broken envelope with
        // `500` + this body. The length is the machine's, not counted here.
        const MEASURED: &str = "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><ns0:UPnPError xmlns:ns0=\"urn:schemas-upnp-org:control-1-0\"><errorCode>402</errorCode><errorDescription>Invalid Args</errorDescription></ns0:UPnPError></detail></s:Fault></s:Body></s:Envelope>";
        let fault = soap_fault(402, "Invalid Args");
        assert_eq!(MEASURED.len(), 408, "the probe recorded 408");
        assert_eq!(fault, MEASURED.as_bytes());
        assert_eq!(fault.len(), 408);
    }

    #[test]
    fn a_fault_escapes_its_description() {
        crate::pin_test_dir();
        let fault = String::from_utf8(soap_fault(401, "Invalid <Action> & \"more\"")).unwrap();
        assert!(fault.contains("<errorDescription>Invalid &lt;Action&gt; &amp; \"more\"</errorDescription>"));
    }

    /// The name keeps the action spelled the way the wire spells it: this test is
    /// a byte-for-byte witness for `GetTransportInfo`, and flattening it to
    /// `get_transport_info` would cost the one thing the name is for.
    #[allow(non_snake_case)]
    #[test]
    fn GetTransportInfo_is_the_measured_399_bytes() {
        crate::pin_test_dir();
        // `/tmp/dlna-http-witnesses.json`'s `soap_GetTransportInfo.body`, read
        // off the shipped receiver. The three values are what a fresh receiver
        // reports: STOPPED / OK / 1.
        const MEASURED: &str = "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:GetTransportInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><CurrentTransportState>STOPPED</CurrentTransportState><CurrentTransportStatus>OK</CurrentTransportStatus><CurrentSpeed>1</CurrentSpeed></u:GetTransportInfoResponse></s:Body></s:Envelope>";
        let (soap, store, ..) = pair();
        {
            let mut table = store.table();
            table.insert("TransportState".into(), Value::String("STOPPED".into()));
            table.insert("TransportStatus".into(), Value::String("OK".into()));
            table.insert("TransportPlaySpeed".into(), Value::String("1".into()));
        }
        let body = envelope(AVT_URN, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        let reply = soap.handle(body.as_bytes());
        assert_eq!(reply.status, 200);
        assert_eq!(reply.content_type, SOAP_CONTENT_TYPE);
        assert_eq!(reply.body, MEASURED.as_bytes());
        assert_eq!(reply.body.len(), 399, "the probe recorded 399");
    }

    #[test]
    fn an_empty_reply_is_self_closing() {
        crate::pin_test_dir();
        // The shape is asserted, and the length is derived from the bytes this
        // function built. An earlier round carried a "224 bytes" claim for this
        // envelope that is **not** in the probe file, so it is not repeated here
        // as a measurement; re-probe against lxml before quoting one.
        let (soap, ..) = pair();
        let body = envelope(AVT_URN, "Stop", "<InstanceID>0</InstanceID>");
        let reply = soap.handle(body.as_bytes());
        assert_eq!(reply.status, 200);
        let text = String::from_utf8(reply.body.clone()).unwrap();
        assert!(text.ends_with("<u:StopResponse xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"/></s:Body></s:Envelope>"));
        assert!(!text.contains("xmlns=\"\""), "u: is a prefix binding; the default namespace stays unbound");
    }

    #[test]
    fn unreadable_bytes_answer_a_real_fault_not_html() {
        crate::pin_test_dir();
        let (soap, ..) = pair();
        let reply = soap.handle(b"<s:Envelope><r");
        assert_eq!(reply.status, 500);
        assert_eq!(reply.body, soap_fault(402, "Invalid Args"));
    }

    #[test]
    fn undecodable_bytes_take_the_same_402_path() {
        crate::pin_test_dir();
        // Python: lxml raises `XMLSyntaxError` (⇒ 402). Rust: `from_utf8` fails
        // first. Either way the log line quotes the caller's original bytes.
        let (soap, ..) = pair();
        let mut bytes = b"<s:Envelope>".to_vec();
        bytes.push(0xff);
        let reply = soap.handle(&bytes);
        assert_eq!(reply.body, soap_fault(402, "Invalid Args"));
    }

    #[test]
    fn a_missing_body_is_refused_and_says_which_half_is_missing() {
        crate::pin_test_dir();
        let (soap, ..) = pair();
        let reply = soap.handle(b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"></s:Envelope>");
        assert_eq!(reply.status, 500);
        assert_eq!(reply.body, soap_fault(402, "Invalid Args"));
    }

    #[test]
    fn an_empty_body_element_is_refused_too() {
        crate::pin_test_dir();
        let (soap, ..) = pair();
        let reply = soap.handle(
            b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body></s:Body></s:Envelope>",
        );
        assert_eq!(reply.body, soap_fault(402, "Invalid Args"));
    }

    #[test]
    fn an_action_element_without_a_namespace_is_401() {
        crate::pin_test_dir();
        let (soap, ..) = pair();
        let reply = soap.handle(
            b"<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><Stop/></s:Body></s:Envelope>",
        );
        assert_eq!(reply.body, soap_fault(401, "Invalid Action"));
    }

    #[test]
    fn a_service_urn_too_short_to_name_a_service_is_401() {
        crate::pin_test_dir();
        // `service_name_from_urn`'s `.nth(3)` refuses with a message that quotes
        // what it was given; Python's `split(':')[3]` raises IndexError.
        let (soap, ..) = pair();
        let body = "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
                    s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
                    <s:Body><u:Stop xmlns:u=\"urn:short\"/></s:Body></s:Envelope>";
        let reply = soap.handle(body.as_bytes());
        assert_eq!(reply.body, soap_fault(401, "Invalid Action"));
    }

    #[test]
    fn an_unknown_service_or_action_is_401_never_a_handler() {
        crate::pin_test_dir();
        let (soap, ..) = pair();
        // Unknown action, known service.
        let reply = soap.handle(envelope(AVT_URN, "Teleport", "").as_bytes());
        assert_eq!(reply.body, soap_fault(401, "Invalid Action"));
        // Unknown service, so `HandlerKind::for_names` cannot match either.
        let reply = soap.handle(envelope("urn:example-com:service:TimeTravel:1", "Stop", "").as_bytes());
        assert_eq!(reply.body, soap_fault(401, "Invalid Action"));
    }

    #[test]
    fn an_implemented_action_the_document_does_not_declare_is_401() {
        crate::pin_test_dir();
        // `Next`/`Previous` are real handlers in Python's class and are **not**
        // in the AVTransport SCPD, so the branch that runs has to be decided
        // after the schema lookup, not by `HandlerKind` alone. Reading this
        // envelope proves the order: it is refused, and no renderer call lands.
        let (soap, _, renderer, _) = pair();
        let reply = soap.handle(envelope(AVT_URN, "Jump", "").as_bytes());
        assert_eq!(reply.body, soap_fault(401, "Invalid Action"));
        assert!(renderer.take().is_empty());
    }

    #[test]
    fn set_volume_hands_the_renderer_the_wire_string() {
        crate::pin_test_dir();
        let (soap, _, renderer, _) = pair();
        let body = envelope(
            RC_URN,
            "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>42</DesiredVolume>",
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        // `mpv.py:139` sends the value it got, so a string stays a string; the
        // witness is the WIRE spelling, i.e. no quotes are added by us.
        assert_eq!(renderer.take(), vec!["set_media_volume=42".to_string()]);
    }

    #[test]
    fn set_mute_coerces_the_way_python_does() {
        crate::pin_test_dir();
        for (inner, want) in [
            ("<DesiredMute>0</DesiredMute>", "False"),
            ("<DesiredMute>1</DesiredMute>", "True"),
            ("<DesiredMute></DesiredMute>", "True"),
            ("<DesiredMute/>", "True"),
            ("", "True"),
            ("<DesiredMute>00</DesiredMute>", "True"),
        ] {
            let (soap, _, renderer, _) = pair();
            let body = envelope(
                RC_URN,
                "SetMute",
                &format!("<InstanceID>0</InstanceID><Channel>Master</Channel>{inner}"),
            );
            assert_eq!(soap.handle(body.as_bytes()).status, 200, "{inner}");
            assert_eq!(
                renderer.take(),
                vec![format!("set_media_mute={want}")],
                "{inner}"
            );
        }
    }

    #[test]
    fn seek_writes_both_positions_and_sends_the_target_once() {
        crate::pin_test_dir();
        let (soap, store, renderer, recorder) = pair();
        let body = envelope(
            AVT_URN,
            "Seek",
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>00:01:23</Target>",
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        assert_eq!(renderer.take(), vec!["set_media_position=\"00:01:23\"".to_string()]);
        assert_eq!(store.reads("RelativeTimePosition"), Some(Value::String("00:01:23".into())));
        assert_eq!(store.reads("AbsoluteTimePosition"), Some(Value::String("00:01:23".into())));
        // Neither name is in OBSERVED (`store.rs:60-70`), so a seek announces
        // nothing. This is the witness the SetAVTransportURI test cannot be, and
        // it is the reason the two are not merged.
        assert!(recorder.seen().is_empty());
    }

    #[test]
    fn seek_without_a_target_sends_the_empty_string_and_stores_null() {
        crate::pin_test_dir();
        let (soap, store, renderer, _) = pair();
        let body = envelope(
            AVT_URN,
            "Seek",
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit>",
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        // The module doc's unmeasured neighbour: Python would send JSON `null`,
        // our seam can only send `""`. Asserted as what the code does, not as
        // what mpv answers.
        assert_eq!(renderer.take(), vec!["set_media_position=\"\"".to_string()]);
        assert_eq!(store.reads("RelativeTimePosition"), Some(Value::Null));
    }

    #[test]
    fn stop_is_a_player_call_and_two_state_writes_one_of_them_observed() {
        crate::pin_test_dir();
        let (soap, store, renderer, recorder) = pair();
        {
            store.table().insert("TransportState".into(), Value::String("PLAYING".into()));
        }
        let body = envelope(AVT_URN, "Stop", "<InstanceID>0</InstanceID>");
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        assert_eq!(renderer.take(), vec!["set_media_stop".to_string()]);
        assert_eq!(store.reads("TransportState"), Some(Value::String("STOPPED".into())));
        // The SCPD pre-write of `InstanceID` lands too, and says nothing:
        // `A_ARG_TYPE_InstanceID` is not observed.
        assert_eq!(recorder.seen(), vec![("TransportState".to_string(), Value::String("STOPPED".into()))]);
    }

    #[test]
    fn a_state_write_that_did_not_change_announces_nothing() {
        crate::pin_test_dir();
        let (soap, store, _, recorder) = pair();
        {
            store.table().insert("TransportState".into(), Value::String("STOPPED".into()));
        }
        soap.handle(envelope(AVT_URN, "Stop", "<InstanceID>0</InstanceID>").as_bytes());
        assert!(recorder.seen().is_empty(), "changed is the store's answer, not ours");
        assert!(store.written("TransportState"), "and the write still happened");
    }

    #[test]
    fn a_name_the_scpd_does_not_declare_never_reaches_the_store() {
        crate::pin_test_dir();
        // Python's `state_list[name]` is a KeyError on purpose. The trigger
        // cannot come from the wire (every pre-write name comes out of the
        // schema), so the helper is called directly — which is also what makes
        // this a test of the guard rather than of a call site.
        let (soap, store, _, _) = pair();
        let failure = soap
            .set_state("NoSuchVariable", Value::String("x".into()))
            .unwrap_err();
        let reply = failure.into_reply();
        assert_eq!(reply.body, soap_fault(501, "Action Failed"));
        assert!(!store.written("NoSuchVariable"));
    }

    #[test]
    fn a_read_of_a_name_the_store_lacks_is_501_not_an_empty_element() {
        crate::pin_test_dir();
        // The store returns `None` for "no such name" and `Some(Value::Null)`
        // for "the table holds null" (`store.rs:104`). Only the first is a
        // refusal, and the second must serialize as `None` — Python's `str()`.
        let (soap, store, _, _) = pair();
        store.table().remove("TransportPlaySpeed");
        let body = envelope(AVT_URN, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        let reply = soap.handle(body.as_bytes());
        assert_eq!(reply.body, soap_fault(501, "Action Failed"));

        let (soap, store, _, _) = pair();
        {
            let mut table = store.table();
            table.insert("TransportState".into(), Value::Null);
            table.insert("TransportStatus".into(), Value::String("OK".into()));
            table.insert("TransportPlaySpeed".into(), Value::String("1".into()));
        }
        let reply = soap.handle(envelope(AVT_URN, "GetTransportInfo", "<InstanceID>0</InstanceID>").as_bytes());
        assert_eq!(reply.status, 200);
        assert!(String::from_utf8(reply.body).unwrap().contains("<CurrentTransportState>None</CurrentTransportState>"));
    }

    #[test]
    fn set_uri_does_the_twelve_things_in_the_order_the_comment_demands() {
        crate::pin_test_dir();
        let (soap, store, renderer, recorder) = pair();
        let meta = "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
                    xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
                    xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
                    <item id=\"0\" parentID=\"-1\" restricted=\"1\">\
                    <dc:title>Rick Astley</dc:title>\
                    <res protocolInfo=\"http-get:*:video/mp2t:DLNA.ORG_OP=00;DLNA.ORG_FLAGS=01700000000000000000000000000000\">http://a/b.ts</res>\
                    </item></DIDL-Lite>";
        let body = envelope(
            AVT_URN,
            "SetAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><CurrentURI>http://a/b.ts</CurrentURI>{}",
                metadata_field(meta)
            ),
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);

        let calls = renderer.take();
        let position = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap_or_else(|| panic!("no {needle} in {calls:?}"));
        // The ordering argument, as an assertion: the liveness hand-over is
        // strictly before the address changes.
        assert!(
            position("note_media_live=") < position("set_media_url="),
            "the sender's declaration must steer the load it describes: {calls:?}"
        );
        assert_eq!(calls[position("note_media_live=")], "note_media_live=True");
        // `set_media_resume()` takes no argument, so the recorder prints it
        // without an `=`; a needle of `"set_media_resume="` would name a call
        // that no renderer method makes and panic two lines later, for a reason
        // that reads like the ordering contract failed.
        assert!(position("set_media_title=") < position("set_media_resume"));
        assert_eq!(calls[position("set_media_title=")], "set_media_title=Rick Astley");

        assert_eq!(store.reads("CurrentTrackURI"), Some(Value::String("http://a/b.ts".into())));
        assert_eq!(store.reads("CurrentTrackTitle"), Some(Value::String("Rick Astley".into())));
        assert_eq!(store.reads("TransportState"), Some(Value::String("PAUSED_PLAYBACK".into())));
        assert_eq!(store.reads("NumberOfTracks"), Some(Value::from(1i64)));
        assert_eq!(store.reads("CurrentTrack"), Some(Value::from(1i64)));
        assert_eq!(store.urls(), vec!["http://a/b.ts".to_string()]);
        assert_eq!(store.index(), 0);
        assert_eq!(store.history(), vec![("Rick Astley".to_string(), "http://a/b.ts".to_string())]);

        // The broadcasts are exactly the four OBSERVED names this method changes
        // — `CurrentTrackURI`, `CurrentTrackMetaData`, `CurrentTrackTitle` and
        // both time positions announce nothing (`store.rs:60-70`). This
        // replaces an earlier witness over a "double CurrentTrackURI broadcast",
        // which cannot exist.
        let seen = recorder.seen();
        let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["NumberOfTracks", "CurrentTrack", "TransportState", "TransportStatus"]
        );
    }

    #[test]
    fn set_uri_without_a_current_uri_refuses_before_the_swap() {
        crate::pin_test_dir();
        let (soap, store, renderer, _) = pair();
        let body = envelope(
            AVT_URN,
            "SetAVTransportURI",
            "<InstanceID>0</InstanceID>",
        );
        let reply = soap.handle(body.as_bytes());
        assert_eq!(reply.body, soap_fault(402, "Invalid Args"));
        // The deviation in the module doc: no `None` reaches the player.
        assert!(renderer.take().is_empty());
        // …and no address lands in the table, while the InstanceID pre-write
        // does, because it happened before the refusal — also Python's order.
        assert!(!store.written("CurrentTrackURI"));
        assert!(store.written("A_ARG_TYPE_InstanceID"));
    }

    #[test]
    fn metadata_declaring_dc_on_a_descendant_still_answers_liveness() {
        crate::pin_test_dir();
        // The case `Dom::binding`'s doc predicts: Python takes a KeyError on
        // `meta.nsmap['dc']` *after* binding `meta`, so the title is lost and
        // the liveness read still works. A `dom = None` collapse would turn a
        // live stream into "no opinion".
        let (soap, store, renderer, _) = pair();
        let meta = "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\">\
                    <item id=\"0\"><dc:title xmlns:dc=\"http://purl.org/dc/elements/1.1/\">X</dc:title>\
                    <res protocolInfo=\"http-get:*:video/mp2t:DLNA.ORG_OP=00\">u</res></item>\
                    </DIDL-Lite>";
        let body = envelope(
            AVT_URN,
            "SetAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><CurrentURI>u</CurrentURI>{}",
                metadata_field(meta)
            ),
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        let calls = renderer.take();
        assert!(
            calls.iter().any(|c| c == "note_media_live=True"),
            "the document parsed, so its `res` is readable: {calls:?}"
        );
        // The failed branch stores the sender's **original** text, unnormalized.
        assert_eq!(
            store.reads("CurrentTrackMetaData"),
            Some(Value::String(meta.into()))
        );
        // The friendly name stands in for the title we could not find.
        assert!(calls.iter().any(|c| c.starts_with("set_media_title=")));
    }

    #[test]
    fn metadata_that_is_absent_is_a_null_write_not_an_empty_string() {
        crate::pin_test_dir();
        let (soap, store, _, _) = pair();
        let body = envelope(
            AVT_URN,
            "SetAVTransportURI",
            "<InstanceID>0</InstanceID><CurrentURI>http://a/b.ts</CurrentURI>",
        );
        assert_eq!(soap.handle(body.as_bytes()).status, 200);
        assert_eq!(store.reads("CurrentTrackMetaData"), Some(Value::Null));
        assert_eq!(store.reads("TransportState"), Some(Value::String("PAUSED_PLAYBACK".into())));
    }

    #[test]
    fn the_liveness_read_walks_including_the_root_and_reads_local_names() {
        crate::pin_test_dir();
        let (soap, _, renderer, _) = pair();
        // A sender whose root element IS the `res`: `meta.iter()` is
        // self-inclusive, so an implementation that skips the root answers
        // "no opinion" where Python answers "live".
        let meta = "<res xmlns=\"urn:x\" protocolInfo=\"http-get:*:video/mp2t:DLNA.ORG_OP=00\">u</res>";
        let body = envelope(
            AVT_URN,
            "SetAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><CurrentURI>u</CurrentURI>{}",
                metadata_field(meta)
            ),
        );
        // No `dc` at the root, so this also takes the meta_failed branch; the
        // point is the liveness value.
        soap.handle(body.as_bytes());
        assert!(renderer
            .take()
            .iter()
            .any(|c| c == "note_media_live=True"));
    }

    #[test]
    fn live_from_protocol_info_is_three_valued_and_backtracks_like_a_regex() {
        crate::pin_test_dir();
        assert_eq!(live_from_protocol_info("http-get:*:video/mp2t:DLNA.ORG_OP=00"), Some(true));
        assert_eq!(live_from_protocol_info("http-get:*:video/mp2t:DLNA.ORG_OP=01"), Some(false));
        assert_eq!(live_from_protocol_info("http-get:*:video/mp2t:DLNA.ORG_OP=11"), Some(false));
        assert_eq!(live_from_protocol_info("http-get:*:video/mp2t:DLNA.ORG_OP=0a"), Some(false));
        // No statement at all. `None` is not `False`.
        assert_eq!(live_from_protocol_info("http-get:*:video/mp4:*"), None);
        assert_eq!(live_from_protocol_info(""), None);
        // A `DLNA.ORG_OP=` that is not followed by two hex digits must not
        // shadow a later one — this is the `re.search` backtracking the byte
        // scan has to reproduce.
        assert_eq!(
            live_from_protocol_info("DLNA.ORG_OP=zz and DLNA.ORG_OP=00"),
            Some(true)
        );
        assert_eq!(live_from_protocol_info("DLNA.ORG_OP=0"), None);
        assert_eq!(live_from_protocol_info("DLNA.ORG_OP"), None);
    }

    #[test]
    fn protocol_info_from_didl_hands_the_sender_spelling_to_the_reader() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            "<DIDL-Lite xmlns=\"urn:x\"><item><res protocolInfo=\"\">first</res></item></DIDL-Lite>",
        )
        .unwrap();
        // An empty `protocolInfo` is falsy in Python, so the walk continues —
        // and here there is nothing after it, so the answer is "".
        assert_eq!(protocol_info_from_didl(Some(&dom)), "");
        let dom = Dom::parse(
            "<DIDL-Lite><item><res protocolInfo=\"a:DLNA.ORG_OP=01\"/><res protocolInfo=\"b\">x</res></item></DIDL-Lite>",
        )
        .unwrap();
        assert_eq!(protocol_info_from_didl(Some(&dom)), "a:DLNA.ORG_OP=01");
        assert_eq!(protocol_info_from_didl(None), "");
    }

    #[test]
    fn next_and_previous_walk_the_queue_and_stop_at_both_ends() {
        crate::pin_test_dir();
        let (soap, store, renderer, recorder) = pair();
        for uri in ["http://a/1", "http://a/2"] {
            soap.handle(
                envelope(
                    AVT_URN,
                    "SetAVTransportURI",
                    &format!("<InstanceID>0</InstanceID><CurrentURI>{uri}</CurrentURI>"),
                )
                .as_bytes(),
            );
        }
        assert_eq!(store.urls(), vec!["http://a/1".to_string(), "http://a/2".to_string()]);
        assert_eq!(store.index(), 1);
        // Take the two setup loads off the player's log: the assertion below
        // that "an out of range step touches nothing" is only about the step,
        // and the `contains` checks after it must not be satisfied by a load
        // this test never asked for.
        renderer.take();
        // The two `SetAVTransportURI`s above already announced `NumberOfTracks`
        // twice. Everything the steps below prove is about is measured from this
        // mark — `seen()` hands back a clone, so there is nothing to clear.
        let mark = recorder.seen().len();

        // Python's `Next` is `+1`. From index 1 of a two-item queue that is 2,
        // which is out of range, and the handler's answer is `{}`: no index
        // write, no address swap, no liveness note, no broadcast.
        assert_eq!(soap.handle(envelope(AVT_URN, "Next", "<InstanceID>0</InstanceID>").as_bytes()).status, 200);
        assert_eq!(store.index(), 1, "an out of range step does not move the index");
        assert_eq!(
            store.reads("CurrentTrack"),
            Some(Value::from(2i64)),
            "and it does not write the track number either"
        );
        assert_eq!(renderer.take(), Vec::<String>::new());
        assert_eq!(
            recorder.seen().len(),
            mark,
            "silence means silence: nothing may reach the event queue"
        );

        // `Previous` is `-1`; from index 1 it lands on the first item.
        //
        // Before asking for it, put a **different** answer in the table where the
        // step is going to write its own. `_playlist_step` does write
        // `TransportStatus = "OK"` (`protocol.py:1160`), but `set_state` only
        // enqueues when the value `changed` (`protocol.py:998-1043`, and
        // AGENTS.md §4.2's measurement: 464 of 505 duration reports in three
        // hours said nothing new). The two setup loads above already left `'OK'`
        // sitting there, so a step taken now would be *correctly* silent — and the
        // assertion further down that names `TransportStatus` would be asking the
        // broadcast queue to leak. Writing `'ERROR'` straight into the table (not
        // through `set_state`, which would announce it and pollute the mark) makes
        // the step's write a real change, so that assertion asks the gate to open.
        // The half that asks it to stay shut is the `Next` below.
        store
            .table()
            .insert("TransportStatus".to_string(), Value::String("ERROR".into()));
        assert_eq!(soap.handle(envelope(AVT_URN, "Previous", "<InstanceID>0</InstanceID>").as_bytes()).status, 200);
        assert_eq!(store.index(), 0);
        let calls = renderer.take();
        assert!(calls.contains(&"set_media_url=http://a/1".to_string()));
        assert!(calls.contains(&"set_media_title=title-of http://a/1".to_string()));
        assert!(calls.contains(&"note_media_live=None".to_string()));
        // No resume on a playlist step, and no history entry.
        assert!(!calls.contains(&"set_media_resume".to_string()));
        assert_eq!(store.history().len(), 2);
        // `CurrentTrack` is observed; `NumberOfTracks` is not written by a step.
        let names: Vec<String> = recorder.seen()[mark..]
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(names.contains(&"CurrentTrack".to_string()));
        assert!(names.contains(&"TransportState".to_string()));
        assert!(names.contains(&"TransportStatus".to_string()));
        assert!(!names.contains(&"NumberOfTracks".to_string()));
        assert!(!names.contains(&"CurrentTrackURI".to_string()));

        // The other half of the same gate, with nothing forced this time. Step
        // forward again: `CurrentTrack` goes 1 → 2, so that one is announced, while
        // `TransportState` (`PLAYING`, which the previous step already wrote) and
        // `TransportStatus` (`OK`, now genuinely equal to what is in the table) are
        // **identical writes** and must announce nothing. A test that only proves
        // the gate opens is a test that passes when the gate is deleted.
        let mark2 = recorder.seen().len();
        assert_eq!(soap.handle(envelope(AVT_URN, "Next", "<InstanceID>0</InstanceID>").as_bytes()).status, 200);
        assert_eq!(store.index(), 1);
        assert!(
            renderer.take().contains(&"set_media_url=http://a/2".to_string()),
            "the step still swapped the address; only the broadcasting changed"
        );
        let names2: Vec<String> = recorder.seen()[mark2..]
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(names2.contains(&"CurrentTrack".to_string()));
        assert!(
            !names2.contains(&"TransportStatus".to_string()),
            "an unchanged value must not reach the event queue: {:?}",
            names2
        );
        assert!(
            !names2.contains(&"TransportState".to_string()),
            "nor may a repeat of the state the last step wrote: {:?}",
            names2
        );
    }

    #[test]
    fn an_empty_playlist_steps_nothing_and_answers_success() {
        crate::pin_test_dir();
        let (soap, store, renderer, _) = pair();
        assert_eq!(soap.handle(envelope(AVT_URN, "Next", "<InstanceID>0</InstanceID>").as_bytes()).status, 200);
        assert!(renderer.take().is_empty());
        assert_eq!(store.index(), -1);
    }

    #[test]
    fn the_dict_repr_is_pythons() {
        crate::pin_test_dir();
        let mut params = Params::default();
        params.insert("InstanceID".into(), Some("0".into()));
        params.insert("CurrentURIMetaData".into(), None);
        assert_eq!(params.py_dict_repr(), "{'InstanceID': '0', 'CurrentURIMetaData': None}");
        // A repeated element overwrites in place, because that is a dict
        // assignment, and the order is what `logger.info` prints.
        params.insert("InstanceID".into(), Some("7".into()));
        assert_eq!(
            params.py_dict_repr(),
            "{'InstanceID': '7', 'CurrentURIMetaData': None}"
        );
        // The quote flips only when the text has `'` and no `"`.
        let mut one = Params::default();
        one.insert("t".into(), Some("it's".into()));
        assert_eq!(one.py_dict_repr(), "{'t': \"it's\"}");
        let mut both = Params::default();
        both.insert("t".into(), Some("it's \"x\"".into()));
        assert_eq!(both.py_dict_repr(), "{'t': 'it\\'s \"x\"'}");

        let mut res = Res::default();
        res.insert("Track".into(), Value::from(2i64));
        res.insert("RelTime".into(), Value::Null);
        res.insert("URI".into(), Value::String("http://a".into()));
        // `str()` for everything but a string, `repr()` for a string: the int is
        // bare, `None` is four letters, the URI is quoted.
        assert_eq!(res.py_dict_repr(), "{'Track': 2, 'RelTime': None, 'URI': 'http://a'}");
    }

    #[test]
    fn the_repr_helpers_escape_like_python() {
        crate::pin_test_dir();
        assert_eq!(py_str_repr("a\\b\nc\tdé\u{1}"), "'a\\\\b\\nc\\tdé\\x01'");
        assert_eq!(py_str_repr("\u{e9}"), "'é'");
        // C1 controls and NBSP are not printable, in either language.
        assert_eq!(py_str_repr("\u{a0}"), "'\\xa0'");
        // LINE SEPARATOR is where the two part ways, and the difference is
        // documented on `is_printable`: it treats everything above U+00A0 as
        // printable, while CPython escapes the Unicode category-Zs/Cc/Cf/Zl/Zp
        // set. This prints as itself here. Only the parse-failure log line reads
        // this output, so the deviation costs a log's worth of fidelity, never a
        // byte on the wire — and a sender putting U+2028 in a SOAP envelope is
        // reporting that its body is not well-formed XML anyway.
        assert_eq!(py_str_repr("\u{2028}"), "'\u{2028}'");
        assert_eq!(bytes_repr(b"<x\xff".as_slice()), "b'<x\\xff'");
        assert_eq!(bytes_repr(b"ab'c".as_slice()), "b\"ab'c\"");
        assert_eq!(bytes_repr(&[0u8, 1]), "b'\\x00\\x01'");
    }

    #[test]
    fn py_value_repr_is_repr_and_py_value_text_is_str() {
        crate::pin_test_dir();
        // Everything that is not a string answers the same both ways, because
        // `str(x) == repr(x)` for `None`, `True`, `False`, an int and a float.
        for value in [
            Value::Null,
            Value::Bool(true),
            Value::from(12i64),
            Value::from(1.5),
        ] {
            assert_eq!(py_value_repr(&value), py_value_text(&value));
        }
        assert_eq!(py_value_repr(&Value::Null), "None");
        assert_eq!(py_value_repr(&Value::Bool(true)), "True");
        assert_eq!(py_value_repr(&Value::from(12i64)), "12");
        assert_eq!(py_value_repr(&Value::from(1.5)), "1.5");
        // The honest deviation: a list prints as JSON, which is `wire`'s answer,
        // and the store's doc says our writers never produce one.
        assert_eq!(py_value_repr(&Value::Array(vec![Value::from(1i64)])), "[1]");
        assert_eq!(py_value_text(&Value::Array(vec![Value::from(1i64)])), "[1]");

        // A string is the one shape where the two differ, and the pair exists
        // only for this: `repr()` for the log, `str()` for the wire.
        assert_eq!(py_value_repr(&Value::String("STOPPED".into())), "'STOPPED'");
        assert_eq!(py_value_text(&Value::String("STOPPED".into())), "STOPPED");
        // The measured 399-byte `GetTransportInfo` reply
        // (`/tmp/dlna-http-witnesses.json`, asserted verbatim in
        // `GetTransportInfo_is_the_measured_399_bytes`) has
        // `<CurrentTransportState>STOPPED</CurrentTransportState>` between its
        // tags. That is the second witness for the `str()` side, and the reason
        // `build_response` may not call `py_value_repr`: a control point reads
        // this text as an enum name, and `'STOPPED'` is not one.
        let (soap, _, _, _) = pair();
        let mut res = Res::default();
        res.insert(
            "CurrentTransportState".into(),
            Value::String("STOPPED".into()),
        );
        let body = String::from_utf8(soap.build_response("GetTransportInfo", AVT_URN, &res))
            .expect("the envelope we build is UTF-8");
        assert!(
            body.contains("<CurrentTransportState>STOPPED</CurrentTransportState>"),
            "the wire gets `str()`, not `repr()`: {body}"
        );
    }

    #[test]
    fn the_log_gates_are_the_three_names_and_the_two_noisy_replies() {
        crate::pin_test_dir();
        assert!(!logs_request("AVTransport_GetPositionInfo"));
        assert!(!logs_request("AVTransport_GetTransportInfo"));
        assert!(!logs_request("RenderingControl_GetVolume"));
        assert!(logs_request("RenderingControl_GetMute"));
        assert!(logs_request("AVTransport_SetAVTransportURI"));
        // A noisy line still has its twenty asterisks and its `res: `.
        assert_eq!(response_line("AVTransport_GetPositionInfo"), format!("{}res: AVTransport_GetPositionInfo", "*".repeat(20)));
        assert!(NOISY_RESPONSES.contains(&"ConnectionManager_GetProtocolInfo"));
        // ConnectionManager has no handlers, so this one always takes the read
        // branch — the census in the module doc, asserted rather than remembered.
        for (service, action) in [
            ("AVTransport", "GetPositionInfo"),
            ("AVTransport", "GetMediaInfo"),
            ("ConnectionManager", "GetProtocolInfo"),
            ("RenderingControl", "ListPresets"),
        ] {
            assert!(
                HandlerKind::for_names(service, action).is_none(),
                "{service}_{action} must not have a handler"
            );
        }
        assert!(HandlerKind::for_names("AVTransport", "Stop").is_some());
    }
}
