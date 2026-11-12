//! The AppImage's own needs: the system libraries it must carry, and the
//! squashfs inside an AppImage tool that will not run on this host.
//!
//! The AppDir is closed over its libraries. Every ELF file in it is read for
//! what it links (`DT_NEEDED`), and every library neither the AppDir nor
//! every Linux desktop provides is copied in from the build host and read in
//! turn, to the end. Left to the host are the C library, the graphics driver
//! stack, the X and font core, sound, D-Bus, GLib, Wayland and NSS
//! (`HOST_PROVIDED`, after the AppImage project's exclude list). The build
//! fails when a library is still missing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::elf;

/// Libraries every Linux desktop provides and that must come from it. Globs
/// on the file name.
pub const HOST_PROVIDED: &[&str] = &[
    "ld-linux*.so*",
    "libc.so.*",
    "libm.so.*",
    "libmvec.so.*",
    "libdl.so.*",
    "libpthread.so.*",
    "librt.so.*",
    "libresolv.so.*",
    "libutil.so.*",
    "libanl.so.*",
    "libnsl.so.*",
    "libnss_*.so.*",
    "libthread_db.so.*",
    "libBrokenLocale.so.*",
    "libcidn.so.*",
    "libstdc++.so.*",
    "libgcc_s.so.*",
    "libGL.so.*",
    "libEGL.so.*",
    "libGLX.so.*",
    "libGLdispatch.so.*",
    "libOpenGL.so.*",
    "libGLESv2.so.*",
    "libglapi.so.*",
    "libgbm.so.*",
    "libdrm.so.*",
    "libvulkan.so.*",
    "libX11.so.*",
    "libX11-xcb.so.*",
    "libxcb.so.*",
    "libfontconfig.so.*",
    "libfreetype.so.*",
    "libharfbuzz.so.*",
    "libexpat.so.*",
    "libz.so.*",
    "libuuid.so.*",
    "libasound.so.*",
    "libdbus-1.so.*",
    "libglib-2.0.so.*",
    "libgobject-2.0.so.*",
    "libgio-2.0.so.*",
    "libgmodule-2.0.so.*",
    "libgthread-2.0.so.*",
    "libwayland-client.so.*",
    "libwayland-cursor.so.*",
    "libwayland-egl.so.*",
    "libwayland-server.so.*",
    "libnss3.so",
    "libnssutil3.so",
    "libsmime3.so",
    "libnspr4.so",
    "libplc4.so",
    "libplds4.so",
    "libcups.so.*",
    "libcom_err.so.*",
    "libgpg-error.so.*",
];

/// Where a Linux build host keeps its libraries, after `ldconfig -p`.
pub const LIB_DIRS: &[&str] = &[
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/lib64",
    "/usr/lib64",
    "/lib",
    "/usr/lib",
    "/usr/local/lib",
];

/// A shell glob of `*` and `?` against a whole name.
pub fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// Whether the target machine supplies `name` itself.
pub fn host_provided(name: &str) -> bool {
    HOST_PROVIDED.iter().any(|p| glob(p, name))
}

/// The host's 64 bit libraries by file name, as `ldconfig -p` lists them.
pub fn ldconfig_index() -> BTreeMap<String, PathBuf> {
    for exe in ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"] {
        let Ok(out) = Command::new(exe).arg("-p").output() else {
            continue;
        };
        let mut index = BTreeMap::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some((left, target)) = line.split_once(" => ") else {
                continue;
            };
            if !left.contains("x86-64") {
                continue;
            }
            if let Some(name) = left.split_whitespace().next() {
                index
                    .entry(name.to_string())
                    .or_insert_with(|| PathBuf::from(target.trim()));
            }
        }
        return index;
    }
    BTreeMap::new()
}

/// Where the build host keeps `name`, or `None`.
pub fn find_host_library(name: &str, index: &BTreeMap<String, PathBuf>, dirs: &[&str]) -> Option<PathBuf> {
    if let Some(hit) = index.get(name).filter(|p| p.is_file()) {
        return Some(hit.clone());
    }
    dirs.iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file() && elf::is_elf(p))
}

fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let meta = std::fs::symlink_metadata(&p);
        if meta.as_ref().is_ok_and(|m| m.is_dir()) {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Every file and symlink under `root`, sorted, symlinks not followed.
pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// What the AppDir links that neither it nor every desktop supplies:
/// library name to the files that link it.
pub fn unresolved(appdir: &Path, deps: &dyn Fn(&Path) -> Vec<String>) -> BTreeMap<String, Vec<String>> {
    let all = files(appdir);
    let provided: BTreeSet<String> = all
        .iter()
        .filter_map(|p| p.file_name()?.to_str().map(str::to_string))
        .filter(|n| n.contains(".so"))
        .collect();
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for binary in all
        .iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()) && elf::is_elf(p))
    {
        let rel = binary
            .strip_prefix(appdir)
            .unwrap_or(binary)
            .to_string_lossy()
            .replace('\\', "/");
        for name in deps(binary) {
            if provided.contains(&name) || host_provided(&name) {
                continue;
            }
            missing.entry(name).or_default().push(rel.clone());
        }
    }
    missing
}

/// Copies into `libdir` every library the AppDir links and the host may lack,
/// following what each copy links in turn. Returns the libraries copied and
/// what is still missing after.
#[allow(clippy::type_complexity)]
pub fn bundle(
    appdir: &Path,
    libdir: &Path,
    find: &dyn Fn(&str) -> Option<PathBuf>,
    deps: &dyn Fn(&Path) -> Vec<String>,
    log: &mut dyn FnMut(String),
) -> std::io::Result<(BTreeMap<String, PathBuf>, BTreeMap<String, Vec<String>>)> {
    std::fs::create_dir_all(libdir)?;
    let mut copied: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
    loop {
        let mut added = false;
        for name in unresolved(appdir, deps).into_keys() {
            if copied.contains_key(&name) {
                continue;
            }
            let Some(source) = find(&name) else {
                copied.insert(name, None);
                continue;
            };
            let dest = libdir.join(&name);
            std::fs::copy(std::fs::canonicalize(&source)?, &dest)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
            }
            log(format!("    bundled {name}  ({})", source.display()));
            copied.insert(name, Some(source));
            added = true;
        }
        if !added {
            break;
        }
    }
    let copied = copied.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect();
    Ok((copied, unresolved(appdir, deps)))
}

/// The ELF reader as `bundle` and `unresolved` want it: a file that cannot
/// be read links nothing it could tell.
pub fn elf_deps(path: &Path) -> Vec<String> {
    elf::needed(path).unwrap_or_default()
}

/// The message for libraries still missing, or `None` when there are none.
pub fn missing_message(missing: &BTreeMap<String, Vec<String>>) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let lines: Vec<String> = missing
        .iter()
        .map(|(name, users)| {
            format!(
                "{name}  (linked by {})",
                users.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            )
        })
        .collect();
    Some(format!(
        "the AppImage links libraries that are neither bundled nor provided by every Linux desktop, \
         and the build host has none to copy:\n  {}\nInstall them on the build host (see the apt-get \
         line in .github/workflows/build.yml) and build again.",
        lines.join("\n  ")
    ))
}

/// The offset of the squashfs 4.0 file system inside an AppImage: the first
/// `hsqs` superblock whose fields are consistent.
pub fn squashfs_offset(data: &[u8]) -> Option<usize> {
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let mut from = 0;
    while let Some(pos) = data[from..].windows(4).position(|w| w == b"hsqs") {
        let off = from + pos;
        from = off + 1;
        if off + 56 > data.len() {
            continue;
        }
        let sb = &data[off..off + 56];
        let block_size = u32_at(sb, 12);
        let compression = u16_at(sb, 20);
        let block_log = u16_at(sb, 22);
        let (major, minor) = (u16_at(sb, 28), u16_at(sb, 30));
        let bytes_used = u64_at(sb, 40);
        if major == 4
            && minor == 0
            && block_log < 32
            && block_size == 1 << block_log
            && (1..=6).contains(&compression)
            && bytes_used > 0
            && bytes_used <= (data.len() - off) as u64
        {
            return Some(off);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_whole_names() {
        assert!(glob("libc.so.*", "libc.so.6"));
        assert!(!glob("libc.so.*", "libcrypt.so.1"));
        assert!(glob("ld-linux*.so*", "ld-linux-x86-64.so.2"));
        assert!(glob("libnss3.so", "libnss3.so"));
        assert!(!glob("libnss3.so", "libnss3.so.1"));
        assert!(host_provided("libGL.so.1") && !host_provided("libxkbcommon.so.0"));
    }

    #[test]
    fn the_appdir_is_closed_over_what_it_links() {
        let root = std::env::temp_dir().join(format!("corvus-appdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (appdir, host) = (root.join("AppDir"), root.join("host"));
        std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
        std::fs::create_dir_all(&host).unwrap();
        let elf = |p: &Path| std::fs::write(p, b"\x7fELF-fake").unwrap();
        elf(&appdir.join("usr/bin/app"));
        for lib in ["libxkb.so.1", "libdeep.so.2"] {
            elf(&host.join(lib));
        }
        // app links libxkb (copied) and libc (the host's); libxkb links
        // libdeep (copied next round) and libgone (nowhere: missing).
        let deps = |p: &Path| -> Vec<String> {
            match p.file_name().unwrap().to_str().unwrap() {
                "app" => vec!["libxkb.so.1".into(), "libc.so.6".into()],
                "libxkb.so.1" => vec!["libdeep.so.2".into(), "libgone.so.9".into()],
                _ => vec![],
            }
        };
        let find = |name: &str| Some(host.join(name)).filter(|p| p.is_file());
        let mut log = Vec::new();
        let (copied, missing) = bundle(&appdir, &appdir.join("usr/lib"), &find, &deps, &mut |l| log.push(l)).unwrap();
        assert_eq!(copied.keys().collect::<Vec<_>>(), ["libdeep.so.2", "libxkb.so.1"]);
        assert!(appdir.join("usr/lib/libdeep.so.2").is_file());
        assert_eq!(missing.keys().collect::<Vec<_>>(), ["libgone.so.9"]);
        assert_eq!(missing["libgone.so.9"], ["usr/lib/libxkb.so.1"]);
        assert!(missing_message(&missing)
            .unwrap()
            .contains("libgone.so.9  (linked by usr/lib/libxkb.so.1)"));
        assert_eq!(log.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_squashfs_is_found_after_a_decoy() {
        let mut data = vec![0u8; 100];
        data.extend_from_slice(b"hsqs");
        data.extend(vec![0u8; 52]);
        let real = data.len();
        let mut sb = vec![0u8; 56];
        sb[..4].copy_from_slice(b"hsqs");
        sb[12..16].copy_from_slice(&131_072u32.to_le_bytes());
        sb[20..22].copy_from_slice(&4u16.to_le_bytes());
        sb[22..24].copy_from_slice(&17u16.to_le_bytes());
        sb[28..30].copy_from_slice(&4u16.to_le_bytes());
        sb[40..48].copy_from_slice(&200u64.to_le_bytes());
        data.extend(sb);
        data.extend(vec![0u8; 200]);
        assert_eq!(squashfs_offset(&data), Some(real));
        assert_eq!(squashfs_offset(&data[..real]), None);
    }
}
