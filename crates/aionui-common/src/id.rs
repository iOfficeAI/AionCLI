use uuid::Uuid;

/// FNV-1a hash producing an 8-character lowercase hex string at compile time.
pub const fn fnv1a_hex8(input: &[u8]) -> [u8; 8] {
    const BASIS: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    const HEX: [u8; 16] = *b"0123456789abcdef";

    let mut hash = BASIS;
    let mut i = 0;
    while i < input.len() {
        hash ^= input[i] as u32;
        hash = hash.wrapping_mul(PRIME);
        i += 1;
    }

    let mut out = [0u8; 8];
    let mut j = 0;
    while j < 4 {
        let byte = (hash >> (24 - j * 8)) as u8;
        out[j * 2] = HEX[(byte >> 4) as usize];
        out[j * 2 + 1] = HEX[(byte & 0x0f) as usize];
        j += 1;
    }
    out
}

/// Generate a cryptographically random hex ID of `length` characters.
///
/// Matches the frontend `uuid(length)` convention: random bytes → hex string → truncate.
/// When `length` is `None` or `>= 36`, returns a full UUID v7 string instead.
pub fn generate_id_with_length(length: Option<usize>) -> String {
    match length {
        Some(len) if len < 36 => {
            let num_bytes = len.div_ceil(2);
            let mut buf = vec![0u8; num_bytes];
            getrandom::getrandom(&mut buf).expect("getrandom failed");
            let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
            hex[..len].to_string()
        }
        _ => Uuid::now_v7().to_string(),
    }
}

/// Generate a full UUID v7 string (36 chars).
pub fn generate_id() -> String {
    generate_id_with_length(None)
}

/// Generate a short random hex ID (default 8 chars), compatible with the frontend `uuid()` convention.
pub fn generate_short_id() -> String {
    generate_id_with_length(Some(8))
}

/// Generate a prefixed ID (e.g., "cron_01234...", "mcp_01234...").
pub fn generate_prefixed_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_generate_id_is_valid_uuid() {
        let id = generate_id();
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_generate_id_is_v7() {
        let id = generate_id();
        let uuid = Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version_num(), 7);
    }

    #[test]
    fn test_generate_prefixed_id_format() {
        let id = generate_prefixed_id("msg");
        assert!(id.starts_with("msg_"));
        let uuid_part = &id[4..];
        assert!(Uuid::parse_str(uuid_part).is_ok());
    }

    #[test]
    fn test_id_uniqueness() {
        let ids: HashSet<String> = (0..1000).map(|_| generate_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn test_id_time_ordering() {
        let id1 = generate_id();
        let id2 = generate_id();
        assert!(id2 >= id1);
    }

    #[test]
    fn test_long_prefix() {
        let prefix = "a".repeat(1000);
        let id = generate_prefixed_id(&prefix);
        assert!(id.starts_with(&prefix));
    }

    #[test]
    fn test_generate_short_id_length() {
        let id = generate_short_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_id_with_length_custom() {
        let id = generate_id_with_length(Some(12));
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_id_with_length_none_returns_full_uuid() {
        let id = generate_id_with_length(None);
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_generate_id_with_length_large_returns_full_uuid() {
        let id = generate_id_with_length(Some(32));
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn test_short_id_uniqueness() {
        let ids: HashSet<String> = (0..1000).map(|_| generate_short_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn test_fnv1a_hex8_deterministic() {
        let a = fnv1a_hex8(b"claude");
        let b = fnv1a_hex8(b"claude");
        assert_eq!(a, b);
    }

    #[test]
    fn test_fnv1a_hex8_different_inputs() {
        let a = fnv1a_hex8(b"claude");
        let b = fnv1a_hex8(b"codex");
        assert_ne!(a, b);
    }

    #[test]
    fn test_fnv1a_hex8_length() {
        let hash = fnv1a_hex8(b"test");
        assert_eq!(hash.len(), 8);
        for byte in &hash {
            assert!(byte.is_ascii_hexdigit());
        }
    }
}
