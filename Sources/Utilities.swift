import Foundation

// MARK: - Path manipulation

func parentDirectory(of path: String) -> String {
    guard let slash = path.lastIndex(of: "/") else { return "" }
    return String(path[..<slash])
}

func resolveRealPath(_ path: String) -> String {
    guard let resolved = realpath(path, nil) else { return path }
    defer { free(resolved) }
    return String(cString: resolved)
}

// MARK: - String manipulation

func xmlEscape(_ value: String) -> String {
    value
        .replacingOccurrences(of: "&", with: "&amp;")
        .replacingOccurrences(of: "\"", with: "&quot;")
        .replacingOccurrences(of: "<", with: "&lt;")
        .replacingOccurrences(of: ">", with: "&gt;")
        .replacingOccurrences(of: "'", with: "&apos;")
}

func renderTemplate(_ template: String, replacements: [String: String]) -> String {
    replacements.reduce(template) { partial, item in
        partial.replacingOccurrences(of: item.key, with: item.value)
    }
}

func deterministicGuid(for name: String) -> String {
    var h1: UInt64 = 5381
    var h2: UInt64 = 0xcbf29ce484222325
    for byte in name.utf8 {
        h1 = ((h1 << 5) &+ h1) &+ UInt64(byte)
        h2 = (h2 ^ UInt64(byte)) &* 0x100000001b3
    }
    return String(
        format: "{%08X-%04X-%04X-%04X-%04X%08X}",
        UInt32(truncatingIfNeeded: h1 >> 32),
        UInt16(truncatingIfNeeded: h1 >> 16),
        UInt16(truncatingIfNeeded: h1),
        UInt16(truncatingIfNeeded: h2 >> 48),
        UInt16(truncatingIfNeeded: h2 >> 32),
        UInt32(truncatingIfNeeded: h2)
    )
}

// MARK: - File I/O

@discardableResult
func writeIfChanged(content: String, to url: URL) throws -> Bool {
    if let current = try? String(contentsOf: url, encoding: .utf8), current == content {
        return false
    }
    try content.write(to: url, atomically: true, encoding: .utf8)
    return true
}

func modificationDate(of url: URL) -> Date? {
    try? FileManager.default.attributesOfItem(atPath: url.path)[.modificationDate] as? Date
}
