//! Server-side sessions and one-use OIDC login transactions.

use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const LOGIN_TTL_SECONDS: i64 = 600;
pub const SESSION_IDLE_SECONDS: i64 = 1_800;
pub const SESSION_TTL_SECONDS: i64 = 28_800;

const TOKEN_LENGTH: usize = 43;
const MAX_PENDING: i64 = 1_024;
const MAX_SESSIONS: i64 = 4_096;
const CLEANUP_LIMIT: i64 = 128;
const MAX_PROVIDER_TOKEN_BYTES: usize = 32 * 1_024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not open private session storage")]
    Io(#[from] io::Error),
    #[error("session database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("session storage must use a private directory and regular database files")]
    InvalidLocation,
    #[error("session database is incompatible")]
    InvalidSchema,
    #[error("session database is unavailable")]
    Unavailable,
    #[error("session storage capacity reached")]
    Capacity,
    #[error("invalid session data")]
    InvalidData,
    #[error("secure randomness is unavailable")]
    Random,
    #[error("system time is unavailable")]
    Clock,
}

pub struct PendingLogin {
    pub verifier: String,
    pub nonce: String,
    pub return_to: String,
}

pub struct Credentials {
    pub subject: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

pub struct Session {
    pub credentials: Credentials,
    pub csrf_token: String,
}

impl Session {
    pub fn matches_csrf(&self, value: &str) -> bool {
        bool::from(self.csrf_token.as_bytes().ct_eq(value.as_bytes()))
    }
}

pub struct Store {
    db: Mutex<Connection>,
}

impl Store {
    /// Open a database inside a dedicated private directory that already exists.
    pub fn open(path: &Path) -> Result<Self, Error> {
        prepare_location(path)?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_secs(2))?;
        let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::InvalidSchema);
        }
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")?;
        match db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? {
            0 => db.execute_batch(
                "BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS pending_logins (
                    browser_hash BLOB PRIMARY KEY,
                    state_hash BLOB NOT NULL,
                    verifier TEXT NOT NULL,
                    nonce TEXT NOT NULL,
                    return_to TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS pending_login_expiry ON pending_logins (expires_at);
                CREATE TABLE IF NOT EXISTS sessions (
                    cookie_hash BLOB PRIMARY KEY,
                    subject TEXT NOT NULL,
                    access_token TEXT NOT NULL,
                    refresh_token TEXT,
                    csrf_token TEXT NOT NULL,
                    last_seen INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL
                ) WITHOUT ROWID;
                CREATE INDEX IF NOT EXISTS session_expiry ON sessions (expires_at);
                CREATE INDEX IF NOT EXISTS session_activity ON sessions (last_seen);
                CREATE INDEX IF NOT EXISTS session_subject ON sessions (subject);
                PRAGMA user_version=1;
                COMMIT;",
            )?,
            1 => {}
            _ => return Err(Error::InvalidSchema),
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// Issue an independent browser correlation cookie for this authorization flow.
    pub fn begin_login(&self, state: &str, pending: PendingLogin) -> Result<String, Error> {
        if !valid_flow_value(state)
            || !valid_flow_value(&pending.verifier)
            || !valid_flow_value(&pending.nonce)
            || !valid_return_target(&pending.return_to)
        {
            return Err(Error::InvalidData);
        }
        let browser_cookie = random_token()?;
        let browser_hash = hash(&browser_cookie);
        let state_hash = hash(state);
        let now = now()?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM pending_logins WHERE browser_hash IN
             (SELECT browser_hash FROM pending_logins
              WHERE expires_at <= ?1 OR created_at > ?1 LIMIT ?2)",
            params![now, CLEANUP_LIMIT],
        )?;
        let count: i64 =
            tx.query_row("SELECT count(*) FROM pending_logins", [], |row| row.get(0))?;
        if count >= MAX_PENDING {
            // Keep cleanup committed so the next login can prune the next batch.
            tx.commit()?;
            return Err(Error::Capacity);
        }
        tx.execute(
            "INSERT INTO pending_logins
             (browser_hash, state_hash, verifier, nonce, return_to, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                browser_hash.as_slice(),
                state_hash.as_slice(),
                pending.verifier,
                pending.nonce,
                pending.return_to,
                now,
                now + LOGIN_TTL_SECONDS
            ],
        )?;
        tx.commit()?;
        Ok(browser_cookie)
    }

    /// Consume the login exactly once, after matching both browser and provider state.
    pub fn consume_login(&self, cookie: &str, state: &str) -> Result<Option<PendingLogin>, Error> {
        let Some(browser_hash) = cookie_hash(cookie) else {
            return Ok(None);
        };
        if !valid_flow_value(state) {
            return Ok(None);
        }
        let state_hash = hash(state);
        let now = now()?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending = tx
            .query_row(
                "DELETE FROM pending_logins
                 WHERE browser_hash = ?1 AND state_hash = ?2
                   AND expires_at > ?3 AND created_at <= ?3
                 RETURNING verifier, nonce, return_to",
                params![browser_hash.as_slice(), state_hash.as_slice(), now],
                |row| {
                    Ok(PendingLogin {
                        verifier: row.get(0)?,
                        nonce: row.get(1)?,
                        return_to: row.get(2)?,
                    })
                },
            )
            .optional()?;
        tx.commit()?;
        Ok(pending)
    }

    /// Create a new session and invalidate the browser's previous session atomically.
    pub fn create(
        &self,
        credentials: Credentials,
        previous_cookie: Option<&str>,
        access_ttl: Duration,
    ) -> Result<String, Error> {
        let ttl = access_ttl.as_secs().min(SESSION_TTL_SECONDS as u64) as i64;
        if ttl == 0
            || !valid_subject(&credentials.subject)
            || !valid_provider_token(&credentials.access_token)
            || credentials
                .refresh_token
                .as_deref()
                .is_some_and(|value| !valid_provider_token(value))
        {
            return Err(Error::InvalidData);
        }
        let cookie = random_token()?;
        let csrf = random_token()?;
        let digest = hash(&cookie);
        let now = now()?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM sessions WHERE cookie_hash IN
             (SELECT cookie_hash FROM sessions WHERE expires_at <= ?1
              OR last_seen <= ?2 OR last_seen > ?1 LIMIT ?3)",
            params![now, now - SESSION_IDLE_SECONDS, CLEANUP_LIMIT],
        )?;
        if let Some(previous) = previous_cookie.and_then(cookie_hash) {
            tx.execute(
                "DELETE FROM sessions WHERE cookie_hash = ?1",
                [previous.as_slice()],
            )?;
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))?;
        if count >= MAX_SESSIONS {
            tx.commit()?;
            return Err(Error::Capacity);
        }
        tx.execute(
            "INSERT INTO sessions (cookie_hash, subject, access_token, refresh_token, csrf_token,
             last_seen, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                digest.as_slice(),
                credentials.subject,
                credentials.access_token,
                credentials.refresh_token,
                csrf,
                now,
                now + ttl
            ],
        )?;
        tx.commit()?;
        Ok(cookie)
    }

    /// Retrieve an active session and update its idle timestamp.
    pub fn get(&self, cookie: &str) -> Result<Option<Session>, Error> {
        let Some(digest) = cookie_hash(cookie) else {
            return Ok(None);
        };
        let now = now()?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = tx
            .query_row(
                "SELECT subject, access_token, refresh_token, csrf_token, last_seen FROM sessions
                 WHERE cookie_hash = ?1 AND expires_at > ?2 AND last_seen > ?3 AND last_seen <= ?2",
                params![digest.as_slice(), now, now - SESSION_IDLE_SECONDS],
                |row| Ok((read_session(row)?, row.get::<_, i64>(4)?)),
            )
            .optional()?;
        if session
            .as_ref()
            .is_some_and(|(_, last_seen)| *last_seen != now)
        {
            tx.execute(
                "UPDATE sessions SET last_seen = ?2 WHERE cookie_hash = ?1",
                params![digest.as_slice(), now],
            )?;
        }
        tx.commit()?;
        Ok(session.map(|(session, _)| session))
    }

    /// Recheck local revocation after an awaited provider request.
    pub fn is_active(&self, cookie: &str) -> Result<bool, Error> {
        let Some(digest) = cookie_hash(cookie) else {
            return Ok(false);
        };
        let now = now()?;
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions
             WHERE cookie_hash = ?1 AND expires_at > ?2 AND last_seen > ?3 AND last_seen <= ?2)",
            params![digest.as_slice(), now, now - SESSION_IDLE_SECONDS],
            |row| row.get(0),
        )?)
    }

    /// Remove the local session before calling the provider's revocation endpoint.
    pub fn remove(&self, cookie: &str) -> Result<Option<Session>, Error> {
        let Some(digest) = cookie_hash(cookie) else {
            return Ok(None);
        };
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = tx
            .query_row(
                "DELETE FROM sessions WHERE cookie_hash = ?1
                 RETURNING subject, access_token, refresh_token, csrf_token",
                [digest.as_slice()],
                read_session,
            )
            .optional()?;
        tx.commit()?;
        Ok(session)
    }

    /// Revoke all currently issued browser sessions for a verified OIDC subject.
    pub fn revoke_subject(&self, subject: &str) -> Result<usize, Error> {
        if !valid_subject(subject) {
            return Err(Error::InvalidData);
        }
        Ok(self
            .lock()?
            .execute("DELETE FROM sessions WHERE subject = ?1", [subject])?)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, Error> {
        self.db.lock().map_err(|_| Error::Unavailable)
    }
}

fn read_session(row: &Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        credentials: Credentials {
            subject: row.get(0)?,
            access_token: row.get(1)?,
            refresh_token: row.get(2)?,
        },
        csrf_token: row.get(3)?,
    })
}

fn prepare_location(path: &Path) -> Result<(), Error> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(Error::InvalidLocation)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() {
        return Err(Error::InvalidLocation);
    }
    #[cfg(unix)]
    if metadata.mode() & 0o077 != 0 {
        return Err(Error::InvalidLocation);
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    validate_private_file(path)?;
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match validate_private_file(Path::new(&name)) {
            Ok(()) => {}
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn validate_private_file(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(Error::InvalidLocation);
    }
    #[cfg(unix)]
    if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(Error::InvalidLocation);
    }
    Ok(())
}

fn now() -> Result<i64, Error> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Clock)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::Clock)
}

fn random_token() -> Result<String, Error> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn cookie_hash(value: &str) -> Option<[u8; 32]> {
    (value.len() == TOKEN_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then(|| hash(value))
}

fn valid_flow_value(value: &str) -> bool {
    (16..=256).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_return_target(value: &str) -> bool {
    value.starts_with('/')
        && !value.starts_with("//")
        && value.len() <= 2_048
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
}

fn valid_provider_token(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_PROVIDER_TOKEN_BYTES
}

fn valid_subject(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = "test-login-provider-state";
    const TEST_TTL: Duration = Duration::from_secs(3_600);

    fn directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        directory
    }

    fn pending() -> PendingLogin {
        PendingLogin {
            verifier: "test-pkce-verifier-is-a-secret".to_owned(),
            nonce: "test-oidc-random-nonce".to_owned(),
            return_to: "/files/?folder=synthetic".to_owned(),
        }
    }

    fn credentials() -> Credentials {
        Credentials {
            subject: "test-subject".to_owned(),
            access_token: "synthetic-access-token".to_owned(),
            refresh_token: Some("synthetic-refresh-token".to_owned()),
        }
    }

    #[test]
    fn requires_both_login_cookie_and_state_and_consumes_once() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let cookie = store.begin_login(STATE, pending()).unwrap();
        let wrong_cookie = random_token().unwrap();
        assert!(store.consume_login(&wrong_cookie, STATE).unwrap().is_none());
        assert!(
            store
                .consume_login(&cookie, "a-different-provider-state")
                .unwrap()
                .is_none()
        );
        let result = store.consume_login(&cookie, STATE).unwrap().unwrap();
        assert_eq!(result.verifier, pending().verifier);
        assert_eq!(result.nonce, pending().nonce);
        assert_eq!(result.return_to, pending().return_to);
        assert!(store.consume_login(&cookie, STATE).unwrap().is_none());
    }

    #[test]
    fn expired_login_cannot_be_consumed() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let cookie = store.begin_login(STATE, pending()).unwrap();
        store
            .lock()
            .unwrap()
            .execute("UPDATE pending_logins SET expires_at = 0", [])
            .unwrap();
        assert!(store.consume_login(&cookie, STATE).unwrap().is_none());
    }

    #[test]
    fn concurrent_callbacks_have_one_winner_across_connections() {
        let dir = directory();
        let path = dir.path().join("auth.sqlite3");
        let first = Store::open(&path).unwrap();
        let second = Store::open(&path).unwrap();
        let cookie = first.begin_login(STATE, pending()).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                first.consume_login(&cookie, STATE).unwrap().is_some()
            });
            let b = scope.spawn(|| {
                barrier.wait();
                second.consume_login(&cookie, STATE).unwrap().is_some()
            });
            assert_ne!(a.join().unwrap(), b.join().unwrap());
        });
    }

    #[test]
    fn session_rotation_rejects_fixation_and_csrf_reuse() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let previous = store.create(credentials(), None, TEST_TTL).unwrap();
        let previous_csrf = store.get(&previous).unwrap().unwrap().csrf_token;
        let current = store
            .create(credentials(), Some(&previous), TEST_TTL)
            .unwrap();
        assert_ne!(previous, current);
        assert!(!store.is_active(&previous).unwrap());
        assert!(store.get(&previous).unwrap().is_none());
        let session = store.get(&current).unwrap().unwrap();
        assert!(!session.matches_csrf(&previous_csrf));
        assert!(!session.matches_csrf(""));
        assert!(session.matches_csrf(&session.csrf_token));
        let persisted: Vec<u8> = store
            .lock()
            .unwrap()
            .query_row("SELECT cookie_hash FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(persisted, hash(&current));
        assert_eq!(URL_SAFE_NO_PAD.decode(&current).unwrap().len(), 32);
    }

    #[test]
    fn checks_both_idle_and_absolute_expiration() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let idle = store.create(credentials(), None, TEST_TTL).unwrap();
        let absolute = store.create(credentials(), None, TEST_TTL).unwrap();
        let active = store.create(credentials(), None, TEST_TTL).unwrap();
        let now = now().unwrap();
        {
            let db = store.lock().unwrap();
            db.execute(
                "UPDATE sessions SET last_seen = ?1 WHERE cookie_hash = ?2",
                params![now - SESSION_IDLE_SECONDS, hash(&idle).as_slice()],
            )
            .unwrap();
            db.execute(
                "UPDATE sessions SET expires_at = ?1 WHERE cookie_hash = ?2",
                params![now, hash(&absolute).as_slice()],
            )
            .unwrap();
            db.execute(
                "UPDATE sessions SET last_seen = ?1 WHERE cookie_hash = ?2",
                params![now - 10, hash(&active).as_slice()],
            )
            .unwrap();
        }
        assert!(store.get(&idle).unwrap().is_none());
        assert!(store.get(&absolute).unwrap().is_none());
        assert!(!store.is_active(&absolute).unwrap());
        assert!(store.get(&active).unwrap().is_some());
        let last_seen: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT last_seen FROM sessions WHERE cookie_hash = ?1",
                [hash(&active).as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(last_seen >= now);
    }

    #[test]
    fn revocation_persists_after_reopening() {
        let dir = directory();
        let path = dir.path().join("auth.sqlite3");
        let store = Store::open(&path).unwrap();
        let cookie = store.create(credentials(), None, TEST_TTL).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.is_active(&cookie).unwrap());
        let removed = store.remove(&cookie).unwrap().unwrap();
        assert_eq!(removed.credentials.access_token, credentials().access_token);
        assert!(!store.is_active(&cookie).unwrap());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.get(&cookie).unwrap().is_none());
        assert!(store.remove(&cookie).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn database_and_sidecars_are_private_and_symlinks_are_refused() {
        let dir = directory();
        let path = dir.path().join("auth.sqlite3");
        let store = Store::open(&path).unwrap();
        store.create(credentials(), None, TEST_TTL).unwrap();
        for name in ["auth.sqlite3", "auth.sqlite3-wal", "auth.sqlite3-shm"] {
            validate_private_file(&dir.path().join(name)).unwrap();
        }
        drop(store);
        let alias = dir.path().join("alias.sqlite3");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(matches!(Store::open(&alias), Err(Error::InvalidLocation)));
    }

    #[test]
    fn provider_expiry_caps_the_session_lifetime() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let before = now().unwrap();
        let cookie = store.create(credentials(), None, TEST_TTL).unwrap();
        let after = now().unwrap();
        let expires_at: i64 = store
            .lock()
            .unwrap()
            .query_row(
                "SELECT expires_at FROM sessions WHERE cookie_hash = ?1",
                [hash(&cookie).as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        let ttl = TEST_TTL.as_secs() as i64;
        assert!((before + ttl..=after + ttl).contains(&expires_at));
        assert!(matches!(
            store.create(credentials(), None, Duration::ZERO),
            Err(Error::InvalidData)
        ));
    }

    #[test]
    fn revokes_all_sessions_for_one_subject_only() {
        let dir = directory();
        let store = Store::open(&dir.path().join("auth.sqlite3")).unwrap();
        let first = store.create(credentials(), None, TEST_TTL).unwrap();
        let second = store.create(credentials(), None, TEST_TTL).unwrap();
        let third = store
            .create(
                Credentials {
                    subject: "another-test-subject".to_owned(),
                    ..credentials()
                },
                None,
                TEST_TTL,
            )
            .unwrap();
        assert_eq!(store.revoke_subject("test-subject").unwrap(), 2);
        assert!(!store.is_active(&first).unwrap());
        assert!(!store.is_active(&second).unwrap());
        assert!(store.is_active(&third).unwrap());
    }
}
