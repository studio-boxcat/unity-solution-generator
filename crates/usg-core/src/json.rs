//! Minimal hand-rolled JSON value extractors used for asmdef/asmref/manifest.json.
//!
//! Mirrors the Swift implementations in `Utilities.swift`: not a real JSON parser,
//! just enough byte-scanning to pull a few well-known keys cheaply. Operates on
//! UTF-8 byte slices so we avoid Swift's char-by-char index dance.

fn find_key(json: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{}\"", key);
    json.find(&needle).map(|i| i + needle.len())
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn find_value_start(json: &str, key: &str) -> Option<usize> {
    let after_key = find_key(json, key)?;
    let bytes = json.as_bytes();
    let mut i = after_key;
    while i < bytes.len() && bytes[i] != b':' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    i += 1;
    Some(skip_ws(bytes, i))
}

pub fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let start = find_value_start(json, key)?;
    let bytes = json.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let body = start + 1;
    let mut end = body;
    while end < bytes.len() && bytes[end] != b'"' {
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    Some(json[body..end].to_string())
}

pub fn extract_json_bool(json: &str, key: &str) -> Option<bool> {
    let start = find_value_start(json, key)?;
    let rest = &json[start..];
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub fn extract_json_string_array(json: &str, key: &str) -> Vec<String> {
    let Some(start) = find_value_start(json, key) else {
        return Vec::new();
    };
    let bytes = json.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return Vec::new();
    }
    let mut i = start + 1;
    let mut out = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b']' => break,
            b'"' => {
                i += 1;
                let body = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() {
                    break;
                }
                out.push(json[body..i].to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Extract the keys of the object value associated with `key`.
/// E.g. `extract_json_object_keys(manifest, "dependencies")` → list of package names.
pub fn extract_json_object_keys(json: &str, key: &str) -> Vec<String> {
    let Some(after_key) = find_key(json, key) else {
        return Vec::new();
    };
    let bytes = json.as_bytes();
    let mut i = after_key;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return Vec::new();
    }
    i += 1;
    let mut depth = 1;
    let mut out = Vec::new();
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                i += 1;
            }
            b'"' if depth == 1 => {
                i += 1;
                let body = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() {
                    break;
                }
                let k = &json[body..i];
                i += 1;
                let after = skip_ws(bytes, i);
                if bytes.get(after) == Some(&b':') {
                    out.push(k.to_string());
                }
            }
            _ => i += 1,
        }
    }
    out
}

pub fn trim_ws(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\t') {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t') {
        end -= 1;
    }
    &s[start..end]
}
