//! Small filesystem helpers shared across persistence modules.

use std::path::Path;

use anyhow::{Context, Result};

/// Write `contents` to `path` atomically: write to a sibling `.tmp` then rename,
/// so a crash mid-write never leaves a half-written file at `path`.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}
