//! Minimal stand-in for logind on a test bus, for exercising screentimed
//! without a real desktop session. One session for $FAKE_UID; TerminateUser
//! removes it unless $FAKE_KEEP is set. With $FAKE_KDE it also plays
//! Plasma's org.kde.Shutdown, whose logout() ends the session.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use zbus::zvariant::OwnedObjectPath;

struct Manager {
    uid: u32,
    keep: bool,
    alive: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    fn list_sessions(&self) -> Vec<(String, u32, String, String, OwnedObjectPath)> {
        if !self.alive.load(Ordering::SeqCst) {
            return vec![];
        }
        vec![(
            "1".into(),
            self.uid,
            "test".into(),
            "seat0".into(),
            OwnedObjectPath::try_from("/org/freedesktop/login1/session/_31").unwrap(),
        )]
    }
    fn terminate_user(&self, uid: u32) {
        println!("fake-logind: TerminateUser({uid})");
        if !self.keep {
            self.alive.store(false, Ordering::SeqCst);
        }
    }
}

struct KdeShutdown {
    alive: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.kde.Shutdown")]
impl KdeShutdown {
    // Plasma's method is lowercase; zbus would export "Logout" by default.
    #[zbus(name = "logout")]
    fn logout(&self) {
        println!("fake-kde: logout()");
        self.alive.store(false, Ordering::SeqCst);
    }
}

struct Session {
    locked: bool,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(property)]
    fn active(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn locked_hint(&self) -> bool {
        self.locked
    }
    #[zbus(property)]
    fn class(&self) -> String {
        "user".into()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let uid = std::env::var("FAKE_UID").map_or(0, |u| u.parse().unwrap());
    let locked = std::env::var("FAKE_LOCKED").is_ok();
    let alive = Arc::new(AtomicBool::new(true));
    let mut builder = zbus::connection::Builder::session()?
        .name("org.freedesktop.login1")?
        .serve_at(
            "/org/freedesktop/login1",
            Manager {
                uid,
                keep: std::env::var("FAKE_KEEP").is_ok(),
                alive: alive.clone(),
            },
        )?
        .serve_at("/org/freedesktop/login1/session/_31", Session { locked })?;
    if std::env::var("FAKE_KDE").is_ok() {
        builder = builder
            .name("org.kde.Shutdown")?
            .serve_at("/Shutdown", KdeShutdown { alive })?;
    }
    let _conn = builder.build().await?;
    println!("fake-logind: ready (uid {uid}, locked {locked})");
    std::future::pending().await
}
