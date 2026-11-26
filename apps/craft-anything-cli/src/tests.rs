//! `run` called in-process: what each command line prints, on which stream, and the exit code.

use super::run;

struct Ran {
    code: i32,
    out: String,
    err: String,
}

fn invoke(args: &[&str]) -> Ran {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(std::iter::once("craft-anything-cli").chain(args.iter().copied()), &mut out, &mut err);
    Ran { code, out: String::from_utf8_lossy(&out).into_owned(), err: String::from_utf8_lossy(&err).into_owned() }
}

#[test]
fn help_and_version_go_to_stdout_and_succeed_everywhere() {
    for args in [&["--help"][..], &["-h"], &["help", "setup"], &["setup", "--help"], &["ai", "model", "--help"], &["rebuild", "-h"]] {
        let r = invoke(args);
        assert_eq!((r.code, r.err.as_str()), (0, ""), "{args:?}");
        assert!(r.out.contains("Usage"), "{args:?}: {}", r.out);
    }
    // `--version` and `-V` work on the top level and on every built-in subcommand.
    for args in [
        &["--version"][..],
        &["-V"],
        &["setup", "--version"],
        &["doctor", "-V"],
        &["ai", "-V"],
        &["ai", "list", "--version"],
        &["rebuild", "-V"],
        &["chat", "--version"],
    ] {
        let r = invoke(args);
        assert_eq!((r.code, r.err.as_str()), (0, ""), "{args:?}");
        assert!(r.out.contains(env!("CARGO_PKG_VERSION")), "{args:?}: {}", r.out);
    }
}

#[test]
fn usage_errors_exit_2_with_the_message_on_stderr_and_nothing_on_stdout() {
    for args in [
        &["--bogus-flag", "commands"][..],
        &["setup", "--bogus"],
        &["doctor", "--client"],
        &["report", "--html", "--html"],
        &["--deep", "--clean-room", "commands"],
        &["ai", "add"],
        &[],
    ] {
        let r = invoke(args);
        assert_eq!(r.code, 2, "{args:?}: {}", r.err);
        assert_eq!(r.out, "", "{args:?}");
        assert!(r.err.contains("error:") || r.err.contains("Usage"), "{args:?}: {}", r.err);
    }
}

#[test]
fn a_documented_repeatable_flag_is_accepted_twice() {
    // `--client` is repeatable: a repeat is not a usage error (the command itself may still fail).
    let r = invoke(&["doctor", "--help"]);
    assert!(r.out.contains("repeatable"), "{}", r.out);
}

#[test]
fn a_bad_log_level_is_refused_before_any_work() {
    let r = invoke(&["--log", "loud", "commands"]);
    assert_eq!((r.code, r.out.as_str()), (2, ""));
    assert!(r.err.contains("loud"), "{}", r.err);
}

#[test]
fn the_command_is_what_the_completions_and_man_pages_are_made_of() {
    let command = super::command();
    let names: Vec<_> = command.get_subcommands().map(|s| s.get_name().to_string()).collect();
    assert!(names.iter().any(|n| n == "setup") && names.iter().any(|n| n == "mcp"), "{names:?}");
    // The log variable is declared on its flag, so `--help` lists it.
    assert!(command.get_arguments().any(|a| a.get_id() == "log" && a.get_env().is_some()));
}

// The commands below start the engine (settings are only read, nothing is written) and print what
// they found on the run's own streams.

#[test]
fn commands_list_every_tool_as_json_or_text() {
    let r = invoke(&["commands", "--format", "json"]);
    assert_eq!((r.code, r.err.as_str()), (0, ""));
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert!(v.as_array().is_some_and(|a| a.iter().any(|c| c["command"] == "core-session-show")));
    let t = invoke(&["commands", "--format", "text"]);
    assert_eq!(t.code, 0);
    assert!(t.out.contains("core-session-show"), "{}", t.out);
}

#[test]
fn capabilities_say_which_version_and_how_many_tools() {
    let r = invoke(&["--compact", "capabilities"]);
    assert_eq!((r.code, r.err.as_str()), (0, ""));
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["tools"].as_u64().unwrap_or(0) > 0);
    assert_eq!(r.out.lines().count(), 1, "--compact is one line");
    assert!(invoke(&["capabilities"]).out.lines().count() > 1, "indented by default");
}

#[test]
fn a_tool_prints_its_result_and_its_help() {
    let r = invoke(&["core-session-show"]);
    assert_eq!((r.code, r.err.as_str()), (0, ""), "{}", r.out);
    serde_json::from_str::<serde_json::Value>(&r.out).unwrap();
    let h = invoke(&["core-session-show", "--help"]);
    assert_eq!(h.code, 0);
    assert!(h.out.contains("Usage: craft-anything-cli core-session-show"), "{}", h.out);
}

#[test]
fn an_unknown_tool_or_a_bad_tool_option_is_a_usage_error() {
    for args in [
        &["no-such-tool"][..],
        &["core-session-show", "--bogus"],
        &["core-session-show", "--deep", "--clean-room"],
        &["core-session-show", "--output-format", "xml"],
    ] {
        let r = invoke(args);
        assert_eq!((r.code, r.out.as_str()), (2, ""), "{args:?}: {}", r.err);
        assert!(r.err.starts_with("error: "), "{args:?}: {}", r.err);
    }
}

#[test]
fn a_command_that_fails_exits_1_with_the_error_on_both_streams() {
    let gone = std::env::temp_dir().join("craft-anything-cli-no-such-file.bin");
    let r = invoke(&["triage", gone.to_str().unwrap()]);
    assert_eq!(r.code, 1, "{}", r.err);
    assert!(r.out.contains("\"error\""), "{}", r.out);
    assert!(r.err.starts_with("error: "), "{}", r.err);
}
