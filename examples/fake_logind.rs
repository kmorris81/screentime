//! Minimal stand-in for logind on a test bus, for exercising screentimed
//! without a real desktop session. One session for $FAKE_UID; TerminateUser
//! and PowerOff remove it unless $FAKE_KEEP is set, and Lock locks it. With
//! $FAKE_KDE it also plays Plasma's org.kde.Shutdown, whose logout() and
//! logoutAndShutdown() end the session, and org.freedesktop.ScreenSaver.

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
    fn power_off_with_flags(&self, flags: u64) {
        println!("fake-logind: PowerOffWithFlags({flags})");
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
    #[zbus(name = "logoutAndShutdown")]
    fn logout_and_shutdown(&self) {
        println!("fake-kde: logoutAndShutdown()");
        self.alive.store(false, Ordering::SeqCst);
    }
}

struct ScreenSaver {
    locked: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    fn lock(&self) {
        println!("fake-kde: ScreenSaver.Lock()");
        self.locked.store(true, Ordering::SeqCst);
    }
}

struct Session {
    locked: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(property)]
    fn active(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn locked_hint(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn class(&self) -> String {
        "user".into()
    }
    fn lock(&self) {
        println!("fake-logind: Session.Lock()");
        self.locked.store(true, Ordering::SeqCst);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let uid = std::env::var("FAKE_UID").map_or(0, |u| u.parse().unwrap());
    let locked = Arc::new(AtomicBool::new(std::env::var("FAKE_LOCKED").is_ok()));
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
        .serve_at(
            "/org/freedesktop/login1/session/_31",
            Session {
                locked: locked.clone(),
            },
        )?;
    if std::env::var("FAKE_KDE").is_ok() {
        builder = builder
            .name("org.kde.Shutdown")?
            .serve_at("/Shutdown", KdeShutdown { alive })?
            .name("org.freedesktop.ScreenSaver")?
            .serve_at(
                "/ScreenSaver",
                ScreenSaver {
                    locked: locked.clone(),
                },
            )?;
    }
    let _conn = builder.build().await?;
    println!(
        "fake-logind: ready (uid {uid}, locked {})",
        locked.load(Ordering::SeqCst)
    );
    std::future::pending().await
}
