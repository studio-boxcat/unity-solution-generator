/// XML-escape a value for an attribute or text node. Matches the five
/// substitutions used by the Swift generator (`&`, `"`, `<`, `>`, `'`).
pub fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Stable GUID derived from a project name. Mirrors the Swift djb2/fnv-1a
/// blend so generated `.csproj`/`.sln` GUIDs are byte-identical between
/// runtimes — same project name → same GUID.
pub fn deterministic_guid(name: &str) -> String {
    let mut h1: u64 = 5381;
    let mut h2: u64 = 0xcbf29ce484222325;
    for &b in name.as_bytes() {
        h1 = h1.wrapping_shl(5).wrapping_add(h1).wrapping_add(b as u64);
        h2 = (h2 ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let p1 = (h1 >> 32) & 0xFFFF_FFFF;
    let p2 = (h1 >> 16) & 0xFFFF;
    let p3 = h1 & 0xFFFF;
    let p4 = (h2 >> 48) & 0xFFFF;
    let p5 = (h2 >> 32) & 0xFFFF;
    let p6 = h2 & 0xFFFF_FFFF;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:04X}{:08X}}}",
        p1, p2, p3, p4, p5, p6
    )
}
