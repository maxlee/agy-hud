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

#[test]
fn marketplace_manifest_skills_and_scripts_meet_strict_spec() {
    let root = workspace_root();
    let manifest_raw = fs::read_to_string(root.join("plugin.json")).expect("read root plugin.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_raw).expect("parse plugin.json");

    // 1. Version parity between Cargo.toml (HUD_VERSION) and plugin.json
    assert_eq!(
        manifest["version"].as_str(),
        Some(hud_core::types::HUD_VERSION),
        "plugin.json version must stay strictly in sync with Cargo.toml HUD_VERSION"
    );

    // 2. Official Antigravity Marketplace manifest requirements
    assert_eq!(
        manifest["$schema"].as_str(),
        Some("https://antigravity.google/schemas/v1/plugin.json")
    );
    let name = manifest["name"].as_str().expect("plugin.json name");
    assert!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--"),
        "plugin.json name must be lowercase kebab-case: {name}"
    );

    let display_name = manifest["displayName"]
        .as_str()
        .expect("plugin.json displayName");
    for forbidden_prefix in ["Antigravity", "Gemini", "Claude", "Cursor"] {
        assert!(
            !display_name.starts_with(forbidden_prefix),
            "displayName must not start with restricted brand prefix {forbidden_prefix}: {display_name}"
        );
    }

    let desc = manifest["description"]
        .as_str()
        .expect("plugin.json description");
    assert!(
        (80..=200).contains(&desc.chars().count()),
        "Marketplace description should be concise (80..=200 chars), got {}",
        desc.chars().count()
    );

    let logo_rel = manifest["logo"].as_str().expect("plugin.json logo");
    let logo_svg = fs::read_to_string(root.join(logo_rel)).expect("read bundled logo asset");
    assert!(
        logo_svg.contains("viewBox=\"0 0 128 128\""),
        "Bundled SVG logo must declare a 1:1 >=128x128 viewBox"
    );

    let prompts = manifest["suggestedPrompts"]
        .as_array()
        .expect("suggestedPrompts array");
    assert!(
        (1..=3).contains(&prompts.len()),
        "Marketplace allows 1..=3 suggestedPrompts"
    );
    assert!(
        prompts
            .iter()
            .all(|p| p.as_str().is_some_and(|s| !s.trim().is_empty()))
    );

    let author_url = manifest["author"]["url"]
        .as_str()
        .expect("plugin.json author.url");
    assert!(author_url.starts_with("https://"));
    assert!(
        manifest["homepage"]
            .as_str()
            .is_some_and(|s| s.starts_with("https://"))
    );
    assert!(
        manifest["repository"]
            .as_str()
            .is_some_and(|s| s.starts_with("https://"))
    );
    assert!(manifest["license"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        manifest["keywords"]
            .as_array()
            .is_some_and(|k| !k.is_empty())
    );

    // 3. Every skill directory name must match its SKILL.md YAML frontmatter `name:`
    let skills_root = root.join("skills");
    for entry in fs::read_dir(&skills_root).expect("read skills/").flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let skill_md =
            fs::read_to_string(path.join("SKILL.md")).expect("every skill dir needs SKILL.md");
        let frontmatter = skill_md
            .strip_prefix("---\n")
            .or_else(|| skill_md.strip_prefix("---\r\n"))
            .and_then(|rest| rest.split("\n---").next())
            .expect("SKILL.md must start with YAML frontmatter");
        let declared_name = frontmatter
            .lines()
            .find_map(|l| l.trim().strip_prefix("name:"))
            .map(str::trim)
            .expect("SKILL.md frontmatter must declare `name:`");
        assert_eq!(
            declared_name, dir_name,
            "SKILL.md frontmatter name `{declared_name}` must match folder `skills/{dir_name}`"
        );
    }

    // 4. Executable bits (`0755`) on helper scripts and any bundled `scripts/bin/<plat>/agy-hud`
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for rel in [
            "scripts/agy-hud",
            "scripts/sync-marketplace-bins.sh",
            "skills/hud/scripts/hud-cli.sh",
            "skills/hud-update/scripts/hud-cli.sh",
        ] {
            let p = root.join(rel);
            let mode = fs::metadata(&p)
                .unwrap_or_else(|_| panic!("missing required executable script {rel}"))
                .permissions()
                .mode();
            assert_ne!(
                mode & 0o111,
                0,
                "{rel} must have executable permissions (0755) for Marketplace packaging"
            );
        }

        let bin_root = root.join("scripts/bin");
        if bin_root.is_dir() {
            for ent in fs::read_dir(&bin_root).unwrap().flatten() {
                if ent.path().is_dir() {
                    let cand = ent.path().join("agy-hud");
                    if cand.is_file() {
                        let mode = fs::metadata(&cand).unwrap().permissions().mode();
                        assert_ne!(
                            mode & 0o111,
                            0,
                            "{} must retain 0755 executable bit",
                            cand.display()
                        );
                    }
                }
            }
        }
    }
}
