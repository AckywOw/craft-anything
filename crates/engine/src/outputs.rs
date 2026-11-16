//! Finding what a project's build produced: the built site, the program, and what "Play" starts.

use std::path::{Path, PathBuf};

/// Where a built site lives in `app`: a build folder with index.html, else a plain static site.
/// An Electron app's bundled page is its window, not a site.
pub fn site_dir(app: &Path) -> Option<PathBuf> {
    if uses_electron(app) || crate::toolchains::found_in(app).and_then(|t| t.launch(app)).is_some() {
        return None;
    }
    for d in ["dist", "build", "out"] {
        let p = app.join(d);
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    (app.join("index.html").is_file() && !app.join("package.json").exists()).then(|| app.to_path_buf())
}

/// The program a Rust build in `app` made, so a finished game or app can be started. A workspace
/// often builds several (the app, its CLI, tools), so the project's own Cargo.toml files decide:
/// its `default-run`, then a program whose package uses a window or game library, then the root
/// package's; the newest build of that program wins (release or debug) in the first target
/// directory that has it. Only when the manifests name no programs is it the newest executable.
/// Build scripts and dependencies live in subfolders and are skipped, and so are stray copies
/// (`app.running-123.exe`) of a declared one.
pub fn built_program(app: &Path) -> Option<PathBuf> {
    built_program_in(app, std::env::var_os("CARGO_TARGET_DIR").as_deref())
}

/// [`built_program`] with the environment's `CARGO_TARGET_DIR` given, so a test doesn't depend on
/// the process environment.
fn built_program_in(app: &Path, env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if !app.join("Cargo.toml").is_file() {
        return None;
    }
    let targets = cargo_target_dirs(app, env);
    let built = |target: &Path| -> Vec<PathBuf> {
        ["release", "debug"]
            .iter()
            .filter_map(|d| std::fs::read_dir(target.join(d)).ok())
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_program(p))
            .collect()
    };
    let newest = |mut programs: Vec<PathBuf>, name: Option<&str>| -> Option<PathBuf> {
        if let Some(name) = name {
            programs.retain(|p| p.file_stem().and_then(|s| s.to_str()) == Some(name));
        }
        programs.into_iter().max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
    };
    let wanted = cargo_programs(app);
    if wanted.is_empty() {
        // A shared target directory holds other projects' programs: without a name, only the
        // project's own target directories may supply one.
        return targets.iter().filter(|target| inside_app(app, target)).find_map(|target| newest(built(target), None));
    }
    wanted.iter().find_map(|name| targets.iter().find_map(|target| newest(built(target), Some(name.as_str()))))
}

/// Whether `path` is inside `app`, after resolving both against the working directory.
fn inside_app(app: &Path, path: &Path) -> bool {
    crate::workspace::normalized(path).starts_with(crate::workspace::normalized(app))
}

/// The directories Cargo may have written the build to, best first: `CARGO_TARGET_DIR`, the
/// closest `.cargo/config.toml` (or `.cargo/config`) with `build.target-dir`, then `app/target`.
/// A project's setting is relative to the folder holding `.cargo`; the environment's is relative
/// to `app`, where Cargo runs. The default stays last, so a program built before the setting
/// moved is still found. Reading the settings starts no program, so Play and the checks may call
/// this on the UI thread.
fn cargo_target_dirs(app: &Path, env: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = env.filter(|dir| !dir.is_empty()) {
        let dir = resolved(app, PathBuf::from(dir));
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    // A relative app is relative to the working directory; walk from its absolute form so a
    // config file above that directory is still found.
    let root = crate::workspace::absolute(app);
    let mut here = Some(root.as_path());
    for _ in 0..32 {
        let Some(dir) = here else { break };
        if let Some(target) = cargo_config_target(&dir.join(".cargo")) {
            let target = resolved(dir, target);
            if !dirs.contains(&target) {
                dirs.push(target);
            }
            break;
        }
        here = dir.parent();
    }
    let default = crate::workspace::normalized(&app.join("target"));
    if !dirs.contains(&default) {
        dirs.push(default);
    }
    dirs
}

/// A relative Cargo target dir from `base`, with `.` and `..` resolved, or the absolute one as it is.
fn resolved(base: &Path, dir: PathBuf) -> PathBuf {
    let dir = if dir.is_absolute() { dir } else { base.join(dir) };
    crate::workspace::normalized(&dir)
}

/// `build.target-dir` from a folder's Cargo config, when it sets one. Cargo prefers the legacy
/// `config` when both exist; a `config` without the key (or one that can't be read or parsed)
/// doesn't fall through to `config.toml` in the same folder.
fn cargo_config_target(cargo_dir: &Path) -> Option<PathBuf> {
    let legacy = cargo_dir.join("config");
    let file = if legacy.is_file() { legacy } else { cargo_dir.join("config.toml") };
    let text = std::fs::read_to_string(file).ok()?;
    let config: toml::Table = text.parse().ok()?;
    config.get("build")?.get("target-dir")?.as_str().map(PathBuf::from)
}

/// Libraries that put a window on screen: a package using one makes the program to start.
const WINDOW_CRATES: &[&str] = &[
    "eframe",
    "egui",
    "winit",
    "macroquad",
    "bevy",
    "iced",
    "slint",
    "ggez",
    "fltk",
    "gtk",
    "gtk4",
    "tauri",
    "sdl2",
    "minifb",
    "pixels",
    "glium",
    "raylib",
    "notan",
    "fyrox",
    "nannou",
    "druid",
    "xilem",
    "dioxus",
    "floem",
    "piston_window",
    "tetra",
    "comfy",
    "miniquad",
    "softbuffer",
];

/// The programs `app`'s Cargo manifests declare, best first (see [`built_program`]).
fn cargo_programs(app: &Path) -> Vec<String> {
    let read = |dir: &Path| -> Option<toml::Table> { std::fs::read_to_string(dir.join("Cargo.toml")).ok()?.parse().ok() };
    let Some(root) = read(app) else { return Vec::new() };
    // The packages: the root's (if it has one) first, then the workspace members.
    let mut dirs = vec![app.to_path_buf()];
    let members = root.get("workspace").and_then(|w| w.get("members")).and_then(|m| m.as_array()).cloned().unwrap_or_default();
    for m in members.iter().filter_map(|m| m.as_str()) {
        match m.strip_suffix("/*") {
            Some(parent) => {
                let mut found: Vec<PathBuf> =
                    std::fs::read_dir(app.join(parent)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()).collect();
                found.sort();
                dirs.extend(found);
            }
            None if m != "." => dirs.push(app.join(m)),
            None => {}
        }
    }
    let mut default_run = Vec::new();
    let mut windowed = Vec::new();
    let mut root_bins = Vec::new();
    let mut other = Vec::new();
    for (i, dir) in dirs.iter().enumerate() {
        let Some(t) = read(dir) else { continue };
        let Some(pkg) = t.get("package").and_then(|p| p.as_table()) else { continue };
        let mut bins: Vec<String> = t
            .get("bin")
            .and_then(|b| b.as_array())
            .map(|bs| bs.iter().filter_map(|b| b.get("name")?.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if bins.is_empty()
            && dir.join("src").join("main.rs").is_file()
            && let Some(name) = pkg.get("name").and_then(|n| n.as_str())
        {
            bins.push(name.to_string());
        }
        if let Some(d) = pkg.get("default-run").and_then(|d| d.as_str()) {
            default_run.push(d.to_string());
        }
        // [dependencies] and per-platform [target.'cfg(..)'.dependencies].
        let per_target = t.get("target").and_then(|x| x.as_table()).into_iter().flat_map(|ts| ts.values().filter_map(|v| v.get("dependencies")));
        let uses_window =
            t.get("dependencies").into_iter().chain(per_target).filter_map(|d| d.as_table()).any(|d| d.keys().any(|k| WINDOW_CRATES.contains(&k.as_str())));
        if uses_window {
            windowed.extend(bins.iter().cloned());
        }
        if i == 0 { root_bins.extend(bins) } else { other.extend(bins) }
    }
    let mut out: Vec<String> = Vec::new();
    for name in default_run.into_iter().chain(windowed).chain(root_bins).chain(other) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The program a child should start when it runs with `app` as its working directory: an
/// app-relative file is made absolute so the working directory is not applied to it twice. A
/// bare command name stays for PATH lookup.
pub fn absolute_program(program: &Path) -> PathBuf {
    if program.is_relative() && program.components().count() > 1 { crate::workspace::absolute(program) } else { program.to_path_buf() }
}

/// What "Play" starts for a finished game or app: (program, arguments), run from app/. A Godot
/// project opens in the Godot it was built with, an Electron app in the Electron it installed.
/// Other toolchains may declare an interpreted/runner launch when no packaged program exists.
pub fn launch(app: &Path, godot: Option<&Path>) -> Option<(PathBuf, Vec<String>)> {
    if app.join("project.godot").is_file() {
        let root = std::path::absolute(app).ok()?;
        return godot_launch(&root, godot?);
    }
    if uses_electron(app) {
        let root = std::path::absolute(app).ok()?;
        return electron_program(&root).map(|e| (e, vec![root.display().to_string()]));
    }
    built_program(app).or_else(|| packaged_program(&app.join("Build"))).map(|p| (p, Vec::new())).or_else(|| crate::toolchains::found_in(app)?.launch(app))
}

// Godot decodes --path as UTF-8. A lossy conversion could open a different project.
fn godot_launch(root: &Path, godot: &Path) -> Option<(PathBuf, Vec<String>)> {
    Some((godot.to_path_buf(), vec!["--path".into(), root.to_str()?.into()]))
}

/// The packages `app`'s package.json depends on (dev dependencies too); none without a readable one.
pub fn npm_dependencies(app: &Path) -> Vec<String> {
    let pkg: Option<serde_json::Value> = std::fs::read(app.join("package.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    ["dependencies", "devDependencies"].iter().filter_map(|k| pkg.as_ref()?.get(k)?.as_object()).flat_map(|d| d.keys().cloned()).collect()
}

/// What `app`'s package.json runs for `script`; none without a readable one or that script.
pub fn npm_script(app: &Path, script: &str) -> Option<String> {
    let pkg: serde_json::Value = serde_json::from_slice(&std::fs::read(app.join("package.json")).ok()?).ok()?;
    pkg.get("scripts")?.get(script)?.as_str().map(str::to_string)
}

/// The crates `app`'s Cargo.toml depends on; none without a readable one.
pub fn cargo_dependencies(app: &Path) -> Vec<String> {
    let manifest: Option<toml::Table> = std::fs::read_to_string(app.join("Cargo.toml")).ok().and_then(|t| t.parse().ok());
    manifest.as_ref().and_then(|m| m.get("dependencies")?.as_table()).map(|d| d.keys().cloned().collect()).unwrap_or_default()
}

fn uses_electron(app: &Path) -> bool {
    npm_dependencies(app).iter().any(|d| d == "electron")
}

/// The Electron that `npm install` put in `app`'s node_modules.
fn electron_program(app: &Path) -> Option<PathBuf> {
    let dist = app.join("node_modules").join("electron").join("dist");
    let program = if cfg!(windows) {
        dist.join("electron.exe")
    } else if cfg!(target_os = "macos") {
        dist.join("Electron.app").join("Contents").join("MacOS").join("Electron")
    } else {
        dist.join("electron")
    };
    program.is_file().then_some(program)
}

/// The game a Unity or Unreal build packaged into `dir` (newest; crash reporters skipped).
pub fn packaged_program(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
            let helper = ["crashreport", "crashhandler", "crashpad", "-cmd", "unrealpak", "shadercompile"].iter().any(|s| name.contains(s));
            let at = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            if is_program(&p) && !helper && best.as_ref().is_none_or(|(b, _)| at > *b) {
                best = Some((at, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(all(test, unix))]
mod tests {
    use super::packaged_program;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn packaged_program_finds_executable_with_crash_in_name() {
        let build = tempfile::tempdir().unwrap();
        let reporter = build.path().join("CrashReportClient");
        let game = build.path().join("CrashSample");
        std::fs::write(&reporter, b"reporter").unwrap();
        std::fs::write(&game, b"game").unwrap();
        for path in [&reporter, &game] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::File::open(&reporter).unwrap().set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1))).unwrap();
        std::fs::File::open(&game).unwrap().set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(2))).unwrap();

        assert_eq!(packaged_program(build.path()), Some(game));
    }
}

/// Whether `p` is a program this computer can start.
#[cfg(windows)]
pub fn is_program(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

#[cfg(unix)]
pub fn is_program(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.extension().is_none() && p.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn godot_does_not_open_a_different_project_after_lossy_conversion() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let app = dir.path().join(std::ffi::OsStr::from_bytes(b"game-\xff"));
        let other = dir.path().join("game-\u{fffd}");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("project.godot"), "").unwrap();
        let godot = Path::new("godot");
        assert_eq!(launch(&other, Some(godot)), Some((godot.into(), vec!["--path".into(), other.to_str().unwrap().into()])));
        assert_eq!(godot_launch(&app, godot), None);
    }

    /// The child runs inside the project, so an app path passed to it must survive that cwd change.
    #[test]
    fn godot_and_electron_receive_the_full_project_path() {
        let cwd = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir_in(&cwd).unwrap();
        let relative = dir.path().strip_prefix(&cwd).unwrap();
        let root = dir.path().display().to_string();
        let marker = relative.join("project.godot");
        std::fs::write(&marker, "").unwrap();
        let godot = Path::new("godot");
        assert_eq!(launch(relative, Some(godot)), Some((godot.into(), vec!["--path".into(), root.clone()])));
        std::fs::remove_file(marker).unwrap();

        std::fs::write(relative.join("package.json"), r#"{"devDependencies":{"electron":"1"}}"#).unwrap();
        let dist = relative.join("node_modules/electron/dist");
        let executable = if cfg!(windows) {
            "electron.exe"
        } else if cfg!(target_os = "macos") {
            "Electron.app/Contents/MacOS/Electron"
        } else {
            "electron"
        };
        let binary = dist.join(executable);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        assert_eq!(launch(relative, None), Some((cwd.join(binary), vec![root])));
    }
}

#[cfg(test)]
mod target_dir_tests {
    use super::*;
    use std::ffi::OsStr;

    fn config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".cargo")).unwrap();
        std::fs::write(dir.join(".cargo/config.toml"), body).unwrap();
    }

    fn legacy_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".cargo")).unwrap();
        std::fs::write(dir.join(".cargo/config"), body).unwrap();
    }

    fn exe(name: &str) -> String {
        if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
    }

    fn make_program(path: &Path) {
        std::fs::write(path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn target_dirs_follow_the_environment_then_the_project_setting() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        assert_eq!(cargo_target_dirs(&app, None), vec![app.join("target")], "the default");
        let env = OsStr::new("env-out");
        assert_eq!(cargo_target_dirs(&app, Some(env)), vec![app.join("env-out"), app.join("target")], "the environment wins");
        let absolute = d.path().join("absolute");
        assert_eq!(cargo_target_dirs(&app, Some(absolute.as_os_str())), vec![absolute, app.join("target")], "an absolute environment path stays as it is");
        config(&app, "[build]\ntarget-dir = \"out\"\n");
        assert_eq!(cargo_target_dirs(&app, None), vec![app.join("out"), app.join("target")], "the project's setting");
        assert_eq!(
            cargo_target_dirs(&app, Some(env)),
            vec![app.join("env-out"), app.join("out"), app.join("target")],
            "the environment comes before the project's setting"
        );
    }

    #[test]
    fn target_dirs_read_an_ancestor_setting_when_the_closer_one_has_none() {
        let d = tempfile::tempdir().unwrap();
        let workspace = d.path().join("workspace");
        let app = workspace.join("app");
        std::fs::create_dir_all(&app).unwrap();
        config(&app, "[build]\nother = 1\n");
        config(&workspace, "[build]\ntarget-dir = \"out\"\n");
        assert_eq!(cargo_target_dirs(&app, None), vec![workspace.join("out"), app.join("target")], "the closest setting wins");
    }

    /// A shared target directory holds other projects' programs; without a name of its own a
    /// project must not take one of them.
    #[test]
    fn unnamed_programs_come_only_from_the_apps_own_target_directories() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("app");
        std::fs::create_dir_all(app.join("target/debug")).unwrap();
        std::fs::write(app.join("Cargo.toml"), "[package]").unwrap();
        let local = app.join("target/debug").join(exe("snake"));
        make_program(&local);
        let shared = d.path().join("shared");
        std::fs::create_dir_all(shared.join("debug")).unwrap();
        make_program(&shared.join("debug").join(exe("other")));
        assert_eq!(built_program_in(&app, Some(shared.as_os_str())), Some(local.clone()), "the app's own program");
        std::fs::remove_file(&local).unwrap();
        assert_eq!(built_program_in(&app, Some(shared.as_os_str())), None, "another project's program isn't ours");
    }

    /// A named program may live in the environment's shared target directory; it is the project's.
    #[test]
    fn named_programs_may_come_from_a_shared_target_directory() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("app");
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(app.join("Cargo.toml"), "[package]\nname = \"cargo-target-probe\"\n").unwrap();
        std::fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
        let shared = d.path().join("shared");
        std::fs::create_dir_all(shared.join("debug")).unwrap();
        let program = shared.join("debug").join(exe("cargo-target-probe"));
        make_program(&program);
        assert_eq!(built_program_in(&app, Some(shared.as_os_str())), Some(program));
    }

    /// A relative app is relative to the working directory; the walk must still reach a config
    /// above that directory.
    #[test]
    fn target_dirs_reach_a_config_above_a_relative_app() {
        let cwd = std::env::current_dir().unwrap();
        let d = tempfile::tempdir_in(&cwd).unwrap();
        let app = d.path().strip_prefix(&cwd).unwrap().join("app");
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(app.join("Cargo.toml"), "[package]\nname = \"cargo-target-probe\"\n").unwrap();
        std::fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
        config(d.path(), "[build]\ntarget-dir = \"out\"\n");
        let debug = d.path().join("out/debug");
        std::fs::create_dir_all(&debug).unwrap();
        let program = debug.join(exe("cargo-target-probe"));
        make_program(&program);
        assert_eq!(cargo_target_dirs(&app, None).first(), Some(&d.path().join("out")), "the config above the relative app is found");
        assert_eq!(built_program_in(&app, None), Some(program), "and its program is found");
    }

    /// Cargo prefers the legacy `config` when both files exist.
    #[test]
    fn the_legacy_config_wins_when_both_exist() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        legacy_config(&app, "[build]\ntarget-dir = \"legacy\"\n");
        config(&app, "[build]\ntarget-dir = \"toml\"\n");
        assert_eq!(cargo_target_dirs(&app, None), vec![app.join("legacy"), app.join("target")]);
    }

    /// A legacy `config` that has no key, or can't be parsed, doesn't fall through to
    /// `config.toml` in the same folder; the walk goes on to the parent instead.
    #[test]
    fn a_legacy_config_without_the_key_does_not_fall_through_to_config_toml() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        legacy_config(&app, "[build]\nother = 1\n");
        config(&app, "[build]\ntarget-dir = \"toml\"\n");
        assert_eq!(cargo_target_dirs(&app, None), vec![app.join("target")], "config.toml in the same folder isn't used");
        legacy_config(&app, "[build\n");
        assert_eq!(cargo_target_dirs(&app, None), vec![app.join("target")], "an unreadable legacy config doesn't fall through either");
    }
}
