use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const MAGIC: &[u8] = b"MYSQL_TUI_V1";
const SALT_LEN: usize = 32;

fn argon2() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
}

/// Derive a 32-byte AES key from a password and raw salt bytes.
fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    argon2()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("key derivation failed: {e}"))?;
    Ok(key)
}

/// Generate a new random salt and return its B64 encoding.
pub fn generate_salt() -> String {
    let mut salt = [0u8; SALT_LEN];
    let rand_bytes: [u8; SALT_LEN] = rand::random();
    salt.copy_from_slice(&rand_bytes);
    STANDARD.encode(salt)
}

/// Encrypt plaintext with AES-256-GCM. Returns (nonce_b64, ciphertext_b64).
pub fn encrypt(password: &str, salt_b64: &str, plaintext: &[u8]) -> Result<(String, String), String> {
    let salt = STANDARD
        .decode(salt_b64)
        .map_err(|e| format!("invalid salt encoding: {e}"))?;
    let key_bytes = derive_key(password, &salt)?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    let rand_nonce: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&rand_nonce);

    let mut data = Vec::with_capacity(MAGIC.len() + plaintext.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(plaintext);

    let ciphertext = cipher
        .encrypt(nonce, data.as_ref())
        .map_err(|e| format!("encryption failed: {e}"))?;

    Ok((STANDARD.encode(rand_nonce), STANDARD.encode(ciphertext)))
}

/// Decrypt ciphertext with AES-256-GCM. Verifies the magic prefix before returning.
pub fn decrypt(password: &str, salt_b64: &str, nonce_b64: &str, ciphertext_b64: &str) -> Result<Vec<u8>, String> {
    let salt = STANDARD
        .decode(salt_b64)
        .map_err(|e| format!("invalid salt encoding: {e}"))?;
    let key_bytes = derive_key(password, &salt)?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    let nonce_bytes = STANDARD
        .decode(nonce_b64)
        .map_err(|e| format!("invalid nonce encoding: {e}"))?;
    let ciphertext = STANDARD
        .decode(ciphertext_b64)
        .map_err(|e| format!("invalid ciphertext encoding: {e}"))?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "wrong password or corrupted data".to_string())?;

    if plaintext.len() < MAGIC.len() || &plaintext[..MAGIC.len()] != MAGIC {
        return Err("wrong password or corrupted data".to_string());
    }

    Ok(plaintext[MAGIC.len()..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let password = "my-secret";
        let salt = generate_salt();
        let data = b"hello world config data";

        let (nonce, ct) = encrypt(password, &salt, data).unwrap();
        let decrypted = decrypt(password, &salt, &nonce, &ct).unwrap();
        assert_eq!(decrypted, data);
    }

    #[test]
    fn wrong_password_fails() {
        let salt = generate_salt();
        let (nonce, ct) = encrypt("right", &salt, b"data").unwrap();
        let err = decrypt("wrong", &salt, &nonce, &ct);
        assert!(err.is_err());
    }

    #[test]
    fn different_salts_produce_different_keys() {
        let s1 = generate_salt();
        let s2 = generate_salt();
        let k1 = derive_key("pw", &STANDARD.decode(&s1).unwrap()).unwrap();
        let k2 = derive_key("pw", &STANDARD.decode(&s2).unwrap()).unwrap();
        assert_ne!(k1, k2);
    }
}
