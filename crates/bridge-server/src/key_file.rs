//! Headless Unix server key files; desktop keychain integration remains a separate adapter.
use bridge_core::vault::EncryptionKey;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

type Error = Box<dyn std::error::Error>;

#[cfg(unix)]
fn secure_open(path: &Path, create: bool) -> Result<File, Error> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let info = file.metadata()?;
    // SAFETY: geteuid takes no pointers and does not mutate memory.
    let owner = unsafe { libc::geteuid() };
    if !info.is_file() || info.permissions().mode() & 0o077 != 0 || info.uid() != owner {
        return Err("主密钥须为当前用户拥有的私有普通文件（0600），不接受符号链接".into());
    }
    Ok(file)
}
#[cfg(not(unix))]
fn secure_open(_path: &Path, _create: bool) -> Result<File, Error> {
    Err("此服务器主密钥文件适配器仅支持 Unix；Windows 需使用后续系统凭据适配器".into())
}
pub fn create(path: &Path) -> Result<(), Error> {
    // No fallback key creation on load failure: identity recovery must retain the original key.
    let mut bytes = Zeroizing::new([0; 32]);
    bridge_core::fill_random(bytes.as_mut())?;
    let mut file = secure_open(path, true)?;
    file.write_all(bytes.as_ref())?;
    file.sync_all()?;
    Ok(())
}
pub fn load(path: &Path) -> Result<EncryptionKey, Error> {
    let file = secure_open(path, false)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(33).read_to_end(&mut bytes)?;
    let key: &[u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "主密钥文件必须恰好包含 32 字节")?;
    Ok(EncryptionKey::from_bytes(key)?)
}

/// Server and operator CLI share one database ownership lock, including across network I/O.
#[cfg(unix)]
pub fn lock_database(path: &Path) -> Result<File, Error> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    // Aliases of an existing database must share the same process lock. For a
    // new database resolve its parent, since the file does not exist yet.
    let path = if path.try_exists()? {
        use std::os::unix::fs::MetadataExt;
        let resolved = path.canonicalize()?;
        if resolved.metadata()?.nlink() != 1 {
            return Err("数据库不接受硬链接路径".into());
        }
        resolved
    } else {
        if let Ok(info) = path.symlink_metadata()
            && info.file_type().is_symlink()
        {
            return Err("数据库符号链接的目标不存在".into());
        }
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?
            .join(path.file_name().ok_or("数据库路径无效")?)
    };
    let lock = path.with_extension("admin-lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock)?;
    if !file.metadata()?.is_file() {
        return Err("状态锁文件无效".into());
    }
    // SAFETY: the fd belongs to a live File, and flock keeps no Rust memory references.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("服务器或另一个管理命令正在使用此数据库，请先停止该进程".into());
    }
    Ok(file)
}
#[cfg(not(unix))]
pub fn lock_database(_path: &Path) -> Result<File, Error> {
    Err("此管理入口目前仅支持 Unix".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("bridge-key-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn key_file_never_replaces_existing_key_or_accepts_public_files_and_symlinks() {
        let temp = Temp::new();
        let path = temp.0.join("key");
        create(&path).unwrap();
        let original = Zeroizing::new(std::fs::read(&path).unwrap());
        assert!(load(&path).is_ok());
        assert!(create(&path).is_err());
        assert_eq!(&*original, &std::fs::read(&path).unwrap());
        let link = temp.0.join("link");
        symlink(&path, &link).unwrap();
        assert!(load(&link).is_err());
        assert!(create(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, [1; 33]).unwrap();
        assert!(load(&path).is_err());
        std::fs::write(&path, [1; 31]).unwrap();
        assert!(load(&path).is_err());
        assert!(load(&temp.0.join("missing")).is_err());
    }
    #[test]
    fn admin_process_lock_prevents_overlapping_writers() {
        let temp = Temp::new();
        let path = temp.0.join("state.sqlite");
        let lock = lock_database(&path).unwrap();
        assert!(lock_database(&path).is_err());
        drop(lock);
        assert!(lock_database(&path).is_ok());
    }
    #[test]
    fn database_aliases_share_the_lock_and_hard_links_are_rejected() {
        let temp = Temp::new();
        let path = temp.0.join("state.sqlite");
        std::fs::write(&path, []).unwrap();
        let alias = temp.0.join("alias.sqlite");
        symlink(&path, &alias).unwrap();
        let lock = lock_database(&path).unwrap();
        assert!(lock_database(&alias).is_err());
        drop(lock);
        assert!(lock_database(&alias).is_ok());
        let hard_link = temp.0.join("hard.sqlite");
        std::fs::hard_link(&path, &hard_link).unwrap();
        assert!(lock_database(&hard_link).is_err());
        assert!(lock_database(&path).is_err());
        let dangling = temp.0.join("dangling.sqlite");
        symlink(temp.0.join("missing.sqlite"), &dangling).unwrap();
        assert!(lock_database(&dangling).is_err());
    }
}
