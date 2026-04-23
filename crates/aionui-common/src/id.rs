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

/// Generate a time-ordered globally unique ID (UUID v7).
pub fn generate_id() -> String {
    Uuid::now_v7().to_string()
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
