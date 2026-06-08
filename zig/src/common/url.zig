/// url.zig — Generic URL validation helpers
///
/// Provides simple, allocation-free checks for acceptable API endpoint URLs.
/// Policy:
///   - HTTPS anywhere (public hosts), OR
///   - HTTP only to localhost / 127.x.x.x
///   - Private IP ranges always blocked (SSRF prevention)
const std = @import("std");

/// Checks if a given host string matches a localhost pattern, returning true or false.
pub fn isLocalHost(host: []const u8) bool {
    if (std.mem.eql(u8, host, "localhost")) return true;
    if (std.mem.eql(u8, host, "127.0.0.1")) return true;
    if (std.mem.eql(u8, host, "::1")) return true;
    if (std.mem.startsWith(u8, host, "127.")) return true;
    return false;
}

/// Checks if a given IP slice is a private IP address and returns true or false.
pub fn isPrivateIp(host: []const u8) bool {
    // Strip IPv6 brackets: [::1] → ::1
    const h = if (host.len >= 2 and host[0] == '[' and host[host.len - 1] == ']')
        host[1 .. host.len - 1]
    else
        host;

    // IPv4 private ranges
    if (std.mem.startsWith(u8, h, "10.")) return true;
    if (std.mem.startsWith(u8, h, "192.168.")) return true;
    if (std.mem.startsWith(u8, h, "169.254.")) return true; // IMDS / link-local
    if (std.mem.startsWith(u8, h, "0.")) return true;

    // 172.16.0.0/12 — second octet 16..31
    if (std.mem.startsWith(u8, h, "172.")) {
        const after = h["172.".len..];
        const dot = std.mem.indexOf(u8, after, ".") orelse after.len;
        const octet = std.fmt.parseInt(u8, after[0..dot], 10) catch 0;
        if (octet >= 16 and octet <= 31) return true;
    }

    // IPv6 private: fc00::/7 covers fc** and fd**
    if (h.len >= 2) {
        const lo = std.ascii.toLower(h[0]);
        const lo1 = std.ascii.toLower(h[1]);
        if (lo == 'f' and (lo1 == 'c' or lo1 == 'd')) return true; // fc00::/7
        if (lo == 'f' and lo1 == 'e' and h.len >= 4) {
            const lo2 = std.ascii.toLower(h[2]);
            const lo3 = std.ascii.toLower(h[3]);
            // fe80::/10 — first 10 bits = 1111 1110 10
            if (lo2 == '8' or lo2 == '9' or lo2 == 'a' or lo2 == 'b') {
                _ = lo3;
                return true;
            }
        }
    }

    return false;
}

/// Extracts the host portion from a URL slice, returning a slice of bytes.
fn extractHost(url: []const u8) []const u8 {
    const scheme_end = (std.mem.indexOf(u8, url, "://") orelse 0) + 3;
    const after = url[scheme_end..];
    const host_end = std.mem.indexOfAny(u8, after, ":/") orelse after.len;
    return after[0..host_end];
}

/// Validates whether a given URL is HTTPS or local HTTP, returning a boolean result.
pub fn validateHttpsOrLocalHttp(url: []const u8) !void {
    if (url.len == 0) return error.InvalidApiUrl;
    const is_https = std.mem.startsWith(u8, url, "https://");
    const is_http = std.mem.startsWith(u8, url, "http://");
    if (!is_https and !is_http) return error.InvalidApiUrl;

    const host = extractHost(url);

    if (is_http) {
        if (!isLocalHost(host)) return error.InsecureApiUrl;
    }

    // Block private IP ranges for both HTTP and HTTPS (SSRF prevention).
    // Loopback (127.x) is exempt — it is the only permitted plain-HTTP host.
    if (isPrivateIp(host) and !isLocalHost(host)) return error.SsrfBlockedUrl;
}

// =============================================================================
// Tests
// =============================================================================
