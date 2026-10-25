//! Installation through an external heroic-gogdl executable: native Linux builds when
//! available, otherwise the Windows build (run later via Wine/Proton, like Heroic).
//! No cloud-save commands are invoked here.
use crate::{auth, installed, library};
use std::{path::{Path, PathBuf}, os::unix::{fs::OpenOptionsExt, process::CommandExt}, io::Write, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Clone)]
pub struct Plan {
    pub game: library::Game,
    pub platform: &'static str,
    pub helper: PathBuf,
    pub auth_file: PathBuf,
    pub folder: String,
    pub download_size: u64,
    pub disk_size: u64,
}

fn platform_for(game: &library::Game) -> Result<&'static str, String> {
    if game.linux { Ok("linux") } else if game.windows { Ok("windows") }
    else { Err("This game has no Linux or Windows build available.".into()) }
}

pub fn helper() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("NEBULA_GOGDL") { candidates.push(path.into()); }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("gogdl")));
    }
    candidates.push("/opt/Heroic/resources/app.asar.unpacked/build/bin/x64/linux/gogdl".into());
    candidates.into_iter().find(|p: &PathBuf| p.is_file())
        .ok_or("Install heroic-gogdl or set NEBULA_GOGDL to its executable path.".into())
}

pub fn write_auth(session: &auth::Session) -> Result<PathBuf, String> {
    let path = dirs::config_dir().ok_or("No configuration directory")?.join("nebula/gogdl-auth.json");
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let value = serde_json::json!({"46899977096215655": {
        "access_token": session.access_token, "refresh_token": session.refresh_token,
        "user_id": session.user_id, "expires_in": session.expires_at.saturating_sub(now), "loginTime": now
    }});
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    file.write_all(serde_json::to_string(&value).map_err(|e| e.to_string())?.as_bytes()).map_err(|e| e.to_string())?;
    Ok(path)
}

fn parse_info(bytes: &[u8]) -> Result<(String, u64, u64), String> {
    let info: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "Downloader returned invalid installation information")?;
    let folder = info["folder_name"].as_str().ok_or("Missing installation folder")?;
    if folder.is_empty() || folder == "." || folder == ".." || folder.contains(['/', '\\']) {
        return Err("Downloader returned an unsafe installation folder".into());
    }
    let (download, disk) = if let (Some(dl), Some(dk)) = (info["download_size"].as_u64(), info["disk_size"].as_u64().filter(|n| *n > 0)) {
        (dl, dk)
    } else if let Some(size_map) = info["size"].as_object() {
        let mut best = (0, 0);
        for (_lang, entry) in size_map {
            let dl = entry["download_size"].as_u64().unwrap_or(0);
            let dk = entry["disk_size"].as_u64().unwrap_or(0);
            if dk > best.1 {
                best = (dl, dk);
            }
        }
        if best.1 == 0 {
            return Err("No installable files available for this platform".into());
        }
        best
    } else {
        return Err("Missing download size".into());
    };
    Ok((folder.to_owned(), download, disk))
}

pub async fn prepare(id: i32) -> Result<Plan, String> {
    let game = library::load_cached().into_iter().find(|g| g.id as i32 == id).ok_or("Game is not in the GOG library")?;
    let platform = platform_for(&game)?;
    if platform == "windows" && crate::runner::choose(crate::preferences::Preferences::load().runner.as_deref()).is_none() {
        return Err("This is a Windows game and no Wine or Proton runner was found. Install Wine or Proton-GE (e.g. via Heroic or ProtonUp-Qt).".into());
    }
    let helper = helper()?;
    let session = auth::Session::load().ok_or("Sign in to GOG first")?.ensure_fresh().await.map_err(|e| e.to_string())?;
    let auth_file = write_auth(&session)?;
    let mut command = tokio::process::Command::new(&helper);
    command.args(["--auth-config-path"]).arg(&auth_file).args(["info", &game.id.to_string(), "--platform", platform, "--lang", "en", "--skip-dlcs"]);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(90), command.output()).await
        .map_err(|_| "Timed out reading installation information")?.map_err(|e| e.to_string())?;
    if !output.status.success() { return Err("Could not read the installer information. Check your connection and GOG login.".into()); }
    let (folder, download_size, disk_size) = parse_info(&output.stdout)?;
    Ok(Plan { game, platform, helper, auth_file, folder, download_size, disk_size })
}

pub async fn available_space(path: &Path) -> Result<u64, String> {
    let mut existing = path;
    while !existing.exists() { existing = existing.parent().ok_or("Invalid install directory")?; }
    let output = tokio::process::Command::new("df").args(["-B1", "--output=avail", "--"]).arg(existing).output().await.map_err(|e| e.to_string())?;
    if !output.status.success() { return Err("Could not check free disk space".into()); }
    String::from_utf8_lossy(&output.stdout).lines().last().unwrap_or("").trim().parse().map_err(|_| "Could not read free disk space".into())
}

pub fn required_space(plan: &Plan) -> u64 {
    plan.disk_size.saturating_add(plan.download_size).saturating_add(512 * 1024 * 1024)
}

pub fn percent(line: &str) -> Option<f32> {
    let (_, tail) = line.split_once("Progress: ")?;
    tail.split_whitespace().next()?.parse::<f32>().ok().filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 100.0))
}

async fn stop_group(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = tokio::process::Command::new("kill").args(["-TERM", "--", &format!("-{pid}")]).status().await;
        if tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await.is_err() {
            let _ = tokio::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).status().await;
            let _ = child.wait().await;
        }
    }
}

pub async fn install(plan: Plan, base: PathBuf, cancel: Arc<AtomicBool>, progress: impl Fn(f32)) -> Result<(), String> {
    if !base.is_absolute() { return Err("Choose an absolute installation path.".into()); }
    if available_space(&base).await? < required_space(&plan) {
        return Err(format!("Not enough free space. Need at least {:.1} GiB including temporary files and 512 MiB reserve. Choose another disk.", required_space(&plan) as f64 / 1073741824.0));
    }
    let destination = base.join(&plan.folder);
    let marker = base.join(".nebula-download-id");
    if destination.exists() && std::fs::read_to_string(&marker).ok().as_deref() != Some(&plan.game.id.to_string()) {
        return Err("Destination already exists. Choose an empty folder or import the existing game.".into());
    }
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    std::fs::write(&marker, plan.game.id.to_string()).map_err(|e| e.to_string())?;
    let mut command = tokio::process::Command::new(&plan.helper);
    command.arg("--auth-config-path").arg(&plan.auth_file)
        .args(["download", &plan.game.id.to_string(), "--platform", plan.platform, "--lang", "en", "--skip-dlcs", "--max-workers", "2", "--path"]).arg(&base)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    command.as_std_mut().process_group(0);
    let mut child = command.spawn().map_err(|e| format!("Could not start downloader: {e}"))?;
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => { if let Some(value) = percent(&line) { progress(value); } }
                    Ok(None) => break,
                    Err(_) => { stop_group(&mut child).await; return Err("Could not read downloader progress".into()); }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                if cancel.load(Ordering::SeqCst) { stop_group(&mut child).await; return Err("Installation cancelled. Partial files remain for retry.".into()); }
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if cancel.load(Ordering::SeqCst) { return Err("Installation cancelled. Partial files remain for retry.".into()); }
    let linux = plan.platform == "linux";
    if !status.success() || !destination.is_dir() || (linux && !destination.join(".gogdl-linux-manifest").is_file()) {
        return Err("Installation did not complete. Check your connection and available space, then retry.".into());
    }
    if linux {
        installed::register_gog(&plan.game, &destination.join("start.sh"), Vec::new(), None)?;
    } else {
        let (executable, args, workdir) = installed::play_task(&destination, plan.game.id as u64)?;
        installed::register_gog(&plan.game, &executable, args, workdir)?;
    }
    let _ = std::fs::remove_file(marker);
    progress(100.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_folders_and_missing_sizes() {
        assert!(parse_info(br#"{"folder_name":"../other","download_size":12,"disk_size":40}"#).is_err());
        assert!(parse_info(br#"{"folder_name":"Game"}"#).is_err());
        assert_eq!(parse_info(br#"{"folder_name":"Game","download_size":12,"disk_size":40}"#).unwrap(), ("Game".into(),12,40));
    }
    #[test]
    fn reads_only_progress_values() {
        assert_eq!(percent("[PROGRESS] = Progress: 42.50 100/200"), Some(42.5));
        assert_eq!(percent("arbitrary error text"), None);
        assert_eq!(percent("Progress: NaN"), None);
    }
}

/// Use a small native dialog for destination/size confirmation and cancellable progress.
pub async fn interactive(id: i32) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let plan = prepare(id).await?;
    let base = dirs::home_dir().ok_or("No home directory")?.join("Games/Nebula").join(plan.game.id.to_string());
    let runner_note = if plan.platform == "windows" {
        let name = crate::runner::choose(crate::preferences::Preferences::load().runner.as_deref()).map(|r| r.name).unwrap_or_default();
        format!("Windows build — runs with {name}")
    } else { "Native Linux build".to_owned() };
    let message = format!("Install {} ({runner_note})\nDownload: {:.0} MiB · Installed: {:.0} MiB\nRequired free space including reserve: {:.0} MiB\nChoose a parent directory; {} will be created inside it.", plan.game.title, plan.download_size as f64 / 1048576., plan.disk_size as f64 / 1048576., required_space(&plan) as f64 / 1048576., plan.folder);
    let answer = tokio::process::Command::new("zenity")
        .args(["--entry", "--title=Install game", "--width=560", "--no-markup", "--text", &message, "--entry-text"])
        .arg(base).kill_on_drop(true).output().await.map_err(|e| format!("Install dialog needs Zenity: {e}"))?;
    if !answer.status.success() { return Err("Installation cancelled.".into()); }
    let base = PathBuf::from(String::from_utf8_lossy(&answer.stdout).trim());
    let mut dialog = tokio::process::Command::new("zenity")
        .args(["--progress", "--title=Installing game", "--text=Downloading and extracting game files…", "--percentage=0", "--width=440"])
        .stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true)
        .spawn().map_err(|e| e.to_string())?;
    let mut input = dialog.stdin.take().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<f32>();
    let writer = tokio::spawn(async move {
        while let Some(value) = receiver.recv().await {
            if input.write_all(format!("{value:.0}\n").as_bytes()).await.is_err() { break; }
        }
    });
    let cancel = Arc::new(AtomicBool::new(false));
    let operation = install(plan, base, cancel.clone(), move |p| { let _ = sender.send(p); });
    tokio::pin!(operation);
    let result = tokio::select! {
        result = &mut operation => { let _ = dialog.kill().await; result }
        _ = dialog.wait() => { cancel.store(true, Ordering::SeqCst); operation.await }
    };
    writer.abort();
    result
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn downloader_registers_completed_native_game() {
        if std::env::var_os("NEBULA_INSTALL_FIXTURE").is_none() {
            let dir = std::env::temp_dir().join(format!("nebula-install-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "download::integration_tests::downloader_registers_completed_native_game"])
                .env("NEBULA_INSTALL_FIXTURE", &dir).env("XDG_CONFIG_HOME", &dir).status().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(status.success());
            return;
        }
        let dir = PathBuf::from(std::env::var_os("NEBULA_INSTALL_FIXTURE").unwrap());
        let helper = dir.join("fake-gogdl");
        std::fs::write(&helper, "#!/bin/sh\nfor arg do base=\"$arg\"; done\nmkdir -p \"$base/Fixture\"\nprintf '#!/bin/sh\\nexit 0\\n' > \"$base/Fixture/start.sh\"\nchmod +x \"$base/Fixture/start.sh\"\necho '{}' > \"$base/Fixture/.gogdl-linux-manifest\"\necho 'Progress: 100 1/1' >&2\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let game = library::Game { id: 123, title: "Fixture".into(), slug: "fixture".into(), image: String::new(), windows:false, mac:false, linux:true, category:String::new() };
        let plan = Plan { game, platform:"linux", helper, auth_file:dir.join("unused"), folder:"Fixture".into(), download_size:1, disk_size:1 };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(install(plan, dir.join("games"), Arc::new(AtomicBool::new(false)), |_| {})).unwrap();
        let installed = installed::load().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].id, 123);
        assert!(installed[0].executable.ends_with("Fixture/start.sh"));
    }
    #[tokio::test]
    #[ignore = "Requires current GOG login and network; reads metadata only"]
    async fn live_linux_install_metadata() {
        let plan = prepare(1293297882).await.unwrap();
        println!("Native Linux metadata: {} — download {} MiB, disk {} MiB", plan.game.title, plan.download_size / 1048576, plan.disk_size / 1048576);
    }
    #[tokio::test]
    #[ignore = "Requires current GOG login and network; reads metadata only"]
    async fn live_windows_install_metadata() {
        let plan = prepare(1460447406).await.unwrap();
        println!("Windows metadata: {} — download {} MiB, disk {} MiB, folder: {}", plan.game.title, plan.download_size / 1048576, plan.disk_size / 1048576, plan.folder);
        assert_eq!(plan.platform, "windows");
    }
}
