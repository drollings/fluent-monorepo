/// url.zig — Generic URL validation helpers
///
/// Provides simple, allocation-free checks for acceptable API endpoint URLs.
/// The policy enforced here — HTTPS anywhere, plain HTTP only to localhost —
/// is a sensible default for any CLI tool that calls local or remote APIs.
const std = @import("std");

/// Return true when `host` resolves to the loopback interface.
/// Covers the common aliases: "localhost", "127.0.0.1", "::1", and the
/// full 127.x.x.x range.
pub fn isLocalHost(host: []const u8) bool {
    if (std.mem.eql(u8, host, "localhost")) return true;
    if (std.mem.eql(u8, host, "127.0.0.1")) return true;
    if (std.mem.eql(u8, host, "::1")) return true;
    if (std.mem.startsWith(u8, host, "127.")) return true;
    return false;
}

/// Validate that `url` is either:
///   - an `https://` URL (any host), or
///   - an `http://` URL whose host is localhost / 127.x / ::1
///
/// Returns `error.InvalidApiUrl` for empty or non-http(s) strings and
/// `error.InsecureApiUrl` for plain-HTTP URLs that point at a remote host.
pub fn validateHttpsOrLocalHttp(url: []const u8) !void {
    if (url.len == 0) return error.InvalidApiUrl;
    const is_https = std.mem.startsWith(u8, url, "https://");
    const is_http = std.mem.startsWith(u8, url, "http://");
    if (!is_https and !is_http) return error.InvalidApiUrl;

    if (is_http) {
        // Extract host from http://host:port/path
        const after = url["http://".len..];
        const host_end = std.mem.indexOfAny(u8, after, ":/") orelse after.len;
        const host = after[0..host_end];
        if (!isLocalHost(host)) return error.InsecureApiUrl;
    }
}

// =============================================================================
// Tests
// =============================================================================

test "isLocalHost recognises loopback aliases" {
    try std.testing.expect(isLocalHost("localhost"));
    try std.testing.expect(isLocalHost("127.0.0.1"));
    try std.testing.expect(isLocalHost("127.0.0.2"));
    try std.testing.expect(isLocalHost("::1"));
    try std.testing.expect(!isLocalHost("example.com"));
    try std.testing.expect(!isLocalHost("192.168.1.1"));
}

test "validateHttpsOrLocalHttp accepts https" {
    try validateHttpsOrLocalHttp("https://api.openai.com/v1/embeddings");
}

test "validateHttpsOrLocalHttp accepts local http" {
    try validateHttpsOrLocalHttp("http://localhost:11434/api/embed");
    try validateHttpsOrLocalHttp("http://127.0.0.1:8080/v1");
}

test "validateHttpsOrLocalHttp rejects empty" {
    try std.testing.expectError(error.InvalidApiUrl, validateHttpsOrLocalHttp(""));
}

test "validateHttpsOrLocalHttp rejects bare hostname" {
    try std.testing.expectError(error.InvalidApiUrl, validateHttpsOrLocalHttp("example.com"));
}

test "validateHttpsOrLocalHttp rejects remote http" {
    try std.testing.expectError(error.InsecureApiUrl, validateHttpsOrLocalHttp("http://remote.host/api"));
}
