use bridge_core::vault::EncryptionKey;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
pub fn load(existing_database: bool) -> Result<EncryptionKey, String> {
    use security_framework::passwords::{get_generic_password, set_generic_password};
    const SERVICE: &str = "com.geniucker.codex-rc-bridge";
    const ACCOUNT: &str = "desktop-database-key";
    let bytes = match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(error) if error.code() == -25300 && !existing_database => {
            let mut bytes = Zeroizing::new(vec![0; 32]);
            bridge_core::fill_random(&mut bytes).map_err(|_| "Cannot create the master key.")?;
            set_generic_password(SERVICE, ACCOUNT, &bytes)
                .map_err(|_| "Cannot save the master key to Keychain.")?;
            bytes
        }
        Err(_) => {
            return Err("Cannot read the original master key from Keychain. Unlock Keychain or restore the original key and retry.".into());
        }
    };
    let key: &[u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "Invalid Keychain master-key format.")?;
    EncryptionKey::from_bytes(key).map_err(|_| "Cannot load the master key.".into())
}
#[cfg(not(target_os = "macos"))]
pub fn load(_: bool) -> Result<EncryptionKey, String> {
    Err("This desktop build requires macOS Keychain.".into())
}
