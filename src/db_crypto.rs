//! 本机数据库加密密钥（B6）。
//!
//! 密钥为 32 字节随机数，使用 Windows DPAPI（当前用户范围）加密后保存为
//! `数据目录/db.key`；数据库本身由 SQLCipher 透明加密。密钥只能由同一
//! Windows 用户解密，因此不需要每次启动输入密码，也不会把明文密钥落盘。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, CryptProtectData, CryptUnprotectData, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};

/// 密钥文件名（位于数据目录下）。
pub const KEY_FILE: &str = "db.key";
/// DPAPI 保护后的密钥文件标识（防止误把其它文件当密钥）。
const KEY_MAGIC: &[u8] = b"CLIPNEST-DBKEY-1";

/// 密钥文件路径。
pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(KEY_FILE)
}

/// 是否存在数据库密钥文件。
pub fn key_exists(data_dir: &Path) -> bool {
    key_path(data_dir).is_file()
}

/// 生成新的 32 字节随机密钥，返回小写十六进制字符串（64 个字符）。
pub fn generate_key_hex() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(hex(&bytes))
}

/// 读取并解密密钥；文件不存在时返回 `Ok(None)`。
pub fn load_key_hex(data_dir: &Path) -> io::Result<Option<String>> {
    let path = key_path(data_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let protected = fs::read(&path)?;
    if protected.len() <= KEY_MAGIC.len() || &protected[..KEY_MAGIC.len()] != KEY_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "数据库密钥文件格式不正确",
        ));
    }
    let plain = unprotect(&protected[KEY_MAGIC.len()..])?;
    let text = String::from_utf8(plain)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "数据库密钥不是有效文本"))?;
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "数据库密钥长度不正确",
        ));
    }
    Ok(Some(text))
}

/// 用 DPAPI 保护并原子写入密钥。
pub fn save_key_hex(data_dir: &Path, key_hex: &str) -> io::Result<()> {
    if key_hex.len() != 64 || !key_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "数据库密钥长度不正确",
        ));
    }
    fs::create_dir_all(data_dir)?;
    let protected = protect(key_hex.as_bytes())?;
    let mut payload = Vec::with_capacity(KEY_MAGIC.len() + protected.len());
    payload.extend_from_slice(KEY_MAGIC);
    payload.extend_from_slice(&protected);
    let path = key_path(data_dir);
    let tmp = path.with_extension("key.tmp");
    fs::write(&tmp, &payload)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// 删除密钥文件（关闭加密时调用）。
pub fn delete_key(data_dir: &Path) -> io::Result<()> {
    let path = key_path(data_dir);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// 数据库文件是否是 SQLCipher 密文（非 SQLite 明文头）。
pub fn is_encrypted_file(db_path: &Path) -> bool {
    let mut header = [0u8; 16];
    match fs::File::open(db_path).and_then(|mut f| {
        use std::io::Read;
        f.read_exact(&mut header)
    }) {
        Ok(()) => header != *b"SQLite format 3\0",
        Err(_) => false,
    }
}

fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes = take_blob(output);
    Ok(bytes)
}

fn unprotect(protected: &[u8]) -> io::Result<Vec<u8>> {
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: protected.len() as u32,
        pbData: protected.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(output))
}

fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() || blob.cbData == 0 {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(blob.pbData as *mut core::ffi::c_void);
    }
    bytes
}

fn hex(bytes: &[u8]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(CHARS[(byte >> 4) as usize] as char);
        out.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip_is_dpapi_protected() {
        let dir = std::env::temp_dir().join(format!(
            "clipnest-dbkey-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let key = generate_key_hex().unwrap();
        assert_eq!(key.len(), 64);
        save_key_hex(&dir, &key).unwrap();
        assert_eq!(load_key_hex(&dir).unwrap().as_deref(), Some(key.as_str()));
        let raw = std::fs::read(key_path(&dir)).unwrap();
        assert!(raw.windows(key.len()).all(|w| w != key.as_bytes()));
        delete_key(&dir).unwrap();
        assert!(!key_exists(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_header_detection() {
        let dir = std::env::temp_dir().join(format!(
            "clipnest-dbhead-{}-{}",
            std::process::id(),
            crate::model::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain.db");
        std::fs::write(&plain, b"SQLite format 3\0rest").unwrap();
        assert!(!is_encrypted_file(&plain));
        let encrypted = dir.join("enc.db");
        std::fs::write(
            &encrypted,
            [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        )
        .unwrap();
        assert!(is_encrypted_file(&encrypted));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
