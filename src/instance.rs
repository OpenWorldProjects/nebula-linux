//! A private local socket forwards subsequent launches to the existing window.
use std::{io::{Read, Write}, os::unix::{fs::PermissionsExt, net::{UnixListener, UnixStream}}, path::PathBuf, sync::mpsc::Sender, time::Duration};

pub struct Instance { listener: UnixListener, path: PathBuf }

impl Instance {
    pub fn acquire() -> std::io::Result<Option<Self>> {
        let dir = dirs::runtime_dir().unwrap_or_else(|| dirs::config_dir().unwrap_or_else(|| ".".into()).join("nebula"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("nebula-instance.sock");
        let token = std::env::var("XDG_ACTIVATION_TOKEN").ok()
            .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok()).unwrap_or_default();
        Self::at(path, &token)
    }

    fn at(path: PathBuf, token: &str) -> std::io::Result<Option<Self>> {
        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(token.as_bytes())?;
                stream.shutdown(std::net::Shutdown::Write)?;
                return Ok(None);
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                // Only remove a stale endpoint, never another file at this location.
                use std::os::unix::fs::FileTypeExt;
                if std::fs::symlink_metadata(&path)?.file_type().is_socket() { std::fs::remove_file(&path)?; }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match UnixListener::bind(&path) {
            Ok(listener) => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                Ok(Some(Self { listener, path }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                let mut stream = UnixStream::connect(&path)?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(token.as_bytes())?;
                stream.shutdown(std::net::Shutdown::Write)?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub fn listen(&self, sender: Sender<crate::tray::TrayEvent>) -> std::io::Result<()> {
        let listener = self.listener.try_clone()?;
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut token = String::new();
                if (&mut stream).take(4096).read_to_string(&mut token).is_ok() {
                    if sender.send(crate::tray::TrayEvent::Show((!token.is_empty()).then_some(token))).is_err() { break; }
                }
            }
        });
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_launch_forwards_and_stale_socket_is_recovered() {
        let path = std::env::temp_dir().join(format!("nebula-instance-test-{}.sock", std::process::id()));
        let first = Instance::at(path.clone(), "").unwrap().unwrap();
        assert!(Instance::at(path.clone(), "test-token").unwrap().is_none());
        let (mut connection, _) = first.listener.accept().unwrap();
        let mut message = String::new();
        connection.read_to_string(&mut message).unwrap();
        assert_eq!(message, "test-token");
        drop(first);
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        drop(Instance::at(path.clone(), "").unwrap().unwrap());
        assert!(!path.exists());
    }
}
