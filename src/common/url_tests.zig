//! Tests for url.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const url_mod = @import("url.zig");

test "isLocalHost recognises loopback aliases" {
    try std.testing.expect(url_mod.isLocalHost("localhost"));
    try std.testing.expect(url_mod.isLocalHost("127.0.0.1"));
    try std.testing.expect(url_mod.isLocalHost("127.0.0.2"));
    try std.testing.expect(url_mod.isLocalHost("::1"));
    try std.testing.expect(!url_mod.isLocalHost("example.com"));
    try std.testing.expect(!url_mod.isLocalHost("192.168.1.1"));
}
test "validateHttpsOrLocalHttp accepts https" {
    try url_mod.validateHttpsOrLocalHttp("https://api.openai.com/v1/embeddings");
}
test "validateHttpsOrLocalHttp accepts local http" {
    try url_mod.validateHttpsOrLocalHttp("http://localhost:11434/api/embed");
    try url_mod.validateHttpsOrLocalHttp("http://127.0.0.1:8080/v1");
}
test "validateHttpsOrLocalHttp rejects empty" {
    try std.testing.expectError(error.InvalidApiUrl, url_mod.validateHttpsOrLocalHttp(""));
}
test "validateHttpsOrLocalHttp rejects bare hostname" {
    try std.testing.expectError(error.InvalidApiUrl, url_mod.validateHttpsOrLocalHttp("example.com"));
}
test "validateHttpsOrLocalHttp rejects remote http" {
    try std.testing.expectError(error.InsecureApiUrl, url_mod.validateHttpsOrLocalHttp("http://remote.host/api"));
}
test "isPrivateIp detects private ranges" {
    try std.testing.expect(url_mod.isPrivateIp("10.0.0.1"));
    try std.testing.expect(url_mod.isPrivateIp("10.255.255.255"));
    try std.testing.expect(url_mod.isPrivateIp("192.168.1.1"));
    try std.testing.expect(url_mod.isPrivateIp("192.168.0.0"));
    try std.testing.expect(url_mod.isPrivateIp("169.254.169.254")); // AWS IMDS
    try std.testing.expect(url_mod.isPrivateIp("172.16.0.1"));
    try std.testing.expect(url_mod.isPrivateIp("172.31.255.255"));
    try std.testing.expect(url_mod.isPrivateIp("0.0.0.0"));
    try std.testing.expect(!url_mod.isPrivateIp("172.15.0.1")); // just outside /12
    try std.testing.expect(!url_mod.isPrivateIp("172.32.0.1")); // just outside /12
    try std.testing.expect(!url_mod.isPrivateIp("8.8.8.8"));
    try std.testing.expect(!url_mod.isPrivateIp("example.com"));
}
test "validateHttpsOrLocalHttp blocks AWS metadata endpoint via http" {
    // Plain-HTTP to a non-localhost host is already rejected as InsecureApiUrl;
    // the SSRF check fires for HTTPS variants of the same host.
    try std.testing.expectError(
        error.InsecureApiUrl,
        url_mod.validateHttpsOrLocalHttp("http://169.254.169.254/latest/meta-data/"),
    );
}
test "validateHttpsOrLocalHttp blocks AWS metadata endpoint via https" {
    try std.testing.expectError(
        error.SsrfBlockedUrl,
        url_mod.validateHttpsOrLocalHttp("https://169.254.169.254/latest/meta-data/"),
    );
}
test "validateHttpsOrLocalHttp blocks private class A via http" {
    // http:// to non-localhost → InsecureApiUrl first
    try std.testing.expectError(
        error.InsecureApiUrl,
        url_mod.validateHttpsOrLocalHttp("http://10.0.0.5:8080/"),
    );
}
test "validateHttpsOrLocalHttp blocks private class A via https" {
    try std.testing.expectError(
        error.SsrfBlockedUrl,
        url_mod.validateHttpsOrLocalHttp("https://10.0.0.5/api"),
    );
}
test "validateHttpsOrLocalHttp blocks private class C via https" {
    try std.testing.expectError(
        error.SsrfBlockedUrl,
        url_mod.validateHttpsOrLocalHttp("https://192.168.1.1/"),
    );
}
test "validateHttpsOrLocalHttp allows localhost http" {
    try url_mod.validateHttpsOrLocalHttp("http://localhost:11434/api/embed");
    try url_mod.validateHttpsOrLocalHttp("http://127.0.0.1:8080/v1");
}
test "validateHttpsOrLocalHttp allows public https" {
    try url_mod.validateHttpsOrLocalHttp("https://api.openai.com/v1/embeddings");
    try url_mod.validateHttpsOrLocalHttp("https://example.com/api");
}
