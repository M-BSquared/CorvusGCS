//! The repository's contracts that only show up in a shipped artifact or on
//! the live website, weeks later: where builds land, what they copy, how the
//! macOS bundle is sealed, how the AppImage is packed, what the Android app
//! reads its version from, that nothing needs Python, and that the manual and
//! the website are whole. Everything is read off disk as text: these files
//! are consumed by bash, PowerShell, Gradle, CI runners and browsers, none of
//! which a test can run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use corvus_tools::{guide, html, repo_root};
use regex::Regex;

fn root() -> PathBuf {
    repo_root()
}

fn text(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const BUILDS: &[&str] = &["build-appimage.sh", "build-macos-app.sh", "build-windows.ps1"];
const SCRIPTS: &[&str] = &[
    "build.sh",
    "build-appimage.sh",
    "build-deb.sh",
    "build-macos-app.sh",
    "build-windows.ps1",
    "build-android.sh",
    "run.sh",
    "release.sh",
    ".github/workflows/build.yml",
    ".github/scripts/check-bundle.sh",
];

/// The lines of a script that run something: comments left out.
fn command_lines(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with("//"))
        .collect()
}

#[test]
fn nothing_in_the_build_or_ci_needs_python() {
    // A call, not a mention: an interpreter, pip or a Python tool run as a
    // command, a script path ending in .py, a venv, or the CI's Python setup.
    // The checks that look for Python in a bundle only to refuse it name it in
    // quotes or as a variable, and pass.
    let calls = [
        Regex::new(r"(?:^|[\s;|&(`])(?:python3?(?:\.\d+)?|pip3?|pytest|ruff|maturin)(?:\s|$)").unwrap(),
        Regex::new(r#"(?:^|\s)[\w./${}"-]*\.py(?:\s|"|$)"#).unwrap(),
        Regex::new(r"setup-python|\.venv\b|--group|pyproject\.toml").unwrap(),
    ];
    let calls_python = |line: &str| calls.iter().any(|c| c.is_match(line));
    for was in [
        r#"python3 "$REPO_DIR/python/tools/appimage_libs.py" bundle "$APPDIR""#,
        "(cd python && \"$PY\" -m pytest -q)",
        r#"PY="$REPO_DIR/.venv/bin/python""#,
        "      - uses: actions/setup-python@v6",
        "        run: pip install ../src/link",
        r#"if ! python3 - "$OUTPUT" <<'PY'"#,
    ] {
        assert!(calls_python(was), "the check misses {was}");
    }
    for guard in [
        r#"-o -name 'pip' -o -name '*.pyc' -o -name 'serve.py' -o -name 'corvus_link*' -o -name 'PySide*' \) -print)""#,
        "if ($python) {",
        "- name: Check that no Python is inside",
    ] {
        assert!(!calls_python(guard), "a guard is taken for a call: {guard}");
    }
    let mut found = Vec::new();
    for script in SCRIPTS {
        for line in command_lines(&text(script)) {
            if calls_python(line) {
                found.push(format!("{script}: {line}"));
            }
        }
    }
    assert!(found.is_empty(), "a build step calls Python:\n  {}", found.join("\n  "));
    assert!(!root().join("python").exists(), "python/ is gone; nothing reads it");
    assert!(!root().join("src/link/pyproject.toml").exists());
}

#[test]
fn no_python_file_is_left_in_the_repository() {
    let mut found = Vec::new();
    fn walk(dir: &Path, root: &Path, found: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if ["target", ".git", "build", "dist", ".venv", "node_modules", ".gradle"].contains(&name.as_str()) {
                continue;
            }
            if p.is_dir() {
                walk(&p, root, found);
            } else if name.ends_with(".py") || name == "pyproject.toml" || name.starts_with("requirements") {
                found.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let r = root();
    walk(&r, &r, &mut found);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn the_pipeline_checks_the_rust_side_and_is_the_only_one() {
    let ci = text(".github/workflows/build.yml");
    for needle in [
        "cargo fmt --check",
        "cargo clippy --workspace --all-targets --locked -- -D warnings",
        "cargo test --workspace --locked",
    ] {
        assert!(ci.contains(needle), "{needle}");
    }
    assert!(!root().join(".gitlab-ci.yml").exists(), "GitHub is the only CI");
}

#[test]
fn builds_write_into_dist_not_the_repository_root() {
    for (script, expected) in [
        ("build-appimage.sh", r#"OUTPUT="$REPO_DIR/dist/"#),
        ("build-macos-app.sh", r#"DMG="$DIST_DIR/"#),
        ("build-windows.ps1", r#""dist\Corvus_GCS-"#),
    ] {
        assert!(
            text(script).contains(expected),
            "{script} does not write its artifact into dist/"
        );
    }
    assert!(text(".gitignore").lines().any(|l| l.trim() == "/dist/"));
}

#[test]
fn no_build_copies_the_website_or_the_tools() {
    let copy = Regex::new(r"\b(rsync|cp|copytree|Copy-Item)\b").unwrap();
    let docs = Regex::new(r#"[/\\]docs\b|[/\\]src[/\\]tools\b|'docs'|'tools'"#).unwrap();
    for script in BUILDS {
        for line in text(script).lines().filter(|l| copy.is_match(l)) {
            assert!(!docs.is_match(line), "{script}: {line}");
        }
    }
}

#[test]
fn the_macos_bundle_is_signed_once_after_it_is_final_and_checked() {
    let s = text("build-macos-app.sh");
    let sign = r#"codesign --force --sign "$CODESIGN_IDENTITY" "$APP""#;
    assert_eq!(s.matches("codesign --force").count(), 1);
    let at = s.find(sign).expect("the signing line");
    for before in [
        r#"cat > "$APP/Contents/Info.plist""#,
        r#"-o "$RES/corvus-gcs.icns""#,
        r#"cp -RX "$REPO_DIR/assets/licenses""#,
    ] {
        assert!(s.find(before).expect(before) < at, "{before} comes after signing");
    }
    let check = "codesign --verify --deep --strict";
    assert!(s.find(check).unwrap() > at, "verified after signing");
    let dmg = s.find("hdiutil create").unwrap();
    assert!(s[dmg..].contains(check), "the copy inside the .dmg is verified too");
    assert!(s.contains("hdiutil attach -nobrowse -readonly"));
    let create = s.find("if hdiutil create").unwrap();
    let lp = s[..create].rfind("for attempt in").unwrap();
    assert!(create - lp < 400, "hdiutil create is not inside the retry loop");
    let failure = s[create..].find(r#"echo "ERROR: hdiutil failed"#).unwrap() + create;
    assert!(s[failure..failure + 300].contains(r#"tail -n 20 "$BUILD_DIR/hdiutil.log""#));
}

#[test]
fn the_appimage_is_packed_with_the_current_tool_and_a_static_runtime() {
    let s = text("build-appimage.sh");
    let ci = text(".github/workflows/build.yml");
    assert!(!s.contains("github.com/AppImage/AppImageKit/"));
    assert!(s.contains("github.com/AppImage/appimagetool/releases/download/continuous/"));
    assert!(s.contains("github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64"));
    let calls: Vec<&str> = s
        .lines()
        .filter(|l| l.contains(r#""$APPDIR" "$OUTPUT""#) && l.contains("APPIMAGETOOL"))
        .collect();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|l| l.contains(r#"--runtime-file "$RUNTIME""#)));
    assert!(s.contains("carries the old AppImage runtime") && s.contains(r#""$CORVUS_BUILD" elf-static "$OUTPUT""#));
    assert!(!ci.contains("key: appimagetool") && !ci.contains("libfuse2"));
}

#[test]
fn the_android_app_reads_its_version_and_starts_in_tablet_mode() {
    let gradle = text("android/app/build.gradle");
    assert!(gradle.contains(r#"file("../../VERSION")"#) && gradle.contains("versionName corvusVersion"));
    let literal = Regex::new(r"\b20\d\d\.\d\d\.\d\d\b").unwrap();
    let mut files = vec![root().join("build-android.sh")];
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .extension()
                .is_some_and(|x| ["gradle", "java", "xml", "properties"].contains(&x.to_str().unwrap_or("")))
            {
                out.push(p);
            }
        }
    }
    walk(&root().join("android"), &mut files);
    assert!(files.len() > 2);
    for p in files {
        assert!(
            !literal.is_match(&std::fs::read_to_string(&p).unwrap()),
            "{}",
            p.display()
        );
    }
    let java = text("android/app/src/main/java/de/corvusgcs/app/MainActivity.java");
    assert!(java.contains(r#"" CorvusAndroid/""#));
    let manifest = text("android/app/src/main/AndroidManifest.xml");
    assert!(
        manifest.contains(r#"android:usesCleartextTraffic="true""#) && manifest.contains("android.permission.INTERNET")
    );
    let ci = text(".github/workflows/build.yml");
    assert!(ci.contains("./build-android.sh"));
    assert!(Regex::new(r"release:\s*\n\s*needs: \[[^\]]*android-app")
        .unwrap()
        .is_match(&ci));
    let script = text("build-android.sh");
    assert!(
        script.contains(r#"VERSION="$(cat "$REPO_DIR/VERSION")""#)
            && script.contains("Corvus_GCS-${VERSION}-android.apk")
    );
}

// --- the manual ---------------------------------------------------------------

fn guide_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(guide::guide_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    v.sort();
    v
}

#[test]
fn every_page_frame_of_the_manual_is_current() {
    let stale = guide::build(&guide::guide_dir(), true).unwrap();
    assert!(stale.is_empty(), "run corvus-build guide: {stale:?}");
}

#[test]
fn the_sidebar_lists_exactly_the_pages_on_disk() {
    let listed: BTreeSet<String> = guide::PAGES.iter().map(|p| p.file.to_string()).collect();
    let on_disk: BTreeSet<String> = guide_files()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, on_disk);
    assert_eq!(guide::PAGES[0].file, "index.html");
}

#[test]
fn every_page_starts_with_one_h1() {
    for p in guide::PAGES {
        let body = std::fs::read_to_string(guide::guide_dir().join(p.file)).unwrap();
        let content = guide::content_of(&body).unwrap();
        assert!(content.trim().starts_with("<h1>"), "{}", p.file);
        assert_eq!(content.matches("<h1").count(), 1, "{}", p.file);
    }
}

#[test]
fn every_local_link_and_anchor_of_the_manual_resolves() {
    let ids: BTreeMap<PathBuf, BTreeSet<String>> = guide_files()
        .into_iter()
        .map(|p| {
            let s = html::scan(&std::fs::read_to_string(&p).unwrap());
            (p, s.ids.into_iter().collect())
        })
        .collect();
    let mut broken = Vec::new();
    for path in ids.keys() {
        let scan = html::scan(&std::fs::read_to_string(path).unwrap());
        for r in scan.refs {
            if r.contains("://") || r.starts_with("//") || r.starts_with("data:") || r.starts_with("mailto:") {
                continue;
            }
            let (file, fragment) = r.split_once('#').map_or((r.as_str(), ""), |(a, b)| (a, b));
            let file = file.split('?').next().unwrap_or("");
            let target = if file.is_empty() {
                path.clone()
            } else {
                path.parent().unwrap().join(percent_decode(file))
            };
            let Ok(target) = target.canonicalize() else {
                broken.push(format!("{}: {r}", path.file_name().unwrap().to_string_lossy()));
                continue;
            };
            if !target.is_file() {
                broken.push(format!("{}: {r}", path.file_name().unwrap().to_string_lossy()));
                continue;
            }
            if !fragment.is_empty() && target.extension().is_some_and(|x| x == "html") {
                let known: BTreeSet<String> = ids
                    .iter()
                    .find(|(p, _)| p.canonicalize().ok().as_ref() == Some(&target))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| {
                        html::scan(&std::fs::read_to_string(&target).unwrap())
                            .ids
                            .into_iter()
                            .collect()
                    });
                if !known.contains(fragment) {
                    broken.push(format!("{}: {r}", path.file_name().unwrap().to_string_lossy()));
                }
            }
        }
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn screenshots_in_the_manual_have_alt_text_and_both_sizes() {
    let figure = Regex::new(r#"(?s)<figure class="doc-shot">(.*?)</figure>"#).unwrap();
    let alt = Regex::new(r#"alt="[^"]{20,}""#).unwrap();
    let full = Regex::new(r#"<a href="\.\./(assets/images/[\w-]+)\.jpg">"#).unwrap();
    for path in guide_files() {
        let body = std::fs::read_to_string(&path).unwrap();
        for f in figure.captures_iter(&body) {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(alt.is_match(&f[1]), "{name}");
            let base = full.captures(&f[1]).unwrap_or_else(|| panic!("{name}"));
            assert!(f[1].contains(&format!("../{}-800.jpg 800w", &base[1])), "{name}");
        }
    }
}

#[test]
fn the_prose_of_the_manual_uses_no_dashes() {
    let dash = Regex::new(r"[\u{2013}\u{2014}]| - ").unwrap();
    let mut offenders = Vec::new();
    for path in guide_files() {
        let body = std::fs::read_to_string(&path).unwrap();
        let prose = html::scan(guide::content_of(&body).unwrap()).prose;
        let prose = prose.split_whitespace().collect::<Vec<_>>().join(" ");
        for m in dash.find_iter(&prose) {
            let start = prose.floor_char_boundary(m.start().saturating_sub(30));
            let end = prose.ceil_char_boundary((m.end() + 30).min(prose.len()));
            offenders.push(format!(
                "{}: ...{}...",
                path.file_name().unwrap().to_string_lossy(),
                &prose[start..end]
            ));
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}

#[test]
fn the_website_points_at_the_manual() {
    let index = text("docs/index.html");
    assert!(index.contains(r#"href="guide/""#) || index.contains(r#"href="guide/index.html""#));
    assert!(!index.contains("github.com/M-BSquared/CorvusGCS#readme"));
}

// --- the website's pictures -------------------------------------------------------

fn jpeg_size(path: &Path) -> (u32, u32) {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut i = 2;
    while i + 9 < data.len() {
        let marker = data[i + 1];
        let length = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]);
            return (u32::from(w), u32::from(h));
        }
        i += 2 + length;
    }
    panic!("{}: no frame header", path.display())
}

#[test]
fn every_picture_the_website_names_exists() {
    let index = text("docs/index.html");
    let refs: BTreeSet<String> = Regex::new(r"assets/images/[\w.-]+\.(?:jpg|png|svg)")
        .unwrap()
        .find_iter(&index)
        .map(|m| m.as_str().to_string())
        .collect();
    assert!(!refs.is_empty());
    let missing: Vec<&String> = refs.iter().filter(|r| !root().join("docs").join(r).is_file()).collect();
    assert!(
        missing.is_empty(),
        "the page links pictures that are not in docs/: {missing:?}"
    );
}

#[test]
fn every_screenshot_on_the_website_has_both_sizes() {
    let index = text("docs/index.html");
    let fulls: Vec<String> = Regex::new(r#"data-full="(assets/images/[\w-]+)\.jpg""#)
        .unwrap()
        .captures_iter(&index)
        .map(|c| c[1].to_string())
        .collect();
    assert_eq!(
        fulls.len(),
        fulls.iter().collect::<BTreeSet<_>>().len(),
        "a screenshot appears twice in the gallery"
    );
    let docs = root().join("docs");
    for base in &fulls {
        assert_eq!(jpeg_size(&docs.join(format!("{base}.jpg"))), (1600, 1000), "{base}");
        assert_eq!(jpeg_size(&docs.join(format!("{base}-800.jpg"))), (800, 500), "{base}");
        assert!(
            index.contains(&format!(r#"srcset="{base}-800.jpg 800w, {base}.jpg 1600w""#)),
            "{base}"
        );
    }
}

#[test]
fn the_hero_has_a_picture_for_each_theme() {
    let index = text("docs/index.html");
    let hero = Regex::new(r#"(?s)<img id="hero-image"[^>]*>"#)
        .unwrap()
        .find(&index)
        .expect("the hero image lost its id")
        .as_str()
        .to_string();
    for theme in ["dark", "light"] {
        let base = Regex::new(&format!(r#"data-{theme}="(assets/images/[\w-]+)""#)).unwrap();
        let base = base
            .captures(&hero)
            .unwrap_or_else(|| panic!("no {theme} hero picture"))[1]
            .to_string();
        for suffix in [".jpg", "-800.jpg"] {
            assert!(
                root().join("docs").join(format!("{base}{suffix}")).is_file(),
                "{base}{suffix}"
            );
        }
    }
    assert!(text("docs/script.js").contains("getElementById('hero-image')"));
}

#[test]
fn every_screenshot_on_the_website_has_a_caption_and_alt_text() {
    let index = text("docs/index.html");
    let figures: Vec<String> = Regex::new(r#"(?s)<figure class="shot[^"]*">(.*?)</figure>"#)
        .unwrap()
        .captures_iter(&index)
        .map(|c| c[1].to_string())
        .collect();
    assert!(!figures.is_empty());
    let alt = Regex::new(r#"alt="[^"]{20,}""#).unwrap();
    let caption = Regex::new(r"<figcaption><strong>[^<]+</strong> [^<]+</figcaption>").unwrap();
    for f in figures {
        assert!(alt.is_match(&f) && caption.is_match(&f), "{}", &f[..f.len().min(120)]);
    }
}
