package codeg

// Outbound fetch hardening for fetch_url.
//
// fetch_url is reachable from the model (via codeg-mcp) and, before the local
// API guard, from any web page. An unguarded client turns it into an SSRF
// primitive: 127.0.0.1 services, LAN hosts and the cloud metadata address
// (169.254.169.254) are all valid http:// targets, and redirects can walk from
// a public URL into one of them.
//
// The guard is applied at dial time rather than by string-matching the URL:
//   - the hostname is resolved here, every candidate IP is classified, and the
//     already-vetted IP is dialled directly. Resolving and then re-resolving
//     inside net.Dialer would leave a DNS-rebinding window.
//   - CheckRedirect validates every hop's scheme; each hop still goes through
//     the same DialContext, so a redirect into a blocked address is refused.
//
// TLS is unaffected: http.Transport performs the handshake itself using the
// URL hostname for SNI, because DialContext only returns the raw TCP conn.

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"time"
)

// blockedFetchNets are ranges the net.IP helpers do not classify but that are
// still unsafe or meaningless as fetch targets.
var blockedFetchNets = func() []*net.IPNet {
	cidrs := []string{
		"0.0.0.0/8",     // "this network"
		"100.64.0.0/10", // CGNAT
		"192.0.0.0/24",  // IETF protocol assignments
		"198.18.0.0/15", // benchmarking
		"240.0.0.0/4",   // reserved
		"2001:db8::/32", // documentation
	}
	out := make([]*net.IPNet, 0, len(cidrs))
	for _, cidr := range cidrs {
		if _, n, err := net.ParseCIDR(cidr); err == nil {
			out = append(out, n)
		}
	}
	return out
}()

// blockedFetchIP reports whether ip must not be contacted by fetch_url.
// Loopback, RFC1918/ULA, link-local (including 169.254.169.254), multicast,
// unspecified and the ranges above are all refused.
func blockedFetchIP(ip net.IP) bool {
	if ip == nil {
		return true
	}
	// Normalise IPv4-mapped IPv6 (::ffff:127.0.0.1) so the v4 classifiers see
	// the embedded address.
	if v4 := ip.To4(); v4 != nil {
		ip = v4
	}
	if ip.IsLoopback() || ip.IsPrivate() || ip.IsUnspecified() ||
		ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() ||
		ip.IsMulticast() || ip.IsInterfaceLocalMulticast() {
		return true
	}
	for _, n := range blockedFetchNets {
		if n.Contains(ip) {
			return true
		}
	}
	return false
}

// safeFetchClient returns the default client for fetch_url: no proxy (an
// HTTP(S)_PROXY would bypass the dial guard), no keep-alive, and a dial-time
// address classifier.
func safeFetchClient() *http.Client {
	dialer := &net.Dialer{Timeout: 10 * time.Second}
	transport := &http.Transport{
		Proxy: nil,
		DialContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			host, port, err := net.SplitHostPort(addr)
			if err != nil {
				return nil, err
			}
			ips, err := net.DefaultResolver.LookupIPAddr(ctx, host)
			if err != nil {
				return nil, err
			}
			var lastErr error
			tried := false
			for _, candidate := range ips {
				if blockedFetchIP(candidate.IP) {
					continue
				}
				tried = true
				conn, err := dialer.DialContext(ctx, "tcp", net.JoinHostPort(candidate.IP.String(), port))
				if err == nil {
					return conn, nil
				}
				lastErr = err
			}
			if !tried {
				return nil, fmt.Errorf("目标地址不允许访问：%s", host)
			}
			return nil, lastErr
		},
		TLSHandshakeTimeout:   10 * time.Second,
		ResponseHeaderTimeout: 15 * time.Second,
		DisableKeepAlives:     true,
	}
	return &http.Client{
		Transport: transport,
		Timeout:   fetchTimeout,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= fetchMaxRedirect {
				return fmt.Errorf("重定向次数过多")
			}
			if req.URL.Scheme != "http" && req.URL.Scheme != "https" {
				return fmt.Errorf("不支持的重定向协议：%s", req.URL.Scheme)
			}
			return nil
		},
	}
}
