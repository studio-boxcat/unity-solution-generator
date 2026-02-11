import Darwin

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

func joinPath(_ base: String, _ component: String) -> String {
    if base.hasSuffix("/") { return "\(base)\(component)" }
    return "\(base)/\(component)"
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

// MARK: - File I/O (POSIX)

func readFile(_ path: String) throws -> String {
    let fd = open(path, O_RDONLY)
    guard fd >= 0 else { throw POSIXError(errno, path: path) }
    defer { close(fd) }

    var st = stat()
    guard fstat(fd, &st) == 0 else { throw POSIXError(errno, path: path) }
    let size = Int(st.st_size)
    guard size > 0 else { return "" }

    let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: size)
    defer { buf.deallocate() }

    var read = 0
    while read < size {
        let n = Darwin.read(fd, buf + read, size - read)
        guard n > 0 else { throw POSIXError(errno, path: path) }
        read += n
    }
    return String(decoding: UnsafeBufferPointer(start: buf, count: size), as: UTF8.self)
}

@discardableResult
func writeFileIfChanged(_ path: String, _ content: String) throws -> Bool {
    let bytes = Array(content.utf8)

    // Compare with existing file.
    let fd = open(path, O_RDONLY)
    if fd >= 0 {
        defer { close(fd) }
        var st = stat()
        if fstat(fd, &st) == 0, Int(st.st_size) == bytes.count {
            let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: bytes.count)
            defer { buf.deallocate() }
            var read = 0
            while read < bytes.count {
                let n = Darwin.read(fd, buf + read, bytes.count - read)
                if n <= 0 { break }
                read += n
            }
            if read == bytes.count, memcmp(buf, bytes, bytes.count) == 0 {
                return false
            }
        }
    }

    // Write new content.
    let wfd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
    guard wfd >= 0 else { throw POSIXError(errno, path: path) }
    defer { close(wfd) }
    var written = 0
    while written < bytes.count {
        let n = bytes.withUnsafeBufferPointer {
            Darwin.write(wfd, $0.baseAddress! + written, bytes.count - written)
        }
        guard n > 0 else { throw POSIXError(errno, path: path) }
        written += n
    }
    return true
}

func fileExists(_ path: String) -> Bool {
    access(path, F_OK) == 0
}

func createDirectoryRecursive(_ path: String) {
    var current = ""
    for component in path.split(separator: "/") {
        current += "/\(component)"
        mkdir(current, 0o755)
    }
}

func listDirectory(_ path: String) -> [String] {
    guard let dir = opendir(path) else { return [] }
    defer { closedir(dir) }
    var entries: [String] = []
    while let entry = readdir(dir) {
        var d_name = entry.pointee.d_name
        let name = withUnsafePointer(to: &d_name) {
            String(cString: UnsafeRawPointer($0).assumingMemoryBound(to: CChar.self))
        }
        if name == "." || name == ".." { continue }
        entries.append(name)
    }
    return entries
}

// MARK: - Minimal JSON extraction

func extractJsonString(_ json: String, key: String) -> String? {
    let needle = "\"\(key)\""
    guard let keyRange = json.range(of: needle) else { return nil }
    var idx = keyRange.upperBound
    while idx < json.endIndex && json[idx] != ":" { json.formIndex(after: &idx) }
    guard idx < json.endIndex else { return nil }
    json.formIndex(after: &idx)
    while idx < json.endIndex && (json[idx] == " " || json[idx] == "\t" || json[idx] == "\n" || json[idx] == "\r") {
        json.formIndex(after: &idx)
    }
    guard idx < json.endIndex, json[idx] == "\"" else { return nil }
    json.formIndex(after: &idx)
    let start = idx
    while idx < json.endIndex && json[idx] != "\"" { json.formIndex(after: &idx) }
    guard idx < json.endIndex else { return nil }
    return String(json[start..<idx])
}

func extractJsonStringArray(_ json: String, key: String) -> [String] {
    let needle = "\"\(key)\""
    guard let keyRange = json.range(of: needle) else { return [] }
    var idx = keyRange.upperBound
    while idx < json.endIndex && json[idx] != "[" { json.formIndex(after: &idx) }
    guard idx < json.endIndex else { return [] }
    json.formIndex(after: &idx)
    var results: [String] = []
    while idx < json.endIndex {
        let ch = json[idx]
        if ch == "]" { break }
        if ch == "\"" {
            json.formIndex(after: &idx)
            let start = idx
            while idx < json.endIndex && json[idx] != "\"" { json.formIndex(after: &idx) }
            guard idx < json.endIndex else { break }
            results.append(String(json[start..<idx]))
            json.formIndex(after: &idx)
        } else {
            json.formIndex(after: &idx)
        }
    }
    return results
}

// MARK: - Error types

struct POSIXError: Error, CustomStringConvertible {
    let code: Int32
    let path: String
    init(_ code: Int32, path: String) {
        self.code = code
        self.path = path
    }
    var description: String {
        String(cString: strerror(code)) + ": \(path)"
    }
}
