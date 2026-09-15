// Roughcut — a keyboard-driven assembly editor that exports MLT XML.
// Copyright (C) 2026 Roughcut contributors
//
// This program is free software: you can redistribute it and/or modify it
// under the terms of the GNU General Public License as published by the Free
// Software Foundation, either version 3 of the License, or (at your option)
// any later version. It is distributed WITHOUT ANY WARRANTY; see the GNU
// General Public License at <https://www.gnu.org/licenses/> for details.

//! Reviewing the bin from a phone.
//!
//! A page served from inside the window, over the local network, that shows
//! every clip in the bin one at a time — the real video, scrubbable — with two
//! decisions to make about each: which stretches are good, and whether it
//! belongs in the bin at all. Under that is what was said in the clip, from
//! the transcript, each line a seek target.
//!
//! It lives in the window rather than beside it because everything it needs
//! is already here. The copies a phone can play are made by the same worker
//! pool and the same frame-exact transcode that makes the window's own
//! proxies, just smaller — and they are only made once a phone has actually
//! connected, so a session that never sees one never spends a second on them.
//! A stretch kept from the phone is the same edit as one kept at the desk:
//! undoable, autosaved, and on screen in the window before the phone has
//! finished redrawing.
//!
//! Nothing here waits for a copy to exist. The phone is handed the best file
//! there is — the phone copy, the window's proxy, or the original — and the
//! page swaps to the copy when it lands.
//!
//! Idle costs nothing: the listener blocks on `accept`, and the window is
//! woken only by a request that needs it. Reads never touch the window at
//! all; they come from a snapshot the window publishes when the bin changes.

use roughcut_core::model::{ClipId, Project};
use roughcut_core::paths;
use roughcut_core::proxy::Tier;
use roughcut_core::time::Rational;
use roughcut_core::transcript::{self, Transcript};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the phone connects. Fixed, so the address on the phone never
/// changes from one session to the next.
pub const PORT: u16 = 8788;

/// How long a write waits for the window to apply it before the phone is
/// told to try again. Ordinarily it is one frame.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// Request handlers. A phone streaming video holds one for as long as it
/// is reading, so one is not enough; four is more phones than there are.
const HANDLERS: usize = 4;

/// What the phone is told about one clip.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipView {
    pub id: ClipId,
    pub name: String,
    pub path: PathBuf,
    pub frames: i64,
    pub highlights: Vec<(i64, i64)>,
    pub archived: bool,
    /// Nothing a phone can play: a photograph or a sound file.
    pub unplayable: bool,
    /// The window's own proxy, when it has one — playable on a phone too.
    pub proxy_path: Option<PathBuf>,
}

/// What the phone is told about the bin. Published by the window whenever
/// it changes; read by the server without ever asking the window.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub project: String,
    pub fps: Rational,
    pub proxy_dir: Option<PathBuf>,
    pub clips: Vec<ClipView>,
}

impl Snapshot {
    pub fn of(project: &Project, project_path: Option<&Path>, proxy_dir: Option<PathBuf>) -> Self {
        Self {
            project: project_path
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Roughcut".into()),
            fps: project.fps(),
            proxy_dir,
            clips: project
                .clips
                .iter()
                .map(|c| ClipView {
                    id: c.id,
                    name: c.file_name(),
                    path: c.path.clone(),
                    frames: c.duration_frames,
                    highlights: c.highlights.iter().map(|h| (h.in_frame, h.out_frame)).collect(),
                    archived: c.archived,
                    unplayable: c.still || c.audio_only,
                    proxy_path: c.proxy_path.clone(),
                })
                .collect(),
        }
    }
}

/// The answer to a write: the clip's kept stretches afterwards.
pub type Reply = Result<Vec<(i64, i64)>, String>;

/// What the phone asks the window to do. Only writes, and the two moments
/// worth knowing about: a phone has connected, and which clip it is on.
#[derive(Debug)]
pub enum Request {
    Connected,
    Watching(ClipId),
    Keep {
        clip: ClipId,
        in_frame: i64,
        out_frame: i64,
        reply: crossbeam_channel::Sender<Reply>,
    },
    Drop {
        clip: ClipId,
        index: usize,
        reply: crossbeam_channel::Sender<Reply>,
    },
    Archive {
        clip: ClipId,
        archived: bool,
        reply: crossbeam_channel::Sender<Reply>,
    },
}

struct Shared {
    snapshot: Mutex<Option<Snapshot>>,
    /// The clip the phone last asked the picture of, so a video element's
    /// many range requests for one clip wake the window once, not fifty
    /// times.
    watching: Mutex<Option<ClipId>>,
    tx: crossbeam_channel::Sender<Request>,
    ctx: egui::Context,
}

/// The window's handle on the server.
pub struct Phone {
    shared: Arc<Shared>,
    requests: crossbeam_channel::Receiver<Request>,
    published: Option<Snapshot>,
    /// Where a phone should go, or why it cannot.
    address: Result<String, String>,
}

impl Phone {
    /// Start listening. A port already in use is reported, not fatal: the
    /// window works exactly as before, only without a phone.
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, requests) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(None),
            watching: Mutex::new(None),
            tx,
            ctx,
        });
        let address = match tiny_http::Server::http(("0.0.0.0", PORT)) {
            Ok(server) => {
                let server = Arc::new(server);
                for i in 0..HANDLERS {
                    let server = server.clone();
                    let shared = shared.clone();
                    std::thread::Builder::new()
                        .name(format!("roughcut-phone-{i}"))
                        .spawn(move || serve(&server, &shared))
                        .expect("cannot spawn phone thread");
                }
                let url = format!("http://{}:{PORT}/", host_name());
                log::info!("phone: {url}");
                Ok(url)
            }
            Err(e) => {
                log::warn!("phone: cannot listen on port {PORT}: {e}");
                Err(format!("port {PORT} is in use — {e}"))
            }
        };
        Self {
            shared,
            requests,
            published: None,
            address,
        }
    }

    /// Where a phone should go.
    pub fn address(&self) -> Result<&str, &str> {
        self.address.as_deref().map_err(|e| e.as_str())
    }

    /// Tell the server what the bin looks like now. Cheap to call every
    /// pass: nothing is stored unless something changed.
    pub fn publish(&mut self, snapshot: Snapshot) {
        if self.published.as_ref() == Some(&snapshot) {
            return;
        }
        *self.shared.snapshot.lock().unwrap() = Some(snapshot.clone());
        self.published = Some(snapshot);
    }

    /// What the phone has asked for. Never blocks.
    pub fn poll(&self) -> impl Iterator<Item = Request> + '_ {
        self.requests.try_iter()
    }

    /// Forget which clip the phone was on, so the next one it asks for
    /// wakes the window again — after a project change, say.
    pub fn forget_watching(&self) {
        *self.shared.watching.lock().unwrap() = None;
    }
}

/// This machine, as the phone would name it. On a tailnet MagicDNS resolves
/// the plain host name, which is the one address that survives a change of
/// network.
fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|h| h.to_ascii_lowercase())
        .unwrap_or_else(|_| "localhost".into())
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

const PAGE: &str = include_str!("phone.html");

fn serve(server: &tiny_http::Server, shared: &Shared) {
    for request in server.incoming_requests() {
        if let Err(e) = handle(request, shared) {
            log::debug!("phone: {e:#}");
        }
    }
}

/// The path without its query — the page appends which copy it expects, so
/// a clip whose copy has just landed is a fresh request rather than a cached
/// one, and the server has no reason to read it.
fn path_of(request: &tiny_http::Request) -> &str {
    request.url().split('?').next().unwrap_or("")
}

fn handle(mut request: tiny_http::Request, shared: &Shared) -> anyhow::Result<()> {
    use tiny_http::Method;
    let path = path_of(&request).to_string();
    match (request.method().clone(), path.as_str()) {
        (Method::Get, "/") => {
            let project = shared
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| s.project.clone())
                .unwrap_or_else(|| "Roughcut".into());
            let page = PAGE.replace("__PROJECT__", &html_escape(&project));
            respond(request, text(200, "text/html; charset=utf-8", page))
        }
        (Method::Get, "/state") => {
            // A page load is the moment a phone is known to be here.
            let _ = shared.tx.send(Request::Connected);
            shared.ctx.request_repaint();
            respond(request, json_response(200, state(shared)))
        }
        (Method::Get, p) if p.starts_with("/video/") => {
            let id = parse_id(&p["/video/".len()..]);
            let Some(id) = id else {
                return respond(request, text(404, "text/plain", "not here".into()));
            };
            {
                let mut watching = shared.watching.lock().unwrap();
                if *watching != Some(id) {
                    *watching = Some(id);
                    let _ = shared.tx.send(Request::Watching(id));
                    shared.ctx.request_repaint();
                }
            }
            let file = shared
                .snapshot
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|s| s.clips.iter().find(|c| c.id == id).and_then(|c| best_file(s, c)));
            match file {
                Some((path, _)) => serve_file(request, &path),
                None => respond(request, text(404, "text/plain", "not here".into())),
            }
        }
        (Method::Get, p) if p.starts_with("/transcript/") => {
            let id = parse_id(&p["/transcript/".len()..]);
            let clip = id.and_then(|id| {
                let snap = shared.snapshot.lock().unwrap();
                snap.as_ref().and_then(|s| {
                    s.clips
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| (c.path.clone(), c.frames, s.fps))
                })
            });
            let body = match clip {
                Some((path, frames, fps)) => match cached_transcript(&path) {
                    Some(t) => json!({ "lines": lines(&t, fps, frames), "available": true }),
                    None => json!({ "lines": [], "available": false }),
                },
                None => json!({ "lines": [], "available": false }),
            };
            respond(request, json_response(200, body))
        }
        (Method::Post, "/keep") | (Method::Post, "/drop") | (Method::Post, "/archive") => {
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body)?;
            let Ok(body) = serde_json::from_str::<Value>(&body) else {
                return respond(request, json_response(400, json!({ "error": "bad json" })));
            };
            let Some(clip) = body.get("clip").and_then(Value::as_str).and_then(parse_id) else {
                return respond(request, json_response(404, json!({ "error": "no such clip" })));
            };
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            let req = match path.as_str() {
                "/keep" => Request::Keep {
                    clip,
                    in_frame: body.get("in").and_then(Value::as_i64).unwrap_or(0),
                    out_frame: body.get("out").and_then(Value::as_i64).unwrap_or(0),
                    reply: reply_tx,
                },
                "/drop" => Request::Drop {
                    clip,
                    index: body.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize,
                    reply: reply_tx,
                },
                _ => Request::Archive {
                    clip,
                    archived: body.get("archived").and_then(Value::as_bool).unwrap_or(false),
                    reply: reply_tx,
                },
            };
            let archiving = matches!(req, Request::Archive { archived: true, .. });
            let _ = shared.tx.send(req);
            shared.ctx.request_repaint();
            // The window applies it on its next pass, which the repaint just
            // asked for. The wait is the one blocking thing in here, and it
            // is bounded.
            let response = match reply_rx.recv_timeout(REPLY_TIMEOUT) {
                Ok(Ok(_)) if path == "/archive" => json_response(200, json!({ "archived": archiving })),
                Ok(Ok(hi)) => json_response(200, json!({ "hi": pairs(&hi) })),
                Ok(Err(e)) if e == "no such clip" => json_response(404, json!({ "error": e })),
                Ok(Err(e)) => json_response(409, json!({ "error": e })),
                Err(_) => json_response(
                    503,
                    json!({ "error": "the Roughcut window did not answer — is it open?" }),
                ),
            };
            respond(request, response)
        }
        _ => respond(request, text(404, "text/plain", "not here".into())),
    }
}

/// The bin as the page wants it: one small object per clip, and which file
/// the picture will come from.
fn state(shared: &Shared) -> Value {
    let snap = shared.snapshot.lock().unwrap();
    let Some(s) = snap.as_ref() else {
        return json!({ "project": "Roughcut", "fps": 30.0, "clips": [] });
    };
    let clips: Vec<Value> = s
        .clips
        .iter()
        .map(|c| {
            json!({
                "id": c.id.to_string(),
                "name": c.name,
                "frames": c.frames,
                "hi": pairs(&c.highlights),
                "arch": c.archived,
                "src": best_source(s, c),
            })
        })
        .collect();
    json!({ "project": s.project, "fps": s.fps.as_f64(), "clips": clips })
}

/// Which copy of a clip a phone gets, best first: the phone copy, the
/// window's proxy, the original. A copy exists only when it is complete —
/// the transcode writes elsewhere and renames — so existence is the test.
fn best_source(s: &Snapshot, c: &ClipView) -> Option<&'static str> {
    best_file(s, c).map(|(_, which)| which)
}

fn best_file(s: &Snapshot, c: &ClipView) -> Option<(PathBuf, &'static str)> {
    if c.unplayable {
        return None;
    }
    if let Some(dir) = &s.proxy_dir {
        let phone = Tier::Phone.path(dir, c.id);
        if phone.is_file() {
            return Some((phone, "phone"));
        }
    }
    if let Some(p) = &c.proxy_path {
        if p.is_file() {
            return Some((p.clone(), "edit"));
        }
    }
    if c.path.is_file() {
        return Some((c.path.clone(), "original"));
    }
    None
}

fn pairs(hi: &[(i64, i64)]) -> Vec<[i64; 2]> {
    hi.iter().map(|&(a, b)| [a, b]).collect()
}

fn parse_id(s: &str) -> Option<ClipId> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// What was said, and when
// ---------------------------------------------------------------------------

/// The transcript the window has already cached for this file, if any. Read
/// from disk on each request rather than kept: the window may make one while
/// the page is open, and this is how the page finds out.
fn cached_transcript(path: &Path) -> Option<Transcript> {
    let dir = paths::transcript_cache_dir()?;
    let file = transcript::cache_file(&dir, paths::fingerprint(path, &[]));
    let text = std::fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

/// A transcript grouped into tappable lines.
///
/// Whisper gives words, each carrying the moment it is spoken, and a wall of
/// two hundred separate words is not something to read or aim a thumb at.
/// Lines break where a sentence ends, and failing that where a breath does —
/// a gap long enough to be a pause — so a line is a thing somebody said
/// rather than an arbitrary ten words.
pub fn lines(t: &Transcript, fps: Rational, frames: i64) -> Vec<Value> {
    // Punctuation is what actually ends a thought. A pause only breaks a
    // line when it is a long one and there is already a line's worth of
    // words — speech is full of half-second hesitations, and treating
    // those as breaks chopped "the best piece of software / engineering /
    // advice he ever gave me" into three lines.
    const GAP_MS: i64 = 1500;
    const MIN_BEFORE_GAP: usize = 5;
    const MAX_WORDS: usize = 16; // so a monologue without punctuation still breaks

    let last = (frames - 1).max(0);
    let words: Vec<_> = t.words().collect();
    let mut out = Vec::new();
    let mut cur: Vec<&transcript::Word> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        cur.push(w);
        let text = w.text.trim();
        let next = words.get(i + 1);
        let gap = next.map_or(0, |n| n.start_ms - w.start_ms);
        let ends = text.ends_with(['.', '?', '!', '…']);
        let breathed = gap >= GAP_MS && cur.len() >= MIN_BEFORE_GAP;
        if next.is_none() || ends || breathed || cur.len() >= MAX_WORDS {
            let said = cur
                .iter()
                .map(|c| c.text.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !said.is_empty() {
                out.push(json!({
                    "frame": transcript::ms_to_frame(cur[0].start_ms, fps).min(last),
                    "end": transcript::ms_to_frame(cur[cur.len() - 1].start_ms, fps).min(last),
                    "text": said,
                }));
            }
            cur.clear();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Responses, and the byte ranges that make video work at all
// ---------------------------------------------------------------------------

type Response = tiny_http::Response<Box<dyn Read + Send>>;

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("ascii header")
}

fn text(code: u16, content_type: &str, body: String) -> Response {
    let len = body.len();
    tiny_http::Response::new(
        tiny_http::StatusCode(code),
        vec![
            header("Content-Type", content_type),
            header("Cache-Control", "no-store"),
        ],
        Box::new(std::io::Cursor::new(body.into_bytes())),
        Some(len),
        None,
    )
}

fn json_response(code: u16, body: Value) -> Response {
    text(code, "application/json", body.to_string())
}

fn respond(request: tiny_http::Request, response: Response) -> anyhow::Result<()> {
    // A phone that seeked and moved on closes the connection mid-file. That
    // is the ordinary case, not an error.
    let _ = request.respond(response);
    Ok(())
}

/// `bytes=A-B`, `bytes=A-`, or `bytes=-N`, as (first, last) within `size`.
/// `None` for no header or one this does not understand — served whole.
/// A start past the end is `Some(Err(()))`: 416.
pub fn byte_range(header: Option<&str>, size: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header?.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    let last = size.checked_sub(1)?;
    let range = match (a.parse::<u64>().ok(), b.parse::<u64>().ok()) {
        (Some(first), Some(end)) => (first, end.min(last)),
        (Some(first), None) => (first, last),
        // The final N bytes — how a player reads an index at the end.
        (None, Some(n)) => (size.saturating_sub(n), last),
        (None, None) => return None,
    };
    if range.0 > last || range.0 > range.1 {
        return Some(Err(()));
    }
    Some(Ok(range))
}

/// A byte-range file server.
///
/// iOS Safari will not play a video from a server that ignores Range: it
/// asks for the first few bytes to read the header, and a 200 with the whole
/// file makes it give up rather than buffer. So the 206 path is the normal
/// path here, not an optimisation.
fn serve_file(request: tiny_http::Request, path: &Path) -> anyhow::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let content_type = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    };
    let range = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());
    let (code, first, last) = match byte_range(range.as_deref(), size) {
        None => (200, 0, size.saturating_sub(1)),
        Some(Ok((a, b))) => (206, a, b),
        Some(Err(())) => {
            let r = tiny_http::Response::new(
                tiny_http::StatusCode(416),
                vec![header("Content-Range", &format!("bytes */{size}"))],
                Box::new(std::io::empty()) as Box<dyn Read + Send>,
                Some(0),
                None,
            );
            return respond(request, r);
        }
    };
    let len = if size == 0 { 0 } else { last - first + 1 };
    file.seek(SeekFrom::Start(first))?;
    let mut headers = vec![
        header("Content-Type", content_type),
        header("Accept-Ranges", "bytes"),
        header("Cache-Control", "no-store"),
    ];
    if code == 206 {
        headers.push(header("Content-Range", &format!("bytes {first}-{last}/{size}")));
    }
    let r = tiny_http::Response::new(
        tiny_http::StatusCode(code),
        headers,
        Box::new(file.take(len)) as Box<dyn Read + Send>,
        Some(len as usize),
        None,
    );
    respond(request, r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use roughcut_core::transcript::{Segment, Word};

    #[test]
    fn ranges_are_read_the_way_a_player_writes_them() {
        assert_eq!(byte_range(None, 100), None);
        assert_eq!(byte_range(Some("bytes=0-1"), 100), Some(Ok((0, 1))));
        assert_eq!(byte_range(Some("bytes=10-"), 100), Some(Ok((10, 99))));
        // The end is clamped, not refused: players over-ask routinely.
        assert_eq!(byte_range(Some("bytes=10-5000"), 100), Some(Ok((10, 99))));
        // The final N bytes, which is how an index at the end gets read.
        assert_eq!(byte_range(Some("bytes=-10"), 100), Some(Ok((90, 99))));
        assert_eq!(byte_range(Some("bytes=-500"), 100), Some(Ok((0, 99))));
        // Past the end is unsatisfiable, and says so.
        assert_eq!(byte_range(Some("bytes=100-"), 100), Some(Err(())));
        assert_eq!(byte_range(Some("bytes=50-10"), 100), Some(Err(())));
        // Not bytes, or not a range at all: served whole.
        assert_eq!(byte_range(Some("items=0-1"), 100), None);
        assert_eq!(byte_range(Some("bytes=-"), 100), None);
        assert_eq!(byte_range(Some("bytes=0-"), 0), None);
    }

    fn transcript(words: &[(&str, i64)]) -> Transcript {
        Transcript {
            segments: vec![Segment {
                words: words
                    .iter()
                    .map(|(t, ms)| Word {
                        text: t.to_string(),
                        start_ms: *ms,
                        end_ms: *ms + 200,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn lines_break_at_sentences_and_long_pauses_but_not_hesitations() {
        let fps = Rational::new(30, 1);
        let t = transcript(&[
            ("The", 0),
            ("best", 300),
            ("advice.", 600),
            // A half-second hesitation is not a break.
            ("He", 1200),
            ("said", 1500),
            ("it", 1800),
            ("twice", 2100),
            ("and", 2400),
            // A long pause after a line's worth of words is.
            ("meant", 4500),
            ("it", 4800),
        ]);
        let out = lines(&t, fps, 10_000);
        let texts: Vec<&str> = out.iter().map(|l| l["text"].as_str().unwrap()).collect();
        assert_eq!(texts, ["The best advice.", "He said it twice and", "meant it"]);
        // Each line seeks to its first word, in frames.
        assert_eq!(out[1]["frame"], 36);
        // ...and never past the clip.
        assert_eq!(lines(&t, fps, 20)[2]["frame"], 19);
    }
}
