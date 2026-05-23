use crate::db::Db;
use crate::notifier::{self, UserChoice};
use crate::converter::{self, ConvertError, ConvertOutput};
use anyhow::Result;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub async fn run(watch_path: PathBuf, db: Arc<Db>, data_dir: PathBuf) -> Result<()> {
    tracing::info!(path = %watch_path.display(), "watching");
    let mut prev = snapshot(&watch_path);

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let curr = snapshot(&watch_path);

        let removed: HashMap<&PathBuf, u64> = prev.iter()
            .filter(|(p, _)| !curr.contains_key(*p))
            .map(|(p, s)| (p, *s))
            .collect();
        let mut added: HashMap<&PathBuf, u64> = curr.iter()
            .filter(|(p, _)| !prev.contains_key(*p))
            .map(|(p, s)| (p, *s))
            .collect();

        if !removed.is_empty() || !added.is_empty() {
            tracing::debug!(
                removed = ?removed.keys().collect::<Vec<_>>(),
                added   = ?added.keys().collect::<Vec<_>>(),
                "snapshot diff"
            );
        }

        // ── sequence-group detection ──────────────────────────────────────────
        // key: (parent, base_stem, to_ext)  value: [(from, to, seq)]
        let mut groups: HashMap<(PathBuf, String, String), Vec<(PathBuf, PathBuf, u32)>> = HashMap::new();
        let mut grouped_to: Vec<PathBuf> = Vec::new();

        for (to, to_size) in &added {
            let from_match = removed.iter().find(|(from, from_size)| {
                *from_size == to_size
                    && from.parent() == to.parent()
                    && from.file_stem() == to.file_stem()
                    && from.extension() != to.extension()
            });
            if let Some((from, _)) = from_match {
                if let Some((base, seq)) = parse_sequence_stem(to.file_stem().unwrap_or(OsStr::new(""))) {
                    let to_ext = to.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
                    let parent = to.parent().unwrap_or(std::path::Path::new("")).to_path_buf();
                    groups.entry((parent, base, to_ext))
                        .or_default()
                        .push(((*from).clone(), (*to).clone(), seq));
                    grouped_to.push((*to).clone());
                }
            }
        }

        // remove grouped entries from `added` so the single-rename loop skips them
        for p in &grouped_to { added.remove(p); }

        for ((parent, base, to_ext), mut members) in groups {
            if members.len() < 2 {
                // put back into added for single-rename processing
                if let Some((_, to, _)) = members.into_iter().next() {
                    if let Some(size) = curr.get(&to) {
                        added.insert(curr.get_key_value(&to).map(|(k,_)| k).unwrap(), *size);
                    }
                }
                continue;
            }
            members.sort_by_key(|(_, _, seq)| *seq);
            let db2 = Arc::clone(&db);
            let dd  = data_dir.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_sequence_merge(members, parent, base, to_ext, db2, dd).await {
                    tracing::error!("sequence merge: {e:#}");
                }
            });
        }

        // ── single-rename loop ────────────────────────────────────────────────
        for (to, to_size) in &added {
            let from_match = removed.iter().find(|(from, from_size)| {
                *from_size == to_size
                    && from.parent() == to.parent()
                    && from.file_stem() == to.file_stem()
                    && from.extension() != to.extension()
            });
            if let Some((from, _)) = from_match {
                let from = (*from).clone();
                let to   = (*to).clone();
                tracing::debug!(from = %from.display(), to = %to.display(), "rename detected");
                let db2 = Arc::clone(&db);
                let dd  = data_dir.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_rename(from, to, db2, dd).await {
                        tracing::error!("rename handler: {e:#}");
                    }
                });
            }
        }

        prev = curr;
    }
}

// ── sequence merge ────────────────────────────────────────────────────────────

async fn handle_sequence_merge(
    frames: Vec<(PathBuf, PathBuf, u32)>,
    parent: PathBuf,
    base: String,
    to_ext: String,
    db: Arc<Db>,
    data_dir: PathBuf,
) -> Result<()> {
    tracing::debug!(base, to_ext, count = frames.len(), "sequence merge");

    // backup each renamed file
    for (from, to, _) in &frames {
        let from_ext = from.extension().and_then(|e| e.to_str()).unwrap_or("bak");
        let backup_path = data_dir.join("backups").join(format!("{}.{}", Uuid::new_v4(), from_ext));
        std::fs::copy(to, &backup_path)?;
        db.insert_backup(from, &backup_path, from_ext, None)?;
    }

    let dest = parent.join(format!("{}.{}", base, to_ext));
    let synthetic_from = parent.join(format!("{}.001.{}", base,
        frames[0].0.extension().and_then(|e| e.to_str()).unwrap_or("img")));

    let choice = notifier::prompt_user(&synthetic_from, &dest, Duration::from_secs(30)).await;
    if choice == UserChoice::Reject {
        tracing::info!("user rejected sequence merge");
        return Ok(());
    }

    let frame_paths: Vec<PathBuf> = frames.iter().map(|(_, to, _)| to.clone()).collect();
    let video_exts = ["mp4", "webm", "avi", "mov", "mkv"];

    let result = if video_exts.contains(&to_ext.as_str()) {
        converter::frames_to_video(&frame_paths, &to_ext, &dest)
    } else {
        converter::frames_to_animated(&frame_paths, &to_ext, &dest)
    };

    match result {
        Ok(()) => {
            for (_, to, _) in &frames { let _ = std::fs::remove_file(to); }
            tracing::info!(dest = %dest.display(), "sequence merged");
        }
        Err(ConvertError::ToolNotFound(t)) => tracing::warn!(tool = t, "tool not found, skipping merge"),
        Err(e) => tracing::error!("merge failed: {e:?}"),
    }
    Ok(())
}

// ── single rename ─────────────────────────────────────────────────────────────

async fn handle_rename(from: PathBuf, to: PathBuf, db: Arc<Db>, data_dir: PathBuf) -> Result<()> {
    tracing::debug!(from = %from.display(), to = %to.display(), "handle_rename");

    // rename-back detection
    if let Some(row) = db.find_active_backup(&to) {
        tracing::info!(path = %to.display(), "rename-back: restoring");
        std::fs::copy(&row.backup_path, &to)?;
        db.mark_restored(row.id)?;
        notifier::notify_info("PowerEXT: file restored", &format!("Restored {}", to.display()));
        return Ok(());
    }

    let from_ext = from.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    let to_ext   = to.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();

    // backup
    let backup_path = data_dir.join("backups")
        .join(format!("{}.{}", Uuid::new_v4(), if from_ext.is_empty() { "bak" } else { &from_ext }));
    tracing::debug!(dst = %backup_path.display(), "backing up");
    std::fs::copy(&to, &backup_path)?;
    db.insert_backup(&from, &backup_path, &from_ext, None)?;

    // prompt
    let choice = notifier::prompt_user(&from, &to, Duration::from_secs(30)).await;
    if choice == UserChoice::Reject {
        tracing::info!(path = %to.display(), "user rejected conversion");
        return Ok(());
    }

    // dispatch
    let dest_dir = to.parent().unwrap_or(std::path::Path::new("."))
        .join(to.file_stem().unwrap_or(OsStr::new("frames")));

    tracing::debug!(from_ext, to_ext, "dispatching conversion");
    match converter::dispatch(&to, &from_ext, &to_ext, &dest_dir) {
        Ok(ConvertOutput::SingleFile(bytes)) => {
            std::fs::write(&to, bytes)?;
            db.set_new_ext(&from, &to_ext)?;
            tracing::info!(from = %from.display(), to = %to.display(), "converted");
        }
        Ok(ConvertOutput::FrameDir) => {
            std::fs::remove_file(&to)?;
            db.set_new_ext(&from, &to_ext)?;
            tracing::info!(dir = %dest_dir.display(), "extracted frames");
            notifier::notify_info("PowerEXT: frames extracted", &format!("{}", dest_dir.display()));
        }
        Ok(ConvertOutput::ExternalFile(out_path)) => {
            if out_path != to { std::fs::rename(&out_path, &to)?; }
            db.set_new_ext(&from, &to_ext)?;
            tracing::info!(from = %from.display(), to = %to.display(), "converted via soffice");
        }
        Err(ConvertError::UnsupportedPair) => {
            notifier::notify_info("PowerEXT: skipped", &format!("No converter .{from_ext} → .{to_ext}"));
            tracing::warn!(from_ext, to_ext, "unsupported pair");
        }
        Err(ConvertError::ToolNotFound(t)) => {
            tracing::warn!(tool = t, from_ext, to_ext, "required tool not found, skipping");
            notifier::notify_info("PowerEXT: tool missing", &format!("{t} not found — install it to convert .{from_ext} → .{to_ext}"));
        }
        Err(ConvertError::ToolFailed(stderr)) => {
            tracing::error!(from_ext, to_ext, stderr, "tool failed");
        }
        Err(e) => tracing::error!("conversion error: {e:?}"),
    }

    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn parse_sequence_stem(stem: &OsStr) -> Option<(String, u32)> {
    let s = stem.to_str()?;
    let dot = s.rfind('.')?;
    let seq: u32 = s[dot + 1..].parse().ok()?;
    Some((s[..dot].to_string(), seq))
}

fn snapshot(root: &PathBuf) -> HashMap<PathBuf, u64> {
    let mut map = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(root) { collect_dir(rd, &mut map); }
    map
}

fn collect_dir(rd: std::fs::ReadDir, map: &mut HashMap<PathBuf, u64>) {
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) { collect_dir(sub, map); }
        } else if let Ok(meta) = entry.metadata() {
            map.insert(path, meta.len());
        }
    }
}
