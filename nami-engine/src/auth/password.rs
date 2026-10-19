use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use password_hash::phc::{PasswordHash, SaltString};
use crate::core::error::AppError;

pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate();
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), (*salt).as_bytes())
        .map(|h| h.to_string())
        .map_err(|_| AppError::Validation("could not hash password".into()))
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
    let parsed = PasswordHash::new(hash)
        .map_err(|_| AppError::Validation("invalid password hash".into()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_succeeds() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
    }

    #[test]
    fn wrong_password_fails() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password("wrong password", &hash).unwrap());
    }
}
