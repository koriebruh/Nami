use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn new_opaque_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let raw = hex::encode(bytes);
    let hash = hash_token(&raw);
    (raw, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_token("abc"), hash_token("abc"));
    }

    #[test]
    fn different_tokens_hash_differently() {
        let (raw1, hash1) = new_opaque_token();
        let (raw2, hash2) = new_opaque_token();
        assert_ne!(raw1, raw2);
        assert_ne!(hash1, hash2);
    }
}
