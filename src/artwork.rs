//! Decode by file signature, never by cache extension. Keep decoding off the UI thread.
use sha1::Digest;
use std::path::Path;

pub fn decode(bytes: &[u8], max_width: u32, max_height: u32) -> Option<image::RgbaImage> {
    let image = image::load_from_memory(bytes).ok()?;
    Some(image.thumbnail(max_width, max_height).into_rgba8())
}

pub fn load(path: &Path, max_width: u32, max_height: u32) -> Option<image::RgbaImage> {
    decode(&std::fs::read(path).ok()?, max_width, max_height)
}

pub fn ui_image(pixels: image::RgbaImage) -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
        pixels.as_raw(), pixels.width(), pixels.height(),
    ))
}

pub async fn fetch(client: &reqwest::Client, url: &str) -> Option<image::RgbaImage> {
    if url.is_empty() { return None; }
    let url = if url.starts_with("//") { format!("https:{url}") } else { url.to_owned() };
    let dir = dirs::cache_dir()?.join("nebula/store_covers");
    let hash = format!("{:x}", sha1::Sha1::digest(url.as_bytes()));
    // Also recover existing PNG files that the old client named .jpg.
    for extension in ["png", "jpg"] {
        if let Some(pixels) = load(&dir.join(format!("{hash}.{extension}")), 960, 540) {
            return Some(pixels);
        }
    }
    let bytes = client.get(&url).send().await.ok()?.error_for_status().ok()?.bytes().await.ok()?;
    let pixels = decode(&bytes, 960, 540)?;
    if std::fs::create_dir_all(&dir).is_ok() {
        let path = dir.join(format!("{hash}.png"));
        let temporary = dir.join(format!("{hash}.tmp"));
        if pixels.save_with_format(&temporary, image::ImageFormat::Png).is_ok() {
            let _ = std::fs::rename(temporary, path);
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_bytes_decode_even_when_cache_name_is_jpeg() {
        let bytes = include_bytes!("../assets/icon-256.png");
        let path = std::env::temp_dir().join(format!("nebula-artwork-test-{}.jpg", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let result = load(&path, 100, 56);
        std::fs::remove_file(path).unwrap();
        let pixels = result.unwrap();
        assert!(pixels.width() <= 100 && pixels.height() <= 56);
        assert!(pixels.as_raw().chunks_exact(4).any(|p| p[3] != 0));
    }
    #[test]
    fn error_responses_are_not_artwork() {
        assert!(decode(b"<html>Bad Gateway</html>", 960, 540).is_none());
    }
}
