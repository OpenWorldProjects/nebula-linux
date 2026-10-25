//! Local UI preferences. Save atomically before reflecting bookmark changes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Default, Serialize, Deserialize)]
pub struct Preferences {
    pub bookmarks: BTreeSet<i32>,
    /// Preferred Wine/Proton runner name; falls back to the best detected one.
    #[serde(default)]
    pub runner: Option<String>,
}

impl Preferences {
    fn path() -> std::path::PathBuf {
        dirs::config_dir().unwrap_or_else(|| ".".into()).join("nebula/bookmarks.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path()).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)
    }
}
