pub mod user;
pub mod token;
pub mod project;
pub mod monitor;

/// Name rule shared by monitors and heartbeats: trimmed, 1-200 characters,
/// and no control characters (names end up in alert email subjects).
pub fn validate_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.chars().count() > 200 {
        return Err("name must be at most 200 characters".into());
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".into());
    }
    Ok(name.to_string())
}
