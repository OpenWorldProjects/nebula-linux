//! User-imported installations. Never moves or deletes game files.
use serde::{Deserialize, Serialize};
use std::{path::{Path, PathBuf}, os::unix::fs::PermissionsExt};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InstalledGame {
    pub id: i32,
    pub title: String,
    pub executable: PathBuf,
    pub windows: bool,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
}

/// Resolve a Windows-style relative path (backslashes, any case) inside `base`.
fn resolve_ci(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    for part in rel.split(['\\', '/']).filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." { return None; }
        let direct = current.join(part);
        if direct.exists() { current = direct; continue; }
        let found = std::fs::read_dir(&current).ok()?.flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        current = found.path();
    }
    Some(current)
}

/// Split a command line into arguments, honouring double quotes.
fn split_args(line: &str) -> Vec<String> {
    let (mut args, mut current, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => { quoted = !quoted; any = true; }
            c if c.is_whitespace() && !quoted => { if any { args.push(std::mem::take(&mut current)); any = false; } }
            c => { current.push(c); any = true; }
        }
    }
    if any { args.push(current); }
    args
}

/// Launch target of a GOG Windows install, read from `goggame-<id>.info`.
pub fn play_task(install_dir: &Path, game_id: u64) -> Result<(PathBuf, Vec<String>, Option<PathBuf>), String> {
    let info = resolve_ci(install_dir, &format!("goggame-{game_id}.info"))
        .ok_or("Game info file not found after download")?;
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(info).map_err(|e| e.to_string())?)
        .map_err(|_| "Game info file is invalid")?;
    let tasks = value["playTasks"].as_array().ok_or("Game info has no launch tasks")?;
    let task = tasks.iter().find(|t| t["isPrimary"].as_bool() == Some(true)).or_else(|| tasks.first())
        .ok_or("Game info has no launch tasks")?;
    let exe = resolve_ci(install_dir, task["path"].as_str().ok_or("Launch task has no path")?)
        .ok_or("Launch executable not found")?;
    let args = split_args(task["arguments"].as_str().unwrap_or(""));
    let workdir = task["workingDir"].as_str().filter(|w| !w.is_empty()).and_then(|w| resolve_ci(install_dir, w));
    Ok((exe, args, workdir))
}

fn registry() -> PathBuf { dirs::config_dir().unwrap_or_else(|| ".".into()).join("nebula/installed.json") }

pub fn load() -> Result<Vec<InstalledGame>, String> {
    match std::fs::read(registry()) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Could not read installed games: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn save(games: &[InstalledGame]) -> Result<(), String> {
    let path = registry();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(games).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

pub fn validate(path: &Path) -> Result<(PathBuf, bool), String> {
    let path = path.canonicalize().map_err(|e| format!("Cannot open executable: {e}"))?;
    let metadata = path.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() { return Err("Choose the game's executable file, not its folder.".into()); }
    let windows = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !windows && metadata.permissions().mode() & 0o111 == 0 {
        return Err("This file is not executable. Select a native game executable or a Windows .exe.".into());
    }
    Ok((path, windows))
}

pub fn import(title: &str, path: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() { return Err("Enter a game title.".into()); }
    let (executable, windows) = validate(Path::new(path.trim()))?;
    let mut games = load()?;
    if games.iter().any(|g| g.executable == executable) { return Err("This executable is already imported.".into()); }
    let gog_ids: Vec<_> = crate::library::load_cached().into_iter().map(|g| g.id as i32).collect();
    let id = (i32::MIN..0).find(|id| !games.iter().any(|g| g.id == *id) && !gog_ids.contains(id))
        .ok_or("No available game identifier")?;
    games.push(InstalledGame { id, title: title.to_owned(), executable, windows, ..Default::default() });
    save(&games)
}

pub fn register_gog(game: &crate::library::Game, path: &Path, args: Vec<String>, workdir: Option<PathBuf>) -> Result<(), String> {
    let (executable, windows) = validate(path).map_err(|e| format!("Files downloaded, but launcher could not be registered: {e}. Use Import game to select its executable."))?;
    let mut games = load()?;
    games.retain(|g| g.id != game.id as i32 && g.executable != executable);
    games.push(InstalledGame { id: game.id as i32, title: game.title.clone(), executable, windows, args, workdir });
    save(&games)
}

pub fn remove(id: i32) -> Result<(), String> {
    let mut games = load()?;
    games.retain(|g| g.id != id);
    save(&games)
}

pub async fn launch(id: i32) -> Result<(), String> {
    let game = load()?.into_iter().find(|g| g.id == id).ok_or("Imported game not found")?;
    launch_game(&game).await
}

async fn launch_game(game: &InstalledGame) -> Result<(), String> {
    let (executable, windows) = validate(&game.executable)?;
    let mut command = if windows {
        let preferred = crate::preferences::Preferences::load().runner;
        let runner = crate::runner::choose(preferred.as_deref())
            .ok_or("No Wine or Proton runner found. Install Wine, or Proton-GE (e.g. via Heroic or ProtonUp-Qt).")?;
        let prefix = crate::runner::prefix_dir(game.id);
        std::fs::create_dir_all(&prefix).map_err(|e| format!("Could not create Wine prefix: {e}"))?;
        crate::runner::command(&runner, &executable, &game.args, &prefix)
    } else {
        let mut command = tokio::process::Command::new(&executable);
        command.args(&game.args);
        command
    };
    command.current_dir(game.workdir.clone().unwrap_or_else(|| executable.parent().unwrap().to_path_buf()));
    let log = dirs::cache_dir().map(|d| d.join("nebula/logs")).and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        std::fs::File::create(d.join(format!("{}.log", game.id))).ok()
    });
    let (out, err) = match log.and_then(|f| f.try_clone().ok().map(|c| (f, c))) {
        Some((a, b)) => (std::process::Stdio::from(a), std::process::Stdio::from(b)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    command.stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    let mut child = command.spawn().map_err(|e| format!("Could not launch {}: {e}", game.title))?;
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{} exited with {status}", game.title)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_native_and_windows_executables_without_running_them() {
        let dir = std::env::temp_dir().join(format!("nebula-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let native = dir.join("a game");
        std::fs::write(&native, "#!/bin/sh\nexit 0\n").unwrap();
        assert!(validate(&native).is_err());
        std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!validate(&native).unwrap().1);
        let windows = dir.join("game.EXE");
        std::fs::write(&windows, b"MZ").unwrap();
        assert!(validate(&windows).unwrap().1);
        assert!(validate(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn launches_paths_with_spaces_in_the_game_directory() {
        let dir = std::env::temp_dir().join(format!("nebula launch test {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("game executable");
        std::fs::write(dir.join("required asset"), "fixture").unwrap();
        std::fs::write(&executable, "#!/bin/sh\n[ -f 'required asset' ]\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let game = InstalledGame { id: -1, title: "Fixture".into(), executable, ..Default::default() };
        launch_game(&game).await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn parses_play_task_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("nebula-task-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Bin")).unwrap();
        std::fs::write(dir.join("Bin/Game.exe"), b"MZ").unwrap();
        std::fs::write(dir.join("goggame-7.info"), r#"{"playTasks":[{"isPrimary":true,"path":"bin\\game.exe","arguments":"-a \"b c\"","workingDir":"BIN"}]}"#).unwrap();
        let (exe, args, wd) = play_task(&dir, 7).unwrap();
        assert!(exe.ends_with("Bin/Game.exe"));
        assert_eq!(args, ["-a", "b c"]);
        assert!(wd.unwrap().ends_with("Bin"));
        assert!(resolve_ci(&dir, "..\\x").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

}
