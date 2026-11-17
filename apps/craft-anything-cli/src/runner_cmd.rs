//! `rebuild-run` (hidden): run one project's build in this process, with no terminal, until it
//! ends. The app starts it ([`craft_anything_ai::rebuild::runner::spawn`]) so a build outlives the
//! app (#1970). It never reads stdin or answers anything: questions and approvals wait in the
//! project's inbox for whoever is attached.
//!
//! Exit codes: 0 the build finished or was paused, 1 it stopped or failed, 3 it couldn't start (the
//! reason is in `inbox/run-error.json`).

use crate::{INTERRUPTED, make_engine};
use craft_anything_ai::AiHub;
use craft_anything_ai::rebuild::runner::{self, RunnerOptions};
use craft_anything_ai::rebuild::{KindRegistry, RebuildRun, Wake};
use craft_anything_engine::workspace::Workspace;
use craft_anything_evidence::ToolError;
use std::path::Path;
use std::sync::atomic::Ordering;

pub fn rebuild_run(project: &Path, options: &Path) -> u8 {
    // Before anything else, so no cleanup removes the copy this runs from.
    let _staged = runner::hold_staged_copy();
    let root = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    let inbox = root.join("inbox");
    let mut run = match start(&root, options) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("rebuild-run: {}", e.message);
            if let Some(r) = &e.remediation {
                eprintln!("  {r}");
            }
            runner::write_error(&inbox, &e);
            return 3;
        }
    };
    runner::mark_started(&inbox);
    let ok = runner::drive(&mut run, || INTERRUPTED.load(Ordering::SeqCst));
    runner::mark_ended(&inbox);
    if ok { 0 } else { 1 }
}

fn start(root: &Path, options: &Path) -> Result<RebuildRun, ToolError> {
    let read = RunnerOptions::read(options);
    // Read once: a runner started again by hand must be given its options again.
    let _ = std::fs::remove_file(options);
    let opts = read?;
    let engine = make_engine(None, Some(opts.deep))?;
    let hub = AiHub::new(engine);
    // Before the build starts, while no session lock is held (#1225).
    if let Err(e) = hub.ensure_session_id() {
        log::warn!("couldn't save the OpenCode Go session id: {e}");
    }
    let ws = Workspace::open(root)?;
    RebuildRun::start(&hub, ws, opts.into_run_options(KindRegistry::built_in(), Default::default(), Wake::default()))
}
