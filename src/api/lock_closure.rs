// Included by `build.rs` and by `tests/lock_closure.rs`.

/// The entries of `Cargo.lock` that `root` is built from, itself included, in an order of their
/// own. A crate it does not reach cannot change what crosses to a plugin library, so adding or
/// updating one leaves the published plugins good. The whole file when `root` is not in it.
fn lock_closure(lock: &str, root: &str) -> String {
    struct Entry {
        name: String,
        version: String,
        text: String,
        deps: Vec<(String, Option<String>)>,
    }
    let mut entries = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let mut e = Entry {
            name: String::new(),
            version: String::new(),
            text: String::new(),
            deps: Vec::new(),
        };
        let mut in_deps = false;
        for line in block.lines().map(str::trim).filter(|l| !l.is_empty()) {
            // `[metadata]` and `[[patch.unused]]` after the last package belong to none.
            if line.starts_with('[') {
                break;
            }
            e.text.push_str(line);
            e.text.push('\n');
            if in_deps {
                if line == "]" {
                    in_deps = false;
                } else {
                    let mut words = line.trim_matches(|c| c == '"' || c == ',').split(' ');
                    if let Some(name) = words.next() {
                        e.deps.push((name.to_string(), words.next().map(str::to_string)));
                    }
                }
            } else if line == "dependencies = [" {
                in_deps = true;
            } else if let Some(v) = line.strip_prefix("name = ") {
                e.name = v.trim_matches('"').to_string();
            } else if let Some(v) = line.strip_prefix("version = ") {
                e.version = v.trim_matches('"').to_string();
            }
        }
        entries.push(e);
    }
    let mut reached = vec![false; entries.len()];
    let mut todo: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].name == root).collect();
    if todo.is_empty() {
        return lock.replace('\r', "");
    }
    while let Some(i) = todo.pop() {
        if std::mem::replace(&mut reached[i], true) {
            continue;
        }
        for (name, version) in &entries[i].deps {
            todo.extend((0..entries.len()).filter(|&j| {
                !reached[j] && entries[j].name == *name && version.as_ref().is_none_or(|v| *v == entries[j].version)
            }));
        }
    }
    let mut kept: Vec<&str> = entries
        .iter()
        .zip(&reached)
        .filter(|(_, r)| **r)
        .map(|(e, _)| e.text.as_str())
        .collect();
    kept.sort_unstable();
    kept.join("\n")
}
