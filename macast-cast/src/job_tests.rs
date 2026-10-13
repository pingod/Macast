//! Resolve-job differential truth: CPython 3.12.15, 162 frozen MR15c rows.
//! Corpus md5: bb0a761962a395021a56c4e6b9e857d8. Expectations are copied,
//! never recomputed from the reference implementation during generation.
//! Each row executes independently, so a failed row cannot hide later rows.
//! Python callbacks can raise; Rust's callback records the same final state
//! without raising. The malformed-host row asserts the declared empty-host
//! divergence separately from Python's recorded ValueError.
use super::*;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn strv(s: &str) -> Value { Value::String(s.to_owned()) }
fn obj(pairs: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs { m.insert((*k).to_owned(), v.clone()); }
    Value::Object(m)
}
// CPython json.dumps separators, preserving the resolve layer's key order.
fn text(v: &Value) -> String {
    match v {
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k,v)| format!("{}: {}", serde_json::to_string(k).unwrap(), text(v))).collect::<Vec<_>>().join(", ")),
        Value::Array(a) => format!("[{}]", a.iter().map(text).collect::<Vec<_>>().join(", ")),
        _ => serde_json::to_string(v).unwrap(),
    }
}
fn assert_json(actual: &Value, expected: &str) {
    assert_eq!(text(actual), expected);
}
fn py_repr_f64(v: f64) -> String {
    let s = serde_json::to_string(&v).unwrap();
    if let Some((mantissa, exponent)) = s.split_once('e') {
        let exponent: i32 = exponent.parse().unwrap();
        format!("{}e{:+03}", mantissa, exponent)
    } else { s }
}
fn probe(v: &Value) -> Probe {
    let mut p = Probe::new();
    if v.is_null() { return p; }
    p.reach = v["reach"].as_str().unwrap_or("unproven").to_owned();
    p.container = v["container"].as_str().map(str::to_owned);
    p.video_codec = v["video_codec"].as_str().map(str::to_owned);
    // Empty Python codec strings are falsy; the port uses None for absence.
    p.audio_codec = v["audio_codec"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    p.video_width = v["width"].as_u64().map(|n| n as u32);
    p.video_height = v["height"].as_u64().map(|n| n as u32);
    p.duration = v["duration"].as_f64();
    p.bitrate = v["bitrate"].as_f64();
    p.is_live = v["is_live"].as_bool().unwrap_or(false);
    p.error = v["error"].as_str().map(str::to_owned);
    p
}
fn candidate(v: &Value) -> Candidate {
    let mut c = Candidate::new(v["url"].as_str().unwrap().to_owned(),
                               v["origin"].as_str().unwrap().to_owned());
    c.headers = v["headers"].as_object().unwrap().iter().map(|(k,v)|
        (k.clone(), v.as_str().unwrap().to_owned())).collect();
    c.label = v["label"].as_str().unwrap().to_owned();
    c.title = v["title"].as_str().map(str::to_owned);
    c.probe = probe(&v["probe"]);
    c.last_resort = v["last_resort"].as_bool().unwrap();
    c.reason = v["reason"].as_str().unwrap().to_owned();
    c.audio_codec = v["audio_codec"].as_str().unwrap().to_owned();
    c.audio_url = v["audio_url"].as_str().map(str::to_owned);
    c.audio_headers = v["audio_headers"].as_object().unwrap().iter().map(|(k,v)|
        (k.clone(), v.as_str().unwrap().to_owned())).collect();
    c.audio_label = v["audio_label"].as_str().unwrap().to_owned();
    c
}
fn candidates(v: &Value) -> Vec<Candidate> {
    v.as_array().unwrap().iter().map(candidate).collect()
}
fn cookies(v: &Value) -> Cookies<'_> {
    Cookies { file: v["file"].as_str().unwrap_or(""), browser: v["browser"].as_str().unwrap_or("") }
}
fn job_fixture(v: &Value) -> ResolveJob {
    let mut j = ResolveJob::new(v["page_url"].as_str().unwrap(), v["max_height"].as_f64().unwrap(),
        None, &cookies(&v["cookies"]), v["note"].as_str().unwrap(), v["started"].as_f64().unwrap());
    j.candidates = candidates(&v["candidates"]);
    j.error = v["error"].as_str().unwrap().to_owned();
    j.done = v["done"].as_bool().unwrap();
    j.step = v["step"].as_str().unwrap().to_owned();
    j.step_started = v["step_started"].as_f64().unwrap();
    j.scraped = v["scraped"].as_u64().unwrap() as usize;
    j.from_ytdlp = v["from_ytdlp"].as_u64().unwrap() as usize;
    j.measured = v["measured"].as_u64().unwrap() as usize;
    j.rejected = v["rejected"].as_u64().unwrap() as usize;
    j.engine_note = v["engine_note"].as_str().unwrap().to_owned();
    j.hidden_silent = v["hidden_silent"].as_u64().unwrap() as usize;
    j
}
struct FixtureOps {
    spec: Value,
    trace: RefCell<Vec<Value>>,
    calls: Cell<usize>,
}
impl FixtureOps {
    fn bump(&self, step: &str, job: &ResolveJob, url: &str) {
        self.trace.borrow_mut().push(Value::Array(vec![strv(step), strv(&job.step),
            Value::from(job.scraped), Value::from(job.from_ytdlp), Value::from(job.measured), strv(url)]));
    }
    fn fail(&self, step: &str) -> Result<(), ResolveError> {
        if self.spec["raise_from"].as_str() == Some(step) &&
            (!self.spec["only_second"].as_bool().unwrap() || self.calls.get() == 2) {
            return Err(ResolveError { kind: self.spec["exc"]["cls"].as_str().unwrap().to_owned(),
                message: self.spec["exc"]["msg"].as_str().unwrap().to_owned() });
        }
        Ok(())
    }
}
impl ResolveOps for FixtureOps {
    fn scrape_page(&self, j: &ResolveJob, page: &str) -> Result<Vec<Candidate>, ResolveError> {
        assert_eq!(page, self.spec["page_url"].as_str().unwrap());
        self.bump("scrape", j, ""); self.fail("scrape")?;
        Ok(candidates(&self.spec["scrape"]))
    }
    fn ytdlp_candidates(&self, j: &ResolveJob, page: &str, c: &Cookies<'_>, sink: &mut Vec<String>) -> Result<Vec<Candidate>, ResolveError> {
        assert_eq!(page, self.spec["page_url"].as_str().unwrap());
        let expected = cookies(&self.spec["cookies"]);
        assert_eq!((c.file, c.browser), (expected.file, expected.browser));
        self.bump("ytdlp", j, ""); self.fail("ytdlp")?;
        if let Some(note) = self.spec["sink_note"].as_str() {
            sink.push(note.to_owned());
            sink.extend(self.spec["sink_extra"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()));
        }
        Ok(candidates(&self.spec["resolved"]))
    }
    fn measure(&self, j: &ResolveJob, c: &mut Candidate) -> Result<Probe, ResolveError> {
        self.bump("measure", j, &c.url); self.calls.set(self.calls.get()+1); self.fail("measure")?;
        let p = probe(&self.spec["probes"][&c.url]);
        c.probe = p.clone(); Ok(p)
    }
    fn rank(&self, _: &ResolveJob, cs: Vec<Candidate>, ceiling: f64) -> Result<Vec<Candidate>, ResolveError> {
        self.fail("rank")?; Ok(rank(cs, ceiling))
    }
    fn has_ytdlp(&self) -> bool { self.spec["has_ytdlp"].as_bool().unwrap() }
    fn max_measured(&self) -> usize { self.spec["max_measured"].as_u64().unwrap() as usize }
    fn max_per_host(&self) -> usize { self.spec["max_per_host"].as_u64().unwrap() as usize }
}
fn scenario(spec: Value) -> Value {
    let ops = FixtureOps { spec, trace: RefCell::new(Vec::new()), calls: Cell::new(0) };
    let s = &ops.spec;
    let fired = Rc::new(RefCell::new(Vec::<Value>::new()));
    let recorder = fired.clone();
    let callback: Option<Box<dyn Fn(&ResolveJob)>> = match s["on_finish"].as_str() {
        Some("record") => Some(Box::new(move |j| recorder.borrow_mut().push(Value::Array(vec![strv(&j.step), Value::Bool(j.done), Value::from(j.candidates.len())])))),
        // Rust callbacks cannot raise Python exceptions. Witness the same call
        // and final state as the Python callback that recorded then raised.
        Some("boom") => Some(Box::new(move |j| recorder.borrow_mut().push(strv(&j.step)))),
        None => None,
        Some(other) => panic!("unknown callback {other}"),
    };
    let clock = || s["t"].as_f64().unwrap();
    let c = cookies(&s["cookies"]);
    let page = s["page_url"].as_str().unwrap();
    let height = s["max_height"].as_f64().unwrap();
    let j = if s["now_style"].as_bool().unwrap() {
        resolve_now(&ops, &clock, page, height, &c)
    } else {
        let mut j = ResolveJob::new(page, height, callback, &c, s["note"].as_str().unwrap(), clock());
        j.run(&ops, &clock); j
    };
    let (status, raises) = match j.status(&clock) {
        Ok(status) => (text(&status), String::new()),
        Err(e) => (String::new(), format!("ValueError: {e}")),
    };
    let fired = text(&Value::Array(fired.borrow().clone()));
    obj(&[("name", s["name"].clone()), ("trace", strv(&text(&Value::Array(ops.trace.into_inner())))),
        ("fired", strv(&fired)),
        ("status", strv(&status)), ("raises", strv(&raises))])
}

#[test]
fn mr15_desc_00() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_01() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "   夏日 电影  ", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "夏日 电影", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_02() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/%E7%94%B5%E5%BD%B1.mp4", "origin": "page", "headers": {}, "label": "", "title": "   ", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/%E7%94%B5%E5%BD%B1.mp4", "origin": "page", "label": "", "title": "电影", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_03() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/%E7%94%B5%E5%BD%B1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/%E7%94%B5%E5%BD%B1.mp4", "origin": "page", "label": "", "title": "电影", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_04() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/MOVIE.M3U8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/MOVIE.M3U8", "origin": "page", "label": "", "title": "MOVIE", "container": "M3U8", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": true, "last_resort": false, "reason": "", "mode": "remux", "relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_05() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.bin/Show%20One.mkv?sign=abc123#t=30", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.bin/Show%20One.mkv?sign=abc123#t=30", "origin": "page", "label": "", "title": "Show One", "container": "MKV", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_06() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/  .mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/  .mp4", "origin": "page", "label": "", "title": "http://a.example/  .mp4", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_07() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "", "origin": "page", "label": "", "title": "", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_08() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/stream", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp,3g2,mj2", "video_codec": "h264", "audio_codec": "aac", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/stream", "origin": "page", "label": "", "title": "stream", "container": "MOV", "video_codec": "h264", "audio_codec": "aac", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_09() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/stream", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/stream", "origin": "page", "label": "", "title": "stream", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_10() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/ep01.mp4", "origin": "ytdlp", "headers": {}, "label": "f1080 视频", "title": "", "probe": {"reach": "opened", "container": "mp4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.3, "bitrate": 7520000.0, "is_live": false, "error": ""}, "last_resort": true, "reason": "源站未证明可读", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/ep01.mp4", "origin": "ytdlp", "label": "f1080 视频", "title": "ep01", "container": "MP4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.3, "bitrate": 7520, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": true, "reason": "源站未证明可读", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_11() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/live.m3u8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "", "video_codec": "hevc", "audio_codec": "mp4a", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": true, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/live.m3u8", "origin": "page", "label": "", "title": "live", "container": "M3U8", "video_codec": "hevc", "audio_codec": "mp4a", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": true, "segmented": true, "last_resort": false, "reason": "", "mode": "remux", "relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_12() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/video.mpd", "origin": "ytdlp", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "http://a.example/audio.m4a", "audio_headers": {"User-Agent": "ua", "Referer": "r"}, "audio_label": "m4a 203 k"}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/video.mpd", "origin": "ytdlp", "label": "", "title": "video", "container": "MPD", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": true, "last_resort": false, "reason": "", "mode": "merge", "relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "headers": [], "merged": true, "audio_label": "m4a 203 k", "silent": false}"#);
}

#[test]
fn mr15_desc_13() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/clip.mp4", "origin": "ytdlp", "headers": {"User-Agent": "ua", "Referer": "http://r/", "Cookie": "c"}, "label": "http 720", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/clip.mp4", "origin": "ytdlp", "label": "http 720", "title": "clip", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "源站要求请求头（Cookie、Referer、User-Agent）：电视不会带上这些，由 Macast 代取", "headers": ["Cookie", "Referer", "User-Agent"], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_14() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/silent.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/silent.mp4", "origin": "page", "label": "", "title": "silent", "container": "MP4", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": true}"#);
}

#[test]
fn mr15_desc_15() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/poster.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "", "video_codec": "mjpeg", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/poster.mp4", "origin": "page", "label": "", "title": "poster", "container": "MP4", "video_codec": "mjpeg", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_16() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/unproven.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "refused", "container": "", "video_codec": "h264", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "http error 403"}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/unproven.mp4", "origin": "page", "label": "", "title": "unproven", "container": "MP4", "video_codec": "h264", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "refused", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_17() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/sound.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "", "video_codec": "h264", "audio_codec": "mp4a", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/sound.mp4", "origin": "page", "label": "", "title": "sound", "container": "MP4", "video_codec": "h264", "audio_codec": "mp4a", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_18() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/song.mp3", "origin": "ytdlp", "headers": {}, "label": "audio only", "title": "", "probe": {"reach": "opened", "container": "mp3", "video_codec": "", "audio_codec": "mp3", "width": 0, "height": 0, "duration": 253.466666, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/song.mp3", "origin": "ytdlp", "label": "audio only", "title": "song", "container": "MP3", "video_codec": "", "audio_codec": "mp3", "width": 0, "height": 0, "duration": 253.5, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_19() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "aac", "audio_url": "http://a.example/a.m4a", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "merge", "relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "headers": [], "merged": true, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_20() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/b.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/b.mp4", "origin": "page", "label": "", "title": "b", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_21() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/b.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 999.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/b.mp4", "origin": "page", "label": "", "title": "b", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 1, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_22() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/b.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/b.mp4", "origin": "page", "label": "", "title": "b", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_23() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/b.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 3500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/b.mp4", "origin": "page", "label": "", "title": "b", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 4, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_24() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/b.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 1500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/b.mp4", "origin": "page", "label": "", "title": "b", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_25() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_26() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxabc", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxa", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_27() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "", "origin": "ytdlp", "headers": {}, "label": "unknown format", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "", "origin": "ytdlp", "label": "unknown format", "title": "", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_28() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.m3u8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "http://a.example/a.m4a", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.m3u8", "origin": "page", "label": "", "title": "x", "container": "M3U8", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": true, "last_resort": false, "reason": "", "mode": "merge", "relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "headers": [], "merged": true, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_29() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.ts", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "mpegts", "video_codec": "h264", "audio_codec": "aac", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.ts", "origin": "page", "label": "", "title": "x", "container": "TS", "video_codec": "h264", "audio_codec": "aac", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_30() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_31() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mp4", "origin": "ytdlp", "headers": {"referer": "low"}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mp4", "origin": "ytdlp", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "源站要求请求头（referer）：电视不会带上这些，由 Macast 代取", "headers": ["referer"], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_32() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/x.mpd", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "dash,mpd", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/x.mpd", "origin": "page", "label": "", "title": "x", "container": "MPD", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": true, "last_resort": false, "reason": "", "mode": "remux", "relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_33() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a.example/名字.mp4", "origin": "page", "headers": {"X-自定义": "v"}, "label": "", "title": "标题", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = describe(&c).unwrap();
    assert_json(&got, r#"{"url": "http://a.example/名字.mp4", "origin": "page", "label": "", "title": "标题", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "源站要求请求头（X-自定义）：电视不会带上这些，由 Macast 代取", "headers": ["X-自定义"], "merged": false, "audio_label": "", "silent": false}"#);
}

#[test]
fn mr15_desc_34() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.m3u8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "plan": "remux", "suffix": ".m3u8", "container": "M3U8", "title": "x"}"#);
}

#[test]
fn mr15_desc_35() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "plan": "proxy", "suffix": ".mp4", "container": "MP4", "title": "x"}"#);
}

#[test]
fn mr15_desc_36() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "http://a/a.m4a", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "plan": "merge", "suffix": ".mp4", "container": "MP4", "title": "x"}"#);
}

#[test]
fn mr15_desc_37() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {"B": "1", "a": "2"}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "源站要求请求头（B、a）：电视不会带上这些，由 Macast 代取", "plan": "proxy", "suffix": ".mp4", "container": "MP4", "title": "x"}"#);
}

#[test]
fn mr15_desc_38() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.m3u8", "origin": "page", "headers": {"B": "1"}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "http://a/a", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "plan": "merge", "suffix": ".m3u8", "container": "M3U8", "title": "x"}"#);
}

#[test]
fn mr15_desc_39() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.m3u8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let got = obj(&[("relay", strv(&relay_reason(&c))), ("plan", strv(plan(&c))), ("suffix", strv(c.path_suffix())), ("container", strv(&container_label(&c))), ("title", strv(&display_title(&c, "").unwrap()))]);
    assert_json(&got, r#"{"relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "plan": "remux", "suffix": ".m3u8", "container": "M3U8", "title": "x"}"#);
}

#[test]
fn mr15_nul_00() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_nul_01() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.m3u8", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"http://a/x.m3u8\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"M3U8\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": true, \"last_resort\": false, \"reason\": \"\", \"mode\": \"remux\", \"relay\": \"清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"http://a/x.m3u8\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"M3U8\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": true, \"last_resort\": false, \"reason\": \"\", \"mode\": \"remux\", \"relay\": \"清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_nul_02() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "ytdlp", "headers": {"Referer": "r"}, "label": "", "title": "T", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"http://a/x.mp4\", \"origin\": \"ytdlp\", \"label\": \"\", \"title\": \"T\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"源站要求请求头（Referer）：电视不会带上这些，由 Macast 代取\", \"headers\": [\"Referer\"], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"http://a/x.mp4\", \"origin\": \"ytdlp\", \"label\": \"\", \"title\": \"T\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"源站要求请求头（Referer）：电视不会带上这些，由 Macast 代取\", \"headers\": [\"Referer\"], \"merged\": false, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_nul_03() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "http://a/a.m4a", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"merge\", \"relay\": \"画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出\", \"headers\": [], \"merged\": true, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"merge\", \"relay\": \"画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出\", \"headers\": [], \"merged\": true, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_nul_04() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"\", \"origin\": \"page\", \"label\": \"\", \"title\": \"\", \"container\": \"\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"\", \"origin\": \"page\", \"label\": \"\", \"title\": \"\", \"container\": \"\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_nul_05() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp3", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let mut zero = c.clone(); zero.probe = Probe::new();
    let got = obj(&[("none", strv(&text(&describe(&c).unwrap()))), ("zero", strv(&text(&describe(&zero).unwrap())))]);
    assert_json(&got, r#"{"none": "{\"url\": \"http://a/x.mp3\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP3\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "zero": "{\"url\": \"http://a/x.mp3\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP3\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}"}"#);
}

#[test]
fn mr15_rnd_00() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.0"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.0, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.0"}"#);
}

#[test]
fn mr15_rnd_01() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.05, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.1, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.05"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.1, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.1, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.05"}"#);
}

#[test]
fn mr15_rnd_02() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.15, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.1, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.15"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.1, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.1, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.15"}"#);
}

#[test]
fn mr15_rnd_03() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.25, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.2, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.25"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.2, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.2, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.25"}"#);
}

#[test]
fn mr15_rnd_04() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.35, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.3, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.35"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.3, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.3, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.35"}"#);
}

#[test]
fn mr15_rnd_05() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 1.05, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 1.1, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"1.05"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 1.1, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 1.1, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "1.05"}"#);
}

#[test]
fn mr15_rnd_06() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 1.25, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 1.2, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"1.25"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 1.2, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 1.2, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "1.25"}"#);
}

#[test]
fn mr15_rnd_07() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 2.675, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 2.7, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"2.675"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 2.7, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 2.7, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "2.675"}"#);
}

#[test]
fn mr15_rnd_08() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 12.34, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 12.3, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"12.34"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 12.3, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 12.3, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "12.34"}"#);
}

#[test]
fn mr15_rnd_09() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 12.35, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 12.3, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"12.35"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 12.3, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 12.3, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "12.35"}"#);
}

#[test]
fn mr15_rnd_10() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.175, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.2, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.175"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.2, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.2, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.175"}"#);
}

#[test]
fn mr15_rnd_11() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.125, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.1, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"0.125"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 0.1, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.1, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "0.125"}"#);
}

#[test]
fn mr15_rnd_12() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 212.25, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 212.2, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"212.25"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 212.2, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 212.2, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "212.25"}"#);
}

#[test]
fn mr15_rnd_13() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 212.35, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 212.3, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"212.35"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 212.3, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 212.3, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "212.35"}"#);
}

#[test]
fn mr15_rnd_14() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 59.95, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 60.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"59.95"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 60.0, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 60.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "59.95"}"#);
}

#[test]
fn mr15_rnd_15() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 3600.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_json(&described, r#"{"url": "http://a/x.mp4", "origin": "page", "label": "", "title": "x", "container": "MP4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 3600.0, "bitrate": 0, "measured": false, "reach": "unproven", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}"#);
    assert_eq!(py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap()).as_bytes(), r#"3600.0"#.as_bytes());
    assert_json(&obj(&[("duration", described["duration"].clone()), ("whole", strv(&text(&described))), ("raw", strv(&py_repr_f64(fixture["probe"]["duration"].as_f64().unwrap())))]), r#"{"duration": 3600.0, "whole": "{\"url\": \"http://a/x.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"x\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 3600.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}", "raw": "3600.0"}"#);
}

#[test]
fn mr15_rnd_16() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"0.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 0, "raw": "0.0"}"#);
}

#[test]
fn mr15_rnd_17() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 1.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"1.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 0, "raw": "1.0"}"#);
}

#[test]
fn mr15_rnd_18() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 999.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"999.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 1, "raw": "999.0"}"#);
}

#[test]
fn mr15_rnd_19() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 1500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"1500.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 2, "raw": "1500.0"}"#);
}

#[test]
fn mr15_rnd_20() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"2500.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 2, "raw": "2500.0"}"#);
}

#[test]
fn mr15_rnd_21() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 3500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"3500.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 4, "raw": "3500.0"}"#);
}

#[test]
fn mr15_rnd_22() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 4500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"4500.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 4, "raw": "4500.0"}"#);
}

#[test]
fn mr15_rnd_23() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 12345.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"12345.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 12, "raw": "12345.0"}"#);
}

#[test]
fn mr15_rnd_24() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 7520000.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"7520000.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 7520, "raw": "7520000.0"}"#);
}

#[test]
fn mr15_rnd_25() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.4, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"0.4"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 0, "raw": "0.4"}"#);
}

#[test]
fn mr15_rnd_26() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 500.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"500.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 0, "raw": "500.0"}"#);
}

#[test]
fn mr15_rnd_27() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2499.9, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"2499.9"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 2, "raw": "2499.9"}"#);
}

#[test]
fn mr15_rnd_28() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 2500.1, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"2500.1"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 3, "raw": "2500.1"}"#);
}

#[test]
fn mr15_rnd_29() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"url": "http://a/x.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "unproven", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 64000.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap();
    let c = candidate(&fixture);
    let described = describe(&c).unwrap();
    assert_eq!(py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap()).as_bytes(), r#"64000.0"#.as_bytes());
    assert_json(&obj(&[("bitrate", described["bitrate"].clone()), ("raw", strv(&py_repr_f64(fixture["probe"]["bitrate"].as_f64().unwrap())))]), r#"{"bitrate": 64, "raw": "64000.0"}"#);
}

#[test]
fn mr15_lab_00() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"queued"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"queued"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "queued", "label": "排队中", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_01() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"scrape"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"scrape"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "scrape", "label": "抓取页面", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_02() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"ytdlp"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"ytdlp"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "ytdlp", "label": "yt-dlp 解析页面", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_03() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"measure"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"measure"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "measure", "label": "度量候选地址", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_04() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"rank"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"rank"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "rank", "label": "排序", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_05() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"done"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"done"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "done", "label": "完成", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_06() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#""#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#""#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "", "label": "", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_07() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"bogus"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"bogus"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "bogus", "label": "bogus", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_08() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"SCRAPE"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"SCRAPE"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "SCRAPE", "label": "SCRAPE", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_09() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"Done"#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"Done"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "Done", "label": "Done", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_10() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = step_label(r#"  rank  "#);
    let keys = Value::Array(keys);
    assert_json(&obj(&[("step", strv(r#"  rank  "#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "  rank  ", "label": "  rank  ", "keys": ["done", "measure", "queued", "rank", "scrape", "ytdlp"]}"#);
}

#[test]
fn mr15_lab_11() {
    crate::pin_test_dir();
    let mut keys: Vec<_> = STEP_LABELS.iter().map(|(k,_)| strv(k)).collect();
    keys.sort_by(|a,b| a.as_str().cmp(&b.as_str()));
    let label = text(&Value::Array(STEPS.iter().map(|s| strv(s)).collect()));
    let keys = strv(&text(&Value::Array(keys)));
    assert_json(&obj(&[("step", strv(r#"__steps__"#)), ("label", strv(&label)), ("keys", keys)]), r#"{"step": "__steps__", "label": "[\"scrape\", \"ytdlp\", \"measure\", \"rank\"]", "keys": "[\"done\", \"measure\", \"queued\", \"rank\", \"scrape\", \"ytdlp\"]"}"#);
}

#[test]
fn mr15_why_00() {
    crate::pin_test_dir();
    let note = r#""#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "", "text": "（yt-dlp 这次也没有交出一个地址；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_01() {
    crate::pin_test_dir();
    let note = r#"ERROR: Unsupported URL: http://p/1"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "ERROR: Unsupported URL: http://p/1", "text": "（yt-dlp 说：ERROR: Unsupported URL: http://p/1；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_02() {
    crate::pin_test_dir();
    let note = r#"ERROR: no supported extractor(s) extracted"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "ERROR: no supported extractor(s) extracted", "text": "（yt-dlp 说：ERROR: no supported extractor(s) extracted；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_03() {
    crate::pin_test_dir();
    let note = r#"ERROR: This site is not supported"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "ERROR: This site is not supported", "text": "（yt-dlp 说：ERROR: This site is not supported；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_04() {
    crate::pin_test_dir();
    let note = r#"ERROR: [BiliBili] cookies to access this webpage"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "ERROR: [BiliBili] cookies to access this webpage", "text": "（yt-dlp 说：ERROR: [BiliBili] cookies to access this webpage）"}"#);
}

#[test]
fn mr15_why_05() {
    crate::pin_test_dir();
    let note = r#"HTTP error 403 Forbidden"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "HTTP error 403 Forbidden", "text": "（yt-dlp 说：HTTP error 403 Forbidden）"}"#);
}

#[test]
fn mr15_why_06() {
    crate::pin_test_dir();
    let note = r#"skipping cookie file entry due to invalid length 1: line"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "skipping cookie file entry due to invalid length 1: line", "text": "（yt-dlp 说：skipping cookie file entry due to invalid length 1: line）"}"#);
}

#[test]
fn mr15_why_07() {
    crate::pin_test_dir();
    let note = r#"a note with
a newline and ，。、；：！？（）【】《》“”"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "a note with\na newline and ，。、；：！？（）【】《》“”", "text": "（yt-dlp 说：a note with\na newline and ，。、；：！？（）【】《》“”）"}"#);
}

#[test]
fn mr15_why_08() {
    crate::pin_test_dir();
    let note = r#"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "text": "（yt-dlp 说：xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx）"}"#);
}

#[test]
fn mr15_why_09() {
    crate::pin_test_dir();
    let note = r#" "#;
    let available = true;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": true, "note": " ", "text": "（yt-dlp 说： ）"}"#);
}

#[test]
fn mr15_why_10() {
    crate::pin_test_dir();
    let note = r#""#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "", "text": "（这台机器没有装 yt-dlp，只能抓 HTML 里写明的地址）"}"#);
}

#[test]
fn mr15_why_11() {
    crate::pin_test_dir();
    let note = r#"ERROR: Unsupported URL: http://p/1"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "ERROR: Unsupported URL: http://p/1", "text": "（yt-dlp 说：ERROR: Unsupported URL: http://p/1；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_12() {
    crate::pin_test_dir();
    let note = r#"ERROR: no supported extractor(s) extracted"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "ERROR: no supported extractor(s) extracted", "text": "（yt-dlp 说：ERROR: no supported extractor(s) extracted；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_13() {
    crate::pin_test_dir();
    let note = r#"ERROR: This site is not supported"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "ERROR: This site is not supported", "text": "（yt-dlp 说：ERROR: This site is not supported；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）"}"#);
}

#[test]
fn mr15_why_14() {
    crate::pin_test_dir();
    let note = r#"ERROR: [BiliBili] cookies to access this webpage"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "ERROR: [BiliBili] cookies to access this webpage", "text": "（yt-dlp 说：ERROR: [BiliBili] cookies to access this webpage）"}"#);
}

#[test]
fn mr15_why_15() {
    crate::pin_test_dir();
    let note = r#"HTTP error 403 Forbidden"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "HTTP error 403 Forbidden", "text": "（yt-dlp 说：HTTP error 403 Forbidden）"}"#);
}

#[test]
fn mr15_why_16() {
    crate::pin_test_dir();
    let note = r#"skipping cookie file entry due to invalid length 1: line"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "skipping cookie file entry due to invalid length 1: line", "text": "（yt-dlp 说：skipping cookie file entry due to invalid length 1: line）"}"#);
}

#[test]
fn mr15_why_17() {
    crate::pin_test_dir();
    let note = r#"a note with
a newline and ，。、；：！？（）【】《》“”"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "a note with\na newline and ，。、；：！？（）【】《》“”", "text": "（yt-dlp 说：a note with\na newline and ，。、；：！？（）【】《》“”）"}"#);
}

#[test]
fn mr15_why_18() {
    crate::pin_test_dir();
    let note = r#"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "text": "（yt-dlp 说：xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx）"}"#);
}

#[test]
fn mr15_why_19() {
    crate::pin_test_dir();
    let note = r#" "#;
    let available = false;
    assert_json(&obj(&[("ytdlp", Value::Bool(available)), ("note", strv(note)), ("text", strv(&ResolveJob::why_empty(note, &|| available)))]), r#"{"ytdlp": false, "note": " ", "text": "（yt-dlp 说： ）"}"#);
}

#[test]
fn mr15_stat_00() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 1000.0, "step": "queued", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_01() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 999.75, "step": "measure", "step_started": 999.65, "scraped": 3, "from_ytdlp": 5, "measured": 2, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "measure", "step_label": "度量候选地址", "seconds": 0.2, "step_seconds": 0.4, "scraped": 3, "from_ytdlp": 5, "measured": 2, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_02() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 1080, "note": "分享文案里的地址", "cookies": {}, "candidates": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "第一集 · 1080P", "probe": {"reach": "opened", "container": "mov,mp4,m4a,3gp", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/index.m3u8", "origin": "ytdlp", "headers": {"User-Agent": "ua", "Referer": "http://r/"}, "label": "480p", "title": "", "probe": {"reach": "opened", "container": "hls,applehttp", "video_codec": "hevc", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": true, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://c/video.mpd", "origin": "ytdlp", "headers": {}, "label": "", "title": "", "probe": {"reach": "opened", "container": "dash", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 12.35, "bitrate": 0.0, "is_live": false, "error": ""}, "last_resort": false, "reason": "", "audio_codec": "aac", "audio_url": "http://c/audio.mpd", "audio_headers": {}, "audio_label": "125 K"}], "error": "", "done": true, "started": 980.0, "step": "done", "step_started": 999.2, "scraped": 2, "from_ytdlp": 9, "measured": 6, "rejected": 3, "engine_note": "", "hidden_silent": 1, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "分享文案里的地址", "done": true, "step": "done", "step_label": "完成", "seconds": 20.0, "step_seconds": 0.8, "scraped": 2, "from_ytdlp": 9, "measured": 6, "rejected": 3, "hidden_silent": 1, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": [{"url": "http://a/1.mp4", "origin": "page", "label": "", "title": "第一集 · 1080P", "container": "MP4", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.2, "bitrate": 3000, "measured": true, "reach": "opened", "live": false, "segmented": false, "last_resort": false, "reason": "", "mode": "proxy", "relay": "由 Macast 供流：签名地址会过期，交给电视自己取迟早 403", "headers": [], "merged": false, "audio_label": "", "silent": false}, {"url": "http://b/index.m3u8", "origin": "ytdlp", "label": "480p", "title": "index", "container": "M3U8", "video_codec": "hevc", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0, "measured": true, "reach": "opened", "live": true, "segmented": true, "last_resort": false, "reason": "", "mode": "remux", "relay": "清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流", "headers": ["Referer", "User-Agent"], "merged": false, "audio_label": "", "silent": true}, {"url": "http://c/video.mpd", "origin": "ytdlp", "label": "", "title": "video", "container": "MPD", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 12.3, "bitrate": 0, "measured": true, "reach": "opened", "live": false, "segmented": true, "last_resort": false, "reason": "", "mode": "merge", "relay": "画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出", "headers": [], "merged": true, "audio_label": "125 K", "silent": false}]}"#);
}

#[test]
fn mr15_stat_03() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "页面里没有读到视频地址（yt-dlp 说：x）", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "x", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "x", "cookie_problem": "", "needs_cookies": false, "error": "页面里没有读到视频地址（yt-dlp 说：x）", "candidates": []}"#);
}

#[test]
fn mr15_stat_04() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 1000.0, "step": "weird", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "weird", "step_label": "weird", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_05() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {"browser": "chrome"}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_06() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 999.75, "step": "queued", "step_started": 999.65, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 0.2, "step_seconds": 0.4, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_07() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 997.4, "step": "queued", "step_started": 994.95, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 2.6, "step_seconds": 5.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_08() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 999.95, "step": "queued", "step_started": 999.949, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 0.0, "step_seconds": 0.1, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_09() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": -0.05, "step": "queued", "step_started": -1e-09, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 0.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 0.1, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_10() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": false, "started": 1234.4499999, "step": "queued", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1234.5}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": false, "step": "queued", "step_label": "排队中", "seconds": 0.1, "step_seconds": 234.5, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_11() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_12() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "ERROR: [BiliBili] cookies to access this webpage", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "ERROR: [BiliBili] cookies to access this webpage", "cookie_problem": "", "needs_cookies": true, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_13() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "ERROR: tiktok: Fresh cookies (not necessarily logged in) are needed", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "ERROR: tiktok: Fresh cookies (not necessarily logged in) are needed", "cookie_problem": "", "needs_cookies": true, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_14() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "Could not copy Chrome cookie database", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "Could not copy Chrome cookie database", "cookie_problem": "locked", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_15() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "could not find chrome cookies database in 'C:/AppData'", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "could not find chrome cookies database in 'C:/AppData'", "cookie_problem": "not_found", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_16() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "ERROR: Failed to decrypt with DPAPI", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "ERROR: Failed to decrypt with DPAPI", "cookie_problem": "decrypt", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_17() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "ERROR: Opera does not support profiles", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "ERROR: Opera does not support profiles", "cookie_problem": "bad_profile", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_18() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "ERROR: Unsupported URL: http://p/1", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "ERROR: Unsupported URL: http://p/1", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_stat_19() {
    crate::pin_test_dir();
    let fixture = serde_json::from_str::<Value>(r#"{"page_url": "http://p/1", "max_height": 0, "note": "", "cookies": {}, "candidates": [], "error": "", "done": true, "started": 1000.0, "step": "done", "step_started": 1000.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "engine_note": "unable to extract video URLs", "hidden_silent": 0, "clock": 1000.0}"#).unwrap();
    let job = job_fixture(&fixture);
    assert_json(&job.status(&|| fixture["clock"].as_f64().unwrap()).unwrap(), r#"{"url": "http://p/1", "note": "", "done": true, "step": "done", "step_label": "完成", "seconds": 0.0, "step_seconds": 0.0, "scraped": 0, "from_ytdlp": 0, "measured": 0, "rejected": 0, "hidden_silent": 0, "engine_note": "unable to extract video URLs", "cookie_problem": "", "needs_cookies": false, "error": "", "candidates": []}"#);
}

#[test]
fn mr15_flow_00() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "normal", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "页面标题", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/9.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [{"url": "http://a/1.mp4", "origin": "ytdlp", "headers": {}, "label": "1080p", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/2.m3u8", "origin": "ytdlp", "headers": {}, "label": "480p", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}, "http://a/9.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}, "http://b/2.m3u8": {"reach": "opened", "container": "hls,applehttp", "video_codec": "hevc", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": true, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "normal", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 2, 0, \"http://a/1.mp4\"], [\"measure\", \"measure\", 2, 2, 1, \"http://a/9.mp4\"], [\"measure\", \"measure\", 2, 2, 2, \"http://b/2.m3u8\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 2, \"measured\": 3, \"rejected\": 2, \"hidden_silent\": 1, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"ytdlp\", \"label\": \"1080p\", \"title\": \"页面标题\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_01() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-no-note-no-ytdlp", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": false, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-no-note-no-ytdlp", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（这台机器没有装 yt-dlp，只能抓 HTML 里写明的地址）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_02() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-no-note-ytdlp", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-no-note-ytdlp", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（yt-dlp 这次也没有交出一个地址；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_03() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-note-unsupported", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": "ERROR: Unsupported URL: http://p/1", "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-note-unsupported", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"ERROR: Unsupported URL: http://p/1\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（yt-dlp 说：ERROR: Unsupported URL: http://p/1；需要 JavaScript 才能算出地址的站点本机解不出来：地址不在 HTML 里，要页面自己的脚本再算一次）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_04() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-note-other", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": "HTTP error 403 Forbidden", "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-note-other", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"HTTP error 403 Forbidden\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（yt-dlp 说：HTTP error 403 Forbidden）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_05() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-note-cookie", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": "ERROR: [BiliBili] cookies to access this webpage", "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-note-cookie", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"ERROR: [BiliBili] cookies to access this webpage\", \"cookie_problem\": \"\", \"needs_cookies\": true, \"error\": \"页面里没有读到视频地址（yt-dlp 说：ERROR: [BiliBili] cookies to access this webpage）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_06() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-two-notes", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": "first note", "sink_extra": ["s", "e", "c", "o", "n", "d", " ", "n", "o", "t", "e"], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-two-notes", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"first note\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（yt-dlp 说：first note）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_07() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "cap", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 2, "max_per_host": 5, "scrape": [{"url": "http://a/0.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/3.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/4.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "cap", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 5, 0, 0, \"\"], [\"measure\", \"measure\", 5, 0, 0, \"http://a/0.mp4\"], [\"measure\", \"measure\", 5, 0, 1, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 5, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/0.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"0\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/2.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"2\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/3.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"3\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/4.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"4\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_08() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "per-host", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 1, "scrape": [{"url": "http://a/0.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "per-host", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 4, 0, 0, \"\"], [\"measure\", \"measure\", 4, 0, 0, \"http://a/0.mp4\"], [\"measure\", \"measure\", 4, 0, 1, \"http://b/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 4, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/0.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"0\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/2.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"2\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://b/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_09() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "all-silent", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 60.0, "bitrate": 2000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "all-silent", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 1, \"hidden_silent\": 1, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"读到的 1 个候选里，1 条只有画面、又配不出能合并的声音，其余没有一个能通过判定\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_10() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "all-refused", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}, "http://a/2.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "all-refused", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/1.mp4\"], [\"measure\", \"measure\", 2, 0, 1, \"http://a/2.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 2, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"读到的 2 个地址没有一个能通过判定（源站拒绝，或没有画面）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_11() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "silent-and-refused", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 60.0, "bitrate": 2000000, "is_live": false, "error": ""}, "http://b/2.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "silent-and-refused", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/1.mp4\"], [\"measure\", \"measure\", 2, 0, 1, \"http://b/2.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 2, \"hidden_silent\": 1, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"读到的 2 个候选里，1 条只有画面、又配不出能合并的声音，其余没有一个能通过判定\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_12() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "dash-merge", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [{"url": "http://a/v.mpd", "origin": "ytdlp", "headers": {}, "label": "1080p", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "aac", "audio_url": "http://a/a.mpd", "audio_headers": {}, "audio_label": "125 K"}], "probes": {"http://a/v.mpd": {"reach": "opened", "container": "dash", "video_codec": "h264", "audio_codec": "", "width": 1920, "height": 1080, "duration": 100.0, "bitrate": 4000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "dash-merge", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"], [\"measure\", \"measure\", 0, 1, 0, \"http://a/v.mpd\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 1, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/v.mpd\", \"origin\": \"ytdlp\", \"label\": \"1080p\", \"title\": \"v\", \"container\": \"MPD\", \"video_codec\": \"h264\", \"audio_codec\": \"\", \"width\": 1920, \"height\": 1080, \"duration\": 100.0, \"bitrate\": 4000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": true, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"merge\", \"relay\": \"画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出\", \"headers\": [], \"merged\": true, \"audio_label\": \"125 K\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_13() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "unmeasured-kept", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 1, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "unmeasured-kept", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/2.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"2\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_14() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "ceiling", "now_style": false, "page_url": "http://p/1", "max_height": 720, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/hd.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/sd.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/hd.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}, "http://a/sd.mp4": {"reach": "opened", "container": "mp4", "video_codec": "h264", "audio_codec": "aac", "width": 640, "height": 360, "duration": 30.0, "bitrate": 800000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "ceiling", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/hd.mp4\"], [\"measure\", \"measure\", 2, 0, 1, \"http://a/sd.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/sd.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"sd\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 640, \"height\": 360, \"duration\": 30.0, \"bitrate\": 800, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/hd.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"hd\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_15() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "header-only", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mp4", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "header-only", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 1, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"读到的 1 个地址没有一个能通过判定（源站拒绝，或没有画面）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_16() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "poster-only", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mp4", "video_codec": "mjpeg", "audio_codec": "", "width": 1920, "height": 1080, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "poster-only", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 1, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"读到的 1 个地址没有一个能通过判定（源站拒绝，或没有画面）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_flow_17() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "audio-only-row", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp3", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp3": {"reach": "opened", "container": "mp3", "video_codec": "", "audio_codec": "mp3", "width": 0, "height": 0, "duration": 200.0, "bitrate": 190000, "is_live": false, "error": ""}, "http://a/2.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "", "width": 1280, "height": 720, "duration": 60.0, "bitrate": 2000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "audio-only-row", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/1.mp3\"], [\"measure\", \"measure\", 2, 0, 1, \"http://a/2.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 1, \"hidden_silent\": 1, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp3\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP3\", \"video_codec\": \"\", \"audio_codec\": \"mp3\", \"width\": 0, \"height\": 0, \"duration\": 200.0, \"bitrate\": 190, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried sound but no picture, kept for an audio cast\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_18() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "same-url-both-engines", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "原标题", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [{"url": "http://a/1.mp4", "origin": "ytdlp", "headers": {}, "label": "", "title": "引擎标题", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "same-url-both-engines", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 1, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 1, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"ytdlp\", \"label\": \"\", \"title\": \"引擎标题\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_19() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "max-measured-zero", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 0, "max_per_host": 5, "scrape": [{"url": "http://[oops/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "max-measured-zero", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"]]", "fired": "[]", "status": "", "raises": "ValueError: Invalid IPv6 URL"}"#);
}

#[test]
fn mr15_flow_20() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "finish-record", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": "record", "exc": null}"#).unwrap()), r#"{"name": "finish-record", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[[\"done\", true, 1]]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_flow_21() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "finish-boom", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": "boom", "exc": null}"#).unwrap()), r#"{"name": "finish-boom", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[\"done\"]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_err_00() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "value-error", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": "scrape", "only_second": false, "on_finish": null, "exc": {"cls": "ValueError", "msg": "boom"}}"#).unwrap()), r#"{"name": "value-error", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：ValueError: boom\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_err_01() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-message", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": "scrape", "only_second": false, "on_finish": null, "exc": {"cls": "RuntimeError", "msg": ""}}"#).unwrap()), r#"{"name": "empty-message", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：RuntimeError: \", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_err_02() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "key-error", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": "ytdlp", "only_second": false, "on_finish": null, "exc": {"cls": "KeyError", "msg": "'x'"}}"#).unwrap()), r#"{"name": "key-error", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：KeyError: 'x'\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_err_03() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "os-error", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": "measure", "only_second": false, "on_finish": null, "exc": {"cls": "OSError", "msg": "disk"}}"#).unwrap()), r#"{"name": "os-error", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：OSError: disk\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_err_04() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "named-class", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}}, "sink_note": null, "sink_extra": [], "raise_from": "rank", "only_second": false, "on_finish": null, "exc": {"cls": "WeirdError", "msg": "a sentence"}}"#).unwrap()), r#"{"name": "named-class", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 1, 0, 0, \"\"], [\"measure\", \"measure\", 1, 0, 0, \"http://a/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 1, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：WeirdError: a sentence\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_err_05() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "second-measure", "now_style": false, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {"http://a/1.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}}, "sink_note": null, "sink_extra": [], "raise_from": "measure", "only_second": true, "on_finish": null, "exc": {"cls": "WeirdError", "msg": "twice"}}"#).unwrap()), r#"{"name": "second-measure", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 0, 0, \"http://a/1.mp4\"], [\"measure\", \"measure\", 2, 0, 1, \"http://a/2.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 0, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"解析出错：WeirdError: twice\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_now_00() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "normal", "now_style": true, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [{"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "页面标题", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/9.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [{"url": "http://a/1.mp4", "origin": "ytdlp", "headers": {}, "label": "1080p", "title": "页面标题", "probe": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}, "last_resort": false, "reason": "carried a castable program", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/2.m3u8", "origin": "ytdlp", "headers": {}, "label": "480p", "title": "", "probe": {"reach": "opened", "container": "hls,applehttp", "video_codec": "hevc", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": true, "error": ""}, "last_resort": false, "reason": "carried a castable program", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "probes": {"http://a/1.mp4": {"reach": "opened", "container": "mov,mp4,m4a", "video_codec": "h264", "audio_codec": "aac", "width": 1920, "height": 1080, "duration": 212.25, "bitrate": 3000000, "is_live": false, "error": ""}, "http://a/9.mp4": {"reach": "refused", "container": "", "video_codec": "", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": false, "error": "HTTP error 403 Forbidden"}, "http://b/2.m3u8": {"reach": "opened", "container": "hls,applehttp", "video_codec": "hevc", "audio_codec": "", "width": 0, "height": 0, "duration": 0.0, "bitrate": 0.0, "is_live": true, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "normal", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 2, 0, 0, \"\"], [\"measure\", \"measure\", 2, 2, 0, \"http://a/1.mp4\"], [\"measure\", \"measure\", 2, 2, 1, \"http://a/9.mp4\"], [\"measure\", \"measure\", 2, 2, 2, \"http://b/2.m3u8\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 2, \"from_ytdlp\": 2, \"measured\": 3, \"rejected\": 2, \"hidden_silent\": 1, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/1.mp4\", \"origin\": \"ytdlp\", \"label\": \"1080p\", \"title\": \"页面标题\", \"container\": \"MP4\", \"video_codec\": \"h264\", \"audio_codec\": \"aac\", \"width\": 1920, \"height\": 1080, \"duration\": 212.2, \"bitrate\": 3000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": false, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_now_01() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "empty-no-note-no-ytdlp", "now_style": true, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": false, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "empty-no-note-no-ytdlp", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 0, \"measured\": 0, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"页面里没有读到视频地址（这台机器没有装 yt-dlp，只能抓 HTML 里写明的地址）\", \"candidates\": []}", "raises": ""}"#);
}

#[test]
fn mr15_now_02() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "per-host", "now_style": true, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 1, "scrape": [{"url": "http://a/0.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": true, "reason": "unmeasurable, offered as a last resort", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": true, "reason": "unmeasurable, offered as a last resort", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://a/2.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": true, "reason": "unmeasurable, offered as a last resort", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}, {"url": "http://b/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": true, "reason": "unmeasurable, offered as a last resort", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}], "resolved": [], "probes": {}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "per-host", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 4, 0, 0, \"\"], [\"measure\", \"measure\", 4, 0, 0, \"http://a/0.mp4\"], [\"measure\", \"measure\", 4, 0, 1, \"http://b/1.mp4\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 4, \"from_ytdlp\": 0, \"measured\": 2, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/0.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"0\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://a/2.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"2\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}, {\"url\": \"http://b/1.mp4\", \"origin\": \"page\", \"label\": \"\", \"title\": \"1\", \"container\": \"MP4\", \"video_codec\": \"\", \"audio_codec\": \"\", \"width\": 0, \"height\": 0, \"duration\": 0.0, \"bitrate\": 0, \"measured\": false, \"reach\": \"unproven\", \"live\": false, \"segmented\": false, \"last_resort\": true, \"reason\": \"unmeasurable, offered as a last resort\", \"mode\": \"proxy\", \"relay\": \"由 Macast 供流：签名地址会过期，交给电视自己取迟早 403\", \"headers\": [], \"merged\": false, \"audio_label\": \"\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_now_03() {
    crate::pin_test_dir();
    assert_json(&scenario(serde_json::from_str::<Value>(r#"{"name": "dash-merge", "now_style": true, "page_url": "http://p/1", "max_height": 0, "note": "", "cookies": null, "t": 1000.0, "has_ytdlp": true, "max_measured": 8, "max_per_host": 5, "scrape": [], "resolved": [{"url": "http://a/v.mpd", "origin": "ytdlp", "headers": {}, "label": "1080p", "title": "", "probe": {"reach": "opened", "container": "dash", "video_codec": "h264", "audio_codec": "", "width": 1920, "height": 1080, "duration": 100.0, "bitrate": 4000000, "is_live": false, "error": ""}, "last_resort": false, "reason": "carried a castable program", "audio_codec": "aac", "audio_url": "http://a/a.mpd", "audio_headers": {}, "audio_label": "125 K"}], "probes": {"http://a/v.mpd": {"reach": "opened", "container": "dash", "video_codec": "h264", "audio_codec": "", "width": 1920, "height": 1080, "duration": 100.0, "bitrate": 4000000, "is_live": false, "error": ""}}, "sink_note": null, "sink_extra": [], "raise_from": null, "only_second": false, "on_finish": null, "exc": null}"#).unwrap()), r#"{"name": "dash-merge", "trace": "[[\"scrape\", \"scrape\", 0, 0, 0, \"\"], [\"ytdlp\", \"ytdlp\", 0, 0, 0, \"\"], [\"measure\", \"measure\", 0, 1, 0, \"http://a/v.mpd\"]]", "fired": "[]", "status": "{\"url\": \"http://p/1\", \"note\": \"\", \"done\": true, \"step\": \"done\", \"step_label\": \"完成\", \"seconds\": 0.0, \"step_seconds\": 0.0, \"scraped\": 0, \"from_ytdlp\": 1, \"measured\": 1, \"rejected\": 0, \"hidden_silent\": 0, \"engine_note\": \"\", \"cookie_problem\": \"\", \"needs_cookies\": false, \"error\": \"\", \"candidates\": [{\"url\": \"http://a/v.mpd\", \"origin\": \"ytdlp\", \"label\": \"1080p\", \"title\": \"v\", \"container\": \"MPD\", \"video_codec\": \"h264\", \"audio_codec\": \"\", \"width\": 1920, \"height\": 1080, \"duration\": 100.0, \"bitrate\": 4000, \"measured\": true, \"reach\": \"opened\", \"live\": false, \"segmented\": true, \"last_resort\": false, \"reason\": \"carried a castable program\", \"mode\": \"merge\", \"relay\": \"画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出\", \"headers\": [], \"merged\": true, \"audio_label\": \"125 K\", \"silent\": false}]}", "raises": ""}"#);
}

#[test]
fn mr15_err_06() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://[oops/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    let raises = format!("ValueError: {}", describe(&c).unwrap_err());
    assert_json(&obj(&[("name", strv(r#"describe-bracketed"#)), ("trace", strv("[]")), ("fired", strv("[]")), ("status", strv("")), ("raises", strv(&raises))]), r#"{"name": "describe-bracketed", "trace": "[]", "fired": "[]", "status": "", "raises": "ValueError: Invalid IPv6 URL"}"#);
}

#[test]
fn mr15_err_07() {
    crate::pin_test_dir();
    let c = candidate(&serde_json::from_str::<Value>(r#"{"url": "http://[oops/1.mp4", "origin": "page", "headers": {}, "label": "", "title": "", "probe": null, "last_resort": false, "reason": "", "audio_codec": "", "audio_url": "", "audio_headers": {}, "audio_label": ""}"#).unwrap());
    // Declared divergence: host() safely returns empty while Python raises.
    assert_eq!(c.host().as_bytes(), b"");
    let raises = format!("ValueError: {}", py_urlsplit(&c.url, "", true).err().expect("Python rejects this host"));
    assert_json(&obj(&[("name", strv(r#"__divergent__-host"#)), ("trace", strv("[]")), ("fired", strv("[]")), ("status", strv("")), ("raises", strv(&raises))]), r#"{"name": "__divergent__-host", "trace": "[]", "fired": "[]", "status": "", "raises": "ValueError: Invalid IPv6 URL"}"#);
}

