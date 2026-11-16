//! Encryption key for the history database: OS keyring, file fallback.
//!
//! Residual risk: the key lives in ordinary memory for the process lifetime and
//! is not zeroized (no zeroize crate is allowed by the dependency policy).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::StoreError;

const SERVICE: &str = "com.clipboardhistory.app";
const USER: &str = "history-key";

/// The 32-byte database key and where it came from.
pub struct KeyInfo {
    pub key: [u8; 32],
    /// The key lives in a plain file (weaker than the OS keyring).
    pub from_file: bool,
}

pub trait KeyProvider {
    /// Returns the existing key. A new key is generated only when `allow_create`
    /// is true (the caller has no database yet); otherwise a missing key is
    /// `KeyUnavailable` and nothing is written anywhere.
    fn key(&self, allow_create: bool) -> Result<KeyInfo, StoreError>;
}

/// A fixed key, for tests.
pub struct StaticKey(pub [u8; 32]);

impl KeyProvider for StaticKey {
    fn key(&self, _allow_create: bool) -> Result<KeyInfo, StoreError> {
        Ok(KeyInfo { key: self.0, from_file: false })
    }
}

/// Creates `dir` (and parents); a newly created leaf gets mode 0700 on unix.
pub(super) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let existed = dir.exists();
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    if !existed {
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
}

/// A key in a file (mode 0600 on unix), created on first use.
pub struct FileKey {
    pub path: PathBuf,
}

impl KeyProvider for FileKey {
    fn key(&self, allow_create: bool) -> Result<KeyInfo, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Ok(KeyInfo { key: decode_key(&s)?, from_file: true }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !allow_create {
                    return Err(no_key_for_existing_db());
                }
                let key = random_key()?;
                self.create(&key)?;
                Ok(KeyInfo { key, from_file: true })
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl FileKey {
    /// Writes a temp file (0600) and renames it into place, so a crash never
    /// leaves a partial key file.
    fn create(&self, key: &[u8; 32]) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            create_private_dir(dir)?;
        }
        let mut tmp_name = self.path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        f.write_all(hex::encode(key).as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn no_key_for_existing_db() -> StoreError {
    StoreError::KeyUnavailable("no key found for the existing history database".into())
}

fn decode_key(s: &str) -> Result<[u8; 32], StoreError> {
    hex::decode(s.trim())
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok())
        .ok_or_else(|| StoreError::KeyUnavailable("stored key is not 64 hex characters".into()))
}

/// 32 random bytes from the OS CSPRNG.
fn random_key() -> Result<[u8; 32], StoreError> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|e| StoreError::KeyUnavailable(format!("random generator failed: {e}")))?;
    Ok(key)
}

/// Why a keyring call failed, as far as the fallback decision cares.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RingFault {
    /// There is no keyring backend at all (no secret service / session bus).
    NoBackend,
    /// Anything else: locked, timed out, bad value. Possibly transient.
    Other(String),
}

/// The slice of the OS keyring the decision logic needs.
pub(super) trait Ring {
    fn get(&self) -> Result<Option<String>, RingFault>;
    fn set(&self, value: &str) -> Result<(), RingFault>;
}

/// Decides where the key comes from.
///
/// 1. A key file that exists wins; the keyring is not consulted (sticky).
/// 2. A key in the keyring is used; a malformed one is an error, never replaced.
/// 3. No key anywhere: with `allow_create` (no database yet) generate one into
///    the keyring, or into the key file when there is no keyring backend. With
///    an existing database, fail without writing anything: a key made now could
///    never open it, and would shadow the real one on the next launch.
/// 4. Only `NoBackend` falls back to a key file. Every other keyring error is
///    `KeyUnavailable`: a transient outage must not mint a new key.
pub(super) fn resolve(
    file: &FileKey,
    ring: &dyn Ring,
    allow_create: bool,
) -> Result<KeyInfo, StoreError> {
    if file.path.exists() {
        return file.key(allow_create);
    }
    let unavailable = |m: String| StoreError::KeyUnavailable(format!("OS keyring: {m}"));
    match ring.get() {
        Ok(Some(s)) => Ok(KeyInfo { key: decode_key(&s)?, from_file: false }),
        Ok(None) | Err(RingFault::NoBackend) if !allow_create => Err(no_key_for_existing_db()),
        Ok(None) => {
            let key = random_key()?;
            match ring.set(&hex::encode(key)) {
                Ok(()) => Ok(KeyInfo { key, from_file: false }),
                Err(RingFault::NoBackend) => file.key(true),
                Err(RingFault::Other(m)) => Err(unavailable(m)),
            }
        }
        Err(RingFault::NoBackend) => file.key(true),
        Err(RingFault::Other(m)) => Err(unavailable(m)),
    }
}

/// The OS keyring, falling back to `fallback` only when no keyring backend exists.
pub struct OsKeyring {
    pub fallback: FileKey,
}

impl KeyProvider for OsKeyring {
    fn key(&self, allow_create: bool) -> Result<KeyInfo, StoreError> {
        resolve(&self.fallback, &RealRing, allow_create)
    }
}

struct RealRing;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod real {
    use super::{Ring, RingFault, RealRing, SERVICE, USER};

    /// keyring 3.6.3 maps a locked collection, a dismissed prompt and "no
    /// result" to `NoStorageAccess` (transient: not a missing backend). A
    /// missing session bus or service becomes `PlatformFailure` wrapping
    /// secret-service's `Error::Unavailable` (Debug text exactly "Unavailable";
    /// that crate is not ours to depend on, so the text is matched), or a D-Bus
    /// method error naming `ServiceUnknown` / `NameHasNoOwner`. Everything
    /// else (timeouts, other platform failures) stays `Other`. A wrong
    /// NoBackend is safe: key creation only happens with a brand-new database,
    /// and an existing database with no key is refused without writing anything.
    pub(super) fn classify(e: keyring::Error) -> RingFault {
        if let keyring::Error::PlatformFailure(inner) = &e {
            let text = format!("{inner:?}");
            let missing = text == "Unavailable"
                || text.contains("org.freedesktop.DBus.Error.ServiceUnknown")
                || text.contains("org.freedesktop.DBus.Error.NameHasNoOwner")
                // dbus-secret-service (the real path for sync-secret-service):
                // unreachable bus socket, or dbus-launch cannot start.
                || text.contains("org.freedesktop.DBus.Error.FileNotFound")
                || text.contains("org.freedesktop.DBus.Error.Spawn.ExecFailed");
            if missing {
                return RingFault::NoBackend;
            }
        }
        RingFault::Other(e.to_string())
    }

    fn entry() -> Result<keyring::Entry, RingFault> {
        keyring::Entry::new(SERVICE, USER).map_err(classify)
    }

    impl Ring for RealRing {
        fn get(&self) -> Result<Option<String>, RingFault> {
            match entry()?.get_password() {
                Ok(s) => Ok(Some(s)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(classify(e)),
            }
        }

        fn set(&self, value: &str) -> Result<(), RingFault> {
            entry()?.set_password(value).map_err(classify)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl Ring for RealRing {
    fn get(&self) -> Result<Option<String>, RingFault> {
        let _ = (SERVICE, USER);
        Err(RingFault::NoBackend)
    }

    fn set(&self, _: &str) -> Result<(), RingFault> {
        Err(RingFault::NoBackend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeRing {
        get: Result<Option<String>, RingFault>,
        set: Result<(), RingFault>,
        stored: RefCell<Option<String>>,
        consulted: RefCell<bool>,
    }

    impl FakeRing {
        fn new(get: Result<Option<String>, RingFault>, set: Result<(), RingFault>) -> Self {
            Self { get, set, stored: RefCell::new(None), consulted: RefCell::new(false) }
        }
    }

    impl Ring for FakeRing {
        fn get(&self) -> Result<Option<String>, RingFault> {
            *self.consulted.borrow_mut() = true;
            match &self.get {
                Ok(v) => Ok(v.clone()),
                Err(RingFault::NoBackend) => Err(RingFault::NoBackend),
                Err(RingFault::Other(m)) => Err(RingFault::Other(m.clone())),
            }
        }

        fn set(&self, value: &str) -> Result<(), RingFault> {
            *self.consulted.borrow_mut() = true;
            match &self.set {
                Ok(()) => {
                    *self.stored.borrow_mut() = Some(value.to_owned());
                    Ok(())
                }
                Err(RingFault::NoBackend) => Err(RingFault::NoBackend),
                Err(RingFault::Other(m)) => Err(RingFault::Other(m.clone())),
            }
        }
    }

    fn file_in(dir: &tempfile::TempDir) -> FileKey {
        FileKey { path: dir.path().join("history.key") }
    }

    #[test]
    fn existing_key_file_wins_without_consulting_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        let first = file.key(true).unwrap();
        let ring = FakeRing::new(Err(RingFault::Other("down".into())), Ok(()));
        let info = resolve(&file, &ring, false).unwrap();
        assert_eq!((info.key, info.from_file), (first.key, true));
        assert!(!*ring.consulted.borrow());
    }

    #[test]
    fn transient_keyring_error_without_file_is_key_unavailable_and_makes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        for allow in [true, false] {
            let ring = FakeRing::new(Err(RingFault::Other("locked".into())), Ok(()));
            assert!(matches!(resolve(&file, &ring, allow), Err(StoreError::KeyUnavailable(_))));
            assert!(!file.path.exists());
        }
    }

    #[test]
    fn failed_set_password_is_key_unavailable_and_makes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        let ring = FakeRing::new(Ok(None), Err(RingFault::Other("denied".into())));
        assert!(matches!(resolve(&file, &ring, true), Err(StoreError::KeyUnavailable(_))));
        assert!(!file.path.exists());
    }

    #[test]
    fn malformed_keyring_value_is_error_never_fallback_or_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        let ring = FakeRing::new(Ok(Some("not hex".into())), Ok(()));
        assert!(matches!(resolve(&file, &ring, true), Err(StoreError::KeyUnavailable(_))));
        assert!(!file.path.exists());
        assert!(ring.stored.borrow().is_none());
    }

    #[test]
    fn missing_backend_falls_back_to_file_key_when_creation_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        let ring = FakeRing::new(Err(RingFault::NoBackend), Ok(()));
        let info = resolve(&file, &ring, true).unwrap();
        assert!(info.from_file);
        assert!(file.path.exists());
    }

    #[test]
    fn creation_not_allowed_writes_nothing_for_no_entry_or_no_backend() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        for get in [Ok(None), Err(RingFault::NoBackend)] {
            let ring = FakeRing::new(get, Ok(()));
            assert!(matches!(resolve(&file, &ring, false), Err(StoreError::KeyUnavailable(_))));
            assert!(!file.path.exists());
            assert!(ring.stored.borrow().is_none());
        }
    }

    #[test]
    fn no_entry_creates_and_stores_key_in_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(&dir);
        let ring = FakeRing::new(Ok(None), Ok(()));
        let info = resolve(&file, &ring, true).unwrap();
        assert!(!info.from_file);
        assert_eq!(ring.stored.borrow().as_deref(), Some(hex::encode(info.key).as_str()));
        assert!(!file.path.exists());
    }

    #[test]
    fn existing_keyring_key_is_used_even_when_creation_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let ring = FakeRing::new(Ok(Some(hex::encode([7u8; 32]))), Ok(()));
        let info = resolve(&file_in(&dir), &ring, false).unwrap();
        assert_eq!((info.key, info.from_file), ([7u8; 32], false));
    }

    #[cfg(unix)]
    #[test]
    fn new_key_dir_is_0700_and_no_temp_file_remains() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = FileKey { path: dir.path().join("app").join("history.key") };
        file.key(true).unwrap();
        let mode = std::fs::metadata(dir.path().join("app")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(!dir.path().join("app").join("history.key.tmp").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    mod classify {
        use super::super::real::classify;
        use super::super::RingFault;

        #[derive(Debug)]
        struct Unavailable;
        impl std::fmt::Display for Unavailable {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("no secret service provider or dbus session found")
            }
        }
        impl std::error::Error for Unavailable {}

        #[derive(Debug)]
        struct Text(&'static str);
        impl std::fmt::Display for Text {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Text {}

        fn platform(e: impl std::error::Error + Send + Sync + 'static) -> keyring::Error {
            keyring::Error::PlatformFailure(Box::new(e))
        }

        #[test]
        fn unavailable_is_no_backend() {
            assert_eq!(classify(platform(Unavailable)), RingFault::NoBackend);
        }

        #[test]
        fn service_unknown_and_name_has_no_owner_are_no_backend() {
            let a = Text("MethodError(OwnedErrorName(\"org.freedesktop.DBus.Error.ServiceUnknown\"))");
            let b = Text("MethodError(OwnedErrorName(\"org.freedesktop.DBus.Error.NameHasNoOwner\"))");
            assert_eq!(classify(platform(a)), RingFault::NoBackend);
            assert_eq!(classify(platform(b)), RingFault::NoBackend);
        }

        #[test]
        fn dbus_secret_service_connect_and_spawn_failures_are_no_backend() {
            let a = Text("Dbus(D-Bus error: Failed to connect to socket /nonexistent: No such file or directory (org.freedesktop.DBus.Error.FileNotFound))");
            let b = Text("Dbus(D-Bus error: /usr/bin/dbus-launch terminated abnormally without any error message (org.freedesktop.DBus.Error.Spawn.ExecFailed))");
            assert_eq!(classify(platform(a)), RingFault::NoBackend);
            assert_eq!(classify(platform(b)), RingFault::NoBackend);
        }

        #[test]
        fn locked_is_other() {
            let e = keyring::Error::NoStorageAccess(Box::new(Text("object locked")));
            assert!(matches!(classify(e), RingFault::Other(_)));
        }

        #[test]
        fn arbitrary_platform_failure_with_not_found_text_is_other() {
            assert!(matches!(classify(platform(Text("NotFound Address"))), RingFault::Other(_)));
        }
    }
}
