//! The shell lines the launcher and the power buttons send
//! (`_compose_remote_command` and `_compose_power_command` in `server.py`).

/// `shlex.quote`: POSIX quoting, which is what the `sh` on the far end reads.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `cd -- '<dir>' && <command>`; with `detach` the program is started under
/// `nohup` in the background and its pid echoed.
pub fn compose_remote_command(directory: &str, command: &str, detach: bool) -> String {
    let body = if detach {
        format!("{{ nohup {command} >/dev/null 2>&1 & }}; echo $!")
    } else {
        command.to_string()
    };
    if directory.is_empty() {
        body
    } else {
        format!("cd -- {} && {body}", shell_quote(directory))
    }
}

pub fn power_flag(action: &str) -> Option<&'static str> {
    match action {
        "reboot" => Some("-r"),
        "shutdown" => Some("-h"),
        _ => None,
    }
}

/// The line that reboots or powers off the far end: root runs it directly,
/// anyone else goes through `sudo`, which reads the saved password from
/// standard input or, without one, refuses to ask.
pub fn compose_power_command(action: &str, has_password: bool) -> String {
    let run = format!("shutdown {} now", power_flag(action).unwrap_or("-r"));
    let sudo = if has_password { "sudo -S -p ''" } else { "sudo -n" };
    format!("if [ \"$(id -u)\" -eq 0 ]; then {run}; else {sudo} {run}; fi")
}
