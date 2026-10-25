//! System tray via the StatusNotifierItem D-Bus protocol (pure Rust, no GTK/Qt).
//! Works natively on KDE; GNOME needs the "AppIndicator" shell extension.

use ksni::{menu::StandardItem, MenuItem, Tray, TrayMethods};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub enum TrayEvent {
    Show(Option<String>),
    Sync,
    Quit,
}

struct GalaxyTray {
    tx: Sender<TrayEvent>,
}

impl Tray for GalaxyTray {
    fn id(&self) -> String {
        "nebula".into()
    }
    fn title(&self) -> String {
        "Nebula".into()
    }
    // Supply pixels directly: a missing theme name can make hosts display an ellipsis.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let Ok(img) = image::load_from_memory(include_bytes!("../assets/icon-256.png")) else {
            return vec![];
        };
        [22, 24, 32, 48].into_iter().map(|size| {
            let mut data = img.resize_exact(size, size, image::imageops::FilterType::Lanczos3)
                .into_rgba8().into_vec();
            for pixel in data.chunks_exact_mut(4) { pixel.rotate_right(1); }
            ksni::Icon { width: size as i32, height: size as i32, data }
        }).collect()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayEvent::Show(ksni::current_xdg_activation_token()));
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let item = |label: &str, ev: TrayEvent| {
            StandardItem {
                label: label.into(),
                activate: Box::new(move |t: &mut Self| {
                    let event = match &ev { TrayEvent::Show(_) => TrayEvent::Show(ksni::current_xdg_activation_token()), _ => ev.clone() };
                    let _ = t.tx.send(event);
                }),
                ..Default::default()
            }
            .into()
        };
        vec![
            item("Open Nebula", TrayEvent::Show(None)),
            item("Sync cloud saves", TrayEvent::Sync),
            MenuItem::Separator,
            item("Quit", TrayEvent::Quit),
        ]
    }
}

/// Registers the tray icon. Returns the handle (keep it alive) if a tray host exists.
pub async fn spawn(tx: Sender<TrayEvent>) -> Option<ksni::Handle<impl Tray>> {
    GalaxyTray { tx }.spawn().await.ok()
}
