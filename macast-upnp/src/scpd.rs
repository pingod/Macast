//! The service description: what variables a DLNA device says it has, and what
//! actions it says it can perform.
//!
//! Rust counterpart of `macast/protocol.py:490-600` (`DataType`,
//! `StateVariable`, `Argument`, `Action`, `Service`) and
//! `macast/protocol.py:661-731` (`init_services`, `build_action`), plus the
//! nine default writes of `macast/protocol.py:650-659` (`init_state`).
//!
//! This module is the part of the DLNA face that cannot be negotiated. A control
//! point reads `description.xml`, then each service's SCPD document, and from
//! those two it decides what to send us. It does not ask us anything before it
//! does so — a `GetPositionInfo` it never saw declared will not be sent, and a
//! variable we declare as `ui4` will be sent a number. So the shape of these
//! tables *is* the protocol, and getting the shape wrong produces a device that
//! is discoverable and then silent, which AGENTS.md §4.9 has learned to read as
//! "the sender doesn't accept us" rather than "we described ourselves badly".
//!
//! ## Why a DOM, and why this one
//!
//! Python parses the four XML files with lxml and navigates them with
//! `iter()` / `find()` / `findall()` / `.text` / `.attrib`. Those five are not a
//! bag of conveniences, they are five specific navigation rules, and two of them
//! are load-bearing here:
//!
//! * `iter(tag)` walks the whole subtree **including the node it starts at**;
//!   `find`/`findall` look at **direct children only**. `init_services` uses the
//!   deep form (`desc.iter('{…device-1-0}serviceType')`) and `build_action` uses
//!   both — deep for `stateVariable` and `action`, shallow for everything inside
//!   them. [`Dom::iter_subtree`] and [`Dom::find`] exist as two functions because
//!   they are two rules; collapsing them would read a document we don't have.
//! * `.text` is the characters **before the first child element**, and `None`
//!   otherwise. Text after a closed child is that child's `.tail`, which this
//!   code never reads. A reader that concatenated all text would turn
//!   `<a>before<b/>after</a>` into `beforeafter`.
//!
//! The other two are ordinary, but they decide whether a malformed document is
//! refused or silently mis-read: `.attrib` excludes nothing and keeps insertion
//! order (we use it for `sendEvents`, and we keep order for the description's
//! `{placeholder}` template), and an undeclared namespace prefix is a hard parse
//! failure in lxml ("Unbound prefix") — here it is [`Err`] too, because reading
//! `<dlna:X_DLNADOC>` as a bare name would drop a node from the wire.
//!
//! So: parse with quick-xml, keep an arena DOM, and navigate it the way lxml
//! does. Emission is *not* here — see [`crate::description`] and
//! [`crate::eventing`] for the two places bytes go out.
//!
//! ## What the golden files are
//!
//! `tests/golden/scpd_state_table.json` and `tests/golden/scpd_actions.json`
//! were produced by running the real Python (`DLNAProtocol.__new__` +
//! `init_services()` with a hand-supplied `state_list`) against these same four
//! XML files and dumping the resulting tables. They are the witness that this
//! module reads the same document the shipped receiver reads, which is a
//! question no amount of reasoning about the XML can answer. Two examples of
//! what only the golden could catch: the flat state table has **48 rows from 50
//! declarations**, because `LastChange` and `A_ARG_TYPE_InstanceID` are declared
//! by both AVTransport and RenderingControl and the later service overwrites the
//! earlier one *in place* — keeping the earlier position; and `<step>` inside an
//! `allowedValueRange` is never read, so `CurrentTrack`'s range is the bare pair
//! `0..65535`.
//!
//! ## Deviations from Python, all deliberate
//!
//! * `Service` in Python is a **process-global class dict** (`service_map`), and
//!   `Service.get(name)` returns a fabricated default for an unknown name — a
//!   fresh, unnamed `Service` with `namespace=''` and a *shared mutable default*
//!   `actions={}`. Here a service is looked up through [`Scpd::service`] which
//!   returns `Option`, so "the control point asked for a service we do not
//!   serve" is an answer rather than an empty object that replies to nothing.
//! * Python raises `AttributeError` (a `.text` on `None`), `KeyError`
//!   (`attrib['sendEvents']`, `state_list[name] = …` never, but `set_state`
//!   does), `ValueError` (`DataType(...)`), and `IndexError` (`split(":")[3]`).
//!   Each of those is an [`Err`] with a message naming the service, the variable
//!   or action, and what was missing. A document we cannot read is a document we
//!   must not serve.
//! * A `<direction/>` with no text is `None` in Python and lands in **output**
//!   (`if … .text == 'in'` is False). That is reproduced, because it is the one
//!   place where "we don't understand" is the *correct* answer for a control
//!   point.

use serde_json::{json, Value};

/// SCPD namespace — `macast/protocol.py:681`'s `ns` literal.
pub const SCPD_NS: &str = "urn:schemas-upnp-org:service-1-0";

/// Device description namespace — `macast/protocol.py:666`.
pub const DEVICE_NS: &str = "urn:schemas-upnp-org:device-1-0";

/// The implicit `xml:` prefix every XML document carries. lxml resolves it
/// without a declaration; so do we, as the base of every scope stack.
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// `macast/assets/xml/Description.xml`, embedded rather than read at runtime.
///
/// Two reasons, both load-bearing. First, `macast_core::paths::resource_dir()`
/// resolves to `target/debug/deps` under a test binary, which has no `macast/xml`
/// beside it — a runtime read would make these tables untestable in exactly the
/// way they most need testing. Second, there is no setter behind
/// `RESOURCE_DIR`'s `OnceLock` (`macast-core/src/paths.rs`), so a test could not
/// point it somewhere else even if the directory existed.
///
/// `{header_extra}` and `{service_extra}` and the nine other braces are **not**
/// substituted here: this module reads the template, it does not fill it. That
/// belongs to `crate::description`.
pub const DESCRIPTION_XML: &str = include_str!("../../macast/assets/xml/Description.xml");

/// `macast/assets/xml/AVTransport.xml` — the only one of the three that declares a
/// `<defaultValue>` (twice; see the tests).
pub const AVTRANSPORT_XML: &str = include_str!("../../macast/assets/xml/AVTransport.xml");

/// `macast/assets/xml/RenderingControl.xml`.
pub const RENDERING_CONTROL_XML: &str = include_str!("../../macast/assets/xml/RenderingControl.xml");

/// `macast/assets/xml/ConnectionManager.xml`.
pub const CONNECTION_MANAGER_XML: &str = include_str!("../../macast/assets/xml/ConnectionManager.xml");

/// `{ns}local`, the form lxml calls a Clark name and the only form this module
/// compares tags in. Written as pushes rather than `format!` because this runs on
/// every node of every document.
pub fn clark(ns: &str, local: &str) -> String {
    let mut out = String::with_capacity(ns.len() + local.len() + 2);
    out.push('{');
    out.push_str(ns);
    out.push('}');
    out.push_str(local);
    out
}

/// The six datatypes `macast/protocol.py:504-510` declares. Anything else is a
/// document we cannot read: Python raises `ValueError` from `DataType(datatype)`,
/// and an `allowedValueRange` on a `string` would be a different bug we do not
/// have to guess about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    Boolean,
    I2,
    Ui2,
    I4,
    Ui4,
    Str,
}

impl DataType {
    /// Python's `DataType(text)` constructor, including the failure.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "boolean" => Ok(DataType::Boolean),
            "i2" => Ok(DataType::I2),
            "ui2" => Ok(DataType::Ui2),
            "i4" => Ok(DataType::I4),
            "ui4" => Ok(DataType::Ui4),
            "string" => Ok(DataType::Str),
            other => Err(format!("unknown datatype '{other}'")),
        }
    }

    /// The spelling this value came from, so an error can quote the document.
    pub fn as_str(self) -> &'static str {
        match self {
            DataType::Boolean => "boolean",
            DataType::I2 => "i2",
            DataType::Ui2 => "ui2",
            DataType::I4 => "i4",
            DataType::Ui4 => "ui4",
            DataType::Str => "string",
        }
    }

    /// `macast/protocol.py:516`: `self.value = '' if self.datatype ==
    /// DataType.string else 0`.
    ///
    /// Note what that says about a `boolean`: its default is the **integer `0`**,
    /// not `False`. Python never coerces it, and neither do we — a control point
    /// that reads `Mute` before anyone has set it hears `0`, and the golden table
    /// records `0` with `value_type` `int`. Changing that would change what
    /// `str(value)` puts into a `<LastChange>` (`store::wire` renders `True`, not
    /// `true`), so the difference is on the wire, not in a struct field.
    pub fn default_value(self) -> Value {
        match self {
            DataType::Str => json!(""),
            _ => json!(0),
        }
    }
}

/// One `stateVariable` from an SCPD document, with the value Python would be
/// holding after `init_services()`.
///
/// `service` is not decoration: `send_states_to_clients`
/// (`macast/protocol.py:~1050-1100`) filters on
/// `self.state_list[name].service == client.service`, so *which service owns a
/// name* decides who hears a change. With two services declaring the same name,
/// that attribution is the whole difference between correct and wrong, and the
/// golden table has two of those.
#[derive(Clone, Debug, PartialEq)]
pub struct StateVariable {
    pub name: String,
    /// Python's `sendEvents`, same camelCase, because same wire meaning.
    pub send_events: bool,
    pub datatype: DataType,
    pub service: String,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    pub allowed_value_list: Option<Vec<String>>,
    pub value: Value,
}

impl StateVariable {
    /// `macast/protocol.py:511-518`. The datatype arrives as text because that is
    /// how it is read from the document, and because the `ValueError` has to name
    /// the service and variable that caused it.
    pub fn new(
        name: &str,
        send_events: &str,
        datatype: &str,
        service: &str,
    ) -> Result<Self, String> {
        let datatype = DataType::parse(datatype)
            .map_err(|e| format!("{service}/{name}: {e}"))?;
        Ok(Self {
            name: name.to_owned(),
            send_events: send_events == "yes",
            datatype,
            service: service.to_owned(),
            minimum: None,
            maximum: None,
            allowed_value_list: None,
            value: datatype.default_value(),
        })
    }

    /// `set_value`. `None` here is lxml's `.text is None` — an empty element —
    /// and Python stores that `None` without coercion. So do we, as JSON `null`.
    pub fn set_value(&mut self, text: Option<&str>) {
        self.value = match text {
            None => Value::Null,
            Some(s) => Value::String(s.to_owned()),
        };
    }

    /// `set_allowed_value_list`, including its side effect: a list that contains
    /// `NOT_IMPLEMENTED` becomes the current value. That is where the five
    /// `NOT_IMPLEMENTED` rows in the golden table come from, and it is why the
    /// list is stored *before* the search — a reader that set the value only on
    /// the way out would still look right and be wrong.
    pub fn set_allowed_value_list(&mut self, values: Vec<String>) {
        if values.iter().any(|v| v == "NOT_IMPLEMENTED") {
            self.value = json!("NOT_IMPLEMENTED");
        }
        self.allowed_value_list = Some(values);
    }

    /// `set_allowed_value_range`. **`<step>` is never read**, by Python or by us;
    /// `AVTransport.xml:408-415` has a `<step>1</step>` sitting in the middle of
    /// the `CurrentTrack` range and it reaches nobody. Taking two ints rather than
    /// two `&str` puts the `int()` conversion at the call site, where Python has
    /// it, so a range we cannot parse names the variable rather than the field.
    pub fn set_allowed_value_range(&mut self, minimum: i64, maximum: i64) {
        self.minimum = Some(minimum);
        self.maximum = Some(maximum);
    }
}

/// One `argument` of an action. Python's `Argument(name, state, value=None)`
/// carries a third parameter that nothing ever passes and nothing reads; it is
/// dropped, and this sentence is the record that it was not overlooked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argument {
    pub name: String,
    pub state: String,
}

/// One `action`: its name and its arguments split by direction.
///
/// Order is the document's order within each list. SOAP responses are built by
/// walking `output` (`macast/protocol.py:1147-1160`), so a control point reads
/// `<Name>Value</Name>` children in this sequence; a set would not be wrong on
/// any single call and would be wrong on `GetProtocolInfo`, whose two outputs
/// have a fixed order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub name: String,
    pub input: Vec<Argument>,
    pub output: Vec<Argument>,
}

/// One service: the `serviceId` name, the `serviceType` URN it was read from,
/// and its actions in declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub namespace: String,
    pub actions: Vec<Action>,
}

impl Service {
    pub fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.name == name)
    }
}

/// Both tables at once: the flat state list and the services.
///
/// `states` is insertion-ordered and **flat across all three services**, exactly
/// like Python's `state_list` dict, because that ordering is what a `<LastChange>`
/// and a SOAP response child list inherit. See the module doc for the two names
/// that collide.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scpd {
    pub states: Vec<StateVariable>,
    pub services: Vec<Service>,
}

impl Scpd {
    pub fn state(&self, name: &str) -> Option<&StateVariable> {
        self.states.iter().find(|s| s.name == name)
    }

    pub fn state_mut(&mut self, name: &str) -> Option<&mut StateVariable> {
        self.states.iter_mut().find(|s| s.name == name)
    }

    pub fn state_index(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|s| s.name == name)
    }

    pub fn state_names(&self) -> Vec<&str> {
        self.states.iter().map(|s| s.name.as_str()).collect()
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn service_namespace(&self, name: &str) -> Option<&str> {
        self.service(name).map(|s| s.namespace.as_str())
    }

    /// Which service owns a state variable name. This is the GENA filter's
    /// question (`client.service == state_list[name].service`), and it is the
    /// reason the collision handling below is written the way it is.
    pub fn service_of(&self, state_name: &str) -> Option<&str> {
        self.state(state_name).map(|s| s.service.as_str())
    }

    /// `macast/protocol.py:676-731` — read one SCPD document into both tables.
    ///
    /// `namespace` is the `serviceType` text the description gave, and `service`
    /// is the name split out of it; the two arrive as separate arguments because
    /// that is how Python calls it, and because the split has its own failure mode
    /// (see [`service_name_from_urn`]).
    pub fn build_action(&mut self, namespace: &str, service: &str, dom: &Dom) -> Result<(), String> {
        let tag = |local: &str| clark(SCPD_NS, local);

        // ---- state variables -------------------------------------------------
        // `xml.iter(ns + 'stateVariable')` — deep, includes self. The root of an
        // SCPD document is `scpd`, so "includes self" never matches here; the
        // depth matters for the nesting under `serviceStateTable`.
        for sv in dom.iter_subtree(dom.root(), &tag("stateVariable")) {
            let ctx = |what: &str| format!("{service}: stateVariable {what}");
            let name = child_text(dom, sv, &tag("name"), "has no readable <name>")?;
            // Python reads `attrib['sendEvents']`, which is a KeyError when the
            // attribute is absent. Every one of the 50 declarations in these three
            // documents has it, so that raise is unreachable *here* — mirrored as
            // Err anyway, because a document we cannot read is not something to
            // serve on a guess.
            let send_events = dom.attr(sv, "sendEvents").ok_or_else(|| {
                format!("{service}: stateVariable '{name}' declares no sendEvents")
            })?;
            let datatype = child_text(dom, sv, &tag("dataType"), &ctx("'s <dataType>"))?;
            let mut data = StateVariable::new(name, send_events, datatype, service)?;

            // `find` — direct children only, and `None` is a real answer here:
            // only AVTransport.xml declares a defaultValue, twice.
            if let Some(def) = dom.find(sv, &tag("defaultValue")) {
                data.set_value(dom.text(def));
            }
            if let Some(list) = dom.find(sv, &tag("allowedValueList")) {
                let mut values = Vec::new();
                for v in dom.find_all(list, &tag("allowedValue")) {
                    // `value.text` — the element's **own** text, not a child named
                    // `text`. Python would put `None` in the list for an empty
                    // `<allowedValue/>`; we cannot represent that in `Vec<String>`,
                    // and an empty one is a document nobody should serve, so it is
                    // refused rather than silently read as "".
                    let value = dom
                        .text(v)
                        .ok_or_else(|| ctx("has an <allowedValue> with no text"))?;
                    values.push(value.to_owned());
                }
                data.set_allowed_value_list(values);
            }
            if let Some(range) = dom.find(sv, &tag("allowedValueRange")) {
                let minimum = child_text(dom, range, &tag("minimum"), &ctx("'s <minimum>"))?;
                let maximum = child_text(dom, range, &tag("maximum"), &ctx("'s <maximum>"))?;
                // Python does `int(text)`; `trim()` first is the one deliberate
                // widening, and it is safe because a leading newline is how these
                // documents are written, not a value.
                data.set_allowed_value_range(
                    parse_int(minimum, &ctx("'s <minimum>"))?,
                    parse_int(maximum, &ctx("'s <maximum>"))?,
                );
            }

            // `self.state_list[name] = data` — a dict assignment, which in CPython
            // replaces the value and keeps the original insertion position. That is
            // why this is position-then-overwrite and not a plain push: with
            // RenderingControl re-declaring `LastChange` and `A_ARG_TYPE_InstanceID`
            // after AVTransport did, the row that wins is the *later* one while the
            // *slot* stays where the earlier one put it. A push here would produce a
            // 50-row table, and a `send_states_to_clients` that says one thing and
            // a `<LastChange>` that says another.
            match self.states.iter().position(|s| s.name == data.name) {
                Some(i) => self.states[i] = data,
                None => self.states.push(data),
            }
        }

        // ---- actions ---------------------------------------------------------
        let mut actions: Vec<Action> = Vec::new();
        for action in dom.iter_subtree(dom.root(), &tag("action")) {
            let name = child_text(dom, action, &tag("name"), &format!("{service}: action"))?;
            let mut input = Vec::new();
            let mut output = Vec::new();
            if let Some(list) = dom.find(action, &tag("argumentList")) {
                for arg in dom.find_all(list, &tag("argument")) {
                    let ctx = format!("{service}/{name}: argument");
                    let arg_name = child_text(dom, arg, &tag("name"), &ctx)?;
                    let state = child_text(dom, arg, &tag("relatedStateVariable"), &ctx)?;
                    let data = Argument {
                        name: arg_name.to_owned(),
                        state: state.to_owned(),
                    };
                    // `direction` is the one place a *text-less* element is a real
                    // answer (`<direction/>` → None → not `'in'` → output), so this
                    // asks differently from the two above: missing child is an Err,
                    // present-and-empty is `None` and goes to output.
                    let direction = child_optional_text(dom, arg, &tag("direction"), &ctx)?;
                    if direction == Some("in") {
                        input.push(data);
                    } else {
                        output.push(data);
                    }
                }
            }
            // `actions = {}` is local per service and keyed by name, so a repeated
            // action name overwrites in place. Replicated: replace, keep position.
            // The golden proves these three documents declare no duplicates.
            let action = Action {
                name: name.to_owned(),
                input,
                output,
            };
            match actions.iter().position(|a| a.name == action.name) {
                Some(i) => actions[i] = action,
                None => actions.push(action),
            }
        }
        // `Service.build(service, namespace, actions)` writes into a
        // process-global class dict in Python. Here the Scpd owns its services, in
        // the order the *description* asked for them.
        let service = Service {
            name: service.to_owned(),
            namespace: namespace.to_owned(),
            actions,
        };
        match self.services.iter().position(|s| s.name == service.name) {
            Some(i) => self.services[i] = service,
            None => self.services.push(service),
        }
        Ok(())
    }

    /// `macast/protocol.py:661-674` — walk a parsed `description.xml` and build
    /// every service it names.
    ///
    /// `scpd_document` is the seam: this crate embeds the three standard documents
    /// (`Scpd::embedded`), and anything else — a plugin that ships its own SCPD —
    /// supplies its own reader. The description decides **both** the service set
    /// and its order, so a reordered description produces reordered services here
    /// and nowhere else.
    pub fn from_description(
        desc: &Dom,
        scpd_document: &mut dyn FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let mut out = Self::default();
        let tag = clark(DEVICE_NS, "serviceType");
        for node in desc.iter_subtree(desc.root(), &tag) {
            let namespace = desc
                .text(node)
                .ok_or_else(|| "description: <serviceType> has no text".to_owned())?;
            let service = service_name_from_urn(namespace)?;
            let xml = scpd_document(service)?;
            let doc = Dom::parse(&xml)?;
            out.build_action(namespace, service, &doc)?;
        }
        Ok(out)
    }

    /// The three standard services read from the documents this crate embeds, in
    /// the order `Description.xml` names them. Values are the schema defaults
    /// only — call [`Scpd::apply_init_state`] for what the shipped receiver
    /// boots with.
    pub fn embedded() -> Result<Self, String> {
        let desc = Dom::parse(DESCRIPTION_XML)?;
        let mut reader = |service: &str| match doc_for_service(service) {
            Some(text) => Ok(text.to_owned()),
            None => Err(format!("no embedded SCPD document for service '{service}'")),
        };
        Self::from_description(&desc, &mut reader)
    }

    /// `macast/protocol.py:650-659` — the nine writes that run right after
    /// `init_services()`. An unknown name is refused (Python's `set_state` would
    /// `KeyError`) rather than silently added: adding is what would let a typo
    /// become a variable no control point was ever told about.
    pub fn apply_init_state(&mut self, sink_protocol_info: &str) -> Result<(), String> {
        for (name, value) in init_state_seeds(sink_protocol_info) {
            let index = self
                .state_index(&name)
                .ok_or_else(|| format!("init_state: no such state variable '{name}'"))?;
            self.states[index].value = value;
        }
        Ok(())
    }
}

/// The nine writes, in order, as `(name, value)`.
///
/// `sink_protocol_info` is a parameter rather than a read of
/// `macast/xml/SinkProtocolInfo.csv`, because this crate must not become a second
/// owner of that file: `src-tauri/src/host.rs` already reads it at runtime
/// through `paths::base_path` and logs when it cannot. Python's
/// `load_xml(XMLPath.PROTOCOL_INFO.value).strip()`
/// (`macast/protocol.py:659`, and `load_xml` at `macast/utils.py:750-753` is a
/// plain text read — it does not parse) is the same two operations: take the whole
/// file's text, trim it.
///
/// `TransportPlaySpeed` is the **number** `1`, not a string: Python passes an int
/// to `set_state`, so the first `<LastChange>` a control point hears for that
/// variable says `1` and, after a `set` from the wire, may say `1.0`. That is the
/// golden's `value_type` column's whole reason for existing.
pub fn init_state_seeds(sink_protocol_info: &str) -> Vec<(String, Value)> {
    vec![
        ("CurrentPlayMode".to_owned(), json!("NORMAL")),
        ("TransportPlaySpeed".to_owned(), json!(1)),
        ("TransportStatus".to_owned(), json!("OK")),
        (
            "RelativeCounterPosition".to_owned(),
            json!(2147483647i64),
        ),
        (
            "AbsoluteCounterPosition".to_owned(),
            json!(2147483647i64),
        ),
        ("A_ARG_TYPE_Direction".to_owned(), json!("Output")),
        ("CurrentConnectionIDs".to_owned(), json!("0")),
        ("PlaybackStorageMedium".to_owned(), json!("None")),
        (
            "SinkProtocolInfo".to_owned(),
            Value::String(sink_protocol_info.trim().to_owned()),
        ),
    ]
}

/// `macast/protocol.py:669`: `text.split(":")[3]` of
/// `urn:schemas-upnp-org:service:AVTransport:1`. Fewer segments is Python's
/// `IndexError`, so it is an `Err` that quotes what it was given.
pub fn service_name_from_urn(urn: &str) -> Result<&str, String> {
    urn.split(':').nth(3).ok_or_else(|| {
        format!("serviceType '{urn}' is not of the form urn:schemas-upnp-org:service:<name>:<n>")
    })
}

/// Which embedded document serves a service name. The three the receiver ships
/// with; anything else has no default and the caller must supply it.
pub fn doc_for_service(name: &str) -> Option<&'static str> {
    match name {
        "AVTransport" => Some(AVTRANSPORT_XML),
        "RenderingControl" => Some(RENDERING_CONTROL_XML),
        "ConnectionManager" => Some(CONNECTION_MANAGER_XML),
        _ => None,
    }
}

fn parse_int(text: &str, ctx: &str) -> Result<i64, String> {
    text.trim()
        .parse::<i64>()
        .map_err(|e| format!("{ctx} is not an integer ({text:?}): {e}"))
}

/// lxml's `element.find(tag).text` where the caller cannot tolerate `None`: a
/// missing child element **or** a child with no text is refused, because Python
/// would raise `AttributeError` on `None.text` at exactly these two shapes.
fn child_text<'a>(dom: &'a Dom, parent: usize, tag: &str, ctx: &str) -> Result<&'a str, String> {
    let node = dom
        .find(parent, tag)
        .ok_or_else(|| format!("{ctx}: no <{tag}> child element"))?;
    dom.text(node)
        .ok_or_else(|| format!("{ctx}: <{tag}> has no text"))
}

/// lxml's `element.find(tag).text` where a `None` **is** the answer we model —
/// used only for `<direction>`, where `None == 'in'` is False and therefore means
/// "output". A missing child element still raises in Python, so it is still `Err`.
fn child_optional_text<'a>(
    dom: &'a Dom,
    parent: usize,
    tag: &str,
    ctx: &str,
) -> Result<Option<&'a str>, String> {
    let node = dom
        .find(parent, tag)
        .ok_or_else(|| format!("{ctx}: no <{tag}> child element"))?;
    Ok(dom.text(node))
}

// ---------------------------------------------------------------------------
// The DOM
// ---------------------------------------------------------------------------

/// One element. `tag` is the resolved Clark name (or a bare local name when there
/// is no namespace to resolve — which for an unprefixed *attribute* is always, and
/// for an unprefixed element only when the document declares no default namespace).
///
/// `attrs` is in **document order** and holds no namespace declarations. Both are
/// contracts, not tidiness: the description template's placeholders ride on the
/// attribute order of whatever a plugin injects as `{service_extra}`, and lxml's
/// `.attrib` is an ordered dict too.
///
/// `prefixes` is where the declarations that `attrs` drops go, and it is what
/// [`Dom::binding`] reads. A DOM that resolves names but cannot say what a prefix
/// was bound to is fine for our four documents and useless for the one thing that
/// reads a *sender's* XML — see [`Dom::binding`].
/// `Debug` is derived because two tests hold a `Result<Dom, String>` and call
/// `expect_err()` on it, and a type that cannot be printed turns a failure into
/// "the test does not compile" — which is a worse witness than a failure.
#[derive(Debug)]
struct Node {
    tag: String,
    attrs: Vec<(String, String)>,
    prefixes: Vec<(String, String)>,
    text: Option<String>,
    children: Vec<usize>,
}

/// An open element while parsing: where its node is, which namespace bindings its
/// children see, the text gathered so far, and whether a child has started (which
/// is what ends `.text`).
struct Frame {
    node: usize,
    scopes: Vec<(String, String)>,
    text: String,
    saw_child: bool,
}

/// A minimal arena DOM: nodes in a flat `Vec`, addressed by `usize`.
///
/// Handles are indices rather than references on purpose — the parser pushes into
/// the arena while it still needs to reach ancestors, and a tree of `Box`es would
/// make that a borrow fight. Indices are opaque: they are valid only against the
/// [`Dom`] that produced them and only for its lifetime.
#[derive(Debug)]
pub struct Dom {
    nodes: Vec<Node>,
    root: usize,
}

impl Dom {
    /// The root element's handle.
    pub fn root(&self) -> usize {
        self.root
    }

    pub fn tag(&self, node: usize) -> &str {
        &self.nodes[node].tag
    }

    pub fn attr(&self, node: usize, name: &str) -> Option<&str> {
        self.nodes[node]
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The namespace URI declared for `prefix` **on this element** — the default
    /// namespace is `""`, and `None` means this element does not declare it.
    ///
    /// This is what lxml's `nsmap` answers for a *root*, and a root is the only
    /// place it is ever asked: `macast/protocol.py:1090` reads `meta.nsmap['dc']`
    /// on the DIDL-Lite root, and a root has no ancestors to inherit from, so
    /// "declared here" and "in scope here" are the same set for it. A sender that
    /// declares `xmlns:dc` on a *descendant* instead is answered differently by
    /// accident of language — `KeyError` in Python (which lands in the same
    /// `except Exception` as a broken envelope ⇒ `meta_failed`) and `None` here —
    /// so the reader must treat both as "this document never told me where `dc`
    /// is", not as a bug in one of them.
    ///
    /// Two differences from `nsmap` are recorded rather than smoothed over.
    /// `xml` is implicit in lxml and never asked of this function; and a prefix
    /// bound to empty is `None` in lxml's nsmap and `Some("")` here, because our
    /// parser keeps the declared string. The reader passes it to `clark`, which
    /// yields `{}` + local — a tag no node in the arena can have, since name
    /// resolution refuses a prefix bound to nothing — so the answer stays "nothing
    /// matched", exactly as `find('.//{None}title')` finds nothing in Python.
    ///
    /// The declarations shown here are the ones **this element** made, and nothing
    /// else: `nsmap` is a per-element view in lxml too, but its view is the whole
    /// inherited set, so a reader that wants lxml's answer has to walk up. The
    /// `.rev()` is the scope rule's shape kept for the arena, not a promise about
    /// input — `Dom::parse` cannot feed it a same-element redeclaration, because
    /// quick-xml refuses a duplicated attribute outright (measured, and one of the
    /// tests below says so). The redeclarations that do reach the arena are an
    /// ancestor's and a descendant's, and those live on different nodes.
    pub fn binding(&self, node: usize, prefix: &str) -> Option<&str> {
        self.nodes[node]
            .prefixes
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.as_str())
    }

    /// lxml's `.text`: characters before the first child element, `None` if there
    /// is one. No trimming — `<defaultValue> NORMAL </defaultValue>` is a
    /// five-character string in both languages.
    pub fn text(&self, node: usize) -> Option<&str> {
        self.nodes[node].text.as_deref()
    }

    pub fn children(&self, node: usize) -> &[usize] {
        &self.nodes[node].children
    }

    /// Every element in the subtree, `node` included, in document order, with no
    /// tag filter. `iter_subtree` answers "who is called X"; this answers "walk
    /// everything", which is what `macast/protocol.py:143`'s `meta.iter()` does
    /// when it looks for the first `<res>` that has a `protocolInfo` — the local
    /// name is compared *inside* the loop, against a name it cannot know in
    /// advance (the same element may arrive as `res`, `{urn:...}res`, or with the
    /// namespace spelled differently by a different sender).
    pub fn iter_all(&self, node: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(i) = stack.pop() {
            out.push(i);
            for c in self.nodes[i].children.iter().copied().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// `find(tag)` — direct children only, first in document order.
    pub fn find(&self, node: usize, tag: &str) -> Option<usize> {
        self.nodes[node].children.iter().copied().find(|c| self.tag(*c) == tag)
    }

    /// `findall(tag)` — direct children only, document order.
    pub fn find_all(&self, node: usize, tag: &str) -> Vec<usize> {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .filter(|c| self.tag(*c) == tag)
            .collect()
    }

    /// `iter(tag)` — the whole subtree **including `node` itself**, document
    /// order. See the module doc for why this is a separate function.
    pub fn iter_subtree(&self, node: usize, tag: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(i) = stack.pop() {
            if self.tag(i) == tag {
                out.push(i);
            }
            // Reverse push so the explicit stack pops children left to right.
            for c in self.nodes[i].children.iter().copied().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// Parse a document into the arena.
    ///
    /// Everything a real DLNA control point's own parser would refuse, this
    /// refuses, with a message. The witnesses are split, and the split is
    /// deliberate: quick-xml's structural checks own "the bytes are not a
    /// document" (`<r`, an unmatched `</r>` with nothing open, a reference to an
    /// undeclared entity), while **this** owns every question about *names* —
    /// `check_end_names` is turned off below precisely so that a mismatched end
    /// tag reaches the resolved-name comparison rather than dying inside the
    /// reader. Two reasons that one is ours and not quick-xml's: the message a
    /// developer needs names the tags **after** namespace resolution (`<root:a>
    /// was closed by </a>` is about `{urn:x}a`, and the raw pair is what tells
    /// you which scope was wrong), and an undeclared prefix must be refused *as
    /// a prefix*, not silently accepted as an unprefixed name.
    ///
    /// So: no branch below is dead code that reads like a guard. Each one is the
    /// only place its own refusal is decided, and the test that names it checks
    /// that the sentence is the one a person can act on.
    pub fn parse(xml: &str) -> Result<Self, String> {
        let mut reader = quick_xml::Reader::from_str(xml);
        reader.config_mut().check_end_names = false;
        let mut nodes: Vec<Node> = Vec::new();
        let mut frames: Vec<Frame> = Vec::new();
        let mut root: Option<usize> = None;

        loop {
            let event = reader
                .read_event()
                .map_err(|e| format!("xml parse error: {e}"))?;
            match event {
                quick_xml::events::Event::Eof => break,
                quick_xml::events::Event::Start(start) => {
                    let inherited = match frames.last() {
                        Some(f) => f.scopes.clone(),
                        None => base_scopes(),
                    };
                    let (idx, scopes) = make_node(&start, &inherited, &mut nodes)?;
                    attach(&mut frames, &mut nodes, &mut root, idx)?;
                    frames.push(Frame {
                        node: idx,
                        scopes,
                        text: String::new(),
                        saw_child: false,
                    });
                }
                quick_xml::events::Event::Empty(start) => {
                    let inherited = match frames.last() {
                        Some(f) => f.scopes.clone(),
                        None => base_scopes(),
                    };
                    let (idx, _scopes) = make_node(&start, &inherited, &mut nodes)?;
                    attach(&mut frames, &mut nodes, &mut root, idx)?;
                    // No frame: an empty element's text stays None, which is what
                    // `<x/>` means to lxml as well as `<x></x>`.
                }
                quick_xml::events::Event::End(end) => {
                    let frame = frames.pop().ok_or_else(|| {
                        format!("closing tag '</{}>' with nothing open", end.name().0)
                    })?;
                    let closed = resolve_name(end.name().0, &frame.scopes, false)?;
                    if closed != nodes[frame.node].tag {
                        return Err(format!(
                            "<{}> was closed by </{}>",
                            nodes[frame.node].tag, end.name().0
                        ));
                    }
                    nodes[frame.node].text = if frame.text.is_empty() {
                        None
                    } else {
                        Some(frame.text)
                    };
                }
                quick_xml::events::Event::Text(t) => {
                    if let Some(frame) = frames.last_mut() {
                        if !frame.saw_child {
                            frame.text.push_str(&t.into_inner());
                        }
                    }
                }
                quick_xml::events::Event::CData(t) => {
                    // CDATA is text to lxml (`.text` includes it) and it is *not* a
                    // child element, so it does not end `.text` either.
                    if let Some(frame) = frames.last_mut() {
                        if !frame.saw_child {
                            frame.text.push_str(&t.into_inner());
                        }
                    }
                }
                quick_xml::events::Event::GeneralRef(r) => {
                    // quick-xml splits text at every entity reference and hands the
                    // bare name here (`a&amp;b` arrives as Text("a"), GeneralRef,
                    // Text("b")), so an entity is *not* in the Text events. Resolve
                    // every one: a reference we cannot name is a document we should
                    // not serve, so the Err is unconditional even though our own
                    // four documents contain no `&` at all.
                    let name = r.into_inner();
                    let replacement = resolve_reference(&name)?;
                    if let Some(frame) = frames.last_mut() {
                        if !frame.saw_child {
                            frame.text.push_str(&replacement);
                        }
                    }
                }
                quick_xml::events::Event::Comment(_) | quick_xml::events::Event::PI(_) => {
                    // A comment ends `.text` in lxml exactly the way a child
                    // element does, but it is not a child element. Both halves of
                    // that matter, so this arm sets the flag and adds no node.
                    if let Some(frame) = frames.last_mut() {
                        frame.saw_child = true;
                    }
                }
                quick_xml::events::Event::Decl(_) | quick_xml::events::Event::DocType(_) => {}
            }
        }

        if let Some(frame) = frames.first() {
            return Err(format!(
                "document ends with {} element(s) still open, starting at <{}>",
                frames.len(),
                nodes[frame.node].tag
            ));
        }
        let root = root.ok_or_else(|| "document has no root element".to_owned())?;
        Ok(Self { nodes, root })
    }
}

/// The bindings every document starts with, before any `xmlns`.
fn base_scopes() -> Vec<(String, String)> {
    vec![("xml".to_owned(), XML_NS.to_owned())]
}

/// Turn a `BytesStart` into a node, and report the scope stack its children see.
///
/// Two passes, and the order of the passes is the interesting part. Pass 1 reads
/// only namespace declarations; pass 2 resolves the element's **own** name and
/// every ordinary attribute name. That is not a convenience split — a document may
/// declare a prefix on the very element that uses it, and `Description.xml` does:
///
/// ```xml
/// <dlna:X_DLNADOC xmlns:dlna="urn:schemas-dlna-org:device-1-0">DMR-1.50</dlna:X_DLNADOC>
/// ```
///
/// Resolving `dlna:X_DLNADOC` before applying that declaration would be an
/// "undeclared prefix" error on a document the shipped receiver parses daily.
fn make_node(
    start: &quick_xml::events::BytesStart<'_>,
    inherited: &[(String, String)],
    nodes: &mut Vec<Node>,
) -> Result<(usize, Vec<(String, String)>), String> {
    let raw_tag = start.name().0.to_owned();
    let mut scopes = inherited.to_vec();
    let mut prefixes: Vec<(String, String)> = Vec::new();

    for attr in start.attributes() {
        let attr = attr.map_err(|e| format!("<{raw_tag}> has a malformed attribute: {e}"))?;
        match attr.key.as_namespace_binding() {
            // `xmlns="…"` binds the default namespace. quick-xml's `Named("")`
            // (an `xmlns:` with nothing after the colon) is not valid XML and it
            // means the same thing here; folding it in is documented rather than
            // silent, because none of our four documents ever produce it.
            Some(quick_xml::name::PrefixDeclaration::Default)
            | Some(quick_xml::name::PrefixDeclaration::Named("")) => {
                let uri = value_of(&attr, &raw_tag)?;
                scopes.push((String::new(), uri.clone()));
                prefixes.push((String::new(), uri));
            }
            Some(quick_xml::name::PrefixDeclaration::Named(prefix)) => {
                let uri = value_of(&attr, &raw_tag)?;
                scopes.push((prefix.to_owned(), uri.clone()));
                prefixes.push((prefix.to_owned(), uri));
            }
            None => {}
        }
    }

    let tag = resolve_name(&raw_tag, &scopes, false)?;

    let mut attrs: Vec<(String, String)> = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(|e| format!("<{raw_tag}> has a malformed attribute: {e}"))?;
        if attr.key.as_namespace_binding().is_some() {
            continue;
        }
        // An unprefixed attribute is *never* in the default namespace — that is
        // XML's rule, lxml's `attrib` keys, and the reason `sendEvents` is read as
        // a bare name.
        let name = resolve_name(attr.key.0, &scopes, true)?;
        attrs.push((name, value_of(&attr, &raw_tag)?));
    }

    nodes.push(Node {
        tag,
        attrs,
        prefixes,
        text: None,
        children: Vec::new(),
    });
    Ok((nodes.len() - 1, scopes))
}

/// An attribute's value, normalized the way XML says an attribute value is before
/// any consumer sees it: tab/CR/LF to a space, and the five predefined entities
/// resolved. That is exactly lxml's AVNormalize, and it is quick-xml's
/// `normalized_value` — not the deprecated `unescape_value`, which does not fold
/// whitespace and so would keep a literal newline in a value that reaches a wire.
fn value_of(
    attr: &quick_xml::events::attributes::Attribute<'_>,
    raw_tag: &str,
) -> Result<String, String> {
    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .map(|v| v.into_owned())
        .map_err(|e| format!("attribute {} in <{}>: {e}", attr.key.0, raw_tag))
}

/// Resolve a raw qualified name against a scope stack. `is_attribute` selects the
/// one rule that differs between the two: unprefixed attributes have no namespace,
/// ever.
fn resolve_name(raw: &str, scopes: &[(String, String)], is_attribute: bool) -> Result<String, String> {
    if let Some((prefix, local)) = raw.split_once(':') {
        let uri = lookup(scopes, prefix).ok_or_else(|| {
            format!("undeclared namespace prefix '{prefix}' in '{raw}'")
        })?;
        if uri.is_empty() {
            // `xmlns:p=""` is namespace-less, which lxml reads as `{}`local. None
            // of our documents do this; refusing it is the fail-closed side, and
            // this sentence is the record that the choice was made.
            return Err(format!("prefix '{prefix}' in '{raw}' is bound to no namespace"));
        }
        return Ok(clark(uri, local));
    }
    if is_attribute {
        return Ok(raw.to_owned());
    }
    match lookup(scopes, "") {
        Some(uri) if !uri.is_empty() => Ok(clark(uri, raw)),
        // `xmlns=""` un-binds the default namespace for this subtree, so elements
        // below it are bare — again matching lxml.
        _ => Ok(raw.to_owned()),
    }
}

/// Innermost binding for a prefix wins, and a later binding of the same prefix at
/// the same level wins — so the scan is from the end.
fn lookup<'s>(scopes: &'s [(String, String)], prefix: &str) -> Option<&'s str> {
    scopes
        .iter()
        .rev()
        .find(|(p, _)| p == prefix)
        .map(|(_, uri)| uri.as_str())
}

/// Put a finished node under its parent (or make it the root).
fn attach(
    frames: &mut Vec<Frame>,
    nodes: &mut Vec<Node>,
    root: &mut Option<usize>,
    idx: usize,
) -> Result<(), String> {
    let Some(frame) = frames.last() else {
        if root.is_some() {
            return Err("document has two root elements".to_owned());
        }
        *root = Some(idx);
        return Ok(());
    };
    let parent = frame.node;
    nodes[parent].children.push(idx);
    // `saw_child` after the push: the borrow above ends with the index, and this
    // is the only reason the two are separate statements rather than one
    // `frames.last_mut()`.
    if let Some(frame) = frames.last_mut() {
        frame.saw_child = true;
    }
    Ok(())
}

/// Resolve one of the five predefined entities, or a numeric character reference
/// (`&#65;`, `&#x41;`). Anything else is undeclared and an `Err`, which is also
/// what an XML parser owes the document.
///
/// Numeric resolution is ours rather than `quick_xml::escape::parse_number`,
/// because that helper is `pub(crate)`. quick-xml hands numeric references to the
/// same `GeneralRef` event as named ones — the `&`-scanner does not distinguish
/// them — so forgetting this branch would put a literal `#65;` in a title.
fn resolve_reference(name: &str) -> Result<String, String> {
    match name {
        "amp" => return Ok("&".to_owned()),
        "lt" => return Ok("<".to_owned()),
        "gt" => return Ok(">".to_owned()),
        "quot" => return Ok("\"".to_owned()),
        "apos" => return Ok("'".to_owned()),
        _ => {}
    }
    let digits = name.strip_prefix('#').ok_or_else(|| {
        format!("undeclared entity reference '&{name};'")
    })?;
    let (radix, body) = match digits.strip_prefix(['x', 'X']) {
        Some(rest) => (16, rest),
        None => (10, digits),
    };
    // `from_str_radix` accepts a leading `+`, and no XML character reference has
    // one; `u32::from_str_radix("", 10)` is an error, so an empty body is covered.
    if body.is_empty() || body.starts_with('+') || body.starts_with('-') {
        return Err(format!("malformed character reference '&{name};'"));
    }
    let code = u32::from_str_radix(body, radix)
        .map_err(|e| format!("malformed character reference '&{name};': {e}"))?;
    // Surrogates and out-of-range code points have no `char`; this is the same
    // refusal lxml makes, and the same one a NOTIFY body would have to make.
    char::from_u32(code)
        .map(|c| c.to_string())
        .ok_or_else(|| format!("character reference '&{name};' is not a Unicode scalar value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag `{SCPD_NS}local`, written out short so the tests read as navigation.
    fn s(local: &str) -> String {
        clark(SCPD_NS, local)
    }

    fn d(local: &str) -> String {
        clark(DEVICE_NS, local)
    }

    fn golden(name: &str) -> Value {
        crate::pin_test_dir();
        let text = match name {
            "state" => include_str!("../tests/golden/scpd_state_table.json"),
            "actions" => include_str!("../tests/golden/scpd_actions.json"),
            other => panic!("no such golden: {other}"),
        };
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} golden is not json: {e}"))
    }

    /// The rows the way the golden states them, so a comparison says which field
    /// moved rather than only that something did.
    fn row(sv: &StateVariable) -> Value {
        json!({
            "name": sv.name,
            "service": sv.service,
            "sendEvents": sv.send_events,
            "datatype": sv.datatype.as_str(),
            "value": sv.value,
            "value_type": match sv.value {
                Value::Null => "NoneType",
                Value::Bool(_) => "bool",
                Value::String(_) => "str",
                Value::Number(_) => "int",
                _ => "other",
            },
            "min": sv.minimum,
            "max": sv.maximum,
            "allowed": sv.allowed_value_list,
        })
    }

    fn built() -> Scpd {
        crate::pin_test_dir();
        Scpd::embedded().expect("the three embedded SCPD documents build")
    }

    // ---- the table, against the golden ---------------------------------------

    #[test]
    fn forty_eight_rows_come_from_fifty_declarations() {
        crate::pin_test_dir();
        let scpd = built();
        assert_eq!(scpd.states.len(), 48, "the flat state table");
        // The 50 is the documents' own count, not an inference from the table:
        // AVTransport declares 32, RenderingControl 8, ConnectionManager 10.
        for (doc, expected) in [
            (AVTRANSPORT_XML, 32usize),
            (RENDERING_CONTROL_XML, 8),
            (CONNECTION_MANAGER_XML, 10),
        ] {
            let dom = Dom::parse(doc).expect("parse");
            let n = dom.iter_subtree(dom.root(), &s("stateVariable")).len();
            assert_eq!(n, expected, "stateVariable declarations in {doc}");
        }
    }

    #[test]
    fn every_row_matches_the_real_python_table() {
        crate::pin_test_dir();
        let scpd = built();
        let gold = golden("state");
        let rows: Vec<Value> = scpd.states.iter().map(row).collect();
        let want = gold.as_array().expect("golden is a list");
        assert_eq!(rows.len(), want.len(), "row count");
        for (i, (got, expected)) in rows.iter().zip(want.iter()).enumerate() {
            // Name first: a mismatch on `value` alone is far harder to read than
            // a line that says which variable moved.
            assert_eq!(
                got["name"], expected["name"],
                "row {i} name"
            );
            assert_eq!(got, expected, "row {i} ({})", got["name"]);
        }
    }

    #[test]
    fn the_two_colliding_names_take_the_later_service_and_the_earlier_slot() {
        crate::pin_test_dir();
        let scpd = built();
        // Both are declared by AVTransport first and RenderingControl second.
        for (name, position) in [("LastChange", 2usize), ("A_ARG_TYPE_InstanceID", 12)] {
            let i = scpd.state_index(name).unwrap();
            assert_eq!(i, position, "{name} keeps AVTransport's slot");
            assert_eq!(
                scpd.states[i].service, "RenderingControl",
                "{name}: the later declaration owns it, and that ownership is what \
                 send_states_to_clients filters on"
            );
        }
        // 32 AVTransport + 8 RenderingControl = 40 declarations for 36 slots.
        let per = |service: &str| {
            scpd.states
                .iter()
                .filter(|s| s.service == service)
                .count()
        };
        assert_eq!((per("AVTransport"), per("RenderingControl"), per("ConnectionManager")), (30, 8, 10));
    }

    #[test]
    fn only_four_names_declare_send_events_yes() {
        crate::pin_test_dir();
        let scpd = built();
        let yes: Vec<&str> = scpd
            .states
            .iter()
            .filter(|s| s.send_events)
            .map(|s| s.name.as_str())
            .collect();
        // Document order, not sorted: this is the order a <LastChange> inherits.
        assert_eq!(
            yes,
            ["LastChange", "SourceProtocolInfo", "SinkProtocolInfo", "CurrentConnectionIDs"]
        );
        // And the table really is the whole table: 50 declarations in three
        // documents collapse to 48 rows here, four of which say `yes`. There is no
        // shape where a row stays silent — `sendEvents` is read with no default
        // (a declaration that omits it is refused, and that refusal has its own
        // test below), so "44 answered no" is a measurement of the parse, not a
        // restatement of the four above.
        assert_eq!(scpd.states.len(), 48, "the flat table's size is a wire fact");
        assert_eq!(scpd.states.iter().filter(|s| !s.send_events).count(), 44);
    }

    #[test]
    fn the_schema_leaves_exactly_seven_rows_non_default() {
        crate::pin_test_dir();
        let scpd = built();
        let odd: Vec<(&str, &Value, &str)> = scpd
            .states
            .iter()
            .filter(|s| {
                s.value
                    != s.datatype.default_value()
            })
            .map(|s| (s.name.as_str(), &s.value, s.service.as_str()))
            .collect();
        assert_eq!(odd.len(), 7, "rows: {odd:?}");
        // The order is document order of the `<stateVariable>` openings, which is the
        // flat table's order, and it is **not** "the one defaultValue, then the five
        // lists": `DisplayCurrentSubtitle` (AVTransport.xml:342-346) opens between the
        // first NOT_IMPLEMENTED list (327) and the second (359). Measured against real
        // lxml, and the whole ordered vector is asserted rather than picked at, because
        // the row that surprised us last time is exactly the one an index-based
        // assertion would have kept in the wrong slot.
        let want: Vec<(&str, &str)> = [
            ("CurrentPlayMode", "NORMAL"),
            ("RecordStorageMedium", "NOT_IMPLEMENTED"),
            ("DisplayCurrentSubtitle", "1"),
            ("CurrentRecordQualityMode", "NOT_IMPLEMENTED"),
            ("PossibleRecordQualityModes", "NOT_IMPLEMENTED"),
            ("RecordMediumWriteStatus", "NOT_IMPLEMENTED"),
            ("PossibleRecordStorageMedia", "NOT_IMPLEMENTED"),
        ]
        .to_vec();
        let got: Vec<(&str, String)> = odd
            .iter()
            .map(|(name, value, _)| (*name, value.as_str().unwrap_or("<not a string>").to_owned()))
            .collect();
        assert_eq!(
            got,
            want.iter().map(|(n, v)| (*n, (*v).to_owned())).collect::<Vec<_>>(),
            "rows: {odd:?}"
        );
        // Every one of the seven is an AVTransport row: the two documents that declare
        // no `NOT_IMPLEMENTED` list (RenderingControl, ConnectionManager) therefore also
        // contribute nothing here, which is what makes the count mean something.
        for row in &odd {
            assert_eq!(row.2, "AVTransport", "{}", row.0);
        }
    }

    /// The caps are the allowed value itself (`NOT_IMPLEMENTED`), so the name
    /// quotes the wire rather than describing it in prose.
    #[allow(non_snake_case)]
    #[test]
    fn an_allowed_value_list_containing_NOT_IMPLEMENTED_becomes_the_value() {
        crate::pin_test_dir();
        let mut sv = StateVariable::new("X", "no", "string", "Svc").unwrap();
        assert_eq!(sv.value, json!(""));
        sv.set_allowed_value_list(vec!["A".to_owned(), "NOT_IMPLEMENTED".to_owned()]);
        assert_eq!(sv.value, json!("NOT_IMPLEMENTED"));
        assert_eq!(
            sv.allowed_value_list.as_deref(),
            Some(&["A".to_owned(), "NOT_IMPLEMENTED".to_owned()][..]),
            "the list is stored as well, not instead"
        );
        sv.set_value(Some("later"));
        assert_eq!(sv.value, json!("later"), "set_value is not gated by the list");
    }

    #[test]
    fn a_default_value_is_stored_verbatim_and_never_coerced() {
        crate::pin_test_dir();
        // The real one: RenderingControl declares DisplayCurrentSubtitle as a
        // boolean whose <defaultValue> is the character 1. Python stores the string
        // "1", not True — so does the golden, and that is why `wire()` matters.
        let scpd = built();
        let sv = scpd.state("DisplayCurrentSubtitle").unwrap();
        assert_eq!(sv.datatype, DataType::Boolean);
        assert_eq!(sv.value, json!("1"));
        assert_eq!(crate::store::wire(&sv.value), "1");
    }

    #[test]
    fn a_boolean_default_is_the_integer_zero() {
        crate::pin_test_dir();
        let scpd = built();
        let mute = scpd.state("Mute").unwrap();
        assert_eq!(mute.datatype, DataType::Boolean);
        assert_eq!(mute.value, json!(0), "Python: `0`, not `False`");
        assert_eq!(crate::store::wire(&mute.value), "0");
        assert_eq!(DataType::Boolean.default_value(), json!(0));
        assert_eq!(DataType::Str.default_value(), json!(""));
    }

    #[test]
    fn allowed_value_range_reads_minimum_and_maximum_and_nothing_else() {
        crate::pin_test_dir();
        // The synthetic pair first: `<step>` present, and a range that is not one of
        // the four the real documents carry, so this tests the *rule*.
        let dom = Dom::parse(
            r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><serviceStateTable>
               <stateVariable sendEvents="no"><name>N</name><dataType>ui4</dataType>
                 <allowedValueRange><minimum>3</minimum><maximum>9</maximum><step>2</step></allowedValueRange>
               </stateVariable></serviceStateTable></scpd>"#,
        )
        .unwrap();
        let mut scpd = Scpd::default();
        scpd.build_action("urn:schemas-upnp-org:service:Test:1", "Test", &dom)
            .unwrap();
        let n = scpd.state("N").unwrap();
        assert_eq!((n.minimum, n.maximum), (Some(3), Some(9)));
        let sv = *dom
            .find_all(n_service(&dom), &s("stateVariable"))
            .first()
            .expect("the synthetic document keeps its one stateVariable");
        let range = dom
            .find(sv, &s("allowedValueRange"))
            .expect("the synthetic range is there");
        assert_eq!(
            dom.children(range).len(),
            3,
            "minimum, maximum and a step nobody reads"
        );
        assert_eq!(
            dom.children(range)
                .iter()
                .filter(|c| dom.tag(**c) == s("step"))
                .count(),
            1,
            "the step is really in the document, so `set_allowed_value_range` \
             ignoring it is a fact about this code and not about the file"
        );

        // The real one: CurrentTrack's `<step>1</step>` (AVTransport.xml:408-415).
        let scpd = built();
        let ct = scpd.state("CurrentTrack").unwrap();
        assert_eq!((ct.minimum, ct.maximum), (Some(0), Some(65535)));
        let ranges: Vec<(&str, i64, i64)> = scpd
            .states
            .iter()
            .filter(|s| s.minimum.is_some())
            .map(|s| (s.name.as_str(), s.minimum.unwrap(), s.maximum.unwrap()))
            .collect();
        assert_eq!(
            ranges,
            [
                ("CurrentTrack", 0, 65535),
                ("NumberOfTracks", 0, 65535),
                ("Volume", 0, 100),
                ("VolumeDB", -32767, 32767),
            ]
        );
    }

    /// Helper for the synthetic range test above: the `stateVariable` node.
    fn n_service(dom: &Dom) -> usize {
        dom.find(dom.root(), &s("serviceStateTable")).unwrap()
    }

    #[test]
    fn a_range_missing_its_minimum_is_refused_not_half_read() {
        crate::pin_test_dir();
        for (body, want) in [
            ("<allowedValueRange><maximum>9</maximum></allowedValueRange>", "<minimum>"),
            ("<allowedValueRange><minimum>3</minimum></allowedValueRange>", "<maximum>"),
        ] {
            let dom = Dom::parse(&format!(
                r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><serviceStateTable>
                   <stateVariable sendEvents="no"><name>N</name><dataType>ui4</dataType>{body}
                   </stateVariable></serviceStateTable></scpd>"#
            ))
            .unwrap();
            let mut scpd = Scpd::default();
            let err = scpd
                .build_action("urn:schemas-upnp-org:service:Test:1", "Test", &dom)
                .expect_err("Python raises AttributeError here, so we refuse");
            assert!(err.contains(want), "{want} named in: {err}");
        }
    }

    // ---- actions -------------------------------------------------------------

    #[test]
    fn every_action_and_argument_matches_the_real_python_table() {
        crate::pin_test_dir();
        let scpd = built();
        let gold = golden("actions");
        let map = gold.as_object().expect("golden actions is an object");
        // Top-level key order is the description's service order, and serde_json
        // keeps it (`preserve_order` in the workspace manifest, for exactly the
        // reason stated there).
        let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["AVTransport", "RenderingControl", "ConnectionManager"]);
        for key in keys {
            let want = &map[key];
            let service = scpd
                .service(key)
                .unwrap_or_else(|| panic!("no service {key}"));
            assert_eq!(
                service.namespace,
                want["namespace"].as_str().unwrap(),
                "{key} namespace"
            );
            let order: Vec<&str> = want["order"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let got: Vec<&str> = service.actions.iter().map(|a| a.name.as_str()).collect();
            assert_eq!(got, order, "{key} action order");
            let args = want["args"].as_object().unwrap();
            // The golden's `args` map covers exactly its `order` list, in both
            // directions. The first version of this line was
            // `assert_eq!(args.keys().collect::<Vec<_>>().sort(), (), …)`, which
            // asked nothing at all — `sort()` returns `()` — so it passed for every
            // input while reading as a coverage claim. That matters here more than
            // usual: `args[name]` on a missing key yields JSON `null` rather than
            // panicking, and `null` read as "no arguments" equals the real answer
            // for the actions that genuinely have none.
            let mut covered: Vec<&str> = args.keys().map(|k| k.as_str()).collect();
            covered.sort_unstable();
            let mut expected = order.clone();
            expected.sort_unstable();
            assert_eq!(covered, expected, "{key}: golden args vs order list");
            assert_eq!(
                args.len(),
                order.len(),
                "{key}: the golden's order list has no duplicate action name"
            );
            for name in order {
                let action = service.action(name).unwrap();
                let entry = &args[name];
                assert_eq!(
                    pairs(&action.input),
                    read_pairs(&entry["in"]),
                    "{key}/{name} inputs"
                );
                assert_eq!(
                    pairs(&action.output),
                    read_pairs(&entry["out"]),
                    "{key}/{name} outputs"
                );
            }
        }
        assert_eq!(scpd.services.len(), 3);
        assert_eq!(scpd.service("AVTransport").unwrap().actions.len(), 14);
        assert_eq!(scpd.service("RenderingControl").unwrap().actions.len(), 8);
        assert_eq!(scpd.service("ConnectionManager").unwrap().actions.len(), 3);
    }

    fn pairs(args: &[Argument]) -> Vec<(String, String)> {
        args.iter().map(|a| (a.name.clone(), a.state.clone())).collect()
    }

    fn read_pairs(v: &Value) -> Vec<(String, String)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let r = row.as_array().unwrap();
                (
                    r[0].as_str().unwrap().to_owned(),
                    r[1].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn two_actions_whose_shapes_the_wire_cares_about_verbatim() {
        crate::pin_test_dir();
        let scpd = built();
        // Seek: three inputs, no outputs, in the document's order. A control point
        // sends them in this order and we read them positionally in SOAP.
        let seek = scpd.service("AVTransport").unwrap().action("Seek").unwrap();
        assert_eq!(
            pairs(&seek.input),
            vec![
                ("InstanceID".to_owned(), "A_ARG_TYPE_InstanceID".to_owned()),
                ("Unit".to_owned(), "A_ARG_TYPE_SeekMode".to_owned()),
                ("Target".to_owned(), "A_ARG_TYPE_SeekTarget".to_owned()),
            ]
        );
        assert!(seek.output.is_empty());
        // GetProtocolInfo: no inputs, two outputs whose *order* is the response's
        // child order.
        let gpi = scpd
            .service("ConnectionManager")
            .unwrap()
            .action("GetProtocolInfo")
            .unwrap();
        assert!(gpi.input.is_empty());
        assert_eq!(
            pairs(&gpi.output),
            vec![
                ("Source".to_owned(), "SourceProtocolInfo".to_owned()),
                ("Sink".to_owned(), "SinkProtocolInfo".to_owned()),
            ]
        );
    }

    #[test]
    fn service_order_and_namespace_come_from_the_description_not_the_files() {
        crate::pin_test_dir();
        // A reordered description must produce reordered services, and the
        // namespace must be the one it names, not one inferred from the name.
        let desc = Dom::parse(
            r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><serviceList>
               <service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType></service>
               <service><serviceType>urn:schemas-upnp-org:service:AVTransport:2</serviceType></service>
               </serviceList></root>"#,
        )
        .unwrap();
        let mut reader = |name: &str| match doc_for_service(name) {
            Some(t) => Ok(t.to_owned()),
            None => Err(format!("no document for {name}")),
        };
        let scpd = Scpd::from_description(&desc, &mut reader).unwrap();
        let names: Vec<&str> = scpd.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ConnectionManager", "AVTransport"]);
        assert_eq!(
            scpd.service_namespace("AVTransport").unwrap(),
            "urn:schemas-upnp-org:service:AVTransport:2",
            "the version digit came from the description"
        );
    }

    // ---- navigation rules ----------------------------------------------------

    #[test]
    fn iter_is_deep_and_includes_self_while_find_is_children_only() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><a><stateVariable sendEvents="no"><name>In</name><dataType>string</dataType><b><stateVariable sendEvents="no"><name>Deep</name><dataType>string</dataType></stateVariable></b></stateVariable></a></scpd>"#,
        )
        .unwrap();
        let tag = s("stateVariable");
        // Deep: the nested one is found, which `find` would never return.
        let deep = dom.iter_subtree(dom.root(), &tag);
        let names: Vec<Option<&str>> = deep
            .iter()
            .map(|n| dom.find(*n, &s("name")).and_then(|c| dom.text(c)))
            .collect();
        assert_eq!(names, vec![Some("In"), Some("Deep")]);
        // Shallow from the root: nothing.
        assert!(dom.find_all(dom.root(), &tag).is_empty());
        assert!(dom.find(dom.root(), &s("a")).is_some());
        // And iter() counts the node it starts from.
        let root_tag = s("scpd");
        assert_eq!(dom.iter_subtree(dom.root(), &root_tag), vec![dom.root()]);
    }

    #[test]
    fn text_is_the_characters_before_the_first_child() {
        crate::pin_test_dir();
        let dom = Dom::parse("<r><a>before<b/>after</a><c><b/>after</c><d>only</d></r>").unwrap();
        let a = dom.find(dom.root(), "a").unwrap();
        assert_eq!(dom.text(a), Some("before"));
        let c = dom.find(dom.root(), "c").unwrap();
        assert_eq!(dom.text(c), None, "text after a closed child is that child's tail");
        let dd = dom.find(dom.root(), "d").unwrap();
        assert_eq!(dom.text(dd), Some("only"));
    }

    #[test]
    fn a_comment_ends_the_text_without_becoming_a_child() {
        crate::pin_test_dir();
        let dom = Dom::parse("<r><a>before<!--gone-->after</a></r>").unwrap();
        let a = dom.find(dom.root(), "a").unwrap();
        assert_eq!(dom.text(a), Some("before"));
        assert_eq!(dom.children(a).len(), 0, "a comment is not an element child");
        // And without the comment the same document keeps both halves' first one
        // anyway; this asserts the *comment* is what cut it.
        let plain = Dom::parse("<r><a>beforeafter</a></r>").unwrap();
        let p = plain.find(plain.root(), "a").unwrap();
        assert_eq!(plain.text(p), Some("beforeafter"));
    }

    #[test]
    fn empty_element_forms_read_the_way_lxml_reads_them() {
        crate::pin_test_dir();
        let dom = Dom::parse("<r><a/><b></b><c> </c></r>").unwrap();
        for local in ["a", "b"] {
            let n = dom.find(dom.root(), local).unwrap();
            assert_eq!(dom.text(n), None, "<{local}>");
        }
        let c = dom.find(dom.root(), "c").unwrap();
        assert_eq!(dom.text(c), Some(" "), "one space is text, and nothing trims it");
    }

    #[test]
    fn entities_and_numeric_references_are_resolved_rather_than_dropped() {
        crate::pin_test_dir();
        // quick-xml splits text at references, so a reader that only concatenated
        // Text events would answer "ab" here. Synthetic input on purpose:
        assert_eq!(
            resolve_reference("amp").unwrap(),
            "&",
            "the five predefined ones"
        );
        assert_eq!(resolve_reference("#65").unwrap(), "A");
        assert_eq!(resolve_reference("#x41").unwrap(), "A");
        assert_eq!(resolve_reference("#X41").unwrap(), "A");
        assert!(resolve_reference("#0x41").is_err(), "0x is not a radix prefix here");
        assert!(resolve_reference("#").is_err());
        assert!(resolve_reference("#-1").is_err());
        assert!(resolve_reference("#+1").is_err());
        assert!(resolve_reference("#D800").is_err(), "a lone surrogate is not a scalar value");
        assert!(resolve_reference("#1114112").is_err(), "past the last code point");
        assert!(resolve_reference("notdefined").is_err());

        let dom = Dom::parse("<r><a>x&amp;y &#65;&#x42;z</a></r>").unwrap();
        let a = dom.find(dom.root(), "a").unwrap();
        assert_eq!(dom.text(a), Some("x&y ABz"));
        let bad = Dom::parse("<r><a>x&nope;y</a></r>");
        assert!(bad.is_err(), "an undeclared entity is refused: {:?}", bad.err());

        // The claim that the real documents make this test synthetic: none of the
        // four contains a `&` at all. If that ever changes, this assertion is the
        // one that says so, and the test above stops being the only witness.
        for doc in [DESCRIPTION_XML, AVTRANSPORT_XML, RENDERING_CONTROL_XML, CONNECTION_MANAGER_XML] {
            assert_eq!(doc.matches('&').count(), 0, "an entity appeared in a document we embed");
        }
    }

    #[test]
    fn attribute_values_are_normalized_before_an_entity_is_resolved() {
        crate::pin_test_dir();
        // lxml's AVNormalize: tab/CR/LF to space, then the five references. This is
        // the difference between `normalized_value` and the deprecated
        // `unescape_value`, and it is why the latter is not used.
        let dom = Dom::parse("<r a=\"x\t\ny\" b=\"1 &lt; 2\"></r>").unwrap();
        assert_eq!(dom.attr(dom.root(), "a"), Some("x  y"));
        assert_eq!(dom.attr(dom.root(), "b"), Some("1 < 2"));
    }

    #[test]
    fn attributes_keep_document_order_and_exclude_declarations() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            r#"<r xmlns:p="urn:p" p:one="1" two="2" xmlns:q="urn:q" three="3"></r>"#,
        )
        .unwrap();
        // A direct read of the arena, because the order is the contract and the
        // accessors above only look one name up.
        let names: Vec<&str> = dom.nodes[dom.root]
            .attrs
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(names, ["{urn:p}one", "two", "three"]);
        assert_eq!(dom.nodes[dom.root].attrs[0].1, "1");
    }

    #[test]
    fn binding_answers_what_a_prefix_was_declared_to_on_this_element() {
        crate::pin_test_dir();
        // The shape `SetAVTransportURI` actually meets: a DIDL-Lite root that binds
        // `dc`, plus a root that binds nothing because its sender put the
        // declaration somewhere else.
        let dom = Dom::parse(
            r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/"><item><dc:title>首</dc:title></item></DIDL-Lite>"#,
        )
        .unwrap();
        let root = dom.root();
        assert_eq!(dom.binding(root, "dc"), Some("http://purl.org/dc/elements/1.1/"));
        assert_eq!(
            dom.binding(root, ""),
            Some("urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"),
            "the default namespace is readable under the empty prefix, as in nsmap"
        );
        assert_eq!(dom.binding(root, "nope"), None);
        // The declaration is on the root, so it is in scope for the item, and yet
        // `binding` answers `None` for the item — the difference from lxml's `nsmap`
        // that the method doc records. It costs nothing here because the only
        // question ever asked of a binding is asked of a root.
        let item = dom.find(root, "{urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/}item").unwrap();
        assert_eq!(dom.binding(item, "dc"), None);
        // …and a sender that declares `dc` only on a descendant leaves the root's
        // answer as `None`, which is what the reader treats as "never told", not as
        // a parse failure.
        let lower = Dom::parse(
            r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"><item xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>次</dc:title></item></DIDL-Lite>"#,
        )
        .unwrap();
        assert_eq!(lower.binding(lower.root(), "dc"), None);
        let lower_item = dom_tag(&lower, "item");
        assert_eq!(
            lower.binding(lower_item, "dc"),
            Some("http://purl.org/dc/elements/1.1/"),
            "the element that declares it can be asked"
        );
    }

    #[test]
    fn a_redeclared_prefix_is_answered_at_the_element_that_redeclared_it() {
        crate::pin_test_dir();
        // The reachable shape of "a later declaration wins": an inner element that
        // re-binds a prefix an ancestor already bound. `make_node` copies the
        // parent's scopes and then overwrites, so a name inside resolves against
        // the *new* URI — while each element's own answer is only its own
        // declaration, which is why the root's stays the old one.
        let dom = Dom::parse(
            r#"<r xmlns:p="urn:one"><m xmlns:p="urn:two"><p:leaf>3</p:leaf></m></r>"#,
        )
        .unwrap();
        assert_eq!(
            dom.tag(dom_tag(&dom, "leaf")),
            "{urn:two}leaf",
            "the innermost declaration is the one in scope for the name it prefixes"
        );
        assert_eq!(dom.binding(dom.root(), "p"), Some("urn:one"));
        assert_eq!(
            dom.binding(dom.find(dom.root(), "m").unwrap(), "p"),
            Some("urn:two"),
            "and the element that redeclared it is the one that can be asked"
        );
        // A same-element redeclaration never reaches this rule through the parser:
        // quick-xml refuses a duplicated attribute before `make_node` sees a second
        // `xmlns:p`. The wording is recorded here because the method doc used to
        // promise the opposite, and it is the arena, not the XML, that allows it.
        let err = Dom::parse(r#"<r xmlns:p="urn:one" xmlns:p="urn:two"></r>"#)
            .expect_err("a duplicated attribute is refused, not last-wins");
        assert!(err.contains("duplicated attribute"), "{err}");
        // An explicit unbind is a declaration with an empty URI. lxml's `nsmap`
        // shows `None` for it; here it is `Some("")` because the parser keeps what
        // was written, and the caller that turns it into a tag gets one no node can
        // have — name resolution refuses a prefix bound to nothing.
        let unbind = Dom::parse(r#"<r xmlns:p="urn:one"><c xmlns:p=""></c></r>"#).unwrap();
        let c = unbind
            .find(unbind.root(), "c")
            .expect("<c> is bare, the unbind has no bearing on its own name");
        assert_eq!(unbind.binding(c, "p"), Some(""));
        assert_eq!(
            clark(unbind.binding(c, "p").unwrap(), "title"),
            "{}title",
            "an empty binding makes a tag that cannot match anything"
        );
        assert!(unbind.find(c, "{}title").is_none());
    }

    #[test]
    fn iter_all_walks_the_whole_subtree_in_document_order_including_self() {
        crate::pin_test_dir();
        let dom = Dom::parse(r#"<r><a><b/><c><d/></c></a><e/></r>"#).unwrap();
        let tags: Vec<&str> = dom.iter_all(dom.root()).iter().map(|i| dom.tag(*i)).collect();
        assert_eq!(tags, ["r", "a", "b", "c", "d", "e"]);
        // A walk that starts in the middle starts at that node, so a caller asking
        // "is there a `res` under here" cannot miss the node it was handed.
        let a = dom.find(dom.root(), "a").unwrap();
        let c = dom.find(a, "c").unwrap();
        let sub: Vec<&str> = dom.iter_all(c).iter().map(|i| dom.tag(*i)).collect();
        assert_eq!(sub, ["c", "d"]);
        let leaf = dom.find(a, "b").unwrap();
        assert_eq!(dom.iter_all(leaf), vec![leaf]);
    }

    #[test]
    fn iter_all_finds_a_res_whatever_namespace_it_arrives_wrapped_in() {
        crate::pin_test_dir();
        // This is why `protocol_info_from_didl` compares a *local* name inside the
        // loop instead of asking for a tag: two senders spell the same element
        // differently, and neither spelling is wrong.
        let plain = Dom::parse(
            r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"><item><res protocolInfo="http-get:*:video/mp4:*">u</res></item></DIDL-Lite>"#,
        )
        .unwrap();
        let bare = Dom::parse(
            r#"<DIDL-Lite><item><res protocolInfo="dlna-org:*:mp4">u</res></item></DIDL-Lite>"#,
        )
        .unwrap();
        let named = |dom: &Dom| -> Vec<usize> {
            dom.iter_all(dom.root())
                .into_iter()
                .filter(|i| local_of(dom.tag(*i)) == "res")
                .collect()
        };
        assert_eq!(named(&plain).len(), 1, "the namespaced spelling is a res");
        assert_eq!(named(&bare).len(), 1, "the bare spelling is the same res");
        assert_eq!(
            plain.attr(named(&plain)[0], "protocolInfo"),
            Some("http-get:*:video/mp4:*")
        );
        assert_eq!(bare.attr(named(&bare)[0], "protocolInfo"), Some("dlna-org:*:mp4"));
        // The attribute is read by its bare name on both shapes: `protocolInfo`
        // carries no prefix in any sender's DIDL that we have seen, and a reader
        // that asked for a namespaced attribute would answer `None` — which the
        // caller reads as "no protocolInfo" rather than "no res".
        assert_eq!(
            plain.attr(named(&plain)[0], "{urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/}protocolInfo"),
            None
        );
    }

    /// The tag with its `{namespace}` stripped — the same question
    /// `macast/protocol.py`'s `_local_name` answers.
    fn local_of(tag: &str) -> &str {
        match tag.rfind('}') {
            Some(i) => &tag[i + 1..],
            None => tag,
        }
    }

    /// First node under the root whose local name is `local`, whatever namespace it
    /// arrived in.
    fn dom_tag(dom: &Dom, local: &str) -> usize {
        *dom.iter_all(dom.root())
            .iter()
            .find(|i| local_of(dom.tag(**i)) == local)
            .expect("no such element")
    }

    #[test]
    fn a_prefix_declared_on_its_own_element_resolves() {
        crate::pin_test_dir();
        // The real shape from Description.xml, which is why make_node applies
        // declarations before resolving the element's name.
        let dom = Dom::parse(
            r#"<root xmlns:dlna="urn:schemas-dlna-org:device-1-0" xmlns="urn:schemas-upnp-org:device-1-0"><dlna:X_DLNADOC xmlns:dlna="urn:schemas-dlna-org:device-1-0">DMR-1.50</dlna:X_DLNADOC></root>"#,
        )
        .unwrap();
        let want = "{urn:schemas-dlna-org:device-1-0}X_DLNADOC";
        let n = dom.find(dom.root(), want).unwrap();
        assert_eq!(dom.text(n), Some("DMR-1.50"));
        // And the embedded description really carries that node.
        let desc = Dom::parse(DESCRIPTION_XML).unwrap();
        assert_eq!(
            desc.iter_subtree(desc.root(), want).len(),
            1,
            "the DMR-1.50 marker"
        );
    }

    #[test]
    fn an_undeclared_prefix_is_refused() {
        crate::pin_test_dir();
        let err = Dom::parse("<r><z:x/></r>").expect_err("lxml raises Unbound prefix");
        assert!(err.contains("'z'"), "{err}");
        // The xml: prefix is the one that needs no declaration, so it is not refused.
        let dom = Dom::parse(r#"<r xml:lang="zh"></r>"#).unwrap();
        assert_eq!(
            dom.attr(dom.root(), "{http://www.w3.org/XML/1998/namespace}lang"),
            Some("zh")
        );
    }

    #[test]
    fn an_unprefixed_element_follows_the_default_namespace_and_xml_empty_unbinds_it() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            r#"<r xmlns="urn:x"><a/><inner xmlns=""><b/></inner></r>"#,
        )
        .unwrap();
        assert!(dom.find(dom.root(), "{urn:x}a").is_some());
        // `xmlns=""` unbinds the name of **the element that carries it**, not just
        // its children: lxml puts `<inner xmlns="">` in the *empty* namespace and
        // only the subtree below it follows. (Measured against real lxml:
        // `t.find("{urn:x}inner")` is None while `t.find("inner")` is the element.)
        let inner = dom.find(dom.root(), "inner").unwrap();
        assert!(dom.find(inner, "b").is_some(), "xmlns='' unbinds the subtree");
        assert!(
            dom.find(dom.root(), "{urn:x}inner").is_none(),
            "the default binding does not reach the element that drops it"
        );
        // A document with no default binding at all is bare names throughout.
        let bare = Dom::parse("<r><a/></r>").unwrap();
        assert!(bare.find(bare.root(), "a").is_some());
    }

    #[test]
    fn malformed_documents_are_refused_with_a_message() {
        crate::pin_test_dir();
        // Each row says who refuses it. The first three are **our** end-tag stack —
        // quick-xml is configured with `check_end_names = false`, so the only witness
        // that a close tag names the element it closes is the one below — and the last
        // two are quick-xml still doing its job on bytes that are not a document at
        // all. The expected strings are the messages these two layers actually
        // produce, measured by running them, not written from a guess about wording.
        for (xml, want) in [
            ("<r><a></r>", "<a> was closed by </r>"),
            ("<r><a></b></r>", "<a> was closed by </b>"),
            ("<r></r><s></s>", "document has two root elements"),
            (
                "</r>",
                "close tag `</r>` does not match any open tag",
            ),
            ("<r", "tag not closed: `>` not found before end of input"),
        ] {
            let err = Dom::parse(xml).expect_err(&format!("{xml:?} must be refused"));
            assert!(
                err.contains(want),
                "{xml:?} refused with {err:?}, which does not say {want:?}"
            );
        }
    }

    #[test]
    fn an_unknown_datatype_is_refused_and_names_the_variable() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><serviceStateTable>
               <stateVariable sendEvents="no"><name>N</name><dataType>float</dataType></stateVariable>
               </serviceStateTable></scpd>"#,
        )
        .unwrap();
        let mut scpd = Scpd::default();
        let err = scpd
            .build_action("urn:schemas-upnp-org:service:Test:1", "Test", &dom)
            .expect_err("Python raises ValueError from DataType(text)");
        assert!(err.contains("float") && err.contains("N"), "{err}");
        assert_eq!(scpd.states.len(), 0, "a refused document leaves no half-table");
        // The six names Python declares.
        for (text, want) in [
            ("boolean", DataType::Boolean),
            ("i2", DataType::I2),
            ("ui2", DataType::Ui2),
            ("i4", DataType::I4),
            ("ui4", DataType::Ui4),
            ("string", DataType::Str),
        ] {
            assert_eq!(DataType::parse(text).unwrap(), want);
            assert_eq!(DataType::parse(text).unwrap().as_str(), text);
        }
    }

    #[test]
    fn a_state_variable_that_declares_no_send_events_is_refused() {
        crate::pin_test_dir();
        let dom = Dom::parse(
            r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><serviceStateTable>
               <stateVariable><name>N</name><dataType>string</dataType></stateVariable>
               </serviceStateTable></scpd>"#,
        )
        .unwrap();
        let mut scpd = Scpd::default();
        let err = scpd
            .build_action("urn:schemas-upnp-org:service:Test:1", "Test", &dom)
            .expect_err("Python raises KeyError from attrib['sendEvents']");
        assert!(err.contains("sendEvents") && err.contains("'N'"), "{err}");
    }

    #[test]
    fn a_direction_with_no_text_is_an_output_and_a_missing_one_is_refused() {
        crate::pin_test_dir();
        let mk = |dir: &str| {
            format!(
                r#"<scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList><action>
                   <name>A</name><argumentList><argument><name>x</name><relatedStateVariable>S</relatedStateVariable>{dir}</argument></argumentList>
                   </action></actionList></scpd>"#
            )
        };
        let build = |dir: &str| {
            let dom = Dom::parse(&mk(dir)).unwrap();
            let mut scpd = Scpd::default();
            scpd.build_action("urn:schemas-upnp-org:service:Test:1", "Test", &dom)?;
            Ok::<_, String>(scpd)
        };
        let scpd = build("<direction/>").unwrap();
        let a = scpd.service("Test").unwrap().action("A").unwrap();
        assert!(a.input.is_empty());
        assert_eq!(a.output.len(), 1, "None == 'in' is False in Python too");
        // A spelling that is merely not 'in' is also an output — this branch is a
        // comparison, not a match on two values.
        let scpd = build("<direction>IN</direction>").unwrap();
        assert_eq!(
            scpd.service("Test").unwrap().action("A").unwrap().output.len(),
            1
        );
        // And the missing element is Python's AttributeError.
        assert!(build("").is_err());
    }

    // ---- urn / seeds ---------------------------------------------------------

    #[test]
    fn service_name_from_urn_splits_where_python_splits() {
        crate::pin_test_dir();
        assert_eq!(
            service_name_from_urn("urn:schemas-upnp-org:service:ConnectionManager:1").unwrap(),
            "ConnectionManager"
        );
        assert_eq!(
            service_name_from_urn("urn:schemas-upnp-org:service:AVTransport:2").unwrap(),
            "AVTransport"
        );
        // Python indexes [3]; three segments is an IndexError.
        let err = service_name_from_urn("urn:a:b").expect_err("short");
        assert!(err.contains("urn:a:b"), "{err}");
        // Python indexes [3], and `[3]` is a **column**, not "the last meaningful
        // piece": empty segments between colons are counted. Measured against real
        // Python — `"urn:::Name:1"`[3] is `Name`, `"urn::::Name:1"`[3] is `''`. The
        // second one is the shape that keeps this honest: an empty service name is
        // returned as-is because that is what the code we port does with it, and the
        // file it then opens (`xml/.xml`) does not exist, so the failure belongs to
        // the caller rather than being quietly pre-empted here.
        assert_eq!(service_name_from_urn("urn:::Name:1").unwrap(), "Name");
        assert_eq!(service_name_from_urn("urn::::Name:1").unwrap(), "");
    }

    #[test]
    fn the_nine_init_state_writes_in_order_with_their_two_odd_shapes() {
        crate::pin_test_dir();
        let seeds = init_state_seeds("  http-get:*:video/mp4:*,http-live \n");
        let names: Vec<&str> = seeds.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "CurrentPlayMode",
                "TransportPlaySpeed",
                "TransportStatus",
                "RelativeCounterPosition",
                "AbsoluteCounterPosition",
                "A_ARG_TYPE_Direction",
                "CurrentConnectionIDs",
                "PlaybackStorageMedium",
                "SinkProtocolInfo",
            ]
        );
        // The number, not the string: `str(1)` is "1" and `str("1")` is also "1",
        // so the golden's value_type column is the only place this shows.
        assert_eq!(seeds[1].1, json!(1));
        assert_eq!(crate::store::wire(&seeds[1].1), "1");
        // And the file text, trimmed — both sides of `.strip()`.
        assert_eq!(
            seeds[8].1,
            json!("http-get:*:video/mp4:*,http-live")
        );
        // 2147483647 is `2**31 - 1`, the "uninitialised counter" of the original.
        assert_eq!(seeds[3].1, json!(2147483647i64));
        assert_eq!(seeds[4].1, json!(2147483647i64));
    }

    #[test]
    fn apply_init_state_lands_on_the_built_table_and_refuses_a_name_it_does_not_have() {
        crate::pin_test_dir();
        let mut scpd = built();
        // The schema-only golden says the ninth row is empty; the real boot reads a
        // file. Assert the seam rather than the 16 KB content.
        assert_eq!(scpd.state("SinkProtocolInfo").unwrap().value, json!(""));
        scpd.apply_init_state("http-get:*:video/mp4:*").unwrap();
        assert_eq!(
            scpd.state("SinkProtocolInfo").unwrap().value,
            json!("http-get:*:video/mp4:*")
        );
        assert_eq!(scpd.state("TransportStatus").unwrap().value, json!("OK"));
        assert_eq!(scpd.state("TransportPlaySpeed").unwrap().value, json!(1));

        let mut partial = Scpd::default();
        let err = partial
            .apply_init_state("x")
            .expect_err("an empty table has none of the nine");
        assert!(err.contains("CurrentPlayMode"), "{err}");
    }

    // ---- the documents themselves -------------------------------------------

    #[test]
    fn each_embedded_document_parses_and_is_the_thing_it_claims_to_be() {
        crate::pin_test_dir();
        let desc = Dom::parse(DESCRIPTION_XML).unwrap();
        assert_eq!(desc.tag(desc.root()), d("root"));
        for (name, text) in [
            ("AVTransport", AVTRANSPORT_XML),
            ("RenderingControl", RENDERING_CONTROL_XML),
            ("ConnectionManager", CONNECTION_MANAGER_XML),
        ] {
            let dom = Dom::parse(text).unwrap();
            assert_eq!(dom.tag(dom.root()), s("scpd"), "{name}");
            assert_eq!(doc_for_service(name).unwrap(), text);
        }
        assert!(doc_for_service("SomethingElse").is_none());
    }

    #[test]
    fn the_description_template_keeps_all_eleven_placeholders_for_the_module_that_fills_them() {
        crate::pin_test_dir();
        // This module reads the template, it does not substitute it. A silent
        // substitution here would show up as a description that always says the same
        // friendly name, so the check is that the braces are still there.
        for token in [
            "{uuid}",
            "{friendly_name}",
            "{manufacturer}",
            "{manufacturer_url}",
            "{model_description}",
            "{model_name}",
            "{model_number}",
            "{model_url}",
            "{serial_num}",
            "{header_extra}",
            "{service_extra}",
        ] {
            assert!(
                DESCRIPTION_XML.contains(token),
                "{token} is no longer in the embedded template"
            );
        }
        // And the two that sit where a plugin could change structure.
        let dom = Dom::parse(DESCRIPTION_XML).unwrap();
        let service_list = dom
            .find(
                dom.find(dom.root(), &d("device")).unwrap(),
                &d("serviceList"),
            )
            .unwrap();
        assert_eq!(
            dom.iter_subtree(service_list, &d("serviceType")).len(),
            3,
            "the three standard services, before any {{service_extra}}"
        );
    }

    // ---- drift against src-tauri's own table --------------------------------

    /// The text between the first pair of `q` in `line`. The caller has already
    /// counted the quotes, so this reads a column rather than guessing one.
    fn field(line: &str, q: char) -> String {
        let open = line.find(q).expect("an opening quote");
        let rest = &line[open + q.len_utf8()..];
        let close = rest.find(q).expect("a closing quote");
        rest[..close].to_owned()
    }

    #[test]
    fn the_host_table_declares_the_same_names_this_one_builds() {
        crate::pin_test_dir();
        // `src-tauri/src/host.rs` keeps its own list of the state variables the page
        // can show, because that page must be able to render before this crate is
        // wired to it. It is written in **this crate's built order** — the document
        // order of the four service XMLs, the same order as the golden table — so the
        // comparison below is ordered, not a set comparison: a host table that sorted
        // itself alphabetically would be a real drift in the row order the page shows,
        // and a set comparison would let it pass. The second column is compared too,
        // because it is host.rs's own claim about what each variable boots to.
        let host = include_str!("../../src-tauri/src/host.rs");
        let anchor = host
            .find("const DECLARED")
            .expect("host.rs declares a `const DECLARED` table");
        let mut names: Vec<(String, char)> = Vec::new();
        for line in host[anchor..].lines() {
            let line = line.trim();
            if line.starts_with(']') {
                break;
            }
            if !line.starts_with('(') {
                continue;
            }
            // A row is `("Name", 'x'),` — the name in **double** quotes because it is a
            // `&str`, the datatype in **single** quotes because it is a `char`. (The
            // first version of this scanner looked for two double-quoted fields and read
            // every row as one, so this sentence is measured, not inferred.) Each column
            // must be quoted exactly once, so a row that loses a field fails here rather
            // than being skipped by the shape test above.
            for q in ['"', '\''] {
                assert_eq!(
                    line.matches(q).count(),
                    2,
                    "row {line:?} must hold exactly one {q}-quoted field"
                );
            }
            names.push((
                field(line, '"'),
                field(line, '\'').chars().next().unwrap(),
            ));
        }
        assert_eq!(names.len(), 48, "extracted rows: {names:?}");

        let scpd = built();
        let mine: Vec<&str> = scpd.state_names();
        let theirs: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(mine, theirs, "the two tables of DLNA state variables, in order");
        for (name, letter) in &names {
            let row = scpd.state(name).unwrap_or_else(|| panic!("{name} is not in this table"));
            let datatype = row.datatype.as_str();
            assert_eq!(
                datatype.chars().next(),
                Some(*letter),
                "{name}: host.rs says {:?} but the schema says {datatype:?}",
                letter
            );
            // And the letter is only a shorthand for the boot value, so check the
            // value it stands for too: `string` boots to `""`, everything else to `0`.
            if *letter == 's' {
                assert_eq!(row.datatype.default_value(), json!(""), "{name}");
            } else {
                assert_eq!(row.datatype.default_value(), json!(0), "{name}");
            }
        }
    }
}
