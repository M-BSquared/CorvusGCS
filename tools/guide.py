#!/usr/bin/env python3
"""Corvus GCS documentation: one frame around every page of the manual.

The manual lives in ``docs/guide/*.html`` and is served by GitHub Pages next
to the project website (``master:/docs``). Each page file holds its own text
between the ``<!-- guide:content -->`` markers, and that text is the only part
anybody edits by hand. Everything around it (head, header, sidebar, "On this
page", previous and next, footer) is written by this script from ``PAGES``, so
adding or renaming a page is one edit here and one run, and the sidebar can
never list a page that is not there.

    python3 tools/guide.py           # rewrite the frame of every page
    python3 tools/guide.py --check   # exit 1 when a page is out of date

A page that does not exist yet is created with an empty content block.

stdlib only.
"""
from __future__ import annotations

import html
import re
import sys
from dataclasses import dataclass
from pathlib import Path

GUIDE = Path(__file__).resolve().parents[1] / "docs" / "guide"
SITE = "https://m-bsquared.github.io/CorvusGCS"
REPO = "https://github.com/M-BSquared/CorvusGCS"

START = "<!-- guide:content -->"
END = "<!-- /guide:content -->"


@dataclass(frozen=True)
class Page:
    """One page of the manual: its file, its sidebar group and its words."""

    file: str
    group: str
    title: str
    description: str


PAGES: tuple[Page, ...] = (
    Page("index.html", "Start", "Overview",
         "What Corvus GCS is, what it is for, and where in this manual to find each part of it."),
    Page("install.html", "Start", "Install and run",
         "Download and start Corvus GCS on macOS, Linux or Windows, the first start setup, and running it from source."),
    Page("connect.html", "Start", "Connect to the aircraft",
         "How Corvus finds a link on its own, connecting by hand over serial, UDP or TCP, sharing the link with QGroundControl, and what the status bar says."),
    Page("flying.html", "Fly", "Map, HUD and flight controls",
         "The live map, the flown track, click to fly, the flight HUD, the flight bar, notifications, manual controls and the preflight checklist."),
    Page("3d-offline.html", "Fly", "3D, terrain and offline maps",
         "The 3D view and the globe, elevation models, OpenStreetMap buildings, and preparing maps and terrain for a field with no internet."),
    Page("mission.html", "Fly", "Mission planner",
         "Draw a mission on the map, read it as an altitude profile against the terrain, save it, upload it and follow it in flight."),
    Page("airframe.html", "Set up", "Airframe and motors",
         "The airframe drawn to scale, motor outputs, frame size and sensor positions, the motor test, the output protocol and ESC calibration."),
    Page("safety.html", "Set up", "Safety, sensors and battery",
         "Geofence limits, return to launch, failsafe actions, distance sensors and optical flow, the preflight checklist, and the battery and power page."),
    Page("parameters.html", "Set up", "Parameters and calibration",
         "The parameter editor with firmware defaults, import and export, checking written values, rebooting the autopilot, and sensor calibration."),
    Page("radio-tuning.html", "Set up", "Radio control and tuning",
         "The transmitter drawn live, channel bindings, radio calibration, PID tuning with live plots, and the in-flight autotune."),
    Page("hardware.html", "Set up", "Telemetry radio, RTK and Remote ID",
         "Programming a SiK radio pair, RTK GPS with a base station or NTRIP, and the Remote ID broadcast."),
    Page("firmware.html", "Set up", "Firmware",
         "Flashing PX4 or ArduPilot over USB: picking the right board from a release, developer builds, peripherals and working offline."),
    Page("video.html", "Set up", "Video and windows",
         "Cameras over RTSP or WebRTC, floating camera and terminal windows, second screens, and how camera passwords are kept."),
    Page("analysis.html", "Review", "Logs and Flight Review",
         "Downloading vehicle logs, the tlogs Corvus records, and reading either in Flight Review or Telemetry Review."),
    Page("workspace.html", "Tools", "Console and SSH",
         "The MAVLink console and its commands, and the SSH terminal to a companion computer."),
    Page("plugins.html", "Tools", "Plugins",
         "The Vibration Monitor, the SSH Launcher and Schwalby, and writing a plugin of your own."),
    Page("settings.html", "Tools", "Settings",
         "Every page of Settings: appearance, map services and keys, controls, pages, files, export and import, and updates."),
    Page("compatibility.html", "Reference", "PX4 and ArduPilot",
         "Which firmware versions Corvus targets, where PX4 and ArduPilot differ, and what ArduPilot does not get."),
    Page("security.html", "Reference", "Network and security",
         "Who can reach the ground station, the browser protections, SSH host keys and MAVLink signing."),
    Page("reference.html", "Reference", "Files, settings and variables",
         "Where Corvus keeps its files, the config file, command-line arguments and environment variables."),
    Page("build.html", "Reference", "Build the apps",
         "Building the macOS, Linux and Windows apps yourself, and what each build needs."),
)

_ICONS = {
    "github": '<g id="i-github" fill="currentColor" stroke="none" transform="scale(1.5)"><path d="M8 0c4.42 0 8 3.58 8 8a8.013 8.013 0 0 1-5.45 7.59c-.4.08-.55-.17-.55-.38 0-.27.01-1.13.01-2.2 0-.75-.25-1.23-.54-1.48 1.78-.2 3.65-.88 3.65-3.95 0-.88-.31-1.59-.82-2.15.08-.2.36-1.02-.08-2.12 0 0-.67-.22-2.2.82-.64-.18-1.32-.27-2-.27-.68 0-1.36.09-2 .27-1.53-1.03-2.2-.82-2.2-.82-.44 1.1-.16 1.92-.08 2.12-.51.56-.82 1.28-.82 2.15 0 3.06 1.86 3.75 3.64 3.95-.23.2-.44.55-.51 1.07-.46.21-1.61.55-2.33-.66-.15-.24-.6-.83-1.23-.82-.67.01-.27.38.01.53.34.19.73.9.82 1.13.16.45.68 1.31 2.69.94 0 .67.01 1.3.01 1.49 0 .21-.15.45-.55.38A7.995 7.995 0 0 1 0 8c0-4.42 3.58-8 8-8Z"/></g>',
    "sun": '<g id="i-sun"><circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/></g>',
    "moon": '<g id="i-moon"><path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401"/></g>',
    "menu": '<g id="i-menu"><path d="M4 5h16"/><path d="M4 12h16"/><path d="M4 19h16"/></g>',
    "close": '<g id="i-close"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></g>',
    "prev": '<g id="i-prev"><path d="m15 18-6-6 6-6"/></g>',
    "next": '<g id="i-next"><path d="m9 18 6-6-6-6"/></g>',
    "book": '<g id="i-book"><path d="M12 7v14"/><path d="M3 18a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h5a4 4 0 0 1 4 4 4 4 0 0 1 4-4h5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-6a3 3 0 0 0-3 3 3 3 0 0 0-3-3z"/></g>',
    "info": '<g id="i-info"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></g>',
    "alert": '<g id="i-alert"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/></g>',
    "edit": '<g id="i-edit"><path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/></g>',
}

_HEADING = re.compile(r'<h2 id="([\w-]+)"[^>]*>(.*?)</h2>', re.S)
_TAG = re.compile(r"<[^>]+>")


def _icon(name: str, cls: str = "icon") -> str:
    return (f'<svg class="{cls}" viewBox="0 0 24 24" aria-hidden="true" focusable="false">'
            f'<use href="#i-{name}"></use></svg>')


def _text(fragment: str) -> str:
    """The plain words of an HTML fragment, entities kept as they were."""
    return " ".join(_TAG.sub("", fragment).split())


def headings(content: str) -> list[tuple[str, str]]:
    """``(id, text)`` of every ``<h2 id>`` in *content*, in order."""
    return [(anchor, _text(label)) for anchor, label in _HEADING.findall(content)]


def _sidebar(current: Page) -> str:
    out = ['<nav class="guide-nav" id="guide-nav" aria-label="Documentation">',
           '  <button class="guide-nav-toggle" id="guide-nav-toggle" type="button" '
           'aria-expanded="false" aria-controls="guide-nav-body">',
           f'    {_icon("book")}<span>Contents</span>'
           f'<span class="guide-nav-here">{html.escape(current.title)}</span>'
           f'{_icon("next", "icon guide-nav-chevron")}',
           '  </button>',
           '  <div class="guide-nav-body" id="guide-nav-body">']
    groups: list[str] = []
    for page in PAGES:
        if page.group not in groups:
            groups.append(page.group)
    for group in groups:
        out.append('    <div class="guide-nav-group">')
        out.append(f'      <p class="guide-nav-heading">{html.escape(group)}</p>')
        out.append("      <ul>")
        for page in PAGES:
            if page.group != group:
                continue
            here = ' aria-current="page"' if page is current else ""
            out.append(f'        <li><a href="{page.file}"{here}>{html.escape(page.title)}</a></li>')
        out.append("      </ul>")
        out.append("    </div>")
    out.append("  </div>")
    out.append("</nav>")
    return "\n".join(out)


def _toc(content: str) -> str:
    items = headings(content)
    if not items:
        return ""
    out = ['<aside class="guide-toc" aria-labelledby="toc-title">',
           '  <p class="guide-toc-title" id="toc-title">On this page</p>',
           "  <ul>"]
    for anchor, label in items:
        out.append(f'    <li><a href="#{anchor}">{label}</a></li>')
    out.append("  </ul>")
    out.append("</aside>")
    return "\n".join(out)


def _pager(index: int) -> str:
    out = ['<nav class="doc-pager" aria-label="Previous and next page">']
    if index > 0:
        prev = PAGES[index - 1]
        out.append(f'  <a class="doc-pager-prev" href="{prev.file}" rel="prev">'
                   f'{_icon("prev")}<span><small>Previous</small>{html.escape(prev.title)}</span></a>')
    if index < len(PAGES) - 1:
        nxt = PAGES[index + 1]
        out.append(f'  <a class="doc-pager-next" href="{nxt.file}" rel="next">'
                   f'<span><small>Next</small>{html.escape(nxt.title)}</span>{_icon("next")}</a>')
    out.append("</nav>")
    return "\n".join(out)


def render(page: Page, content: str) -> str:
    """The whole file for *page* around its hand-written *content*."""
    index = PAGES.index(page)
    title = html.escape(page.title)
    desc = html.escape(page.description, quote=True)
    url = f"{SITE}/guide/{'' if page.file == 'index.html' else page.file}"
    full_title = ("Corvus GCS documentation" if page.file == "index.html"
                  else f"{title} · Corvus GCS documentation")
    sprite = "\n    ".join(_ICONS.values())
    toc = _toc(content)
    layout = "guide-layout" + ("" if toc else " guide-layout-wide")
    return f"""<!DOCTYPE html>
<!-- Written by tools/guide.py around the content block below. Edit the text
     between the guide:content markers; run the script for everything else. -->
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
        <li><a href="index.html"{' aria-current="page"' if page.file == 'index.html' else ''}>Documentation</a></li>
        <li><a href="{REPO}/releases/latest" target="_blank" rel="noopener">Download</a></li>
        <li><a class="nav-github" href="{REPO}" target="_blank" rel="noopener">
          {_icon("github")}GitHub</a></li>
      </ul>
    </nav>

    <div class="header-actions">
      <button class="icon-btn theme-toggle" id="theme-toggle" type="button" aria-label="Switch to the light theme" aria-pressed="false">
        {_icon("sun", "icon icon-sun")}
        {_icon("moon", "icon icon-moon")}
      </button>
      <button class="icon-btn nav-toggle" id="nav-toggle" type="button" aria-label="Open the menu" aria-expanded="false" aria-controls="site-nav">
        {_icon("menu", "icon icon-open")}
        {_icon("close", "icon icon-shut")}
      </button>
    </div>
  </div>
</header>

<div class="wrap {layout}">
{_sidebar(page)}

<main id="main" class="guide-main">
<article class="doc">
<p class="eyebrow">{html.escape(page.group)}</p>
{START}
{content.strip()}
{END}
</article>

{_pager(index)}

<p class="doc-edit">
  <a href="{REPO}/edit/master/docs/guide/{page.file}" target="_blank" rel="noopener">{_icon("edit")}Suggest a change to this page</a>
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
"""


def content_of(text: str) -> str:
    """The hand-written block of a page file, without its markers."""
    start = text.find(START)
    end = text.find(END)
    if start < 0 or end < start:
        raise ValueError("no guide:content block")
    return text[start + len(START):end]


def build(check: bool = False) -> list[str]:
    """Rewrite (or with *check*, compare) every page; return the stale ones."""
    stale: list[str] = []
    for page in PAGES:
        path = GUIDE / page.file
        current = path.read_text(encoding="utf-8") if path.is_file() else ""
        content = content_of(current) if current else f"<h1>{html.escape(page.title)}</h1>"
        wanted = render(page, content)
        if current != wanted:
            stale.append(page.file)
            if not check:
                path.write_text(wanted, encoding="utf-8")
    return stale


def main(argv: list[str]) -> int:
    check = "--check" in argv[1:]
    stale = build(check=check)
    if check and stale:
        print("out of date, run python3 tools/guide.py: " + ", ".join(stale))
        return 1
    for name in stale:
        print(f"wrote docs/guide/{name}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
