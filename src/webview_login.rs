//! Embedded GOG login window (runs in a child process: `galaxy-linux --login-webview`).
//!
//! A separate process keeps WebKitGTK's GTK event loop away from the Slint UI, and means the
//! web engine is only loaded while the user is actually logging in. On success the
//! authorization code is printed to stdout; exit code 2 means the window was closed.

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::unix::WindowExtUnix,
    window::WindowBuilder,
};
use wry::{WebViewBuilder, WebViewBuilderExtUnix};

const SUCCESS_PREFIX: &str = "https://embed.gog.com/on_login_success";

fn script(email: &str, password: &str, provider: &str) -> String {
    let e = serde_json::to_string(email).unwrap();
    let p = serde_json::to_string(password).unwrap();
    let v = serde_json::to_string(provider).unwrap();
    format!(
        r#"(function () {{
  const email = {e}, pass = {p}, provider = {v}.toLowerCase();
  let tries = 0, submitted = false;
  const setVal = (el, val) => {{
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(el, val);
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  }};
  const timer = setInterval(() => {{
    if (++tries > 80) return clearInterval(timer);
    if (provider) {{
      const el = [...document.querySelectorAll('a, button, [data-provider], [class*=social]')].find(e =>
        ((e.href || '') + ' ' + e.className + ' ' + (e.dataset.provider || '') + ' ' + e.textContent)
          .toLowerCase().includes(provider));
      if (el) {{ clearInterval(timer); el.click(); }}
      return;
    }}
    const u = document.getElementById('login_username');
    const p = document.getElementById('login_password');
    if (u && p && email && !submitted) {{
      submitted = true;
      setVal(u, email); setVal(p, pass);
      setTimeout(() => {{ const b = document.getElementById('login_login'); if (b) b.click(); }}, 1500);
    }}
  }}, 300);
}})();"#
    )
}

pub fn run(provider: String, read_creds: bool) -> ! {
    let (mut email, mut password) = (String::new(), String::new());
    if read_creds {
        let stdin = std::io::stdin();
        let _ = stdin.read_line(&mut email);
        let _ = stdin.read_line(&mut password);
    }
    let (email, password) = (email.trim_end().to_string(), password.trim_end().to_string());

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("GOG.com login")
        .with_inner_size(LogicalSize::new(480.0, 680.0))
        .build(&event_loop)
        .expect("window");

    let vbox = window.default_vbox().expect("gtk vbox");
    let _webview = WebViewBuilder::new()
        .with_url(crate::auth::login_url())
        .with_initialization_script(script(&email, &password, &provider))
        .with_navigation_handler(|url| {
            if url.starts_with(SUCCESS_PREFIX) {
                if let Some(code) = crate::auth::extract_code(&url) {
                    println!("{code}");
                    std::process::exit(0);
                }
                return false;
            }
            true
        })
        .build_gtk(vbox)
        .expect("webview");

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            std::process::exit(2);
        }
    });
}
