//! Program Status Protocol (`OSC 7501`): a program tells its terminal what it
//! is doing — `idle`, `working`, `done`, `blocked` (on the user), or `error`.
//!
//! Spec: <https://www.superlogical.com/rex/docs/build/program-status>.
//!
//! The Host is the terminal of record for every hosted PTY, so it is the
//! party that answers the feature-detection query (`OSC 7501 ; ? ST`, see
//! `OutputQueryScanner`) and keeps the records. Reports travel inside the
//! PTY output the Host already reads: no hook port, no session environment,
//! and they survive nested shells and SSH. The PTY core scans each published
//! chunk ([`ProgramStatusScanner`]), applies reports to a per-Session
//! [`ProgramStatusStore`], and edge-writes the store's [`ProgramStatusSummary`]
//! into the manifest; the worker publishes it as `activitySource: "program"`,
//! which outranks hooks and the screen tier while a record is live.
//!
//! Every report is untrusted input: limits are enforced, free text is base64
//! UTF-8 with control characters refused and bidi overrides disarmed, and the
//! Host never writes record contents back to the PTY.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The reply to `OSC 7501 ; ? ST`, sent by the Host on behalf of the
/// terminal. ST-terminated like the spec's examples.
pub const FEATURE_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// Whole-sequence limit (`ESC ] 7501 ;` + body + ST).
const MAX_SEQUENCE_BYTES: usize = 4096;
/// Body bytes the scanner keeps: the sequence limit minus `ESC ] 7501 ;`
/// and the shortest terminator.
const MAX_BODY_BYTES: usize = MAX_SEQUENCE_BYTES - 7 - 1;
const MAX_MSG_BYTES: usize = 2048;
const MAX_TITLE_BYTES: usize = 192;
const MAX_APP_BYTES: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_LEVELS: usize = 8;
/// Records per terminal; the least recently updated is evicted past this.
const MAX_RECORDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl ProgramState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    /// `working` and `blocked` describe a live process: they are dropped
    /// when it exits or a new shell prompt begins. `idle` may be dropped
    /// then too (the Host does); `done` and `error` survive both.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Idle | Self::Working | Self::Blocked)
    }

    /// The finished states a completion notification may announce.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }

    /// Which record speaks for the whole terminal: anything waiting on the
    /// user first, then live work, then a failure, then a success.
    fn urgency(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Working => 3,
            Self::Error => 2,
            Self::Done => 1,
            Self::Idle => 0,
        }
    }
}

/// What a `blocked` program waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedKind {
    /// An approval.
    Permission,
    /// A typed answer.
    Question,
    /// A credential.
    Auth,
}

impl BlockedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

/// One record, exactly as its last report stated it (full replacement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramRecord {
    pub state: ProgramState,
    pub kind: Option<BlockedKind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// A parsed report body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Replace the record at `id` (empty = root).
    Set { id: String, record: ProgramRecord },
    /// Remove the record at `id` and every record beneath it; an empty id
    /// removes every record on the terminal.
    Clear { id: String },
}

/// Parse an `OSC 7501` body — the bytes after `7501;`, without the
/// terminator. `None` means the whole report is discarded: no or unknown
/// `state`, a broken limit, invalid base64, or decoded text with a control
/// character. Malformed pairs are skipped, unknown keys ignored, and a
/// repeated key's last value wins.
pub fn parse_report(body: &[u8]) -> Option<Report> {
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    let body = std::str::from_utf8(body).ok()?;
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key.is_empty()
            || !key.bytes().all(|byte| byte.is_ascii_lowercase())
            || !value.bytes().all(is_value_byte)
        {
            continue;
        }
        match pairs.iter_mut().find(|(existing, _)| *existing == key) {
            Some(slot) => slot.1 = value,
            None => pairs.push((key, value)),
        }
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(existing, _)| *existing == key)
            .map(|(_, value)| *value)
    };

    let id = match get("id") {
        Some(id) => validate_id(id)?,
        None => String::new(),
    };
    let state = match get("state")? {
        "clear" => return Some(Report::Clear { id }),
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        _ => return None,
    };
    let kind = (state == ProgramState::Blocked)
        .then(|| match get("kind")? {
            "permission" => Some(BlockedKind::Permission),
            "question" => Some(BlockedKind::Question),
            "auth" => Some(BlockedKind::Auth),
            _ => None,
        })
        .flatten();
    let progress = matches!(state, ProgramState::Working | ProgramState::Blocked)
        .then(|| {
            get("progress")?
                .parse::<u8>()
                .ok()
                .filter(|value| *value <= 100)
        })
        .flatten();
    let app = match get("app") {
        Some("") | None => None,
        Some(app) if app.len() > MAX_APP_BYTES => return None,
        Some(app) => Some(app.to_string()),
    };
    let title = decode_text(get("title"), MAX_TITLE_BYTES)?;
    let msg = decode_text(get("msg"), MAX_MSG_BYTES)?;
    Some(Report::Set {
        id,
        record: ProgramRecord {
            state,
            kind,
            progress,
            app,
            title,
            msg,
        },
    })
}

/// `[A-Za-z0-9_.,+/=-]`: the value alphabet, which needs no escaping.
fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

fn validate_id(id: &str) -> Option<String> {
    if id.is_empty() {
        return Some(String::new());
    }
    let segments: Vec<&str> = id.split('/').collect();
    let valid = id.len() <= MAX_ID_BYTES
        && segments.len() <= MAX_ID_LEVELS
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.len() <= MAX_ID_SEGMENT_BYTES);
    valid.then(|| id.to_string())
}

/// Standard base64 of UTF-8, padding optional. `Some(None)` when absent;
/// `None` (discard the report) when invalid, over `limit` decoded bytes, or
/// containing any control character.
fn decode_text(value: Option<&str>, limit: usize) -> Option<Option<String>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Some(None);
    };
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let bytes = engine.decode(value).ok()?;
    if bytes.len() > limit {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    // Disarm bidirectional overrides and isolates so a report cannot
    // reorder surrounding UI text.
    let text: String = text
        .chars()
        .filter(|ch| !matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect();
    let text = text.trim();
    Some((!text.is_empty()).then(|| text.to_string()))
}

/// The process that owned the terminal's foreground when a record was
/// reported: pid plus kernel start time, so a recycled pid never keeps a
/// departed program's `working` alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessOwner {
    pub pid: u32,
    pub started_at: u64,
}

#[derive(Debug, Clone)]
struct StoredRecord {
    record: ProgramRecord,
    /// Monotonic update order, for "most recent" and LRU eviction.
    seq: u64,
    /// Wall-clock ms of the last change to this record's content.
    changed_at: u64,
    owner: Option<ProcessOwner>,
}

/// One terminal's records, keyed by `/`-separated id (`""` = root).
#[derive(Debug, Default)]
pub struct ProgramStatusStore {
    records: BTreeMap<String, StoredRecord>,
    clock: u64,
}

impl ProgramStatusStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Apply one report. Returns whether any record changed.
    pub fn apply(&mut self, report: Report, owner: Option<ProcessOwner>, now_ms: u64) -> bool {
        match report {
            Report::Clear { id } => self.clear(&id),
            Report::Set { id, record } => {
                self.clock += 1;
                let seq = self.clock;
                if let Some(stored) = self.records.get_mut(&id) {
                    stored.seq = seq;
                    stored.owner = owner.or(stored.owner);
                    if stored.record == record {
                        return false;
                    }
                    stored.record = record;
                    stored.changed_at = now_ms;
                    return true;
                }
                self.records.insert(
                    id,
                    StoredRecord {
                        record,
                        seq,
                        changed_at: now_ms,
                        owner,
                    },
                );
                if self.records.len() > MAX_RECORDS {
                    if let Some(oldest) = self
                        .records
                        .iter()
                        .min_by_key(|(_, stored)| stored.seq)
                        .map(|(id, _)| id.clone())
                    {
                        self.records.remove(&oldest);
                    }
                }
                true
            }
        }
    }

    fn clear(&mut self, id: &str) -> bool {
        let before = self.records.len();
        if id.is_empty() {
            self.records.clear();
        } else {
            let prefix = format!("{id}/");
            self.records
                .retain(|key, _| key != id && !key.starts_with(&prefix));
        }
        self.records.len() != before
    }

    /// A new shell prompt began (`OSC 133 ; A`): drop live-process records.
    pub fn drop_transient(&mut self) -> bool {
        let before = self.records.len();
        self.records
            .retain(|_, stored| !stored.record.state.is_transient());
        self.records.len() != before
    }

    /// Full terminal reset (`RIS`): every record goes.
    pub fn reset(&mut self) -> bool {
        let changed = !self.records.is_empty();
        self.records.clear();
        changed
    }

    /// Drop live-process records whose reporting process has exited.
    /// `is_alive` must verify identity (pid + start time), never a bare pid.
    pub fn sweep_departed(&mut self, is_alive: impl Fn(&ProcessOwner) -> bool) -> bool {
        let before = self.records.len();
        self.records.retain(|_, stored| {
            !stored.record.state.is_transient() || stored.owner.as_ref().is_none_or(&is_alive)
        });
        self.records.len() != before
    }

    /// Whether any record could be dropped by [`Self::sweep_departed`].
    pub fn has_owned_transient(&self) -> bool {
        self.records
            .values()
            .any(|stored| stored.record.state.is_transient() && stored.owner.is_some())
    }

    /// The record that speaks for the whole terminal: the most urgent state,
    /// most recently updated among equals; `app` inherited from ancestors.
    pub fn summary(&self) -> Option<ProgramStatusSummary> {
        let (id, stored) = self.records.iter().max_by(|(_, left), (_, right)| {
            left.record
                .state
                .urgency()
                .cmp(&right.record.state.urgency())
                .then(left.seq.cmp(&right.seq))
        })?;
        let record = &stored.record;
        let app = record.app.clone().or_else(|| self.inherited_app(id));
        Some(ProgramStatusSummary {
            state: record.state,
            kind: record.kind,
            progress: record.progress,
            app,
            title: record.title.clone(),
            msg: record.msg.clone(),
            id: (!id.is_empty()).then(|| id.clone()),
            changed_at: stored.changed_at,
        })
    }

    fn inherited_app(&self, id: &str) -> Option<String> {
        let mut path = id;
        while !path.is_empty() {
            path = path
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("");
            if let Some(app) = self
                .records
                .get(path)
                .and_then(|stored| stored.record.app.clone())
            {
                return Some(app);
            }
        }
        None
    }
}

/// What the manifest carries for the worker and Controllers: the record
/// that speaks for the terminal. Absent when the terminal holds no records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatusSummary {
    pub state: ProgramState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<BlockedKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    /// The record's id; absent for the root record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Wall-clock ms the record's content last changed.
    #[serde(default)]
    pub changed_at: u64,
}

impl ProgramStatusSummary {
    /// The Controller projection (`programStatus` on a session row):
    /// camelCase and additive. The text is untrusted and display-only.
    pub fn controller_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state.as_str(),
            "kind": self.kind.map(BlockedKind::as_str),
            "progress": self.progress,
            "app": self.app,
            "title": self.title,
            "message": self.msg,
            "updatedAtUnixMs": self.changed_at,
        })
    }
}

/// Something in the output stream that affects records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// An `OSC 7501` body (after `7501;`, without the terminator).
    Report(Vec<u8>),
    /// `OSC 133 ; A`: a new shell prompt began.
    PromptStart,
    /// `ESC c` (RIS): full terminal reset.
    FullReset,
}

/// Bytes a non-matching or overflowed OSC string may run before the scanner
/// abandons it: an unterminated OSC must not hide the rest of the output.
const SKIP_BUDGET_BYTES: usize = 64 * 1024;
/// Longest `OSC 133` payload kept (only its first byte matters).
const MAX_PROMPT_PAYLOAD_BYTES: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScanState {
    Ground,
    Escape,
    /// After `ESC ]`, collecting the numeric code up to `;`.
    Code,
    Payload,
    PayloadEscape,
    Skip,
    SkipEscape,
}

/// Passive scanner over the hosted child's output (already passed through,
/// recorded, and broadcast). Carries partial sequences across chunks.
#[derive(Debug)]
pub struct ProgramStatusScanner {
    state: ScanState,
    code: u32,
    code_digits: usize,
    payload: Vec<u8>,
    skipped: usize,
}

impl Default for ProgramStatusScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgramStatusScanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Ground,
            code: 0,
            code_digits: 0,
            payload: Vec::new(),
            skipped: 0,
        }
    }

    pub fn scan(&mut self, input: &[u8]) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        // Steady output without escapes costs one pass and no allocation.
        if self.state == ScanState::Ground && !input.contains(&0x1b) {
            return events;
        }
        for &byte in input {
            match self.state {
                ScanState::Ground => {
                    if byte == 0x1b {
                        self.state = ScanState::Escape;
                    }
                }
                ScanState::Escape => self.after_escape(byte, &mut events),
                ScanState::Code => match byte {
                    b'0'..=b'9' if self.code_digits < 6 => {
                        self.code = self.code * 10 + u32::from(byte - b'0');
                        self.code_digits += 1;
                    }
                    b';' if matches!(self.code, 7501 | 133) => {
                        self.payload.clear();
                        self.state = ScanState::Payload;
                    }
                    0x07 => self.state = ScanState::Ground,
                    0x1b => self.state = ScanState::SkipEscape,
                    _ => {
                        self.skipped = 0;
                        self.state = ScanState::Skip;
                    }
                },
                ScanState::Payload => match byte {
                    0x07 => {
                        self.finish(&mut events);
                        self.state = ScanState::Ground;
                    }
                    0x1b => self.state = ScanState::PayloadEscape,
                    _ => {
                        let limit = if self.code == 7501 {
                            MAX_BODY_BYTES
                        } else {
                            MAX_PROMPT_PAYLOAD_BYTES
                        };
                        if self.payload.len() < limit {
                            self.payload.push(byte);
                        } else {
                            // Over the limit: the report is discarded whole.
                            self.payload.clear();
                            self.skipped = 0;
                            self.state = ScanState::Skip;
                        }
                    }
                },
                ScanState::PayloadEscape => {
                    if byte == b'\\' {
                        self.finish(&mut events);
                        self.state = ScanState::Ground;
                    } else {
                        // Aborted OSC; the ESC starts a new sequence.
                        self.payload.clear();
                        self.after_escape(byte, &mut events);
                    }
                }
                ScanState::Skip => {
                    self.skipped += 1;
                    match byte {
                        0x07 => self.state = ScanState::Ground,
                        0x1b => self.state = ScanState::SkipEscape,
                        _ if self.skipped > SKIP_BUDGET_BYTES => self.state = ScanState::Ground,
                        _ => {}
                    }
                }
                ScanState::SkipEscape => {
                    if byte == b'\\' {
                        self.state = ScanState::Ground;
                    } else {
                        self.after_escape(byte, &mut events);
                    }
                }
            }
        }
        events
    }

    /// The byte after an ESC (in ground or aborting a string).
    fn after_escape(&mut self, byte: u8, events: &mut Vec<StreamEvent>) {
        self.state = match byte {
            b']' => {
                self.code = 0;
                self.code_digits = 0;
                ScanState::Code
            }
            b'c' => {
                events.push(StreamEvent::FullReset);
                ScanState::Ground
            }
            0x1b => ScanState::Escape,
            _ => ScanState::Ground,
        };
    }

    fn finish(&mut self, events: &mut Vec<StreamEvent>) {
        let payload = std::mem::take(&mut self.payload);
        match self.code {
            7501 => events.push(StreamEvent::Report(payload)),
            133 if payload.first() == Some(&b'A') => events.push(StreamEvent::PromptStart),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn set(body: &str) -> (String, ProgramRecord) {
        match parse_report(body.as_bytes()) {
            Some(Report::Set { id, record }) => (id, record),
            other => panic!("expected a set report for {body:?}, got {other:?}"),
        }
    }

    #[test]
    fn parses_claude_code_reports() {
        let (id, record) =
            set("state=working:app=claude-code:msg=Q3JlYXRpbmcgcHJvYmUgZmlsZSBpbiAvdG1w");
        assert_eq!(id, "");
        assert_eq!(record.state, ProgramState::Working);
        assert_eq!(record.app.as_deref(), Some("claude-code"));
        assert_eq!(record.msg.as_deref(), Some("Creating probe file in /tmp"));
        assert_eq!(
            parse_report(b"state=clear"),
            Some(Report::Clear { id: String::new() })
        );
    }

    #[test]
    fn pairs_follow_the_spec_rules() {
        // Malformed pairs skipped, unknown keys ignored, last value wins.
        let (_, record) =
            set("bogus:state=idle:x-y=1:future=ok:state=blocked:kind=auth:progress=40");
        assert_eq!(record.state, ProgramState::Blocked);
        assert_eq!(record.kind, Some(BlockedKind::Auth));
        assert_eq!(record.progress, Some(40));
        // kind only with blocked; progress only with working/blocked.
        let (_, record) = set("state=done:kind=auth:progress=10");
        assert_eq!((record.kind, record.progress), (None, None));
        // Out-of-range progress is indeterminate, not a discard.
        let (_, record) = set("state=working:progress=101");
        assert_eq!(record.progress, None);
        // Padding is optional.
        let (_, record) = set(&format!(
            "state=done:title={}",
            b64("hi").trim_end_matches('=')
        ));
        assert_eq!(record.title.as_deref(), Some("hi"));
    }

    #[test]
    fn whole_reports_are_discarded() {
        for body in [
            "app=cargo".to_string(),
            "state=sleeping".to_string(),
            "state=working:msg=A".to_string(),
            format!("state=working:msg={}", b64("two\nlines")),
            format!("state=working:msg={}", b64("tab\there")),
            format!("state=working:title={}", b64(&"x".repeat(193))),
            format!("state=working:msg={}", b64(&"x".repeat(2049))),
            format!("state=working:app={}", "a".repeat(33)),
            format!("state=working:id={}", "a".repeat(33)),
            "state=working:id=a//b".to_string(),
            format!("state=working:id={}", ["a"; 9].join("/")),
        ] {
            assert_eq!(parse_report(body.as_bytes()), None, "{body}");
        }
        assert_eq!(parse_report(&vec![b'a'; MAX_BODY_BYTES + 1]), None);
    }

    #[test]
    fn bidi_overrides_are_disarmed() {
        let (_, record) = set(&format!("state=done:msg={}", b64("ok\u{202E}txt.exe")));
        assert_eq!(record.msg.as_deref(), Some("oktxt.exe"));
    }

    fn report(body: &str) -> Report {
        parse_report(body.as_bytes()).expect(body)
    }

    #[test]
    fn store_replaces_clears_subtrees_and_inherits_app() {
        let mut store = ProgramStatusStore::new();
        assert!(store.apply(report("state=working:app=deploy"), None, 1));
        assert!(store.apply(report("state=working:id=us-east"), None, 2));
        assert!(store.apply(report("state=blocked:kind=permission:id=eu-west"), None, 3));
        let summary = store.summary().unwrap();
        assert_eq!(summary.state, ProgramState::Blocked);
        assert_eq!(summary.id.as_deref(), Some("eu-west"));
        assert_eq!(summary.app.as_deref(), Some("deploy"));
        // An identical re-report is not a change.
        assert!(!store.apply(report("state=working:id=us-east"), None, 4));
        assert!(store.apply(report("state=clear:id=eu-west"), None, 5));
        assert_eq!(store.summary().unwrap().state, ProgramState::Working);
        // Full replacement: the omitted app is gone.
        assert!(store.apply(report("state=done"), None, 6));
        assert!(store.apply(report("state=clear:id=us-east"), None, 7));
        let summary = store.summary().unwrap();
        assert_eq!((summary.state, summary.app), (ProgramState::Done, None));
        assert!(store.apply(report("state=clear"), None, 8));
        assert!(store.summary().is_none());
    }

    #[test]
    fn clearing_a_parent_removes_children_but_not_siblings_sharing_a_prefix() {
        let mut store = ProgramStatusStore::new();
        store.apply(report("state=working:id=build"), None, 1);
        store.apply(report("state=working:id=build/test"), None, 1);
        store.apply(report("state=working:id=buildx"), None, 1);
        store.apply(report("state=clear:id=build"), None, 2);
        assert_eq!(store.summary().unwrap().id.as_deref(), Some("buildx"));
    }

    #[test]
    fn lifetime_events_keep_finished_records() {
        let mut store = ProgramStatusStore::new();
        store.apply(report("state=done:id=a"), None, 1);
        store.apply(report("state=working:id=b"), None, 1);
        store.apply(report("state=idle:id=c"), None, 1);
        assert!(store.drop_transient());
        assert_eq!(store.summary().unwrap().state, ProgramState::Done);
        assert!(store.reset());
        assert!(store.is_empty());
    }

    #[test]
    fn sweep_drops_only_departed_live_records() {
        let gone = ProcessOwner {
            pid: 7,
            started_at: 1,
        };
        let here = ProcessOwner {
            pid: 8,
            started_at: 1,
        };
        let mut store = ProgramStatusStore::new();
        store.apply(report("state=working:id=a"), Some(gone), 1);
        store.apply(report("state=error:id=b"), Some(gone), 1);
        store.apply(report("state=working:id=c"), Some(here), 1);
        assert!(store.sweep_departed(|owner| owner.pid == 8));
        assert_eq!(store.summary().unwrap().id.as_deref(), Some("c"));
        store.apply(report("state=clear:id=c"), None, 2);
        assert_eq!(store.summary().unwrap().state, ProgramState::Error);
    }

    #[test]
    fn store_evicts_least_recently_updated() {
        let mut store = ProgramStatusStore::new();
        for index in 0..=MAX_RECORDS {
            store.apply(report(&format!("state=done:id=r{index}")), None, 1);
        }
        assert_eq!(store.records.len(), MAX_RECORDS);
        assert!(!store.records.contains_key("r0"));
    }

    #[test]
    fn scanner_finds_reports_prompts_and_resets_across_chunks() {
        let mut scanner = ProgramStatusScanner::new();
        let stream = b"hi\x1b]0;title\x07\x1b]7501;state=working:app=x\x1b\\mid\x1b]133;A\x07\x1bc\x1b]7501;state=done\x07";
        let mut events = Vec::new();
        for byte in stream.chunks(3) {
            events.extend(scanner.scan(byte));
        }
        assert_eq!(
            events,
            vec![
                StreamEvent::Report(b"state=working:app=x".to_vec()),
                StreamEvent::PromptStart,
                StreamEvent::FullReset,
                StreamEvent::Report(b"state=done".to_vec()),
            ]
        );
        // 133;B (command start) is not a prompt start; 75010 is not 7501.
        assert!(scanner
            .scan(b"\x1b]133;B\x07\x1b]75010;state=done\x07")
            .is_empty());
    }

    #[test]
    fn scanner_drops_oversized_and_aborted_reports() {
        let mut scanner = ProgramStatusScanner::new();
        let mut oversized = b"\x1b]7501;state=working:msg=".to_vec();
        oversized.extend(std::iter::repeat_n(b'A', MAX_SEQUENCE_BYTES));
        oversized.extend_from_slice(b"\x07");
        assert!(scanner.scan(&oversized).is_empty());
        // An ESC that is not ST aborts and starts the next sequence.
        let events = scanner.scan(b"\x1b]7501;state=working\x1b]7501;state=idle\x07");
        assert_eq!(events, vec![StreamEvent::Report(b"state=idle".to_vec())]);
    }
}
