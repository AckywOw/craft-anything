//! Builds started in a runner process from the app's side ([`start_in_runner`]): the follower
//! comes back at once, shows the runner's run once it has begun, takes a pause asked for before
//! then, and ends with the reason when the runner can't start.
//!
//! The runner is the built `craft-anything-cli` (`cargo build -p craft-anything-cli` first). Its
//! builder is an OpenAI-compatible backend on a loopback server that accepts and never answers,
//! so a run that starts stays in its first request: no network, no coding agent, no key. Every
//! runner a test starts is ended before the test returns, and the test waits for its lock to go.

use super::*;
use crate::KeyStore;
use crate::rebuild::{BUILDING_ELSEWHERE, Elsewhere, attach, feed};
use craft_anything_engine::Engine;
use craft_anything_engine::workspace::{Status, TargetRef};
use craft_anything_platform::process::Process;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// The built CLI, beside this test program's folder.
fn cli() -> PathBuf {
    cli_beside_app().expect("these tests start the built CLI: run `cargo build -p craft-anything-cli` first")
}

/// A loopback server that takes connections and never answers, so a builder's first request waits.
struct Silent {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Silent {
    fn start() -> Silent {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !s.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((conn, _)) => held.push(conn),
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        Silent { port, stop, thread: Some(thread) }
    }
}

impl Drop for Silent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A project, a hub whose settings (in a config folder of its own, which the runner is given)
/// name an API builder on `port` (or, with `None`, an agent app only this process has), and a
/// staging folder. Dropped last: it ends a runner still holding the project.
struct Bench {
    dir: tempfile::TempDir,
    hub: AiHub,
    ws: Workspace,
    staging: PathBuf,
    runner: Option<u32>,
}

fn bench(port: Option<u16>) -> Bench {
    let dir = tempfile::Builder::new().prefix("starter").tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let ai = match port {
        // Its answer may take 2 s to start: a run that's asked to pause while it waits sees the
        // pause when that wait ends, instead of five minutes later.
        Some(port) => format!(
            "[[backends]]\nname = \"API\"\nkind = \"openai_compatible\"\nmodel = \"test-model\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\nenabled = true\ntimeout_secs = 2\n"
        ),
        None => "[[backends]]\nname = \"Agent\"\nkind = \"agent_cli\"\nmodel = \"claude-code\"\n".to_string(),
    };
    // Only an API builder is saved where the runner reads its settings; the agent app is this
    // process's alone, so the runner finds no coding agent.
    if port.is_some() {
        let saved = ai.lines().map(|l| if l.starts_with("[[") { "[[ai.backends]]".to_string() } else { l.to_string() }).collect::<Vec<_>>().join("\n");
        std::fs::write(config.join("config.toml"), saved).unwrap();
    }
    let mut s = craft_anything_engine::testing::session();
    s.settings.ai = toml::from_str(&ai).unwrap();
    let hub = AiHub::with_stores(Arc::new(Engine::new(s)), config.join("config.toml"), KeyStore::file_only(config.join("keys.toml")));
    let ws = Workspace::create(&dir.path().join("projects"), "Notes", "A small notes page", TargetRef::None).unwrap();
    let staging = dir.path().join("staging");
    Bench { dir, hub, ws, staging, runner: None }
}

impl Bench {
    fn options(&self, shown: &str) -> RunOptions {
        RunOptions { shown: Some(shown.into()), max_rounds: 0, clean_room: Some(true), ..RunOptions::default() }
    }

    /// Wait for the runner's build to begin, and remember the runner to end it.
    fn runner_began(&mut self) -> u32 {
        let inbox = self.ws.inbox_dir();
        wait(Duration::from_secs(30), "the runner's build begins", || build_lock::held(&self.ws.root) && read_started(&inbox).is_some());
        let pid = read_started(&inbox).map(|s| s.pid).unwrap_or_default();
        self.runner = Some(pid);
        pid
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        // Ended whatever happened to the test, and waited for: nothing is left running.
        if let Some(pid) = self.runner.take()
            && build_lock::held(&self.ws.root)
        {
            end_pid(pid);
            let deadline = Instant::now() + Duration::from_secs(15);
            while build_lock::held(&self.ws.root) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = &self.dir;
    }
}

/// End a runner we started, by its id, and what it started.
fn end_pid(pid: u32) {
    let p = if cfg!(windows) {
        Process::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"])
    } else {
        Process::new("kill").args(["-KILL", &pid.to_string()])
    };
    let _ = p.timeout(Duration::from_secs(20)).run();
}

fn wait(limit: Duration, what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll `run` until it finishes; every event it sent.
fn to_the_end(run: &mut RebuildRun, limit: Duration) -> Vec<RunEvent> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + limit;
    while run.is_running() {
        seen.extend(run.poll());
        assert!(Instant::now() < deadline, "the follower never finished: {seen:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    seen.extend(run.poll());
    seen
}

fn shows(items: &[feed::FeedItem], text: &str) -> bool {
    items.iter().any(|i| matches!(i, feed::FeedItem::User(u) if u.text() == text))
}

#[test]
fn a_build_in_a_runner_is_followed_at_once_and_paused_from_here() {
    let server = Silent::start();
    let mut b = bench(Some(server.port));
    let before = Instant::now();
    let mut run = start_in_runner(&b.hub, b.ws.clone(), b.options("Make it blue"), &cli(), &b.staging).unwrap();
    assert!(before.elapsed() < Duration::from_secs(2), "it returns without waiting for the runner: {:?}", before.elapsed());
    assert!(run.is_attached() && run.is_running());
    assert!(shows(&run.feed_now(), "Make it blue"), "the message shows while the runner starts");

    let pid = b.runner_began();
    assert_ne!(pid, std::process::id(), "the build runs in a process of its own");
    // The runner's own run: its first step arrives here.
    let mut seen = Vec::new();
    wait(Duration::from_secs(30), "the runner's first step", || {
        seen.extend(run.poll());
        seen.iter().any(|e| matches!(e, RunEvent::Step { .. }))
    });
    assert!(shows(&run.feed_now(), "Make it blue"), "the run's feed, with the message it began with");
    let log = std::fs::read_to_string(attach::events_path(&b.ws.root)).unwrap();
    assert!(log.contains("Make it blue"), "and it's the runner's log that says so: {log}");

    // A second build isn't started beside it.
    assert_eq!(start_in_runner(&b.hub, b.ws.clone(), b.options("again"), &cli(), &b.staging).err().map(|e| e.message).as_deref(), Some(BUILDING_ELSEWHERE));
    run.stop();
    let end = to_the_end(&mut run, Duration::from_secs(60));
    assert!(matches!(end.last(), Some(RunEvent::Finished { paused: true, .. })), "Pause reached the runner: {end:?}");
    wait(Duration::from_secs(15), "the runner lets the project go", || !build_lock::held(&b.ws.root));
    assert_eq!(Workspace::open(&b.ws.root).unwrap().manifest.status, Status::Stopped, "saved as paused, by the runner");
    drop(server);
}

/// The run clears control/ as it begins, so a pause asked for before then is written again.
#[test]
fn a_pause_asked_before_the_runner_began_still_reaches_it() {
    let server = Silent::start();
    let mut b = bench(Some(server.port));
    let mut run = start_in_runner(&b.hub, b.ws.clone(), b.options("Make it blue"), &cli(), &b.staging).unwrap();
    run.stop();
    assert!(!attach::stop_path(&b.ws.root).exists(), "not written before the run has begun");
    b.runner_began();
    let end = to_the_end(&mut run, Duration::from_secs(60));
    assert!(matches!(end.last(), Some(RunEvent::Finished { paused: true, .. })), "{end:?}");
    wait(Duration::from_secs(15), "the runner lets the project go", || !build_lock::held(&b.ws.root));
    drop(server);
}

#[test]
fn a_runner_that_cannot_start_ends_its_follower_with_why() {
    // The runner's own reason: its settings have no coding agent (this process's has one).
    let mut b = bench(None);
    b.ws.set_status(Status::Building, "Building the first version…").unwrap();
    let mut run = start_in_runner(&b.hub, b.ws.clone(), b.options("Make it blue"), &cli(), &b.staging).unwrap();
    let end = to_the_end(&mut run, Duration::from_secs(30));
    let finished: Vec<&RunEvent> = end.iter().filter(|e| matches!(e, RunEvent::Finished { .. })).collect();
    assert_eq!(finished.len(), 1, "said once: {end:?}");
    let Some(RunEvent::Finished { ok: false, message, paused: false }) = finished.first() else { panic!("{end:?}") };
    assert!(message.contains("coding agent"), "the runner's own words: {message}");
    assert!(!build_lock::held(&b.ws.root), "nothing holds the project");
    let saved = Workspace::open(&b.ws.root).unwrap().manifest;
    assert_eq!((saved.status, saved.note.as_str()), (Status::NeedsYou, message.as_str()), "not left saying it builds");
    assert!(matches!(run.feed_now().last(), Some(feed::FeedItem::Error { .. })), "{:?}", run.feed_now());

    // A program that isn't there.
    let mut b = bench(None);
    let missing = b.dir.path().join("nothing").join(craft_anything_platform::which::cli_file_name());
    let mut run = start_in_runner(&b.hub, b.ws.clone(), b.options("Make it blue"), &missing, &b.staging).unwrap();
    let end = to_the_end(&mut run, Duration::from_secs(30));
    assert!(matches!(end.as_slice(), [RunEvent::Finished { ok: false, .. }]), "{end:?}");
    assert!(!build_lock::held(&b.ws.root));
    assert_eq!(Workspace::open(&b.ws.root).unwrap().manifest.status, Status::NeedsYou);
    b.runner = None;
}

#[test]
fn what_start_refuses_at_once_is_refused_before_a_runner_starts() {
    let b = bench(None);
    // Another build holds the project.
    let owner = Elsewhere::start(&b.ws, "Make it red").unwrap();
    let err = start_in_runner(&b.hub, b.ws.clone(), b.options("Make it blue"), &cli(), &b.staging).err().unwrap();
    assert_eq!(err.message, BUILDING_ELSEWHERE);
    drop(owner);
    // No coding agent here.
    let hub = AiHub::with_stores(
        Arc::new(Engine::new(craft_anything_engine::testing::session())),
        b.dir.path().join("none.toml"),
        KeyStore::file_only(b.dir.path().join("keys.toml")),
    );
    assert!(start_in_runner(&hub, b.ws.clone(), b.options("Make it blue"), &cli(), &b.staging).is_err());
    assert!(!b.staging.exists(), "no runner was staged");
}
