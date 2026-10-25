//! Acknowledge/resolve link tokens (R16): `base64url(payload) "."
//! base64url(mac)`, payload `v1|<incident>|<ack|resolve>|<user>|<exp unix>|<nonce>`,
//! MAC = HMAC-SHA256 over the encoded payload under the `SUBKEY_INFO` subkey.
//! Pure: single use is `incidents::action`'s job.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{core::crypto::Cipher, incidents::format::ActionLinks};

/// How long a link works after it is signed.
pub const TOKEN_TTL: chrono::Duration = chrono::Duration::hours(24);
/// HKDF info of the MAC key.
pub const SUBKEY_INFO: &str = "nami/incident-action/v1";
/// Longest decoded payload accepted; longer is `Malformed` before any HMAC work.
pub const MAX_PAYLOAD_BYTES: usize = 256;
/// Random bytes per token, so two links signed in one second differ.
const NONCE_BYTES: usize = 9;
/// Unpadded base64 of a 32-byte MAC.
const MAC_CHARS: usize = 43;

/// What a link does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Acknowledge,
    Resolve,
}

impl Action {
    /// The name in the API (`"acknowledge"` | `"resolve"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Acknowledge => "acknowledge",
            Action::Resolve => "resolve",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Action::Acknowledge => "ack",
            Action::Resolve => "resolve",
        }
    }

    fn from_code(code: &str) -> Option<Action> {
        match code {
            "ack" => Some(Action::Acknowledge),
            "resolve" => Some(Action::Resolve),
            _ => None,
        }
    }
}

/// What a token grants: `action` on `incident_id`, as `user_id`, until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub incident_id: Uuid,
    pub action: Action,
    pub user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

/// Why a token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Malformed,
    BadSignature,
    Expired,
}

fn mac(cipher: &Cipher) -> Hmac<Sha256> {
    let key = Zeroizing::new(cipher.derive_subkey(SUBKEY_INFO));
    Hmac::<Sha256>::new_from_slice(&*key).expect("HMAC takes a key of any length")
}

/// Signs `claims` with a fresh 9-byte random nonce (R16); `Claims` does not
/// carry it. `expires_at` is kept to the second. URL-safe (`[A-Za-z0-9_.-]`).
pub fn sign(cipher: &Cipher, claims: &Claims) -> String {
    let mut nonce = [0u8; NONCE_BYTES];
    rand::thread_rng().fill_bytes(&mut nonce);
    let payload = URL_SAFE_NO_PAD.encode(format!(
        "v1|{}|{}|{}|{}|{}",
        claims.incident_id,
        claims.action.code(),
        claims.user_id,
        claims.expires_at.timestamp(),
        URL_SAFE_NO_PAD.encode(nonce)
    ));
    let mut mac = mac(cipher);
    mac.update(payload.as_bytes());
    format!("{payload}.{}", URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// The claims of `token` if its MAC is valid (checked in constant time before
/// the payload is parsed) and it has not expired at `now`.
pub fn verify(cipher: &Cipher, token: &str, now: DateTime<Utc>) -> Result<Claims, Invalid> {
    let (payload, tag) = token.split_once('.').ok_or(Invalid::Malformed)?;
    // Unpadded base64 of MAX_PAYLOAD_BYTES is (MAX_PAYLOAD_BYTES * 4).div_ceil(3) chars.
    if payload.len() > (MAX_PAYLOAD_BYTES * 4).div_ceil(3) || tag.len() != MAC_CHARS {
        return Err(Invalid::Malformed);
    }
    let tag = URL_SAFE_NO_PAD.decode(tag).map_err(|_| Invalid::Malformed)?;
    let mut mac = mac(cipher);
    mac.update(payload.as_bytes());
    mac.verify_slice(&tag).map_err(|_| Invalid::BadSignature)?;

    let decoded = URL_SAFE_NO_PAD.decode(payload).map_err(|_| Invalid::Malformed)?;
    let text = std::str::from_utf8(&decoded).map_err(|_| Invalid::Malformed)?;
    let fields: Vec<&str> = text.split('|').collect();
    let ["v1", incident, action, user, exp, _nonce] = fields[..] else {
        return Err(Invalid::Malformed);
    };
    let claims = Claims {
        incident_id: incident.parse().map_err(|_| Invalid::Malformed)?,
        action: Action::from_code(action).ok_or(Invalid::Malformed)?,
        user_id: user.parse().map_err(|_| Invalid::Malformed)?,
        expires_at: exp
            .parse()
            .ok()
            .and_then(|secs| DateTime::from_timestamp(secs, 0))
            .ok_or(Invalid::Malformed)?,
    };
    if claims.expires_at <= now {
        return Err(Invalid::Expired);
    }
    Ok(claims)
}

/// SHA-256 of the token, the key that makes it single-use.
pub fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// `{base}/incidents/action?token=...` for each action, as `user_id`,
/// expiring `now + TOKEN_TTL`.
pub fn links(
    cipher: &Cipher,
    ui_base_url: &str,
    incident_id: Uuid,
    user_id: Uuid,
    now: DateTime<Utc>,
) -> ActionLinks {
    let base = ui_base_url.trim_end_matches('/');
    let link = |action| {
        let claims = Claims { incident_id, action, user_id, expires_at: now + TOKEN_TTL };
        format!("{base}/incidents/action?token={}", sign(cipher, &claims))
    };
    ActionLinks { acknowledge: link(Action::Acknowledge), resolve: link(Action::Resolve) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::crypto::{Cipher, TEST_KEY};

    fn claims(action: Action, exp_minutes: i64) -> Claims {
        Claims {
            incident_id: Uuid::from_u128(1),
            action,
            user_id: Uuid::from_u128(2),
            expires_at: DateTime::from_timestamp(1_792_000_000 + exp_minutes * 60, 0).unwrap(),
        }
    }
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_792_000_000, 0).unwrap()
    }

    #[test]
    fn a_signed_token_verifies_to_its_claims() {
        let cipher = Cipher::new(&TEST_KEY);
        let token = sign(&cipher, &claims(Action::Acknowledge, 60));
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)), "URL-safe: {token}");
        assert_eq!(verify(&cipher, &token, now()), Ok(claims(Action::Acknowledge, 60)));
    }

    #[test]
    fn tampered_expired_and_foreign_key_tokens_are_rejected() {
        let cipher = Cipher::new(&TEST_KEY);
        let token = sign(&cipher, &claims(Action::Acknowledge, 60));
        assert_eq!(verify(&cipher, &token, now() + chrono::Duration::minutes(61)), Err(Invalid::Expired));
        let other = Cipher::new(b"another-encryption-key-32bytes!!");
        assert_eq!(verify(&other, &token, now()), Err(Invalid::BadSignature));
        // Swap the action inside the payload, keep the MAC.
        let (payload, mac) = token.split_once('.').unwrap();
        let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        let forged = URL_SAFE_NO_PAD.encode(decoded.replace("|ack|", "|resolve|"));
        assert_eq!(verify(&cipher, &format!("{forged}.{mac}"), now()), Err(Invalid::BadSignature));
        for junk in ["", ".", "a.b", "x", &"A".repeat(5000), &format!("{token}.extra")] {
            assert!(matches!(verify(&cipher, junk, now()), Err(Invalid::Malformed | Invalid::BadSignature)), "{junk}");
        }
    }

    #[test]
    fn the_action_key_is_its_own_subkey() {
        let cipher = Cipher::new(&TEST_KEY);
        assert_ne!(cipher.derive_subkey(SUBKEY_INFO), cipher.derive_subkey("nami/hmac/v1"));
    }

    #[test]
    fn a_payload_past_its_cap_is_malformed_even_when_signed() {
        let cipher = Cipher::new(&TEST_KEY);
        let sign_payload = |payload: &str| {
            let encoded = URL_SAFE_NO_PAD.encode(payload);
            let mut m = mac(&cipher);
            m.update(encoded.as_bytes());
            format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(m.finalize().into_bytes()))
        };
        let good = format!("v1|{}|ack|{}|1792003600|", Uuid::from_u128(1), Uuid::from_u128(2));
        let at_cap = format!("{good}{}", "n".repeat(MAX_PAYLOAD_BYTES - good.len()));
        assert_eq!(verify(&cipher, &sign_payload(&at_cap), now()), Ok(claims(Action::Acknowledge, 60)));
        assert_eq!(verify(&cipher, &sign_payload(&format!("{at_cap}n")), now()), Err(Invalid::Malformed));
    }

    /// The single-use key is the hash of the token's text, so no second
    /// spelling of a valid token may verify: 43 base64 chars carry 258 bits
    /// for a 32-byte MAC, and the 2 spare bits must be zero.
    #[test]
    fn a_mac_spelled_with_other_trailing_bits_is_refused() {
        let cipher = Cipher::new(&TEST_KEY);
        let token = sign(&cipher, &claims(Action::Acknowledge, 60));
        let (payload, tag) = token.split_once('.').unwrap();
        let last = tag.chars().last().unwrap();
        const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let index = ALPHABET.find(last).unwrap();
        for spare in 1..4 {
            let respelled = ALPHABET.as_bytes()[index ^ spare] as char;
            let forged = format!("{payload}.{}{respelled}", &tag[..tag.len() - 1]);
            assert_eq!(verify(&cipher, &forged, now()), Err(Invalid::Malformed), "{forged}");
        }
    }

    #[test]
    fn links_carry_distinct_tokens_for_each_action() {
        let cipher = Cipher::new(&TEST_KEY);
        let links = links(&cipher, "http://ui.test/", Uuid::from_u128(1), Uuid::from_u128(2), now());
        let claims_of = |link: &str| {
            let token = link.strip_prefix("http://ui.test/incidents/action?token=").expect(link);
            verify(&cipher, token, now()).unwrap()
        };
        assert_eq!(claims_of(&links.acknowledge), claims(Action::Acknowledge, 24 * 60));
        assert_eq!(claims_of(&links.resolve), claims(Action::Resolve, 24 * 60));
        assert_eq!(hash(&links.acknowledge), hash(&links.acknowledge));
        assert_ne!(hash(&links.acknowledge), hash(&links.resolve));
    }
}
