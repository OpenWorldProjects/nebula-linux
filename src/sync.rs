//! Cloud save sync engine (Galaxy-style).
//!
//! Flow per game, on launch and on exit:
//!   1. Resolve local save dirs (inside the Wine prefix) from GOG metadata.
//!   2. Build a local manifest: relative path -> (sha1, mtime).
//!   3. Fetch the remote manifest from the GOG cloud storage API.
//!   4. Diff the two manifests against the last-synced snapshot (3-way).
//!   5. Download / upload / delete; on real conflicts, ask the user.

use flate2::read::ZlibDecoder;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub sha1: String,
    pub mtime: u64,
}

pub type Manifest = BTreeMap<String, FileEntry>;

#[derive(Debug, Serialize, PartialEq)]
pub enum Action {
    Upload(String),
    Download(String),
    DeleteLocal(String),
    DeleteRemote(String),
    Conflict(String),
}

pub fn build_manifest(root: &Path) -> Result<Manifest, SyncError> {
    let mut m = Manifest::new();
    for e in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !e.file_type().is_file() {
            continue;
        }
        let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let data = std::fs::read(e.path())?;
        let mtime = e
            .metadata()
            .ok()
            .and_then(|md| md.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        m.insert(rel, FileEntry { sha1: hex::encode(Sha1::digest(&data)), mtime });
    }
    Ok(m)
}

/// Three-way diff: `base` is the snapshot from the last successful sync.
pub fn plan(local: &Manifest, remote: &Manifest, base: &Manifest) -> Vec<Action> {
    let mut paths: Vec<&String> = local.keys().chain(remote.keys()).collect();
    paths.sort();
    paths.dedup();
    let sha = |m: &Manifest, p: &String| m.get(p).map(|e| e.sha1.clone());
    let mut out = Vec::new();
    for p in paths {
        let (l, r, b) = (sha(local, p), sha(remote, p), sha(base, p));
        if l == r {
            continue;
        }
        let local_changed = l != b;
        let remote_changed = r != b;
        let a = match (local_changed, remote_changed) {
            (true, false) => if l.is_some() { Action::Upload(p.clone()) } else { Action::DeleteRemote(p.clone()) },
            (false, true) => if r.is_some() { Action::Download(p.clone()) } else { Action::DeleteLocal(p.clone()) },
            _ => Action::Conflict(p.clone()),
        };
        out.push(a);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncDirection {
    TwoWay,
    DownloadOnly,
    UploadOnly,
}

#[derive(Clone, Debug)]
pub struct SaveLocation {
    pub name: String,
    pub local_path: PathBuf,
}

fn timestamps_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| ".".into()).join("nebula/save_timestamps.json")
}

pub fn load_timestamps() -> BTreeMap<String, BTreeMap<String, String>> {
    match std::fs::read(timestamps_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    }
}

pub fn save_timestamps(map: &BTreeMap<String, BTreeMap<String, String>>) -> std::io::Result<()> {
    let path = timestamps_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(tmp, path)
}

/// Retrieve clientId from goggame-<id>.info if present, or query GOG content system.
pub async fn get_client_id(game_id: u64, is_windows: bool, install_dir: Option<&Path>) -> Result<String, String> {
    if let Some(dir) = install_dir {
        let info_path = dir.join(format!("goggame-{game_id}.info"));
        if info_path.is_file() {
            if let Ok(bytes) = std::fs::read(&info_path) {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(cid) = val["clientId"].as_str() {
                        if !cid.is_empty() {
                            return Ok(cid.to_string());
                        }
                    }
                }
            }
        }
    }

    let os = if is_windows { "windows" } else { "linux" };
    let client = reqwest::Client::builder()
        .user_agent("GOGGalaxyCommunicationService/2.0.13.27")
        .build().map_err(|e| e.to_string())?;

    let builds_url = format!("https://content-system.gog.com/products/{game_id}/os/{os}/builds?generation=2");
    let resp = client.get(&builds_url).send().await.map_err(|e| format!("Failed to query GOG builds: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GOG content system returned status {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("Invalid JSON from builds API: {e}"))?;
    let link = json["items"][0]["link"].as_str().ok_or("No build link in manifest")?;

    let meta_resp = client.get(link).send().await.map_err(|e| format!("Failed to download build metadata: {e}"))?;
    let compressed_bytes = meta_resp.bytes().await.map_err(|e| format!("Failed to read metadata bytes: {e}"))?;

    let mut decoder = ZlibDecoder::new(&compressed_bytes[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).map_err(|e| format!("Failed to decompress metadata: {e}"))?;

    let meta: serde_json::Value = serde_json::from_slice(&decompressed).map_err(|e| format!("Invalid metadata JSON: {e}"))?;
    let client_id = meta["clientId"].as_str().ok_or("No clientId in build metadata")?;
    Ok(client_id.to_string())
}

/// Fetch save locations for a game from remote-config and resolve them into local paths.
pub async fn resolve_save_locations(
    game_id: u64,
    client_id: &str,
    is_windows: bool,
    install_dir: Option<&Path>,
    prefix_dir: &Path,
) -> Result<Vec<SaveLocation>, String> {
    let client = reqwest::Client::builder()
        .user_agent("GOGGalaxyCommunicationService/2.0.13.27")
        .build().map_err(|e| e.to_string())?;

    let rc_url = format!("https://remote-config.gog.com/components/galaxy_client/clients/{client_id}?component_version=2.0.45");
    let resp = client.get(&rc_url).send().await.map_err(|e| format!("Failed to query remote config: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Remote config returned status {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("Invalid JSON from remote-config: {e}"))?;

    let os_key = if is_windows { "Windows" } else { "Linux" };
    let storage = &json["content"][os_key]["cloudStorage"];

    if storage["enabled"].as_bool() == Some(false) {
        return Ok(Vec::new());
    }

    let raw_locations = storage["locations"].as_array();
    let mut entries: Vec<(String, String)> = Vec::new();

    if let Some(locs) = raw_locations {
        for l in locs {
            if let (Some(name), Some(loc)) = (l["name"].as_str(), l["location"].as_str()) {
                entries.push((name.to_string(), loc.to_string()));
            }
        }
    }

    if entries.is_empty() {
        if is_windows {
            entries.push((
                "__default".into(),
                format!("%LOCALAPPDATA%/GOG.com/Galaxy/Applications/{client_id}/Storage/Shared/Files"),
            ));
        } else {
            entries.push((
                "__default".into(),
                format!("$HOME/.local/share/GOG.com/Galaxy/Applications/{client_id}/Storage"),
            ));
        }
    }

    let home = dirs::home_dir().unwrap_or_else(|| ".".into());
    let inst_path = install_dir.map(|p| p.to_path_buf()).unwrap_or_else(|| home.join("Games/Nebula").join(game_id.to_string()));

    let mut resolved_locations = Vec::new();

    if is_windows {
        let drive_c = if prefix_dir.join("pfx/drive_c").is_dir() {
            prefix_dir.join("pfx/drive_c")
        } else {
            prefix_dir.join("drive_c")
        };

        let users_dir = drive_c.join("users");
        let mut user_name = "steamuser".to_string();
        if let Ok(entries) = std::fs::read_dir(&users_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != "Public" && !name.starts_with('.') {
                    if name == "steamuser" {
                        user_name = name;
                        break;
                    }
                    user_name = name;
                }
            }
        }
        let user_profile = users_dir.join(user_name);

        for (name, template) in entries {
            let mut path_str = template.replace('\\', "/");
            path_str = path_str.replace("<?INSTALL?>", &inst_path.to_string_lossy());
            path_str = path_str.replace("%INSTALL%", &inst_path.to_string_lossy());
            path_str = path_str.replace("<?SAVED_GAMES?>", &user_profile.join("Saved Games").to_string_lossy());
            path_str = path_str.replace("%USERPROFILE%/Saved Games", &user_profile.join("Saved Games").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_LOCAL?>", &user_profile.join("AppData/Local").to_string_lossy());
            path_str = path_str.replace("%LOCALAPPDATA%", &user_profile.join("AppData/Local").to_string_lossy());
            path_str = path_str.replace("%LocalAppData%", &user_profile.join("AppData/Local").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_LOCAL_LOW?>", &user_profile.join("AppData/LocalLow").to_string_lossy());
            path_str = path_str.replace("%APPDATA%/../LocalLow", &user_profile.join("AppData/LocalLow").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_ROAMING?>", &user_profile.join("AppData/Roaming").to_string_lossy());
            path_str = path_str.replace("%APPDATA%", &user_profile.join("AppData/Roaming").to_string_lossy());
            path_str = path_str.replace("<?DOCUMENTS?>", &user_profile.join("Documents").to_string_lossy());
            path_str = path_str.replace("%USERPROFILE%/Documents", &user_profile.join("Documents").to_string_lossy());
            path_str = path_str.replace("<?USERPROFILE?>", &user_profile.to_string_lossy());
            path_str = path_str.replace("%USERPROFILE%", &user_profile.to_string_lossy());

            let p = PathBuf::from(path_str);
            let _ = std::fs::create_dir_all(&p);
            resolved_locations.push(SaveLocation { name, local_path: p });
        }
    } else {
        for (name, template) in entries {
            let mut path_str = template.replace('\\', "/");
            path_str = path_str.replace("<?INSTALL?>", &inst_path.to_string_lossy());
            path_str = path_str.replace("<?DOCUMENTS?>", &home.join("Documents").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_LOCAL?>", &home.join(".local/share").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_ROAMING?>", &home.join(".config").to_string_lossy());
            path_str = path_str.replace("<?APPLICATION_DATA_LOCAL_LOW?>", &home.join(".config").to_string_lossy());
            path_str = path_str.replace("$HOME", &home.to_string_lossy());

            let p = PathBuf::from(path_str);
            let _ = std::fs::create_dir_all(&p);
            resolved_locations.push(SaveLocation { name, local_path: p });
        }
    }

    Ok(resolved_locations)
}

/// Perform cloud sync for a given game.
pub async fn sync_game(game_id: i32, direction: SyncDirection) -> Result<String, String> {
    let session = crate::auth::Session::load().ok_or("Sign in to GOG first")?
        .ensure_fresh().await.map_err(|e| e.to_string())?;
    let auth_file = crate::download::write_auth(&session)?;
    let helper = crate::download::helper()?;

    let (is_windows, install_dir, title) = if let Ok(installed) = crate::installed::load() {
        if let Some(game) = installed.into_iter().find(|g| g.id == game_id) {
            let dir = game.executable.parent().map(|p| p.to_path_buf());
            (game.windows, dir, game.title)
        } else {
            let g = crate::library::load_cached().into_iter().find(|g| g.id as i32 == game_id);
            let is_win = g.as_ref().map(|g| g.windows).unwrap_or(true);
            let title = g.as_ref().map(|g| g.title.clone()).unwrap_or_else(|| game_id.to_string());
            (is_win, None, title)
        }
    } else {
        (true, None, game_id.to_string())
    };

    let mut prefix_dir = crate::runner::prefix_dir(game_id);
    if !prefix_dir.exists() {
        // Check if Heroic has a prefix for this game title as a fallback
        let home = dirs::home_dir().unwrap_or_default();
        let heroic_pfx = home.join("Games/Heroic/Prefixes").join(&title);
        if heroic_pfx.is_dir() {
            prefix_dir = heroic_pfx;
        }
    }

    let client_id = get_client_id(game_id as u64, is_windows, install_dir.as_deref()).await?;
    let locations = resolve_save_locations(game_id as u64, &client_id, is_windows, install_dir.as_deref(), &prefix_dir).await?;

    if locations.is_empty() {
        return Ok("No cloud save locations configured for this game.".into());
    }

    let mut timestamps_map = load_timestamps();
    let game_entry = timestamps_map.entry(game_id.to_string()).or_default();

    let platform = if is_windows { "windows" } else { "linux" };
    let mut sync_count = 0;

    for loc in locations {
        let ts = game_entry.get(&loc.name).cloned().unwrap_or_else(|| "0".to_string());

        let mut cmd = tokio::process::Command::new(&helper);
        cmd.arg("--auth-config-path").arg(&auth_file)
            .arg("save-sync")
            .arg(&loc.local_path)
            .arg(game_id.to_string())
            .arg("--ts").arg(&ts)
            .arg("--name").arg(&loc.name)
            .arg("--platform").arg(platform);

        match direction {
            SyncDirection::DownloadOnly => { cmd.arg("--skip-upload"); }
            SyncDirection::UploadOnly => { cmd.arg("--skip-download"); }
            SyncDirection::TwoWay => {}
        }

        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output())
            .await.map_err(|_| format!("Sync timed out for location {}", loc.name))?
            .map_err(|e| format!("Failed to run sync tool: {e}"))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Cloud sync error for {}: {err}", loc.name));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(new_ts) = stdout.split_whitespace().last() {
            if new_ts.chars().any(|c| c.is_ascii_digit()) {
                game_entry.insert(loc.name.clone(), new_ts.to_string());
                sync_count += 1;
            }
        }
    }

    let _ = save_timestamps(&timestamps_map);

    Ok(format!("Cloud saves synchronized for {title} ({sync_count} locations updated)."))
}

/// Sync all currently installed games.
pub async fn sync_all_installed() -> Result<usize, String> {
    let installed = crate::installed::load()?;
    let mut count = 0;
    for g in installed {
        if sync_game(g.id, SyncDirection::TwoWay).await.is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(items: &[(&str, &str)]) -> Manifest {
        items.iter().map(|(p, s)| (p.to_string(), FileEntry { sha1: s.to_string(), mtime: 0 })).collect()
    }

    #[test]
    fn local_change_uploads() {
        let a = plan(&m(&[("a", "2")]), &m(&[("a", "1")]), &m(&[("a", "1")]));
        assert_eq!(a, vec![Action::Upload("a".into())]);
    }

    #[test]
    fn remote_change_downloads() {
        let a = plan(&m(&[("a", "1")]), &m(&[("a", "2")]), &m(&[("a", "1")]));
        assert_eq!(a, vec![Action::Download("a".into())]);
    }

    #[test]
    fn both_changed_conflicts() {
        let a = plan(&m(&[("a", "2")]), &m(&[("a", "3")]), &m(&[("a", "1")]));
        assert_eq!(a, vec![Action::Conflict("a".into())]);
    }
    #[test]
    fn deletion_propagates_only_when_other_side_is_unchanged() {
        let base = m(&[("save", "1")]);
        assert_eq!(plan(&m(&[]), &base, &base), vec![Action::DeleteRemote("save".into())]);
        assert_eq!(plan(&base, &m(&[]), &base), vec![Action::DeleteLocal("save".into())]);
        assert_eq!(plan(&m(&[]), &m(&[("save", "2")]), &base), vec![Action::Conflict("save".into())]);
    }

    #[test]
    fn first_sync_and_identical_changes() {
        assert_eq!(plan(&m(&[("save", "1")]), &m(&[]), &m(&[])), vec![Action::Upload("save".into())]);
        assert_eq!(plan(&m(&[]), &m(&[("save", "1")]), &m(&[])), vec![Action::Download("save".into())]);
        assert!(plan(&m(&[("save", "2")]), &m(&[("save", "2")]), &m(&[("save", "1")])).is_empty());
    }

    #[tokio::test]
    async fn reads_client_id_from_info_file() {
        let dir = std::env::temp_dir().join(format!("nebula-clientid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("goggame-1234.info"), r#"{"clientId":"55555"}"#).unwrap();
        let cid = get_client_id(1234, true, Some(&dir)).await.unwrap();
        assert_eq!(cid, "55555");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
