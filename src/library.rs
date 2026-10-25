//! GOG library: owned games + cached cover art.

use crate::auth::Session;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub id: u64,
    pub title: String,
    pub slug: String,
    /// Protocol-relative base URL, e.g. `//images-3.gog-statics.com/<hash>`
    pub image: String,
    pub windows: bool,
    pub mac: bool,
    pub linux: bool,
    pub category: String,
}

/// Moves data from the pre-rename `galaxy-linux` directory so users stay logged in.
pub fn migrate_dir(base: Option<PathBuf>) -> PathBuf {
    let base = base.unwrap_or_else(|| PathBuf::from("."));
    let (old, new) = (base.join("galaxy-linux"), base.join("nebula"));
    if old.exists() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
    new
}

fn cache_dir() -> PathBuf {
    migrate_dir(dirs::cache_dir())
}

pub fn cover_path(id: u64) -> PathBuf {
    cache_dir().join("covers").join(format!("{id}_landscape.png"))
}

/// Tiny pre-blurred copy of the cover, used as the soft backdrop on the game page.
pub fn blur_path(id: u64) -> PathBuf {
    cache_dir().join("covers").join(format!("{id}_landscape_blur.png"))
}

fn make_blur(id: u64) {
    let (src, dst) = (cover_path(id), blur_path(id));
    if dst.exists() || !src.exists() {
        return;
    }
    if let Ok(img) = image::open(&src) {
        let small = img.resize(96, 54, image::imageops::FilterType::Triangle).to_rgba8();
        let _ = image::imageops::blur(&small, 5.0).save(dst);
    }
}

fn library_file() -> PathBuf {
    cache_dir().join("library.json")
}

pub fn load_cached() -> Vec<Game> {
    std::fs::read(library_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_cached(games: &[Game]) {
    let p = library_file();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, serde_json::to_vec(games).unwrap_or_default());
}

fn parse_game(p: &serde_json::Value) -> Option<Game> {
    if !p["isGame"].as_bool().unwrap_or(true) {
        return None;
    }
    let works = &p["worksOn"];
    Some(Game {
        id: p["id"].as_u64()?,
        title: p["title"].as_str()?.to_string(),
        slug: p["slug"].as_str().unwrap_or_default().to_string(),
        image: p["image"].as_str().unwrap_or_default().to_string(),
        windows: works["Windows"].as_bool().unwrap_or(false),
        mac: works["Mac"].as_bool().unwrap_or(false),
        linux: works["Linux"].as_bool().unwrap_or(false),
        category: p["category"].as_str().unwrap_or_default().to_string(),
    })
}

/// Fetches every page of the owned-games list (newest purchases first).
pub async fn fetch(session: &Session) -> Result<Vec<Game>, reqwest::Error> {
    let client = reqwest::Client::new();
    let mut games = Vec::new();
    let mut page = 1u64;
    loop {
        let v: serde_json::Value = client
            .get("https://embed.gog.com/account/getFilteredProducts")
            .query(&[("mediaType", "1"), ("sortBy", "date_purchased")])
            .query(&[("page", page)])
            .bearer_auth(&session.access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(items) = v["products"].as_array() {
            games.extend(items.iter().filter_map(parse_game));
        }
        let total = v["totalPages"].as_u64().unwrap_or(1);
        if page >= total {
            break;
        }
        page += 1;
    }
    save_cached(&games);
    Ok(games)
}

/// Downloads any missing covers (8 at a time) and builds blurred backdrops.
/// `progress(done, total)` is called after each cover.
pub async fn download_covers(games: &[Game], progress: impl Fn(usize, usize)) {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().expect("artwork client");
    let sem = Arc::new(Semaphore::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for g in games {
        let path = cover_path(g.id);
        if path.exists() || g.image.is_empty() {
            continue;
        }
        // The account image is landscape artwork. The vertical transform crops its title.
        let base = if g.image.starts_with("//") { format!("https:{}", g.image) } else { g.image.clone() };
        let url = format!("{base}.jpg");
        let (client, sem) = (client.clone(), sem.clone());
        tasks.spawn(async move {
            let _permit = sem.acquire().await.ok()?;
            let bytes = client.get(url).send().await.ok()?.error_for_status().ok()?.bytes().await.ok()?;
            std::fs::create_dir_all(path.parent()?).ok()?;
            let image = image::load_from_memory(&bytes).ok()?.thumbnail(640, 360);
            image.save_with_format(path, image::ImageFormat::Png).ok()
        });
    }
    let total = tasks.len();
    let mut done = 0;
    while tasks.join_next().await.is_some() {
        done += 1;
        progress(done, total);
    }
    for g in games {
        make_blur(g.id);
    }
}
