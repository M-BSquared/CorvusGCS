//! The manual: one frame around every page of `docs/guide/*.html`.
//!
//! Each page file holds its own text between the `<!-- guide:content -->`
//! markers, and that text is the only part anybody edits by hand. Everything
//! around it (head, header, sidebar, "On this page", previous and next,
//! footer) is written from [`PAGES`], so adding or renaming a page is one edit
//! here and one run, and the sidebar can never list a page that is not there.
//!
//! ```text
//! cargo run -p corvus_tools --bin corvus-build -- guide           rewrite the frames
//! cargo run -p corvus_tools --bin corvus-build -- guide --check   exit 1 when one is stale
//! ```

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::html::escape;

pub const SITE: &str = "https://m-bsquared.github.io/CorvusGCS";
pub const REPO: &str = "https://github.com/M-BSquared/CorvusGCS";
pub const START: &str = "<!-- guide:content -->";
pub const END: &str = "<!-- /guide:content -->";

/// One page of the manual: its file, its sidebar group and its words.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Page {
    pub file: &'static str,
    pub group: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

const fn page(file: &'static str, group: &'static str, title: &'static str, description: &'static str) -> Page {
    Page {
        file,
        group,
        title,
        description,
    }
}

pub const PAGES: &[Page] = &[
    page("index.html", "Start", "Overview",
         "What Corvus GCS is, what it is for, and where in this manual to find each part of it."),
    page("install.html", "Start", "Install and run",
         "Download and start Corvus GCS on macOS, Linux or Windows, the first start setup, and running it from source."),
    page("connect.html", "Start", "Connect to the aircraft",
         "How Corvus finds a link on its own, connecting by hand over serial, UDP or TCP, sharing the link with QGroundControl, and what the status bar says."),
    page("flying.html", "Fly", "Map, HUD and flight controls",
         "The live map, the flown track, several vehicles at once, click to fly, the flight HUD, the flight bar, notifications, manual controls and the preflight checklist."),
    page("3d-offline.html", "Fly", "3D, terrain and offline maps",
         "The 3D view and the globe, elevation models, OpenStreetMap buildings, and preparing maps and terrain for a field with no internet."),
    page("mission.html", "Fly", "Mission planner",
         "Draw a mission on the map, read it as an altitude profile against the terrain, save it, upload it and follow it in flight."),
    page("airframe.html", "Set up", "Airframe and motors",
         "The airframe drawn to scale, motor outputs, frame size and sensor positions, the motor test, the output protocol and ESC calibration."),
    page("safety.html", "Set up", "Safety, sensors and battery",
         "Geofence limits, return to launch, failsafe actions, distance sensors and optical flow, the preflight checklist, and the battery and power page."),
    page("parameters.html", "Set up", "Parameters and calibration",
         "The parameter editor with firmware defaults, import and export, checking written values, rebooting the autopilot, and sensor calibration."),
    page("radio-tuning.html", "Set up", "Radio control and tuning",
         "The transmitter drawn live, channel bindings, radio calibration, PID tuning with live plots, and the in-flight autotune."),
    page("hardware.html", "Set up", "Telemetry radio, RTK and Remote ID",
         "Programming a SiK radio pair, RTK GPS with a base station or NTRIP, and the Remote ID broadcast."),
    page("firmware.html", "Set up", "Firmware",
         "Flashing PX4 or ArduPilot over USB: picking the right board from a release, developer builds, peripherals and working offline."),
    page("video.html", "Set up", "Video and windows",
         "Cameras over RTSP or WebRTC, floating camera and terminal windows, second screens, and how camera passwords are kept."),
    page("analysis.html", "Review", "Logs and Flight Review",
         "Downloading vehicle logs, the tlogs Corvus records, reading either in Flight Review or Telemetry Review, and every message on the live link."),
    page("workspace.html", "Tools", "Console and SSH",
         "The MAVLink console and its commands, and the SSH terminal to a companion computer."),
    page("plugins.html", "Tools", "Plugins",
         "The Vibration Monitor, the SSH Launcher, Schwalby and Map Entities, the NuttX Console and the Trajectory Viewer to install, and writing a plugin of your own."),
    page("mcp.html", "Tools", "AI clients (MCP)",
         "Letting an AI client such as OpenCode read and fly the vehicle through the MCP server, what it may do, and how you stay in charge."),
    page("settings.html", "Tools", "Settings",
         "Every page of Settings: appearance, map services and keys, controls, pages, files, export and import, and updates."),
    page("compatibility.html", "Reference", "PX4 and ArduPilot",
         "Which firmware versions Corvus targets, where PX4 and ArduPilot differ, and what ArduPilot does not get."),
    page("security.html", "Reference", "Network and security",
         "Who can reach the ground station, the browser protections, SSH host keys and MAVLink signing."),
    page("reference.html", "Reference", "Files, settings and variables",
         "Where Corvus keeps its files, the config file, command-line arguments and environment variables."),
    page("build.html", "Reference", "Build the apps",
         "Building the macOS, Linux and Windows apps yourself, and what each build needs."),
];

const ICONS: &[&str] = &[
    r#"<g id="i-github" fill="currentColor" stroke="none" transform="scale(1.5)"><path d="M8 0c4.42 0 8 3.58 8 8a8.013 8.013 0 0 1-5.45 7.59c-.4.08-.55-.17-.55-.38 0-.27.01-1.13.01-2.2 0-.75-.25-1.23-.54-1.48 1.78-.2 3.65-.88 3.65-3.95 0-.88-.31-1.59-.82-2.15.08-.2.36-1.02-.08-2.12 0 0-.67-.22-2.2.82-.64-.18-1.32-.27-2-.27-.68 0-1.36.09-2 .27-1.53-1.03-2.2-.82-2.2-.82-.44 1.1-.16 1.92-.08 2.12-.51.56-.82 1.28-.82 2.15 0 3.06 1.86 3.75 3.64 3.95-.23.2-.44.55-.51 1.07-.46.21-1.61.55-2.33-.66-.15-.24-.6-.83-1.23-.82-.67.01-.27.38.01.53.34.19.73.9.82 1.13.16.45.68 1.31 2.69.94 0 .67.01 1.3.01 1.49 0 .21-.15.45-.55.38A7.995 7.995 0 0 1 0 8c0-4.42 3.58-8 8-8Z"/></g>"#,
    r#"<g id="i-sun"><circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/></g>"#,
    r#"<g id="i-moon"><path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401"/></g>"#,
    r#"<g id="i-menu"><path d="M4 5h16"/><path d="M4 12h16"/><path d="M4 19h16"/></g>"#,
    r#"<g id="i-close"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></g>"#,
    r#"<g id="i-prev"><path d="m15 18-6-6 6-6"/></g>"#,
    r#"<g id="i-next"><path d="m9 18 6-6-6-6"/></g>"#,
    r#"<g id="i-book"><path d="M12 7v14"/><path d="M3 18a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h5a4 4 0 0 1 4 4 4 4 0 0 1 4-4h5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-6a3 3 0 0 0-3 3 3 3 0 0 0-3-3z"/></g>"#,
    r#"<g id="i-info"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></g>"#,
    r#"<g id="i-alert"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/></g>"#,
    r#"<g id="i-edit"><path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/></g>"#,
];

/// The folder of the manual.
pub fn guide_dir() -> PathBuf {
    crate::repo_root().join("docs").join("guide")
}

fn icon(name: &str, cls: &str) -> String {
    format!(
        r##"<svg class="{cls}" viewBox="0 0 24 24" aria-hidden="true" focusable="false"><use href="#i-{name}"></use></svg>"##
    )
}

/// The plain words of an HTML fragment, entities kept as they were.
fn text(fragment: &str) -> String {
    let tag = Regex::new(r"<[^>]+>").unwrap();
    tag.replace_all(fragment, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `(id, text)` of every `<h2 id>` in `content`, in order.
pub fn headings(content: &str) -> Vec<(String, String)> {
    let heading = Regex::new(r#"(?s)<h2 id="([\w-]+)"[^>]*>(.*?)</h2>"#).unwrap();
    heading
        .captures_iter(content)
        .map(|c| (c[1].to_string(), text(&c[2])))
        .collect()
}

fn sidebar(current: &Page) -> String {
    let mut out = vec![
        r#"<nav class="guide-nav" id="guide-nav" aria-label="Documentation">"#.to_string(),
        r#"  <button class="guide-nav-toggle" id="guide-nav-toggle" type="button" aria-expanded="false" aria-controls="guide-nav-body">"#.to_string(),
        format!(
            r#"    {}<span>Contents</span><span class="guide-nav-here">{}</span>{}"#,
            icon("book", "icon"),
            escape(current.title),
            icon("next", "icon guide-nav-chevron")
        ),
        "  </button>".to_string(),
        r#"  <div class="guide-nav-body" id="guide-nav-body">"#.to_string(),
    ];
    let mut groups: Vec<&str> = Vec::new();
    for p in PAGES {
        if !groups.contains(&p.group) {
            groups.push(p.group);
        }
    }
    for group in groups {
        out.push(r#"    <div class="guide-nav-group">"#.to_string());
        out.push(format!(r#"      <p class="guide-nav-heading">{}</p>"#, escape(group)));
        out.push("      <ul>".to_string());
        for p in PAGES.iter().filter(|p| p.group == group) {
            let here = if p == current { r#" aria-current="page""# } else { "" };
            out.push(format!(
                r#"        <li><a href="{}"{here}>{}</a></li>"#,
                p.file,
                escape(p.title)
            ));
        }
        out.push("      </ul>".to_string());
        out.push("    </div>".to_string());
    }
    out.push("  </div>".to_string());
    out.push("</nav>".to_string());
    out.join("\n")
}

fn toc(content: &str) -> String {
    let items = headings(content);
    if items.is_empty() {
        return String::new();
    }
    let mut out = vec![
        r#"<aside class="guide-toc" aria-labelledby="toc-title">"#.to_string(),
        r#"  <p class="guide-toc-title" id="toc-title">On this page</p>"#.to_string(),
        "  <ul>".to_string(),
    ];
    for (anchor, label) in items {
        out.push(format!(r##"    <li><a href="#{anchor}">{label}</a></li>"##));
    }
    out.push("  </ul>".to_string());
    out.push("</aside>".to_string());
    out.join("\n")
}

fn pager(index: usize) -> String {
    let mut out = vec![r#"<nav class="doc-pager" aria-label="Previous and next page">"#.to_string()];
    if index > 0 {
        let prev = &PAGES[index - 1];
        out.push(format!(
            r#"  <a class="doc-pager-prev" href="{}" rel="prev">{}<span><small>Previous</small>{}</span></a>"#,
            prev.file,
            icon("prev", "icon"),
            escape(prev.title)
        ));
    }
    if index + 1 < PAGES.len() {
        let next = &PAGES[index + 1];
        out.push(format!(
            r#"  <a class="doc-pager-next" href="{}" rel="next"><span><small>Next</small>{}</span>{}</a>"#,
            next.file,
            escape(next.title),
            icon("next", "icon")
        ));
    }
    out.push("</nav>".to_string());
    out.join("\n")
}

/// The whole file for `page` around its hand written `content`.
pub fn render(page: &Page, content: &str) -> String {
    let index = PAGES.iter().position(|p| p == page).expect("a page of PAGES");
    let title = escape(page.title);
    let desc = escape(page.description);
    let url = format!(
        "{SITE}/guide/{}",
        if page.file == "index.html" { "" } else { page.file }
    );
    let full_title = if page.file == "index.html" {
        "Corvus GCS documentation".to_string()
    } else {
        format!("{title} · Corvus GCS documentation")
    };
    let sprite = ICONS.join("\n    ");
    let toc = toc(content);
    let layout = if toc.is_empty() {
        "guide-layout guide-layout-wide"
    } else {
        "guide-layout"
    };
    let current_docs = if page.file == "index.html" {
        r#" aria-current="page""#
    } else {
        ""
    };
    let group = escape(page.group);
    let sidebar = sidebar(page);
    let pager = pager(index);
    let content = content.trim();
    let file = page.file;
    let i_github = icon("github", "icon");
    let i_sun = icon("sun", "icon icon-sun");
    let i_moon = icon("moon", "icon icon-moon");
    let i_menu = icon("menu", "icon icon-open");
    let i_close = icon("close", "icon icon-shut");
    let i_edit = icon("edit", "icon");
    format!(
        r##"<!DOCTYPE html>
<!-- Written by corvus-build guide (src/tools) around the content block below.
     Edit the text between the guide:content markers; run it for everything else. -->
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{full_title}</title>
<meta name="description" content="{desc}">
<link rel="canonical" href="{url}">
<meta name="theme-color" content="#0B0E12" media="(prefers-color-scheme: dark)">
<meta name="theme-color" content="#F4F6F8" media="(prefers-color-scheme: light)">
<meta name="color-scheme" content="dark light">
<meta property="og:type" content="article">
<meta property="og:site_name" content="Corvus GCS">
<meta property="og:title" content="{full_title}">
<meta property="og:description" content="{desc}">
<meta property="og:url" content="{url}">
<meta property="og:image" content="{SITE}/assets/images/social-preview.jpg">
<link rel="icon" type="image/png" sizes="32x32" href="../assets/icons/favicon-32.png">
<link rel="apple-touch-icon" href="../assets/icons/apple-touch-icon.png">
<link rel="preload" as="font" type="font/woff2" href="../assets/fonts/Inter-latin.woff2" crossorigin>
<link rel="stylesheet" href="../style.css">
<link rel="stylesheet" href="guide.css">
<script>
  document.documentElement.classList.add('js');
  try {{
    var t = localStorage.getItem('corvus-theme');
    if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
  }} catch (e) {{}}
</script>
<script src="../script.js" defer></script>
<script src="guide.js" defer></script>
</head>

<body class="guide">
<a class="skip-link" href="#main">Skip to content</a>

<svg class="sprite" aria-hidden="true" focusable="false" xmlns="http://www.w3.org/2000/svg">
  <defs>
    {sprite}
  </defs>
</svg>

<header class="site-header" id="site-header">
  <div class="wrap header-inner">
    <a class="brand" href="../index.html" aria-label="Corvus GCS, project website">
      <span class="brand-mark" role="img" aria-hidden="true"></span>
      <span class="brand-text">
        <span class="brand-name">Corvus GCS</span>
        <span class="brand-sub">Documentation</span>
      </span>
    </a>

    <nav class="site-nav" id="site-nav" aria-label="Main">
      <ul class="nav-list">
        <li><a href="../index.html">Website</a></li>
        <li><a href="index.html"{current_docs}>Documentation</a></li>
        <li><a href="{REPO}/releases/latest" target="_blank" rel="noopener">Download</a></li>
        <li><a class="nav-github" href="{REPO}" target="_blank" rel="noopener">
          {i_github}GitHub</a></li>
      </ul>
    </nav>

    <div class="header-actions">
      <button class="icon-btn theme-toggle" id="theme-toggle" type="button" aria-label="Switch to the light theme" aria-pressed="false">
        {i_sun}
        {i_moon}
      </button>
      <button class="icon-btn nav-toggle" id="nav-toggle" type="button" aria-label="Open the menu" aria-expanded="false" aria-controls="site-nav">
        {i_menu}
        {i_close}
      </button>
    </div>
  </div>
</header>

<div class="wrap {layout}">
{sidebar}

<main id="main" class="guide-main">
<article class="doc">
<p class="eyebrow">{group}</p>
{START}
{content}
{END}
</article>

{pager}

<p class="doc-edit">
  <a href="{REPO}/edit/master/docs/guide/{file}" target="_blank" rel="noopener">{i_edit}Suggest a change to this page</a>
</p>
</main>

{toc}
</div>

<footer class="site-footer guide-footer">
  <div class="wrap footer-base">
    <p class="copyright">&copy; <span id="year">2026</span> Maximilian Böck. Developed with the Universität der Bundeswehr München.</p>
    <p class="footer-meta">
      Source available under the Sustainable Use License 1.0 (<em>fair-code</em>).
      <a href="{REPO}/blob/master/LICENSE.md" target="_blank" rel="noopener">License</a>
    </p>
  </div>
</footer>
</body>
</html>
"##
    )
}

/// The hand written block of a page file, without its markers.
pub fn content_of(text: &str) -> Result<&str, String> {
    match (text.find(START), text.find(END)) {
        (Some(s), Some(e)) if e >= s => Ok(&text[s + START.len()..e]),
        _ => Err("no guide:content block".into()),
    }
}

/// Rewrites (or with `check`, compares) every page in `dir`; returns the stale ones.
pub fn build(dir: &Path, check: bool) -> Result<Vec<&'static str>, String> {
    let mut stale = Vec::new();
    for page in PAGES {
        let path = dir.join(page.file);
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        let content = if current.is_empty() {
            format!("<h1>{}</h1>", escape(page.title))
        } else {
            content_of(&current)
                .map_err(|e| format!("{}: {e}", page.file))?
                .to_string()
        };
        let wanted = render(page, &content);
        if current != wanted {
            stale.push(page.file);
            if !check {
                std::fs::write(&path, wanted).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_are_read_with_their_plain_words() {
        let h = headings(
            r#"<h2 id="a-b" class="x">One <code>two</code>
            three</h2><h3 id="no">x</h3><h2 id="c">Four</h2>"#,
        );
        assert_eq!(h, [("a-b".into(), "One two three".into()), ("c".into(), "Four".into())]);
    }

    #[test]
    fn a_rendered_page_gives_back_its_content() {
        let page = &PAGES[3];
        let out = render(page, "\n<h1>Map</h1>\n<h2 id=\"x\">X</h2>\n");
        assert_eq!(content_of(&out).unwrap().trim(), "<h1>Map</h1>\n<h2 id=\"x\">X</h2>");
        assert!(out.contains(r##"<a href="#x">X</a>"##), "the table of contents");
        assert!(out.contains(r#"rel="prev""#) && out.contains(r#"rel="next""#));
        assert!(out.contains("Developed with the Universität der Bundeswehr München."));
        assert!(
            !render(&PAGES[0], "<h1>O</h1>").contains("guide-toc"),
            "no headings, no table"
        );
    }
}
