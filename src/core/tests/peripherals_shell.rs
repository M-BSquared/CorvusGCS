//! The shell on this computer, the background runner and the terminal
//! registry: held to Python where they compute, and run for real on a pty.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use corvus_core::peripherals::shell::local::{self, LocalRunner, LocalSession};
use corvus_core::peripherals::shell::session::{subscribe, Terminal, Terminals};
use serde_json::{json, Value};

fn load() -> Value {
    let path = format!("{}/tests/data/peripherals_shell_cases.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_shell_environment_and_command_lines_are_the_pythons() {
    let d = load();
    for c in d["shell_env"].as_array().unwrap() {
        let env: BTreeMap<String, String> = c["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let got = local::shell_env(&env, c["platform"].as_str().unwrap());
        assert_eq!(json!(got), c["result"], "{c}");
    }
    for c in d["shell_command"].as_array().unwrap() {
        let got = local::shell_command(
            c["text"].as_str().unwrap(),
            c["shell"].as_str().unwrap(),
            c["platform"].as_str().unwrap(),
        );
        let want: Vec<String> = match &c["result"] {
            Value::String(s) => vec![s.clone()],
            v => v
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect(),
        };
        assert_eq!(got, want, "{c}");
    }
    assert_eq!(local::MAX_COMMAND as u64, d["limits"]["max_command"].as_u64().unwrap());
    assert_eq!(local::EARLY_EXIT_S, d["limits"]["early_exit"].as_f64().unwrap());
    assert_eq!(local::OUTPUT_TAIL as u64, d["limits"]["tail"].as_u64().unwrap());
    assert_eq!(local::TERM_TIMEOUT_S, d["limits"]["term"].as_f64().unwrap());
}

#[test]
fn a_folder_is_read_from_home_and_a_missing_one_is_named() {
    let (path, problem) = local::clean_directory("");
    assert_eq!(problem, "");
    assert!(std::path::Path::new(&path).is_dir());
    let (_, problem) = local::clean_directory("definitely/not/a/folder");
    assert_eq!(problem, "There is no folder definitely/not/a/folder on this computer.");
    let (path, _) = local::clean_directory("/");
    if cfg!(unix) {
        assert_eq!(path, "/");
    } else {
        assert!(std::path::Path::new(&path).is_dir(), "{path}");
    }
}

#[cfg(unix)]
mod unix {
    use super::*;

    fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    fn pid_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn output_of(
        t: &Arc<dyn Terminal>,
        feed: &Arc<corvus_core::peripherals::shell::session::Feed>,
        upto: &str,
    ) -> String {
        let mut text = String::new();
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end && !text.contains(upto) {
            if let Some(c) = feed.get(Duration::from_millis(100)) {
                text.push_str(&c);
            }
        }
        let _ = t;
        text
    }

    fn shell(dir: &str) -> Arc<dyn Terminal> {
        named("t", dir)
    }

    fn named(name: &str, dir: &str) -> Arc<dyn Terminal> {
        LocalSession::new(name, dir, "/bin/sh") as Arc<dyn Terminal>
    }

    /// The number printed after `tag` (the echo of the typed line has `$!` there).
    fn pid_after(text: &str, tag: &str) -> Option<i32> {
        text.match_indices(tag).find_map(|(at, _)| {
            let rest = &text[at + tag.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    }

    fn read_pid(t: &Arc<dyn Terminal>, feed: &Arc<corvus_core::peripherals::shell::session::Feed>, tag: &str) -> i32 {
        let mut text = String::new();
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if let Some(c) = feed.get(Duration::from_millis(100)) {
                text.push_str(&c);
            }
            if let Some(p) = pid_after(&text, tag) {
                let _ = t;
                return p;
            }
        }
        panic!("no pid after {tag} in {text}");
    }

    #[test]
    fn a_shell_is_a_real_terminal_with_a_size_and_an_end() {
        let s = shell("");
        assert!(s.connect());
        let (feed, sub) = subscribe(&s);
        assert!(s.connected());
        assert_eq!((s.host().as_str(), s.port()), ("this computer", 0));
        let first = output_of(&s, &feed, "(sh)");
        assert!(first.contains("Shell on this computer (sh)"), "{first}");
        s.send("echo sum-$((1+2))\n");
        assert!(output_of(&s, &feed, "sum-3").contains("sum-3"));
        assert!(s.resize(100, 30));
        s.send("stty size\n");
        assert!(output_of(&s, &feed, "30 100").contains("30 100"));
        s.send("[ -t 0 ] && echo is-a-tty\n");
        assert!(output_of(&s, &feed, "is-a-tty").contains("is-a-tty"));
        s.send("exit\n");
        let end = output_of(&s, &feed, "[Shell ended]");
        assert!(end.contains("[Shell ended]"), "{end}");
        wait_for("disconnected", || !s.connected());
        s.output().remove_sub(sub);
        s.disconnect();
        assert!(!s.resize(10, 10), "a closed shell has no size to set");
    }

    #[test]
    fn a_late_subscriber_gets_the_replay_first() {
        let s = shell("");
        assert!(s.connect());
        s.send("echo marker-one\n");
        std::thread::sleep(Duration::from_millis(600));
        let (feed, _sub) = subscribe(&s);
        let text = output_of(&s, &feed, "marker-one");
        assert!(text.contains("marker-one"));
        assert!(
            text.contains("Shell on this computer"),
            "the banner is replayed: {text}"
        );
        s.disconnect();
    }

    #[test]
    fn a_shell_in_a_missing_folder_says_why_and_a_shell_starts_where_asked() {
        let s = shell("no/such/folder");
        assert!(!s.connect());
        assert_eq!(s.error(), "There is no folder no/such/folder on this computer.");
        let s = shell("/");
        assert!(s.connect());
        let (feed, _) = subscribe(&s);
        s.send("pwd\n");
        // Neither the banner nor the echoed command holds a slash, and a
        // prompt may sit in front of the answer, so the answer is the first one.
        assert!(output_of(&s, &feed, "/\r").contains("/\r"), "starts in /");
        s.disconnect();
    }

    #[test]
    fn closing_a_shell_ends_what_runs_in_it_even_a_job_that_ignores_the_hangup() {
        let s = shell("");
        assert!(s.connect());
        let (feed, _) = subscribe(&s);
        s.send("set +H\nsleep 300 &\necho bg-$!-end\n");
        let pid = read_pid(&s, &feed, "bg-");
        s.send("(trap '' HUP; exec sleep 301) &\necho stubborn-$!-end\n");
        let stubborn = read_pid(&s, &feed, "stubborn-");
        assert!(pid_alive(pid) && pid_alive(stubborn));
        let t0 = Instant::now();
        s.disconnect();
        assert!(!s.connected());
        wait_for("both jobs gone", || !pid_alive(pid) && !pid_alive(stubborn));
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    }

    #[test]
    fn the_registry_replaces_a_name_lists_in_order_and_remembers_why_a_connect_failed() {
        let t = Terminals::new();
        let first = t.open("a", &|| named("a", "") as Arc<dyn Terminal>);
        assert!(first);
        assert!(t.open("b", &|| named("b", "") as Arc<dyn Terminal>));
        let old = t.get("a").unwrap();
        assert!(t.open("a", &|| named("a", "") as Arc<dyn Terminal>));
        assert!(!old.connected(), "the old session under that name was closed");
        let names: Vec<String> = t
            .list()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a", "b"], "a replaced name keeps its place");
        assert_eq!(t.list()[0]["host"], "this computer");
        assert!(!t.open("c", &|| named("c", "/no/such") as Arc<dyn Terminal>));
        assert_eq!(t.connect_error("c"), "There is no folder /no/such on this computer.");
        assert!(t.get("c").is_none());
        assert!(t.send("a", "echo x\n"));
        assert!(!t.send("zz", "x"));
        assert!(t.resize("a", 90, 20));
        assert!(t.disconnect("a"));
        assert!(!t.disconnect("a"));
        t.shutdown();
        assert!(t.get("b").is_none());
        assert!(
            !t.open("d", &|| named("d", "") as Arc<dyn Terminal>),
            "a shut down registry refuses new sessions"
        );
    }

    #[test]
    fn a_background_program_reports_how_it_ended_or_that_it_runs() {
        let r = LocalRunner::with_early_exit(Duration::from_secs(20));
        assert_eq!(r.run("echo hi", "")["output"], "hi");
        let a = r.run("echo out; echo err 1>&2; exit 3", "");
        assert_eq!(
            (a["ok"].clone(), a["exited"].clone(), a["code"].clone()),
            (json!(false), json!(true), json!(3))
        );
        assert_eq!(a["output"], "out\nerr");
        assert_eq!(r.run("", "")["error"], "There is no command to run.");
        assert_eq!(r.run(&"x".repeat(9000), "")["error"], "That command is too long.");
        assert_eq!(r.run("echo a\0b", "")["error"], "That command is too long.");
        assert_eq!(
            r.run("true", "/no/such/folder")["error"],
            "There is no folder /no/such/folder on this computer."
        );
        let long = r.run(
            "i=0; while [ $i -lt 2000 ]; do echo line-$i-padding-padding-padding; i=$((i+1)); done",
            "",
        );
        assert!(
            long["output"].as_str().unwrap().len() <= 4096
                && long["output"]
                    .as_str()
                    .unwrap()
                    .ends_with("line-1999-padding-padding-padding")
        );
        assert_eq!(r.count(), 0);
    }

    #[test]
    fn a_running_program_is_corvus_own_child_and_is_stopped_with_it() {
        let r = LocalRunner::new();
        let t0 = Instant::now();
        let a = r.run("exec sleep 300", "");
        assert!(
            t0.elapsed() >= Duration::from_millis(900),
            "it waits a second to see whether it fails"
        );
        assert_eq!(a["ok"], true);
        let pid = a["pid"].as_i64().unwrap() as i32;
        assert_eq!(r.count(), 1);
        assert!(pid_alive(pid));
        r.shutdown();
        wait_for("the program gone", || !pid_alive(pid));
        assert_eq!(r.count(), 0);
        assert_eq!(r.run("true", "")["error"], "Corvus is shutting down.");
    }

    #[test]
    fn a_program_that_ignores_sigterm_is_killed() {
        let r = LocalRunner::new();
        let a = r.run("trap '' TERM; while :; do sleep 1; done", "");
        let pid = a["pid"].as_i64().unwrap() as i32;
        let t0 = Instant::now();
        r.shutdown();
        wait_for("the stubborn program gone", || !pid_alive(pid));
        assert!(t0.elapsed() < Duration::from_secs(10));
    }
}
