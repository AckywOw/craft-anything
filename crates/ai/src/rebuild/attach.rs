//! Following a build that another process runs (#1970): a second window, the command line, or the
//! app opened again while its last build went on. The run that owns the project (it holds
//! [`BuildLock`]) is the only writer of its feed, activity log and manifest; everything here
//! goes through two folders in the project's `inbox/`:
//!
//! - `run-events.jsonl`, written by the run: emptied when a run starts, then one JSON line per
//!   change. Line 1 is `{"header": {version, run, clean_room, started_ms}}` (`run` names this
//!   run, so a follower notices a new run replacing the file), line 2 is `{"base": {items}}`, the
//!   feed as it was when the run started, then `{"event": {at_ms, event}}` for every
//!   [`RunEvent`] the run sends (`at_ms` from the run's start) and `{"answered": {id, shown}}`
//!   when a card is marked answered in its feed. A follower takes the base and applies every line
//!   in order with the run's own code ([`feed::FeedLog::record_at`], [`feed::apply`]), so it ends
//!   with the run's feed. The base is a copy, not a count of the saved feed's items, because
//!   applying an event can change items from before the run (a checklist updated in place or
//!   replaced, a card answered, a streaming reply ended), and the saved file already has those
//!   changes.
//! - `control/`, read by the run every [`CONTROL_EVERY`]: `stop` pauses it as its own Pause does,
//!   and `answers/<name>.json` (`{id, shown}`) records how an answer given elsewhere reads in the
//!   feed, so the run stays the feed's one writer. Notes need nothing new: a follower writes them
//!   to the notes folder with ids from [`notes::foreign_id`], and the run takes each one it
//!   doesn't know as one of its waiting notes (and gives it up if its file goes before the builder
//!   reads it).
//!
//! Tool inputs and program output are scrubbed of what looks like a secret before they're logged,
//! as the activity log does, and on Unix the log is readable only by its owner.

use super::activity::Activity;
use super::*;
use craft_anything_engine::journal;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

/// The run's event log, in the project's inbox/.
pub const EVENTS_FILE: &str = "run-events.jsonl";
/// The event log's format. A follower refuses a log with a newer one.
pub const EVENTS_VERSION: u32 = 1;
/// What a follower says when the process running the build ended without saying it finished.
pub const ENDED_ELSEWHERE: &str = "The program running this build closed before the build finished, so it stopped part-way.";
/// What [`RebuildRun::attach`] says when nothing is building the project.
pub const NOT_BUILDING: &str = "This project isn't building anywhere now.";

/// How often a run looks for a stop, answers and notes left by its followers.
const CONTROL_EVERY: Duration = Duration::from_millis(250);
/// How often a follower reads the log.
const FOLLOW_EVERY: Duration = Duration::from_millis(200);
/// How long a follower waits for a run that has just taken the lock to write its first lines.
const HEADER_WAIT: Duration = Duration::from_secs(5);
/// A run releases its lock just before it says it finished, so a follower waits this long after
/// the lock goes before taking the run for lost.
const GONE_GRACE: Duration = Duration::from_secs(1);
/// The most a follower reads in one go, and the longest line it keeps.
const MOST_READ: u64 = 16 << 20;
const LONGEST_LINE: usize = 64 << 20;

pub fn events_path(root: &Path) -> PathBuf {
    root.join("inbox").join(EVENTS_FILE)
}

/// Where followers leave word for the run.
pub fn control_dir(root: &Path) -> PathBuf {
    root.join("inbox").join("control")
}

fn stop_path(root: &Path) -> PathBuf {
    control_dir(root).join("stop")
}

fn answers_dir(root: &Path) -> PathBuf {
    control_dir(root).join("answers")
}

fn unix_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Header {
    version: u32,
    /// Names this run, so a follower can tell a new run's log from its own.
    run: String,
    /// The run's mode, for the Just-ask answers a note from a follower may carry.
    clean_room: bool,
    /// When the run began (Unix ms), so a follower's times are the run's.
    started_ms: u64,
}

/// One line as written (borrowed) and as read.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum LineOut<'a> {
    Header(&'a Header),
    Base { items: &'a [FeedItem] },
    Event { at_ms: u64, event: &'a RunEvent },
    Answered { id: &'a str, shown: &'a str },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LineIn {
    Header(Header),
    Base { items: Vec<FeedItem> },
    Event { at_ms: u64, event: RunEvent },
    Answered { id: String, shown: String },
}

use feed::FeedItem;

/// The run's side of `run-events.jsonl`. Each line is flushed as it's written, so a follower sees
/// it at once; a write that fails is said once and the log stops there, and the run goes on.
pub(super) struct EventLog {
    path: PathBuf,
    file: Option<BufWriter<std::fs::File>>,
    told: bool,
}

impl EventLog {
    /// A run begins (it holds the lock): its log starts empty, and anything an earlier run's
    /// followers left in control/ (a stop it never saw) goes.
    pub(super) fn begin(root: &Path) -> EventLog {
        let _ = std::fs::remove_dir_all(control_dir(root));
        let mut log = EventLog { path: events_path(root), file: None, told: false };
        match open_new(&log.path) {
            Ok(f) => log.file = Some(BufWriter::new(f)),
            Err(e) => log.failed(&e),
        }
        log
    }

    /// The header and the feed as it is when the run starts, `elapsed` into it.
    pub(super) fn start(&mut self, clean_room: bool, elapsed: Duration, base: &[FeedItem]) {
        let run = format!("{}-{}-{:x}", std::process::id(), unix_ms(), {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(unix_ms());
            h.finish()
        });
        let header = Header { version: EVENTS_VERSION, run, clean_room, started_ms: unix_ms().saturating_sub(ms(elapsed)) };
        self.write(&LineOut::Header(&header));
        self.write(&LineOut::Base { items: base });
    }

    pub(super) fn event(&mut self, at: Duration, e: &RunEvent) {
        let e = scrubbed(e);
        self.write(&LineOut::Event { at_ms: ms(at), event: &e });
    }

    pub(super) fn answered(&mut self, id: &str, shown: &str) {
        self.write(&LineOut::Answered { id, shown });
    }

    fn write(&mut self, line: &LineOut) {
        let Some(file) = self.file.as_mut() else { return };
        let written = serde_json::to_writer(&mut *file, line).map_err(std::io::Error::other).and_then(|()| file.write_all(b"\n")).and_then(|()| file.flush());
        if let Err(e) = written {
            // A line may be cut off: nothing more goes after it.
            self.file = None;
            self.failed(&e);
        }
    }

    fn failed(&mut self, e: &std::io::Error) {
        if !std::mem::replace(&mut self.told, true) {
            log::warn!("couldn't write {}, so other windows can't follow this build: {e}", self.path.display());
        }
    }
}

fn open_new(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    // It names the commands the builder ran, as the activity log does.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// `e` as it's logged: what a tool was given and what a program printed, with what looks like a
/// secret hidden (field names first, then each string).
fn scrubbed(e: &RunEvent) -> Cow<'_, RunEvent> {
    fn strings(v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(journal::scrubbed_line(s)),
            Value::Array(a) => Value::Array(a.iter().map(strings).collect()),
            Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), strings(v))).collect()),
            other => other.clone(),
        }
    }
    match e {
        RunEvent::Agent(AgentEvent::ToolStarted { call_id, tool, input }) => {
            Cow::Owned(RunEvent::Agent(AgentEvent::ToolStarted { call_id: call_id.clone(), tool: tool.clone(), input: strings(&journal::redacted(input)) }))
        }
        RunEvent::Agent(AgentEvent::Output { text }) => Cow::Owned(RunEvent::Agent(AgentEvent::Output { text: journal::scrubbed_line(text) })),
        _ => Cow::Borrowed(e),
    }
}

/// Look after what followers leave for the run, every [`CONTROL_EVERY`] until `running` goes
/// false (the run finished).
pub(super) fn watch(root: PathBuf, stop: CancelToken, notes: Arc<Mutex<Vec<Note>>>, feed: Arc<Mutex<feed::FeedLog>>, running: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new().name("rebuild-control".into()).spawn(move || {
        let mut control = Control::default();
        while running.load(Ordering::SeqCst) {
            std::thread::sleep(CONTROL_EVERY);
            control.once(&root, &stop, &notes, &feed);
        }
    });
    if let Err(e) = spawned {
        log::warn!("Other windows can't pause this build or add to it: {e}");
    }
}

/// The run's side of control/ and of followers' notes.
#[derive(Default)]
pub(super) struct Control {
    /// Followers' notes already taken, so one isn't taken twice once the builder has it.
    adopted: HashSet<u64>,
}

#[derive(Deserialize)]
struct AnswerFile {
    id: String,
    shown: String,
}

impl Control {
    pub(super) fn once(&mut self, root: &Path, stop: &CancelToken, notes: &Mutex<Vec<Note>>, feed: &Mutex<feed::FeedLog>) {
        let stop_file = stop_path(root);
        if stop_file.exists() {
            stop.cancel();
            let _ = std::fs::remove_file(&stop_file);
        }
        if let Ok(entries) = std::fs::read_dir(answers_dir(root)) {
            let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
            files.sort();
            for path in files {
                if let Some(a) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<AnswerFile>(&b).ok()) {
                    feed.lock().unwrap_or_else(PoisonError::into_inner).answered(&a.id, &a.shown);
                }
                let _ = std::fs::remove_file(&path);
            }
        }
        let dir = notes::notes_dir(&root.join("inbox"));
        // Read the folder without holding the notes, which the screen reads while drawing.
        let theirs: Vec<u64> = notes.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|n| n.id).filter(|id| notes::is_foreign(*id)).collect();
        // A follower's note whose file went before the builder read it was taken back. (The run
        // takes a note out of this list before it removes the note's files.)
        let gone: HashSet<u64> = theirs.into_iter().filter(|id| !notes::is_waiting(&dir, *id) && !notes::was_read(&dir, *id)).collect();
        let new: Vec<notes::Waiting> = notes::waiting(&dir).into_iter().filter(|w| notes::is_foreign(w.id) && !self.adopted.contains(&w.id)).collect();
        if gone.is_empty() && new.is_empty() {
            return;
        }
        let mut notes = notes.lock().unwrap_or_else(PoisonError::into_inner);
        notes.retain(|n| !gone.contains(&n.id));
        for w in new {
            self.adopted.insert(w.id);
            notes.push(Note { id: w.id, text: w.left.text, shown: w.left.shown, pictures: w.left.pictures, questions: w.questions });
        }
    }
}

/// What a follower keeps beside the parts every run has.
#[derive(Debug, Default)]
pub(super) struct Remote {
    /// Where the run serves its preview (its own address, from [`RunEvent::Preview`]).
    preview: Mutex<Option<String>>,
}

impl Remote {
    pub(super) fn preview_url(&self) -> Option<String> {
        self.preview.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// Reads the log from where it last stopped, a line at a time.
struct Reader {
    file: std::fs::File,
    offset: u64,
    partial: Vec<u8>,
    /// A line too long to keep is being skipped to its end.
    skipping: bool,
}

/// The log isn't the one this reader was reading.
struct Replaced;

impl Reader {
    fn open(root: &Path) -> std::io::Result<Reader> {
        Ok(Reader { file: std::fs::File::open(events_path(root))?, offset: 0, partial: Vec::new(), skipping: false })
    }

    /// The complete lines written since the last read.
    fn lines(&mut self) -> Result<Vec<Vec<u8>>, Replaced> {
        let Ok(len) = self.file.metadata().map(|m| m.len()) else { return Ok(Vec::new()) };
        if len < self.offset {
            return Err(Replaced);
        }
        let mut out = Vec::new();
        let mut want = len.saturating_sub(self.offset);
        while want > 0 {
            let mut bytes = Vec::new();
            let read = self.file.seek(SeekFrom::Start(self.offset)).and_then(|_| (&mut self.file).take(want.min(MOST_READ)).read_to_end(&mut bytes));
            match read {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    self.offset = self.offset.saturating_add(n as u64);
                    want = want.saturating_sub(n as u64);
                }
            }
            for part in bytes.split_inclusive(|b| *b == b'\n') {
                let complete = part.last() == Some(&b'\n');
                if self.skipping {
                    self.skipping = !complete;
                    continue;
                }
                self.partial.extend_from_slice(part);
                if complete {
                    out.push(std::mem::take(&mut self.partial));
                } else if self.partial.len() > LONGEST_LINE {
                    log::warn!("A line of a build's event log is too long to follow; it's skipped.");
                    self.partial.clear();
                    self.skipping = true;
                }
            }
        }
        Ok(out)
    }
}

/// The run that wrote the log at `root` now, from its first line.
fn log_run(root: &Path) -> Option<String> {
    let mut first = Vec::new();
    std::fs::File::open(events_path(root)).ok()?.take(4096).read_to_end(&mut first).ok()?;
    let line = first.split(|b| *b == b'\n').next()?;
    match serde_json::from_slice::<LineIn>(line).ok()? {
        LineIn::Header(h) => Some(h.run),
        _ => None,
    }
}

/// The start of a run's log: its header, the feed it began with, and the lines after them.
type LogStart = (Reader, Header, Vec<FeedItem>, Vec<Vec<u8>>);

/// The log's first two lines, once both are there. A log from a newer version is refused.
fn read_start(root: &Path) -> Result<Option<LogStart>, ToolError> {
    let Ok(mut reader) = Reader::open(root) else { return Ok(None) };
    let Ok(mut lines) = reader.lines() else { return Ok(None) };
    if lines.len() < 2 {
        return Ok(None);
    }
    let rest = lines.split_off(2);
    let (Some(first), Some(second)) = (lines.first(), lines.get(1)) else { return Ok(None) };
    let version = serde_json::from_slice::<Value>(first).ok().and_then(|v| v.get("header")?.get("version")?.as_u64());
    if version.is_some_and(|v| v > u64::from(EVENTS_VERSION)) {
        return Err(ToolError::new(ErrorCode::NotSupported, "This build was started by a newer Craft Anything, so this one can't follow it.")
            .with_remediation("Update Craft Anything to follow it here, or watch it where it runs."));
    }
    let (Ok(LineIn::Header(header)), Ok(LineIn::Base { items })) = (serde_json::from_slice::<LineIn>(first), serde_json::from_slice::<LineIn>(second)) else {
        return Ok(None);
    };
    Ok(Some((reader, header, items, rest)))
}

fn not_building() -> ToolError {
    ToolError::new(ErrorCode::NotFound, NOT_BUILDING).with_remediation("Start a build, or open the project to see how its last one ended.")
}

/// A follower's copy of a run: it reads the log and keeps the parts every run has up to date.
struct Tail {
    root: PathBuf,
    run: String,
    reader: Reader,
    feed: Arc<Mutex<feed::FeedLog>>,
    activity: Arc<Activity>,
    tx: Sender<RunEvent>,
    running: Arc<AtomicBool>,
    remote: Arc<Remote>,
    notes: Arc<Mutex<Vec<Note>>>,
    wake: Wake,
    finished: bool,
    /// When the lock was first seen free without the run saying it finished.
    gone_since: Option<Instant>,
    told_bad_line: bool,
}

impl Tail {
    /// Take in complete lines; true when any changed something.
    fn take(&mut self, lines: Vec<Vec<u8>>) -> bool {
        let mut changed = false;
        for line in lines {
            if self.finished {
                break;
            }
            match serde_json::from_slice::<LineIn>(&line) {
                Ok(LineIn::Event { at_ms, event }) => {
                    if let RunEvent::Preview(url) = &event {
                        *self.remote.preview.lock().unwrap_or_else(PoisonError::into_inner) = Some(url.clone());
                    }
                    let done = matches!(event, RunEvent::Finished { .. });
                    self.send(event, Duration::from_millis(at_ms));
                    if done {
                        self.end();
                    }
                    changed = true;
                }
                Ok(LineIn::Answered { id, shown }) => {
                    self.feed.lock().unwrap_or_else(PoisonError::into_inner).answered(&id, &shown);
                    changed = true;
                }
                Ok(LineIn::Header(_) | LineIn::Base { .. }) => {}
                Err(e) => {
                    if !std::mem::replace(&mut self.told_bad_line, true) {
                        log::warn!("Skipped a line of {} that couldn't be read: {e}", events_path(&self.root).display());
                    }
                }
            }
        }
        changed
    }

    /// Recorded and sent under one lock, as the run does, so `feed_now` sees each event once.
    fn send(&self, e: RunEvent, at: Duration) {
        let mut feed = self.feed.lock().unwrap_or_else(PoisonError::into_inner);
        feed.record_at(&e, at);
        let _ = self.tx.send(e);
    }

    fn end(&mut self) {
        self.finished = true;
        self.running.store(false, Ordering::SeqCst);
    }

    /// The run is gone without saying it finished: say so, once.
    fn lost(&mut self) {
        if !self.finished {
            let at = self.activity.elapsed();
            self.send(RunEvent::Finished { ok: false, message: ENDED_ELSEWHERE.into(), paused: false }, at);
            self.end();
        }
    }

    fn ours(&self) -> bool {
        log_run(&self.root).as_deref() == Some(self.run.as_str())
    }

    /// One look at the log, the lock and the notes. False once it's done following.
    fn step(&mut self) -> bool {
        let mut changed = false;
        if !self.ours() {
            self.lost();
            self.wake.call();
            return false;
        }
        match self.reader.lines() {
            // A new run's lines aren't this run's: read again only while the log is still ours.
            Ok(lines) if self.ours() => changed |= self.take(lines),
            Ok(_) | Err(Replaced) => self.lost(),
        }
        if !self.finished {
            if build_lock::held(&self.root) {
                self.gone_since = None;
            } else if self.gone_since.get_or_insert_with(Instant::now).elapsed() >= GONE_GRACE {
                // Its last words may have come in the meantime.
                if let Ok(lines) = self.reader.lines()
                    && self.ours()
                {
                    self.take(lines);
                }
                self.lost();
            }
            changed |= self.refresh_notes();
        }
        if changed || self.finished {
            self.wake.call();
        }
        !self.finished
    }

    /// The notes waiting in the project's folder, whoever added them.
    fn refresh_notes(&self) -> bool {
        let waiting: Vec<Note> = notes::waiting(&notes::notes_dir(&self.root.join("inbox")))
            .into_iter()
            .map(|w| Note { id: w.id, text: w.left.text, shown: w.left.shown, pictures: w.left.pictures, questions: w.questions })
            .collect();
        let mut notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
        if *notes == waiting {
            return false;
        }
        *notes = waiting;
        true
    }
}

impl RebuildRun {
    /// Follow the build another process runs on `ws` (another window, the command line), with the
    /// same calls as a run started here: its events arrive by [`poll`](Self::poll), and
    /// [`stop`](Self::stop), notes and answers reach it through the project's files. Dropping the
    /// follower leaves the build running. It reads the disk (and waits a moment for a build that
    /// has just started), so call it on a worker.
    ///
    /// Fails when nothing builds the project, or when the build was started by a newer version.
    pub fn attach(ws: &Workspace, wake: Wake) -> Result<RebuildRun, ToolError> {
        let root = ws.root.clone();
        if !build_lock::held(&root) {
            return Err(not_building());
        }
        // A run takes the lock first and writes its first lines a moment later.
        let deadline = Instant::now() + HEADER_WAIT;
        let (reader, header, base, rest) = loop {
            if let Some(start) = read_start(&root)? {
                break start;
            }
            if !build_lock::held(&root) {
                return Err(not_building());
            }
            if Instant::now() >= deadline {
                return Err(ToolError::new(ErrorCode::NotSupported, "This project is building, but its build can't be followed from here.")
                    .with_remediation("Watch it where it runs; a build started by an older Craft Anything can't be followed."));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let began = Instant::now().checked_sub(Duration::from_millis(unix_ms().saturating_sub(header.started_ms))).unwrap_or_else(Instant::now);
        let activity = Arc::new(Activity::began_at(vec![ws.app_dir(), root.clone()], began));
        let feed = Arc::new(Mutex::new(feed::FeedLog::mirror(&root, base, activity.clone())));
        let (tx, rx) = std::sync::mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let remote = Arc::new(Remote::default());
        let notes: Arc<Mutex<Vec<Note>>> = Arc::default();
        let mut tail = Tail {
            root: root.clone(),
            run: header.run,
            reader,
            feed: feed.clone(),
            activity: activity.clone(),
            tx,
            running: running.clone(),
            remote: remote.clone(),
            notes: notes.clone(),
            wake,
            finished: false,
            gone_since: None,
            told_bad_line: false,
        };
        // Everything so far, before this returns, so the feed is whole from the start.
        tail.take(rest);
        tail.refresh_notes();
        let stop = CancelToken::default();
        if !tail.finished {
            let quit = stop.clone();
            std::thread::Builder::new()
                .name("rebuild-follow".into())
                .spawn(move || {
                    while !quit.is_cancelled() {
                        std::thread::sleep(FOLLOW_EVERY);
                        if quit.is_cancelled() || !tail.step() {
                            return;
                        }
                    }
                })
                .map_err(|e| ToolError::internal(e.to_string()))?;
        }
        Ok(RebuildRun {
            rx,
            // Cancelled when the follower is dropped: it ends the reading thread, not the build.
            stop,
            inbox: Inbox::new(ws.inbox_dir()),
            running,
            preview: Arc::default(),
            notes,
            clean_room: header.clean_room,
            feed,
            root,
            started: began,
            activity,
            remote: Some(remote),
        })
    }
}

/// A follower asks the run to pause.
pub(super) fn ask_to_stop(root: &Path) {
    if let Err(e) = craft_anything_platform::fs::write_atomic(&stop_path(root), b"") {
        log::warn!("Couldn't ask the build to pause: {}", e.message);
    }
}

/// A follower answers a card: the answer goes to the inbox (the builder is waiting on it), and how
/// it reads goes to the run, which records it in the feed. A run that is gone by then can't, so
/// it goes straight into the saved feed, as with no run.
pub(super) fn answer(run: &RebuildRun, id: &str, answer: &Answer, shown: &str) -> Result<(), ToolError> {
    let request = run
        .feed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .items()
        .iter()
        .find_map(|i| match i {
            FeedItem::Ask { request, .. } if request.id == id => Some(request.clone()),
            _ => None,
        })
        .or_else(|| run.inbox.pending().into_iter().find(|r| r.id == id));
    run.inbox.answer(id, answer)?;
    let told = build_lock::held(&run.root) && {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("{}-{}-{}.json", unix_ms(), std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let bytes = serde_json::to_vec(&json!({"id": id, "shown": shown})).map_err(|e| ToolError::internal(e.to_string()))?;
        craft_anything_platform::fs::write_atomic(&answers_dir(&run.root).join(name), &bytes).is_ok()
    };
    if !told && let Some(request) = request {
        feed::record_answer(&Workspace::open(&run.root)?, &request, shown)?;
    }
    run.feed.lock().unwrap_or_else(PoisonError::into_inner).answered(id, shown);
    Ok(())
}

/// A follower takes the notes still waiting (the run ended first): each one whose file it can
/// take back, so no other run gets it too.
pub(super) fn take_notes(run: &RebuildRun) -> Vec<Note> {
    let dir = notes::notes_dir(run.inbox.dir());
    let waiting = std::mem::take(&mut *run.notes.lock().unwrap_or_else(PoisonError::into_inner));
    waiting.into_iter().filter(|n| notes::take_back(&dir, n.id)).collect()
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
