//! Windows game runners: Proton (preferred) or Wine, discovered like Heroic does.
//! Each game gets its own prefix under `~/Games/Nebula/prefixes/<id>`.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runner {
    pub name: String,
    pub proton: bool,
    /// Proton: the tool directory containing the `proton` script. Wine: the `wine` binary.
    pub path: PathBuf,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Token { Num(u64), Text(String) }

/// "GE-Proton11-10" sorts after "GE-Proton11-9".
fn natural_key(name: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        let digit = c.is_ascii_digit();
        let mut part = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() != digit { break; }
            part.push(c);
            chars.next();
        }
        tokens.push(match (digit, part.parse()) { (true, Ok(n)) => Token::Num(n), _ => Token::Text(part) });
    }
    tokens
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten()
        .map(|e| e.path()).filter(|p| p.is_dir()).collect()
}

pub fn detect() -> Vec<Runner> {
    let home = dirs::home_dir().unwrap_or_default();
    let data = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
    let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    let mut found: Vec<Runner> = Vec::new();

    let mut proton_dirs = Vec::new();
    for root in [home.join(".steam/steam/compatibilitytools.d"), data.join("Steam/compatibilitytools.d"), config.join("heroic/tools/proton")] {
        proton_dirs.extend(subdirs(&root));
    }
    proton_dirs.extend(subdirs(&data.join("Steam/steamapps/common")).into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Proton"))));
    for dir in proton_dirs {
        if dir.join("proton").is_file() {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            found.push(Runner { name, proton: true, path: dir });
        }
    }

    for dir in subdirs(&config.join("heroic/tools/wine")) {
        let binary = dir.join("bin/wine");
        if binary.is_file() {
            found.push(Runner { name: dir.file_name().unwrap().to_string_lossy().into_owned(), proton: false, path: binary });
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        if let Some(binary) = std::env::split_paths(&paths).map(|d| d.join("wine")).find(|p| p.is_file()) {
            found.push(Runner { name: "System Wine".into(), proton: false, path: binary });
        }
    }

    // Steam's ~/.steam/steam symlink makes the same tool appear twice.
    let mut seen = std::collections::HashSet::new();
    found.retain(|r| seen.insert((r.name.clone(), r.proton)));
    found.sort_by(|a, b| {
        let rank = |r: &Runner| if r.name.starts_with("GE-Proton") { 0 } else if r.proton { 1 } else { 2 };
        rank(a).cmp(&rank(b)).then_with(|| natural_key(&b.name).cmp(&natural_key(&a.name)))
    });
    found
}

/// The runner named in preferences if still installed, otherwise the best detected one.
pub fn choose(preferred: Option<&str>) -> Option<Runner> {
    let runners = detect();
    preferred.and_then(|name| runners.iter().find(|r| r.name == name).cloned())
        .or_else(|| runners.into_iter().next())
}

pub fn prefix_dir(game_id: i32) -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Games/Nebula/prefixes").join(game_id.to_string())
}

fn steam_root() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    [home.join(".local/share/Steam"), home.join(".steam/steam")].into_iter()
        .find(|p| p.is_dir()).unwrap_or_else(|| home.join(".local/share/Steam"))
}

pub fn command(runner: &Runner, exe: &Path, args: &[String], prefix: &Path) -> tokio::process::Command {
    if runner.proton {
        let mut command = tokio::process::Command::new(runner.path.join("proton"));
        command.arg("run").arg(exe).args(args)
            .env("STEAM_COMPAT_DATA_PATH", prefix)
            .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam_root());
        command
    } else {
        let mut command = tokio::process::Command::new(&runner.path);
        command.arg(exe).args(args).env("WINEPREFIX", prefix.join("pfx"));
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newer_numbered_versions_sort_first() {
        let mut names = vec!["GE-Proton11-9", "GE-Proton11-10", "GE-Proton10-34"];
        names.sort_by(|a, b| natural_key(b).cmp(&natural_key(a)));
        assert_eq!(names, ["GE-Proton11-10", "GE-Proton11-9", "GE-Proton10-34"]);
    }
    #[test]
    fn proton_and_wine_use_their_own_environment() {
        let proton = Runner { name: "P".into(), proton: true, path: "/tools/P".into() };
        let std_cmd = command(&proton, Path::new("/g/a.exe"), &[], Path::new("/pfx")).as_std().get_program().to_owned();
        assert_eq!(std_cmd, "/tools/P/proton");
        let wine = Runner { name: "W".into(), proton: false, path: "/usr/bin/wine".into() };
        assert_eq!(command(&wine, Path::new("/g/a.exe"), &[], Path::new("/pfx")).as_std().get_program(), "/usr/bin/wine");
    }
}
