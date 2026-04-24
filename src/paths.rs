use std::path::{Path, PathBuf};

/// Claude Code encodes the absolute CWD by replacing both '/' and '.' with '-'.
pub fn encode_cwd(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

pub fn projects_root() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".claude").join("projects"))
}

pub fn project_dir() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?.canonicalize()?;
    Ok(projects_root()?.join(encode_cwd(&cwd)))
}

/// Resolve a `--project` argument to a project directory under `~/.claude/projects/`.
///
/// Two forms:
/// - path-like (`/abs`, `~/...`, `./x`, or contains `/`): expand to an absolute
///   path (canonicalized when it exists) and encode via [`encode_cwd`].
/// - bare name: match project dirs whose encoded name ends with `-<encoded-name>`
///   — i.e. basename of the worktree. Ambiguous matches are listed, not guessed.
pub fn resolve_project(name: &str) -> anyhow::Result<PathBuf> {
    let root = projects_root()?;
    if name.starts_with('/') || name.starts_with('~') || name.starts_with('.') || name.contains('/')
    {
        let expanded = expand_tilde(name)?;
        // canonicalize if possible (resolves symlinks — matches encode_cwd call sites),
        // else fall back to the expanded path as-is.
        let abs = match std::fs::canonicalize(&expanded) {
            Ok(p) => p,
            Err(_) if expanded.is_absolute() => expanded,
            Err(_) => std::env::current_dir()?.join(expanded),
        };
        return Ok(root.join(encode_cwd(&abs)));
    }
    let suffix: String = std::iter::once('-')
        .chain(name.chars().map(|c| if c == '.' { '-' } else { c }))
        .collect();
    let mut matches: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if fname == &suffix[1..] || fname.ends_with(&suffix) {
                matches.push(p);
            }
        }
    }
    match matches.len() {
        0 => anyhow::bail!(
            "no project matches {name:?} under {}\n  (try a path, or `ls ~/.claude/projects`)",
            root.display()
        ),
        1 => Ok(matches.pop().unwrap()),
        _ => {
            let list: Vec<String> = matches
                .iter()
                .filter_map(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                .collect();
            anyhow::bail!(
                "ambiguous project {name:?}, candidates:\n  {}",
                list.join("\n  ")
            )
        }
    }
}

fn expand_tilde(s: &str) -> anyhow::Result<PathBuf> {
    if let Some(rest) = s.strip_prefix("~/") {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(rest))
    } else if s == "~" {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_slashes_and_dots() {
        let p = Path::new("/home/a.b/c");
        assert_eq!(encode_cwd(p), "-home-a-b-c");
    }

    #[test]
    fn encodes_root() {
        assert_eq!(encode_cwd(Path::new("/")), "-");
    }

    #[test]
    fn encodes_trailing_dot_segment() {
        // Matches Claude Code's quirk: every '.' becomes '-'
        assert_eq!(encode_cwd(Path::new("/a/b.ext")), "-a-b-ext");
    }

    #[test]
    fn encodes_multiple_dots() {
        assert_eq!(encode_cwd(Path::new("/x.y.z/w")), "-x-y-z-w");
    }

    #[test]
    fn resolve_project_path_form_encodes() {
        // Path-like args skip the basename scan and just encode.
        let tmp = std::env::temp_dir().join(format!("cch-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let abs = tmp.canonicalize().unwrap();
        let got = resolve_project(abs.to_str().unwrap()).unwrap();
        let expected_suffix = encode_cwd(&abs);
        assert!(
            got.to_string_lossy().ends_with(&expected_suffix),
            "got {got:?}, expected suffix {expected_suffix}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_project_bare_name_missing_errors() {
        let err = resolve_project("definitely-not-a-real-project-name-xyz-9999")
            .expect_err("expected miss");
        assert!(err.to_string().contains("no project matches"));
    }

    #[test]
    fn encodes_preserves_non_special_chars() {
        assert_eq!(
            encode_cwd(Path::new("/home/user_name/my-proj")),
            "-home-user_name-my-proj"
        );
    }
}
