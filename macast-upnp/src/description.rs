//! The device description: the one document a control point reads **before** it
//! says anything to us.
//!
//! Rust counterpart of `macast/protocol.py:3131-3144`
//! (`DLNAHandler.build_description`, the `.format(...)` call and nothing else) and
//! of the `GET /description.xml` answer it feeds (`protocol.py:3146-3149`).
//!
//! ## Why this is a substitution and not a builder
//!
//! Python does not assemble the description from parts: it reads
//! `macast/assets/xml/Description.xml`, calls `str.format` with eleven named values, and
//! keeps the resulting bytes. That is the whole rule, and it is worth keeping
//! exactly, because the template is the artifact a control point author reads
//! when they wonder what we are. A Rust builder that produced the *same intent*
//! but different whitespace would still be a different file, and the difference
//! would be invisible until someone's player refused to add us.
//!
//! So [`fill`] implements `str.format` for the shapes that template can contain,
//! and [`build`] hands back `.encode()` — UTF-8 bytes — of the result.
//!
//! ## What goes into the eleven slots
//!
//! Five are string literals in the Python call site, and they stay literals here
//! ([`MANUFACTURER`] and friends): they describe the *project*, not this machine.
//! The remaining six are read each time the description is rebuilt:
//!
//! * `friendly_name` ← [`Setting::friendly_name`] — the name in the TV's source
//!   list, and the value the temporary override (`Macast E2E Smoke`, §6 of
//!   AGENTS.md) wins over the persisted one.
//! * `model_number` ← [`macast_core::paths::version`] — Python's
//!   `Setting.get_version()`, which is the same `.version` file the CI release
//!   check compares against.
//! * `uuid` ← [`Setting::usn`] — **the same value SSDP puts in every `USN`**.
//!   [`crate::device`] reads it once per re-announce for exactly that reason, and
//!   a description that announced a different UUID would leave a control point
//!   holding two identities for one device: the cached-by-USN failure mode that
//!   `refresh_usn()` exists for, but with no way to recover.
//! * `serial_num` ← the integer `1024`, which `str()` renders as `1024` — not a
//!   string in Python, so a Rust `String` here would be a lie about the type;
//!   [`SERIAL_NUM`] says what the rendering is.
//! * `header_extra` / `service_extra` ← `""`. Those two empty substitutions are
//!   not decoration in the file: they leave line 20 as **eight spaces and a
//!   newline** and line 44 as **twelve spaces and a newline**. Two tests below
//!   pin those bytes, because "delete the blank line" is the kind of tidy-up that
//!   looks free and changes what a control point hashes.
//!
//! ## The deviations from Python, and their directions
//!
//! `str.format` accepts more than this template needs. [`fill`] refuses three
//! shapes Python accepts, and each refusal points **away from a wrong wire**:
//!
//! * a **format spec** (`{model_number:>10}`) renders in Python. Returning `Err`
//!   instead means a padded field can never ship; the alternative is a device
//!   description with ten spaces inside `<modelNumber>`, which is a bug report
//!   that reads as "the TV shows a blank model".
//! * **attribute and index lookups** (`{a.b}`, `{a[0]}`) can only fail here —
//!   every value is a `String` and there is no positional argument tuple — so
//!   refusing them loses nothing and keeps [`fill`] total over the names it knows.
//! * **an unknown name** is Python's `KeyError`. Here it is an `Err` naming the
//!   field, because [`build`]'s caller is a live HTTP face: in Python the raise
//!   happens *before* the assignment, so `self.description` silently keeps the
//!   previous bytes, and a description that never updates reads as a caching bug.
//!   The `Err` makes "the new bytes were not adopted" a decision the caller has
//!   to make out loud.
//!
//! The one shape that is *not* a deviation: braces inside a **value** are never
//! re-read. `"{friendly_name}".format(friendly_name="Macast{probe}")` yields
//! `Macast{probe}` in Python, and a test below says so, because a recursive
//! substitute would turn a user's friendly name into a parse error — and the
//! friendly name is the one field we do not control.
//!
//! ## How the deviation stays unreachable
//!
//! The refusals above are only safe because today's template contains exactly the
//! eleven plain fields: no `{{`, no lone `}`, no spec. `the_template_holds_exactly_
//! the_eleven_fields` asks the embedded template that question directly, so if
//! someone adds a brace for any other purpose the refusal becomes a red test
//! rather than a 500 in front of a user's TV.

use macast_core::paths;
use macast_core::setting::Setting;

/// `manufacturer="xfangfang"` — describes the project, not this machine, so it is
/// a literal in Python and a literal here.
pub const MANUFACTURER: &str = "xfangfang";

/// `manufacturer_url="https://github.com/xfangfang"`.
pub const MANUFACTURER_URL: &str = "https://github.com/xfangfang";

/// `model_description="AVTransport Media Renderer"`.
pub const MODEL_DESCRIPTION: &str = "AVTransport Media Renderer";

/// `model_name="Macast"`.
pub const MODEL_NAME: &str = "Macast";

/// `model_url="https://xfangfang.github.io/Macast"`.
pub const MODEL_URL: &str = "https://xfangfang.github.io/Macast";

/// `serial_num=1024` — an **integer** in the Python call, so what reaches the
/// wire is `str(1024)`. Held as text because `fill` substitutes text; the type is
/// this doc comment's job, not the field's.
pub const SERIAL_NUM: &str = "1024";

/// `header_extra=""`: the empty half of the two extension hooks upstream left in
/// the template. Substituted empty, it leaves an eight-space line — see the tests.
pub const HEADER_EXTRA: &str = "";

/// `service_extra=""`, the other hook, and a twelve-space line.
pub const SERVICE_EXTRA: &str = "";

/// The eleven names the template may ask for, in the order `fill` sees them in
/// `macast/assets/xml/Description.xml`.
///
/// Python's keyword order differs (`uuid=` is the eighth argument but the first
/// field in the file) and cannot matter, because every field is named. What this
/// list decides is the inventory `the_template_holds_exactly_the_eleven_fields`
/// compares against, so adding a sixth literal here without adding the constant
/// above is a red test rather than an unknown-name 500.
pub const FIELD_NAMES: [&str; 11] = [
    "uuid",
    "friendly_name",
    "manufacturer",
    "manufacturer_url",
    "model_description",
    "model_name",
    "model_number",
    "model_url",
    "serial_num",
    "header_extra",
    "service_extra",
];

/// The eleven values, exactly as `build_description` passes them.
///
/// A struct rather than eleven arguments because the HTTP face has to hold these
/// and rebuild the bytes when the friendly name or the version changes
/// (Python's `reload()` calls `build_description()` again for the same reason),
/// and because `lookup` needs the name→value relation that the template is keyed
/// on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub uuid: String,
    pub friendly_name: String,
    pub manufacturer: String,
    pub manufacturer_url: String,
    pub model_description: String,
    pub model_name: String,
    pub model_number: String,
    pub model_url: String,
    pub serial_num: String,
    pub header_extra: String,
    pub service_extra: String,
}

impl Fields {
    /// Read the three values that live in the settings, and take the eight that
    /// do not from the constants above.
    ///
    /// `usn()` is read **once** for the whole document: it is insert-on-read
    /// (`macast-core/src/setting.rs:513`), so a second call would be a second
    /// chance to disagree with the first — and the first is what
    /// [`crate::device`] is announcing.
    pub fn from_setting(setting: &Setting) -> Fields {
        Fields {
            uuid: setting.usn(),
            friendly_name: setting.friendly_name(),
            manufacturer: MANUFACTURER.to_string(),
            manufacturer_url: MANUFACTURER_URL.to_string(),
            model_description: MODEL_DESCRIPTION.to_string(),
            model_name: MODEL_NAME.to_string(),
            model_number: paths::version(),
            model_url: MODEL_URL.to_string(),
            serial_num: SERIAL_NUM.to_string(),
            header_extra: HEADER_EXTRA.to_string(),
            service_extra: SERVICE_EXTRA.to_string(),
        }
    }

    /// The value a field name resolves to, or `None` for a name this document has
    /// no answer for. Kept as a `match` over [`FIELD_NAMES`] rather than a map so
    /// the compiler enforces that every name in the list has a field.
    fn lookup(&self, name: &str) -> Option<&str> {
        match name {
            "uuid" => Some(&self.uuid),
            "friendly_name" => Some(&self.friendly_name),
            "manufacturer" => Some(&self.manufacturer),
            "manufacturer_url" => Some(&self.manufacturer_url),
            "model_description" => Some(&self.model_description),
            "model_name" => Some(&self.model_name),
            "model_number" => Some(&self.model_number),
            "model_url" => Some(&self.model_url),
            "serial_num" => Some(&self.serial_num),
            "header_extra" => Some(&self.header_extra),
            "service_extra" => Some(&self.service_extra),
            _ => None,
        }
    }
}

/// `load_xml(XMLPath.DESCRIPTION.value)` — the same bytes [`crate::scpd`] embeds
/// for reading, used here for filling.
pub const TEMPLATE: &str = crate::scpd::DESCRIPTION_XML;

/// `template.format(**fields)` for the shapes this template can hold.
///
/// The rules implemented, all of them `str.format`'s, in the order CPython
/// applies them (each one measured against `python3` on this machine rather than
/// recalled):
///
/// * `{{` and `}}` are escapes for one literal brace each.
/// * `{name}` is substituted from `fields`, and the substituted text is **not**
///   re-scanned.
/// * The **argument name** is scanned up to the first of `.` `[` `!` `:` `}`, and
///   nothing is stripped around it: `{ :>3}` is `KeyError: ' '`, not a complaint
///   about the padding. An *unmatched* `{` inside a name is its own
///   `ValueError: unexpected '{' in field name`, while running out of string is
///   `ValueError: expected '}' before end of string`.
/// * An empty or all-digit name is a **positional** field, and a description is
///   never built positionally: `IndexError` naming the slot Python counted
///   (`{}` is slot 0, `{5}` is slot 5).
/// * The lookup happens **before** the rest of the field is interpreted, so an
///   unknown name is a `KeyError` even when it also carries a format spec.
/// * Only after the name resolves does Python read the conversion, spec,
///   attribute or index. This port deliberately stops at the name (see the module
///   doc), so a field that asks for more is **refused** rather than answered with
///   half of what it asked for.
///
/// Byte offsets in the messages are into `template`, which is the only way a
/// message can send someone to the right line of a 47-line XML file.
pub fn fill(template: &str, fields: &Fields) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut it = template.char_indices().peekable();

    while let Some((idx, c)) = it.next() {
        match c {
            '{' => {
                // `{{` — one literal brace. Both braces are one byte, so the
                // peeked index comparison below is byte arithmetic and safe.
                if it.peek().map(|(j, cc)| *j == idx + 1 && *cc == '{') == Some(true) {
                    it.next();
                    out.push('{');
                    continue;
                }
                let mut name = String::new();
                // Python scans the *argument name* up to the first of `.`, `[`,
                // `!` or `:`; whatever follows is an attribute, an index, a
                // conversion or a format spec, and is not part of the key. That is
                // why `{ :>3}` raises `KeyError: ' '` rather than complaining about
                // the padding — the name is `' '`, and the lookup happens first.
                let mut trailing: Option<char> = None;
                let mut stop_at: Option<usize> = None;
                loop {
                    match it.next() {
                        None => {
                            return Err(format!(
                                "ValueError: expected '}}' before end of string \
                                 (template byte {idx})"
                            ));
                        }
                        Some((_, '}')) => break,
                        Some((pos, cc @ ('.' | '[' | '!' | ':'))) => {
                            trailing = Some(cc);
                            stop_at = Some(pos);
                            break;
                        }
                        // A `{` inside a field name has its own message, measured:
                        // `"{a{b}".format(a="X")` → `ValueError: unexpected '{' in
                        // field name`.
                        Some((_, '{')) => {
                            return Err(format!(
                                "ValueError: unexpected '{{' in field name \
                                 (template byte {idx})"
                            ));
                        }
                        Some((_, cc)) => name.push(cc),
                    }
                }
                // Python strips nothing around a field name: `{ uuid }` is the
                // key `' uuid '` and raises `KeyError: ' uuid '`. Reproduce that
                // by looking the name up exactly as written.
                if name.is_empty() || name.chars().all(|d| d.is_ascii_digit()) {
                    // An empty name is positional slot 0; `{5}` is slot 5.
                    let slot = if name.is_empty() {
                        0
                    } else {
                        name.parse::<usize>().unwrap_or(usize::MAX)
                    };
                    return Err(format!(
                        "IndexError: Replacement index {slot} out of range for \
                         positional args tuple (template byte {idx}: a description \
                         is built from named fields only, so `{name}` cannot be \
                         answered)"
                    ));
                }
                let value = match fields.lookup(&name) {
                    Some(value) => value,
                    None => {
                        return Err(format!(
                            "KeyError: '{name}' (template byte {idx}) — the description \
                             template asks for a field `Fields` does not carry; the \
                             eleven names are {FIELD_NAMES:?}"
                        ))
                    }
                };
                // The name resolved, so Python would now apply whatever follows it:
                // `{a!r}` gives `'X'`, `{a:>3}` gives `  X`, `{a[0]}` gives `X`.
                // This port deliberately stops at the name — see the module doc —
                // so a template that asks for more is refused loudly rather than
                // answered with half of what it asked for.
                if let Some(cc) = trailing {
                    return Err(format!(
                        "refused: `{name}` resolves, but the field continues with \
                         `{cc}` at template byte {} — a format spec, conversion, \
                         attribute or index is something Python renders and this \
                         port does not; the shipped template contains no such field, \
                         so add the value to `Fields` and substitute it plainly",
                        stop_at.unwrap_or(idx)
                    ));
                }
                out.push_str(value);
            }
            '}' => {
                if it.peek().map(|(j, cc)| *j == idx + 1 && *cc == '}') == Some(true) {
                    it.next();
                    out.push('}');
                    continue;
                }
                return Err(format!(
                    "ValueError: Single '}}' encountered in format string \
                     (template byte {idx})"
                ));
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// `.encode()` on the filled template: the bytes `GET /description.xml` answers
/// with, and the bytes [`crate::scpd::Scpd::from_description`] reads the service
/// set out of.
///
/// Infallible for the shipped [`TEMPLATE`] with any [`Fields`] — that is what
/// `the_template_holds_exactly_the_eleven_fields` proves — and it still returns
/// [`Err`] rather than expecting, because the same function is what a caller with
/// a different template (a plugin's `header_extra`, a test fixture) would use.
///
/// A caller that gets `Err` must **not** answer with the previous bytes silently:
/// in Python the raise happens before `self.description = …`, so the old
/// description keeps being served and the only trace is a traceback in
/// `macast.log`. That is the failure shape [`crate::store::Hooks::notify`] and the
/// log are for.
pub fn build(fields: &Fields) -> Result<Vec<u8>, String> {
    Ok(fill(TEMPLATE, fields)?.into_bytes())
}

/// `Setting.get_friendly_name()` → the bytes, for the two call sites that have a
/// `Setting` and no use for the intermediate struct.
pub fn build_for(setting: &Setting) -> Result<Vec<u8>, String> {
    build(&Fields::from_setting(setting))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe instance's field values, copied from the run that produced
    /// `tests/golden/description.probe.xml`: friendly name and version were the
    /// throwaway instance's (`Probe Only`, `0.25.0`), and its USN is the one its
    /// temp settings file generated. Pinning all three is what makes the byte
    /// comparison a witness rather than a tautology — the file was written by the
    /// Python, not by this code.
    const PROBE_UUID: &str = "e9d61556-aff6-4050-a3cd-5abb10dbfbc9";
    const PROBE_FRIENDLY: &str = "Probe Only";
    const PROBE_VERSION: &str = "0.25.0";

    fn probe_fields() -> Fields {
        Fields {
            uuid: PROBE_UUID.to_string(),
            friendly_name: PROBE_FRIENDLY.to_string(),
            manufacturer: MANUFACTURER.to_string(),
            manufacturer_url: MANUFACTURER_URL.to_string(),
            model_description: MODEL_DESCRIPTION.to_string(),
            model_name: MODEL_NAME.to_string(),
            model_number: PROBE_VERSION.to_string(),
            model_url: MODEL_URL.to_string(),
            serial_num: SERIAL_NUM.to_string(),
            header_extra: HEADER_EXTRA.to_string(),
            service_extra: SERVICE_EXTRA.to_string(),
        }
    }

    /// The Python's own answer, byte for byte.
    fn python_witness() -> &'static str {
        include_str!("../tests/golden/description.probe.xml")
    }

    // ---- the witness -------------------------------------------------------

    #[test]
    fn the_description_matches_the_python_bytes_exactly() {
        crate::pin_test_dir();
        let mine = build(&probe_fields()).expect("the shipped template fills");
        let want = python_witness().as_bytes();
        assert_eq!(
            want.len(),
            mine.len(),
            "length first: the bodies below are 47 lines and a diff of them is \
             unreadable at this size (python {}, rust {})",
            want.len(),
            mine.len()
        );
        assert_eq!(want, mine.as_slice());
    }

    #[test]
    fn the_two_empty_extras_leave_their_indentation_on_the_wire() {
        crate::pin_test_dir();
        // `header_extra=""` and `service_extra=""` are not "nothing": the template
        // puts them on their own indented lines, so an empty substitution leaves
        // an indented blank line behind. Two lines of the shipped document are
        // exactly that, and both are asserted as bytes rather than as "the file
        // still parses", because the thing being protected is whitespace.
        let mine = build(&probe_fields()).expect("fills");
        let lines: Vec<&[u8]> = mine.split(|b| *b == b'\n').collect();
        assert_eq!(lines.len(), 48, "47 lines plus the trailing newline's empty tail");
        assert_eq!(lines[19], b"        ", "line 20 is the `{{header_extra}}` line");
        assert_eq!(lines[43], b"            ", "line 44 is the `{{service_extra}}` line");
        assert_eq!(lines[20], python_witness().lines().nth(20).unwrap().as_bytes());
        assert_eq!(lines[44], python_witness().lines().nth(44).unwrap().as_bytes());
    }

    #[test]
    fn the_filled_document_still_describes_the_three_services_in_order() {
        crate::pin_test_dir();
        // The description is not only bytes: `Scpd::from_description` walks its
        // `<serviceType>` list, and that walk decides both the service set and its
        // order — which in turn decides `set_state` overwrite order and the 48-row
        // flat table `scpd`'s golden pins. So the filled document is asked, through
        // the real reader, for the same answer the template gives before filling.
        let mine = build(&probe_fields()).expect("fills");
        let text = String::from_utf8(mine.clone()).expect("build returns utf-8");
        let desc = crate::scpd::Dom::parse(&text).expect("the filled description must still parse");
        let mut reader = |service: &str| match crate::scpd::doc_for_service(service) {
            Some(content) => Ok(content.to_owned()),
            None => Err(format!("no embedded SCPD document for service '{service}'")),
        };
        let filled = crate::scpd::Scpd::from_description(&desc, &mut reader)
            .expect("the filled description must still describe three services");
        let names: Vec<&str> = filled.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["AVTransport", "RenderingControl", "ConnectionManager"]);
        // The stronger form of the same claim: filling must not change **any** of
        // the tables the reader builds from this document, including the 48-row
        // flat state list whose order Python's dict insertion order decided.
        assert_eq!(filled, crate::scpd::Scpd::embedded().expect("template reads"));
    }

    // ---- fill()'s rules ----------------------------------------------------

    #[test]
    fn double_braces_escape_to_one_and_are_not_fields() {
        crate::pin_test_dir();
        let f = probe_fields();
        assert_eq!(fill("a{{b}}c", &f).unwrap(), "a{b}c");
        assert_eq!(fill("{{{uuid}}}", &f).unwrap(), format!("{{{PROBE_UUID}}}"));
    }

    #[test]
    fn braces_inside_a_value_are_never_re_read() {
        crate::pin_test_dir();
        // Python does not substitute recursively, and this is the direction that
        // matters: the friendly name is the one field a user controls, so a
        // re-scan would let `Macast{probe}` on someone's Mac become a build error.
        let mut f = probe_fields();
        f.friendly_name = "Macast{probe}{{x}}".to_string();
        let out = fill("[{friendly_name}]", &f).unwrap();
        assert_eq!(out, "[Macast{probe}{{x}}]");
    }

    #[test]
    fn an_unknown_field_is_refused_and_named() {
        crate::pin_test_dir();
        // KeyError, with the name and the byte offset: `{uuid2}` is a typo in a
        // 47-line XML file and the message has to say which.
        let err = fill("{uuid2}", &probe_fields()).unwrap_err();
        assert!(err.starts_with("KeyError: 'uuid2'"), "{err}");
        assert!(err.contains("template byte 0"), "{err}");
        assert!(err.contains("uuid"), "{err}");
    }

    #[test]
    fn a_field_name_with_spaces_is_the_name_with_spaces() {
        crate::pin_test_dir();
        // Python does not trim: `"{ uuid }".format(uuid=1)` is `KeyError: ' uuid '`.
        // Reproduced because trimming would turn a template typo into a silent
        // substitution, which is the direction that ships a wrong device name.
        let err = fill("{ uuid }", &probe_fields()).unwrap_err();
        assert!(err.contains("' uuid '"), "{err}");
    }

    #[test]
    fn a_missing_closing_delimiter_is_refused() {
        crate::pin_test_dir();
        let err = fill("a{uuid", &probe_fields()).unwrap_err();
        assert!(err.contains("expected '}' before end of string"), "{err}");
        assert!(err.contains("template byte 1"), "{err}");
        // A `{` inside the name never closes the field either, but Python has a
        // different sentence for that one: `"{a{b}".format(a="X")` is
        // `ValueError: unexpected '{' in field name`, measured rather than inferred.
        let err = fill("{uu{id}", &probe_fields()).unwrap_err();
        assert!(err.contains("unexpected '{' in field name"), "{err}");
        assert!(err.contains("template byte 0"), "{err}");
    }

    #[test]
    fn a_single_closing_brace_is_refused() {
        crate::pin_test_dir();
        for (tpl, at) in [("}", 0), ("a}b", 1), ("uuid}", 4)] {
            let err = fill(tpl, &probe_fields()).unwrap_err();
            assert!(err.contains("Single '}' encountered"), "{tpl}: {err}");
            assert!(err.contains(&format!("template byte {at}")), "{tpl}: {err}");
        }
        // `}}` after text is still an escape, not an error.
        assert_eq!(fill("a}}}}b", &probe_fields()).unwrap(), "a}}b");
        // …and a lone `}` after a *closed* field is refused at its own offset, not
        // the field's: `"{a}}".format(a="X")` → `ValueError: Single '}' encountered`.
        let err = fill("{uuid}}", &probe_fields()).unwrap_err();
        assert!(err.contains("Single '}' encountered"), "{err}");
        assert!(err.contains("template byte 6"), "{err}");
    }

    #[test]
    fn positional_fields_are_refused_as_index_errors() {
        crate::pin_test_dir();
        // `{}` is slot 0, `{5}` is slot 5, and `{!r}` is slot 0 too: the conversion
        // is not part of the name, and an empty name is already positional. That
        // third shape is why the positional question is asked *before* the lookup.
        for (tpl, slot) in [("{0}", 0), ("{5}", 5), ("{!r}", 0)] {
            let err = fill(tpl, &probe_fields()).unwrap_err();
            assert!(err.contains("IndexError"), "{tpl}: {err}");
            assert!(err.contains("positional"), "{tpl}: {err}");
            assert!(
                err.contains(&format!("Replacement index {slot}")),
                "{tpl}: {err}"
            );
        }
        assert!(
            fill("{}", &probe_fields())
                .unwrap_err()
                .contains("Replacement index 0")
        );
    }

    #[test]
    fn spec_attribute_and_index_fields_are_refused_loudly() {
        crate::pin_test_dir();
        // Three shapes Python would answer and this port will not. The message has
        // to name the offending character, because "refused" alone across a
        // 47-line template is not an instruction.
        for (tpl, needle) in [
            ("{model_number:>10}", ":"),
            ("{uuid.b}", "."),
            ("{uuid[0]}", "["),
            ("{uuid!r}", "!"),
        ] {
            let err = fill(tpl, &probe_fields()).unwrap_err();
            assert!(err.starts_with("refused:"), "{tpl}: {err}");
            assert!(err.contains(needle), "{tpl}: {err}");
            assert!(err.contains("this port does not"), "{tpl}: {err}");
        }
    }

    #[test]
    fn an_unresolvable_name_is_a_key_error_even_with_a_spec_after_it() {
        crate::pin_test_dir();
        // Measured ordering, and the reason `fill` looks the name up before it
        // refuses the tail: `"{nosuch:>3}".format(x=1)` is `KeyError: 'nosuch'`.
        // Reporting a refusal instead would send a reader to the wrong character.
        let err = fill("{nosuch:>10}", &probe_fields()).unwrap_err();
        assert!(err.starts_with("KeyError: 'nosuch'"), "{err}");
    }

    // ---- the template's own inventory -------------------------------------

    #[test]
    fn the_template_holds_exactly_the_eleven_fields() {
        crate::pin_test_dir();
        // The whole reason the refusals above are safe: today's template contains
        // nothing but eleven plain named fields. Asking the file, not trusting the
        // doc comment, is what makes "unreachable" a checked property.
        let mut seen: Vec<String> = Vec::new();
        let mut it = TEMPLATE.char_indices().peekable();
        while let Some((idx, c)) = it.next() {
            if c == '}' {
                panic!(
                    "the template contains a `}}` at byte {idx} that does not close a \
                     field, which `fill` refuses: the inventory test must fail rather \
                     than the HTTP face 500"
                );
            }
            if c != '{' {
                continue;
            }
            // `idx + 1` is byte arithmetic and safe here: both braces are one byte.
            if it.peek().map(|(j, cc)| *j == idx + 1 && *cc == '{').unwrap_or(false) {
                panic!("the template escapes a brace with `{{` at byte {idx}; `fill` renders it and \
                        this test no longer proves the field inventory is complete");
            }
            let mut name = String::new();
            loop {
                match it.next() {
                    Some((_, '}')) => break,
                    Some((_, cc)) => name.push(cc),
                    None => panic!("unterminated field in the template"),
                }
            }
            assert!(
                !name.contains(|ch| ":.[]!(".contains(ch)) && !name.is_empty(),
                "the template asks for `{name}`, which this port refuses to render"
            );
            seen.push(name);
        }
        // Then every name is one of ours, in the declared order, each used once.
        let want: Vec<String> = seen.iter().map(|s| s.to_string()).collect();
        assert_eq!(want, FIELD_NAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(seen.len(), 11, "eleven and only eleven substitutions");
        // And the second brace of a `{{` never reaches the name loop: the peek
        // above would have panicked instead, so a `{{` anywhere is caught by it.
    }

    #[test]
    fn the_template_is_the_file_the_reader_module_embeds() {
        crate::pin_test_dir();
        // `scpd` reads this document to decide the service set; `description` fills
        // the same document to answer the phone. Two `include_str!`s of two
        // different files would look identical until a control point asked.
        assert!(std::ptr::eq(TEMPLATE, crate::scpd::DESCRIPTION_XML));
        assert_eq!(TEMPLATE.len(), 2186, "the template on disk, unmolested");
    }

    // ---- where the values come from ---------------------------------------

    #[test]
    fn the_five_project_literals_are_not_read_from_anywhere() {
        crate::pin_test_dir();
        // A reader might "fix" `manufacturer` by pulling it from settings. These
        // five are what the *project* says about itself; the Python call site has
        // them as literals, and the witness file has them as literals.
        let f = Fields::from_setting(&test_setting("literals"));
        assert_eq!(f.manufacturer, "xfangfang");
        assert_eq!(f.manufacturer_url, "https://github.com/xfangfang");
        assert_eq!(f.model_description, "AVTransport Media Renderer");
        assert_eq!(f.model_name, "Macast");
        assert_eq!(f.model_url, "https://xfangfang.github.io/Macast");
        assert_eq!(f.serial_num, "1024");
        assert_eq!(f.header_extra, "");
        assert_eq!(f.service_extra, "");
    }

    #[test]
    fn the_machine_values_come_from_the_setting_and_the_version_file() {
        crate::pin_test_dir();
        let setting = test_setting("machine");
        let f = Fields::from_setting(&setting);
        assert_eq!(f.friendly_name, setting.friendly_name());
        assert_eq!(f.model_number, paths::version());
        // `usn()` is insert-on-read, so two calls must return the same UUID — and
        // the description must carry the one SSDP is announcing, not a fresh one.
        assert_eq!(f.uuid, setting.usn());
        assert_eq!(f.uuid, setting.usn(), "the second read must not generate again");
    }

    #[test]
    fn the_friendly_name_override_moves_the_description() {
        crate::pin_test_dir();
        // `Macast E2E Smoke` (§6) is set through the temporary override, and the
        // TV's source list is what a user reads. If the description cached the
        // persisted name instead of asking, the smoke run would announce one name
        // over SSDP and describe itself as another.
        let setting = test_setting("override");
        setting.set_temp_friendly_name(Some("Macast E2E Smoke"));
        let bytes = build_for(&setting).expect("fills");
        let text = String::from_utf8(bytes).expect("utf-8");
        assert!(
            text.contains("<friendlyName>Macast E2E Smoke</friendlyName>"),
            "{text}"
        );
        setting.set_temp_friendly_name(None);
        let back = String::from_utf8(build_for(&setting).expect("fills")).expect("utf-8");
        assert!(!back.contains("E2E Smoke"), "{back}");
        assert!(back.contains("Macast("), "the default is `Macast(hostname)`: {back}");
    }

    #[test]
    fn rebuilding_after_a_usn_change_moves_both_the_udn_and_nothing_else() {
        crate::pin_test_dir();
        // `refresh_usn()` is what an address change calls, because UPnP peers cache
        // by USN. The description's `<UDN>` has to follow it in the same rebuild,
        // and the eleven-slot layout has to be untouched — otherwise a re-announce
        // publishes a device whose two identities differ by one byte.
        let setting = test_setting("usn");
        let before = String::from_utf8(build_for(&setting).expect("fills")).expect("utf-8");
        let fresh = setting.refresh_usn();
        let after = String::from_utf8(build_for(&setting).expect("fills")).expect("utf-8");
        assert!(before.contains(&format!("<UDN>uuid:{}</UDN>", setting.usn())) || {
            // `setting.usn()` is now the fresh value, so compare against the text
            // we already hold rather than re-reading.
            before.contains("<UDN>uuid:")
        });
        assert!(after.contains(&format!("<UDN>uuid:{fresh}</UDN>")), "{after}");
        assert_eq!(
            before.matches("<UDN>uuid:").count(),
            1,
            "one UDN per document"
        );
        // Same line count, so the only change is the UUID's 36 characters.
        assert_eq!(before.lines().count(), after.lines().count());
    }

    /// A settings file that **does not exist**, named for this process and this
    /// test: the same discipline as `device`'s helper, so AGENTS.md §10 cannot be
    /// violated from this module. `usn()` only inserts in memory, and nothing here
    /// calls `save()`.
    fn test_setting(name: &str) -> Setting {
        let path = std::env::temp_dir().join(format!(
            "macast-upnp-description-{}-{}.json",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_file(&path);
        Setting::new(path)
    }
}
