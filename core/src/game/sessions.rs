//! Game session registry (Zaap `service.instances`, main.js L98512-98685): `hash` (UUIDv4, also the
//! Thrift `gameSession`) → instance. Instance ids come from a process-wide counter starting at 1 that is
//! never reused.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

static NEXT_INSTANCE_ID: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, PartialEq, Eq)]
pub struct GameSession {
    /// Secret: never log.
    pub hash: String,
    pub instance_id: u32,
    pub account_id: Option<i64>,
    pub release: String,
    /// Pid of the spawned child (`cmd.exe` in `zaap-start` mode).
    pub pid: Option<u32>,
    /// Pid of the real `Dofus.exe` once identified (equals `pid` for direct spawns).
    pub game_pid: Option<u32>,
    /// The game called `connect`.
    pub connected: bool,
    /// The game called `release_restartOnExit`.
    pub restart_on_exit: bool,
}

impl std::fmt::Debug for GameSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSession")
            .field("hash", &"<redacted>")
            .field("instance_id", &self.instance_id)
            .field("account_id", &self.account_id)
            .field("release", &self.release)
            .field("pid", &self.pid)
            .field("game_pid", &self.game_pid)
            .field("connected", &self.connected)
            .field("restart_on_exit", &self.restart_on_exit)
            .finish()
    }
}

/// Thread-safe, cheaply clonable (shared) registry.
#[derive(Clone, Default)]
pub struct GameSessions {
    inner: Arc<Mutex<HashMap<String, GameSession>>>,
}

impl GameSessions {
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> MutexGuard<'_, HashMap<String, GameSession>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Register a new session with a fresh hash and instance id.
    pub fn create(&self, release: &str, account_id: Option<i64>) -> GameSession {
        let s = GameSession {
            hash: uuid::Uuid::new_v4().to_string(),
            instance_id: NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
            account_id,
            release: release.to_string(),
            pid: None,
            game_pid: None,
            connected: false,
            restart_on_exit: false,
        };
        self.lock().insert(s.hash.clone(), s.clone());
        s
    }
    pub fn get(&self, hash: &str) -> Option<GameSession> {
        self.lock().get(hash).cloned()
    }
    fn update(&self, hash: &str, f: impl FnOnce(&mut GameSession)) -> bool {
        self.lock().get_mut(hash).map(f).is_some()
    }
    pub fn set_pid(&self, hash: &str, pid: u32) -> bool {
        self.update(hash, |s| s.pid = Some(pid))
    }
    pub fn set_game_pid(&self, hash: &str, pid: u32) -> bool {
        self.update(hash, |s| s.game_pid = Some(pid))
    }
    /// `connect`: returns the session if the hash is known.
    pub fn mark_connected(&self, hash: &str) -> Option<GameSession> {
        let mut m = self.lock();
        let s = m.get_mut(hash)?;
        s.connected = true;
        Some(s.clone())
    }
    pub fn set_restart_on_exit(&self, hash: &str) -> bool {
        self.update(hash, |s| s.restart_on_exit = true)
    }
    pub fn remove(&self, hash: &str) -> Option<GameSession> {
        self.lock().remove(hash)
    }
    pub fn list(&self) -> Vec<GameSession> {
        let mut v: Vec<_> = self.lock().values().cloned().collect();
        v.sort_by_key(|s| s.instance_id);
        v
    }
    /// Accounts currently running `release` (for picking the next free account).
    pub fn running_account_ids(&self, release: &str) -> Vec<i64> {
        self.lock()
            .values()
            .filter(|s| s.release == release)
            .filter_map(|s| s.account_id)
            .collect()
    }
    pub fn len(&self) -> usize {
        self.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lifecycle() {
        let r = GameSessions::new();
        let a = r.create("dofus3", Some(7));
        let b = r.create("dofus3", None);
        assert!(b.instance_id > a.instance_id && a.instance_id >= 1);
        assert_eq!(uuid::Uuid::parse_str(&a.hash).unwrap().get_version_num(), 4);
        assert!(!format!("{a:?}").contains(&a.hash));
        assert!(r.mark_connected(&a.hash).unwrap().connected);
        assert!(r.mark_connected("nope").is_none());
        assert_eq!(r.running_account_ids("dofus3"), vec![7]);
        r.remove(&a.hash);
        assert!(r.get(&a.hash).is_none());
        assert_eq!(r.len(), 1);
    }
}
