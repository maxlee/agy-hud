use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for ent in entries.flatten() {
        let p = ent.path();
        let name = ent.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), ".git" | "target") {
            continue;
        }
        if p.is_dir() {
            collect_files_recursive(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn strip_comments_and_strings(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            out.push('"');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push('"');
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[test]
fn zero_nodejs_or_typescript_files_remain_in_repository() {
    let root = workspace_root();
    let mut all_files = Vec::new();
    collect_files_recursive(&root, &mut all_files);

    for path in all_files {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        assert_ne!(
            name,
            "package.json",
            "Legacy Node file must not exist: {}",
            path.display()
        );
        assert_ne!(
            name,
            "package-lock.json",
            "Legacy Node lockfile must not exist: {}",
            path.display()
        );
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            assert!(
                !matches!(ext, "js" | "mjs" | "cjs" | "ts" | "tsx"),
                "Found non-Rust JS/TS source file: {}",
                path.display()
            );
        }
    }
}

#[test]
fn hud_core_enforces_forbid_unsafe_and_zero_io_capabilities() {
    let root = workspace_root();
    let core_src = root.join("crates/hud-core/src");
    let cli_src = root.join("crates/hud-cli/src");

    // 1. Both crate roots must declare `#![forbid(unsafe_code)]`
    for entry in [
        core_src.join("lib.rs"),
        cli_src.join("lib.rs"),
        cli_src.join("main.rs"),
    ] {
        let text = fs::read_to_string(&entry).expect("read crate entry");
        assert!(
            text.contains("#![forbid(unsafe_code)]"),
            "Missing #![forbid(unsafe_code)] in {}",
            entry.display()
        );
    }

    // 2. hud-core/Cargo.toml must depend ONLY on serde and serde_json
    let core_toml = fs::read_to_string(root.join("crates/hud-core/Cargo.toml")).unwrap();
    let in_deps_section = core_toml
        .split("[dependencies]")
        .nth(1)
        .unwrap_or("")
        .split('[')
        .next()
        .unwrap_or("");
    let dep_names: BTreeSet<&str> = in_deps_section
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split(['=', '.']).next())
        .map(str::trim)
        .collect();
    assert_eq!(
        dep_names,
        BTreeSet::from(["serde", "serde_json"]),
        "hud-core must only depend on serde and serde_json"
    );

    // 3. Scan every .rs file in hud-core/src for forbidden OS/FS/Process/Net/Env APIs
    let forbidden_tokens = [
        "std::fs",
        "std::process",
        "std::net",
        "std::env",
        "std::os",
        "File::",
        "Command::",
        "TcpStream",
        "UdpSocket",
        "UnixStream",
    ];

    for entry in fs::read_dir(&core_src).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let raw = fs::read_to_string(&path).unwrap();
        let scrubbed = strip_comments_and_strings(&raw);
        for tok in forbidden_tokens {
            assert!(
                !scrubbed.contains(tok),
                "Forbidden capability `{tok}` referenced inside pure crate file {}",
                path.display()
            );
        }
    }
}

#[test]
fn architecture_md_mermaid_diagram_matches_hud_core_module_graph() {
    let root = workspace_root();
    let arch_md = fs::read_to_string(root.join("ARCHITECTURE.md")).expect("read ARCHITECTURE.md");

    let mermaid_body = arch_md
        .split("```mermaid")
        .nth(1)
        .and_then(|s| s.split("```").next())
        .expect("Expected ```mermaid block in ARCHITECTURE.md");

    // Parse CORE subgraph boxes: id["Label: file1.rs file2.rs"]
    let after_core = mermaid_body
        .split("subgraph CORE")
        .nth(1)
        .expect("Expected subgraph CORE in ARCHITECTURE.md");
    let core_block: String = after_core
        .lines()
        .take_while(|l| l.trim() != "end")
        .collect::<Vec<_>>()
        .join("\n");

    let mut file_to_box: BTreeMap<String, String> = BTreeMap::new();
    let mut core_boxes: BTreeSet<String> = BTreeSet::new();

    for raw_line in core_block.lines() {
        let line = raw_line.trim();
        let Some(bracket_idx) = line.find("[\"") else {
            continue;
        };
        let Some(end_idx) = line.rfind("\"]") else {
            continue;
        };
        let box_id = line[..bracket_idx].trim().to_string();
        let label = &line[bracket_idx + 2..end_idx];
        let Some((_, files_part)) = label.split_once(':') else {
            continue;
        };
        core_boxes.insert(box_id.clone());
        for f in files_part.split_whitespace() {
            let prev = file_to_box.insert(f.to_string(), box_id.clone());
            assert!(
                prev.is_none(),
                "File {f} listed in multiple CORE boxes in ARCHITECTURE.md"
            );
        }
    }

    // Verify every crates/hud-core/src/*.rs file is accounted for
    let core_src = root.join("crates/hud-core/src");
    let actual_files: BTreeSet<String> = fs::read_dir(&core_src)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let documented_files: BTreeSet<String> = file_to_box.keys().cloned().collect();
    assert_eq!(
        actual_files, documented_files,
        "Every crates/hud-core/src/*.rs file must appear in ARCHITECTURE.md subgraph CORE"
    );

    // Parse declared CORE -> CORE edges in Mermaid diagram
    let mut declared_edges: BTreeSet<(String, String)> = BTreeSet::new();
    for raw_line in mermaid_body.lines() {
        let line = raw_line.trim();
        let Some((lhs, rhs)) = line.split_once("-->") else {
            continue;
        };
        let from = lhs.trim();
        if !core_boxes.contains(from) {
            continue;
        }
        for to in rhs.split('&').map(str::trim) {
            if core_boxes.contains(to) {
                declared_edges.insert((from.to_string(), to.to_string()));
            }
        }
    }

    // Observe actual `pub mod <m>;` and `use crate::<m>` edges across crates/hud-core/src/*.rs
    let mut observed_edges: BTreeSet<(String, String)> = BTreeSet::new();
    for file_name in &actual_files {
        let from_box = file_to_box.get(file_name).unwrap();
        let src = fs::read_to_string(core_src.join(file_name)).unwrap();
        // Exclude `#[cfg(test)] mod tests` tail when computing production module dependencies
        let prod_src = src.split("#[cfg(test)]").next().unwrap_or(&src);
        for line in prod_src.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("pub mod ")
                && let Some(mod_name) = rest.strip_suffix(';')
            {
                let target_file = format!("{}.rs", mod_name.trim());
                if let Some(to_box) = file_to_box.get(&target_file)
                    && to_box != from_box
                {
                    observed_edges.insert((from_box.clone(), to_box.clone()));
                }
            } else if let Some(rest) = trimmed.strip_prefix("use crate::") {
                let mod_name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let target_file = format!("{mod_name}.rs");
                if let Some(to_box) = file_to_box.get(&target_file)
                    && to_box != from_box
                {
                    observed_edges.insert((from_box.clone(), to_box.clone()));
                }
            }
        }
    }

    assert_eq!(
        observed_edges, declared_edges,
        "Observed module dependency edges in hud-core must match ARCHITECTURE.md Mermaid arrows"
    );
}
