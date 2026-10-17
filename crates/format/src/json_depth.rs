//! Bound JSON nesting before recursive deserialization. The document tree may be
//! 64 objects deep; its JSON wrappers require more than serde_json's default 128.
use serde::de::DeserializeOwned;

use crate::FormatError;

const MAX_JSON_DEPTH: usize = 256;

fn check(bytes: &[u8]) -> Result<(), FormatError> {
    let (mut depth, mut string, mut escaped) = (0usize, false, false);
    for &b in bytes {
        if string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                string = false;
            }
        } else {
            match b {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_JSON_DEPTH {
                        return Err(FormatError::NotVectorcraft(format!("its JSON is nested more than {MAX_JSON_DEPTH} levels deep")));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn read<T: DeserializeOwned + Send>(bytes: &[u8]) -> Result<T, FormatError> {
    check(bytes)?;
    vectorcraft_doc::depth::with_deep_stack(|| {
        let mut reader = serde_json::Deserializer::from_slice(bytes);
        // The iterative check above enforces our explicit finite budget first.
        reader.disable_recursion_limit();
        let value = T::deserialize(&mut reader)?;
        reader.end()?;
        Ok(value)
    })
    .map_err(|e: serde_json::Error| FormatError::NotVectorcraft(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_excessive_nesting_before_parsing() {
        let bytes = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(read::<serde_json::Value>(bytes.as_bytes()).is_err());
        let at = format!("{}0{}", "[".repeat(MAX_JSON_DEPTH), "]".repeat(MAX_JSON_DEPTH));
        assert!(read::<serde_json::Value>(at.as_bytes()).is_ok());
    }

    #[test]
    fn quoted_brackets_and_escaped_quotes_do_not_count() {
        let text = format!("{}\"\\{}", "[".repeat(500), "]".repeat(500));
        let bytes = serde_json::to_vec(&text).unwrap();
        assert_eq!(read::<String>(&bytes).unwrap(), text);
        assert!(read::<serde_json::Value>(b"{]}").is_err());
        assert!(read::<serde_json::Value>(b"{} []").is_err());
    }
}
