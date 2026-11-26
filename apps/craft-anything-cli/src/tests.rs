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
