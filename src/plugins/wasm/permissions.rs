//! What a plugin may do beyond drawing, reading telemetry and keeping its own
//! settings. It names what it needs in its manifest, the operator reads that
//! where the plugin is installed, and **the program refuses everything else**:
//! a route, a push topic, a connection or a file dialog that no permission it
//! has covers is answered with an error. Nothing is allowed by default, and
//! nothing a plugin sends can widen what it has.
//!
//! A permission may gain routes in a later version and never loses one: a
//! published plugin has to go on working in every later Corvus GCS.

/// A permission and what the operator reads about it.
pub struct Permission {
    pub name: &'static str,
    pub says: &'static str,
    /// Routes it covers: `(method, path)`. "*" is any method; a path that ends
    /// in "/" covers everything under it.
    routes: &'static [(&'static str, &'static str)],
    /// Push topics of the backend it may follow.
    topics: &'static [&'static str],
}

pub const FETCH: &str = "network";
pub const FILES: &str = "files";
pub const AI_TOOLS: &str = "ai.tools";
pub const ASSISTANT: &str = "assistant";
pub const SSH: &str = "ssh";

pub const ALL: &[Permission] = &[
    Permission {
        name: "vehicle.info",
        says: "Read what the autopilot supports and its Remote ID",
        routes: &[("GET", "/api/mavlink/capabilities"), ("GET", "/api/remoteid")],
        topics: &[],
    },
    Permission {
        name: "telemetry.rates",
        says: "Ask the vehicle for faster telemetry",
        routes: &[("POST", "/api/vibration/stream")],
        topics: &[],
    },
    Permission {
        name: "settings.read",
        says: "Read the settings of Corvus GCS",
        routes: &[("GET", "/api/config")],
        topics: &[],
    },
    Permission {
        name: "shell",
        says: "Run commands on the autopilot over the MAVLink shell",
        routes: &[("*", "/api/mavlink/shell/"), ("POST", "/api/console/save")],
        topics: &["shell"],
    },
    Permission {
        name: SSH,
        says: "Run commands on other computers over your saved SSH connections, and change those",
        routes: &[("*", "/api/ssh/")],
        topics: &["ssh"],
    },
    Permission {
        name: "local",
        says: "Run programs on this computer",
        routes: &[("*", "/api/local/")],
        topics: &[],
    },
    Permission {
        name: "tak",
        says: "Use TAK: read contacts, send markers and chat, and change the connections",
        routes: &[("*", "/api/tak"), ("*", "/api/tak/")],
        topics: &["tak"],
    },
    Permission {
        name: ASSISTANT,
        says: "Use the AI tools of Corvus GCS, within what Settings allows them, and change what that is",
        routes: &[("GET", "/api/mcp"), ("POST", "/api/mcp/settings")],
        topics: &[],
    },
    Permission {
        name: FETCH,
        says: "Connect to other servers on the network and the internet",
        routes: &[],
        topics: &[],
    },
    Permission {
        name: FILES,
        says: "Open files that you pick",
        routes: &[],
        topics: &[],
    },
    Permission {
        name: AI_TOOLS,
        says: "Offer tools to an AI client",
        routes: &[],
        topics: &[],
    },
];

pub fn of(name: &str) -> Option<&'static Permission> {
    ALL.iter().find(|p| p.name == name)
}

/// What the operator reads about permission `name`, in the language in force.
pub fn says(name: &str) -> String {
    match of(name) {
        Some(p) => corvus_api::i18n::tr(p.says).into_owned(),
        // One a later version knows: here it opens nothing.
        None => corvus_api::i18n::tr_fmt("{name} (unknown to this version, so not allowed)", &[("name", name)]),
    }
}

/// What a plugin was granted: the permissions its manifest names.
#[derive(Debug, Clone, Default)]
pub struct Granted(Vec<String>);

/// Whether `path` is one this program's router reads the way it looks: no
/// escapes, no `..`, nothing that could end the request line.
fn is_plain_request(path: &str) -> bool {
    if !path.is_ascii() || path.bytes().any(|b| !b.is_ascii_graphic()) {
        return false;
    }
    let route = path.split('?').next().unwrap_or("");
    route.starts_with("/api/")
        && !route.contains("..")
        && !route.contains("//")
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
}

fn covers(pattern: &str, route: &str) -> bool {
    match pattern.strip_suffix('/') {
        Some(_) => route.starts_with(pattern) && route.len() > pattern.len(),
        None => route == pattern,
    }
}

impl Granted {
    pub fn new(names: &[String]) -> Granted {
        Granted(names.to_vec())
    }

    pub fn has(&self, name: &str) -> bool {
        self.0.iter().any(|n| n == name)
    }

    fn mine(&self) -> impl Iterator<Item = &'static Permission> + '_ {
        self.0.iter().filter_map(|n| of(n))
    }

    /// Whether the plugin may call `method path`.
    pub fn route(&self, method: &str, path: &str) -> Result<(), String> {
        let refused = || format!("This plugin is not allowed to use {method} {path}");
        if !matches!(method, "GET" | "POST") || !is_plain_request(path) {
            return Err(refused());
        }
        let route = path.split('?').next().unwrap_or("");
        let allowed = self.mine().any(|p| {
            p.routes
                .iter()
                .any(|(m, pattern)| (*m == "*" || *m == method) && covers(pattern, route))
        });
        allowed.then_some(()).ok_or_else(refused)
    }

    /// Whether the plugin may follow every one of `topics`, comma separated.
    pub fn topics(&self, topics: &str) -> Result<(), String> {
        let all = topics
            .split(',')
            .all(|t| self.mine().any(|p| p.topics.contains(&t)));
        all.then_some(())
            .ok_or_else(|| format!("This plugin is not allowed to follow {topics}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn granted(names: &[&str]) -> Granted {
        Granted::new(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn nothing_is_allowed_without_a_permission() {
        let none = granted(&[]);
        for path in ["/api/config", "/api/local/run", "/api/vehicle/arm", "/api/plugins"] {
            assert!(none.route("GET", path).is_err(), "{path}");
            assert!(none.route("POST", path).is_err(), "{path}");
        }
        assert!(none.topics("shell").is_err());
        assert!(granted(&["unknown", "everything", "*"]).route("GET", "/api/config").is_err());
    }

    #[test]
    fn a_permission_opens_its_routes_and_no_others() {
        let g = granted(&["telemetry.rates", "ssh"]);
        assert!(g.route("POST", "/api/vibration/stream").is_ok());
        assert!(g.route("GET", "/api/vibration/stream").is_err());
        assert!(g.route("POST", "/api/vibration/stream/more").is_err());
        assert!(g.route("POST", "/api/ssh/run").is_ok());
        assert!(g.route("GET", "/api/ssh/connections?x=1&y=%20").is_ok());
        assert!(g.route("POST", "/api/ssh/").is_err());
        assert!(g.route("POST", "/api/sshx/run").is_err());
        assert!(g.route("POST", "/api/local/run").is_err());
        assert!(g.topics("ssh").is_ok());
        assert!(g.topics("ssh,tak").is_err());
        assert!(g.topics("").is_err());
    }

    #[test]
    fn a_path_cannot_be_dressed_up_as_another() {
        let g = granted(&["ssh", "tak"]);
        for path in [
            "/api/ssh/../local/run",
            "/api/ssh/%2e%2e/local/run",
            "/api/ssh//run",
            "/api/ssh/run HTTP/1.1",
            "/api/ssh/run\r\nHost: x",
            "/api/ssh/run?a=1\r\nX: y",
            "http://127.0.0.1/api/ssh/run",
            "/api/takx",
            "/mcp",
        ] {
            assert!(g.route("POST", path).is_err(), "{path:?}");
        }
        assert!(g.route("DELETE", "/api/ssh/run").is_err());
        assert!(g.route("get", "/api/tak").is_err());
    }

    #[test]
    fn no_plugin_reaches_the_other_plugins_settings_or_the_guards() {
        let everything: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        let g = granted(&everything);
        for path in [
            "/api/plugins",
            "/api/plugins/settings",
            "/api/settings/export",
            "/api/settings/import",
            "/api/key",
        ] {
            assert!(g.route("GET", path).is_err(), "{path}");
            assert!(g.route("POST", path).is_err(), "{path}");
        }
        assert!(g.route("POST", "/api/config").is_err(), "settings are read only");
    }

    #[test]
    fn every_permission_has_a_name_of_its_own_and_words() {
        let mut names: Vec<&str> = ALL.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
        assert!(ALL.iter().all(|p| !p.says.is_empty()));
    }
}
