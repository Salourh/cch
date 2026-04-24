//! `cch grep` — full-text search across transcripts.
//!
//! Submodules follow the data/presentation split enforced by the project
//! conventions: `opts` + `matcher` + `snippet` + `scan` feed the data layer,
//! `render` owns all `println!`.

mod matcher;
mod opts;
mod render;
mod scan;
mod snippet;

use std::time::SystemTime;

use crate::session::first_user_prompt;

pub use crate::timebounds::parse_bound;
pub use opts::Opts;

use matcher::Matcher;
use opts::SessionHits;
use scan::{collect_files, scan_file};

pub fn run(opts: Opts) -> anyhow::Result<i32> {
    let matcher = Matcher::build(&opts.pattern, opts.case_sensitive, opts.regex)?;
    let files = collect_files(opts.here, opts.project.as_deref())?;
    let mut results: Vec<SessionHits> = Vec::new();

    // Modes that don't need per-session metadata — avoids the extra
    // first_user_prompt read for each transcript.
    let skip_prompt = opts.files_with_matches || opts.stats;
    for path in files {
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let hits = scan_file(&path, &opts, &matcher)?;
        if hits.is_empty() {
            continue;
        }
        let first_prompt = if skip_prompt {
            None
        } else {
            first_user_prompt(&path)
        };
        results.push(SessionHits {
            path,
            mtime,
            first_prompt,
            hits,
        });
    }

    if results.is_empty() {
        return Ok(1);
    }

    if opts.reverse {
        results.sort_by_key(|r| r.mtime);
    } else {
        results.sort_by_key(|r| std::cmp::Reverse(r.mtime));
    }

    if opts.files_with_matches {
        render::emit_ids(&results);
        return Ok(0);
    }
    if opts.json {
        render::emit_json(&results);
        return Ok(0);
    }
    if opts.stats {
        render::emit_stats(&results);
        return Ok(0);
    }

    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            println!();
        }
        render::print_session(r);
    }
    Ok(0)
}
