package main

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/net/proxy"
	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

func localControl(t *testing.T, hooks ...func(*http.Request) (int, string, string)) *testcontrol.Server {
	t.Helper()
	netns.SetEnabled(false)
	t.Cleanup(func() { netns.SetEnabled(true) })
	control := &testcontrol.Server{DERPMap: integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1"), RequireAuth: true, DNSConfig: &tailcfg.DNSConfig{Proxied: true}, MagicDNSDomain: "passession.test", Logf: logger.Discard}
	if len(hooks) > 0 {
		control.MaybeRejectRequest = hooks[0]
	}
	control.HTTPTestServer = httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/favicon.ico" {
			http.NotFound(w, r)
			return
		}
		if strings.HasPrefix(r.URL.Path, "/auth/") {
			if control.CompleteAuth(r.URL.Path) {
				io.WriteString(w, "Local test node authorized. You may close this window.")
			} else {
				http.NotFound(w, r)
			}
			return
		}
		control.ServeHTTP(w, r)
	}))
	control.HTTPTestServer.Start()
	t.Cleanup(control.HTTPTestServer.Close)
	return control
}
func testNode(t *testing.T, ctx context.Context, control *testcontrol.Server, name string) *node {
	t.Helper()
	var key [32]byte
	if _, err := rand.Read(key[:]); err != nil {
		t.Fatal(err)
	}
	n := startTestNode(t, ctx, config{StateDir: t.TempDir(), Hostname: name, ControlURL: control.HTTPTestServer.URL}, key)
	authorizeTestNode(t, ctx, control, n)
	return n
}
func startTestNode(t *testing.T, ctx context.Context, cfg config, key [32]byte) *node {
	t.Helper()
	life, cancel := context.WithCancel(context.Background())
	n := &node{cfg: cfg, key: key, ctx: life, cancel: cancel, gate: make(chan struct{}, 1), closed: make(chan struct{})}
	t.Cleanup(func() {
		cancel()
		if err := n.shutdown(); err != nil {
			t.Error(err)
		}
	})
	if _, err := n.call(ctx, 1); err != nil {
		t.Fatal(err)
	}
	return n
}
func authorizeTestNode(t *testing.T, ctx context.Context, control *testcontrol.Server, n *node) {
	t.Helper()
	st, err := n.status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if st.BackendState == "Running" {
		t.Fatal("configured node enrolled without explicit login")
	}
	raw, err := n.call(ctx, 3)
	if err != nil {
		t.Fatal(err)
	}
	authURL, ok := raw.(string)
	if !ok || !strings.HasPrefix(authURL, control.HTTPTestServer.URL+"/auth/") {
		t.Fatal("login did not return local testcontrol URL")
	}
	if !control.CompleteAuth(authURL) {
		t.Fatal("testcontrol rejected local authorization")
	}
	if _, err := n.server.Up(ctx); err != nil {
		t.Fatal(err)
	}
}
func waitPeer(t *testing.T, ctx context.Context, n *node, name string) netip.Addr {
	t.Helper()
	lc, err := n.server.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()
	last := "peer has not appeared in the authenticated map"
	for {
		st, err := lc.Status(ctx)
		if err != nil {
			t.Fatal(err)
		}
		for _, p := range st.Peer {
			if strings.HasPrefix(p.DNSName, name+".") {
				last = fmt.Sprintf("IPs=%v map=%v magicsock=%v relay=%q", p.TailscaleIPs, p.InNetworkMap, p.InMagicSock, p.Relay)
				// A DNS name arrives before the peer's transport metadata.
				// Relay comes from magicsock's observed HomeDERP, not merely
				// the control-plane Online flag. The fixture owns this DERP.
				if len(p.TailscaleIPs) != 0 && p.InNetworkMap && p.InMagicSock && p.Relay != "" {
					return p.TailscaleIPs[0]
				}
			}
		}
		select {
		case <-ctx.Done():
			t.Fatalf("waiting for %s to observe reachable peer %s: %s: %v", n.cfg.Hostname, name, last, ctx.Err())
		case <-tick.C:
		}
	}
}
func socksDial(ctx context.Context, endpoint, password, address string) (net.Conn, error) {
	d, err := proxy.SOCKS5("tcp", endpoint, &proxy.Auth{User: "tsnet", Password: password}, &net.Dialer{Timeout: 3 * time.Second})
	if err != nil {
		return nil, err
	}
	return d.(proxy.ContextDialer).DialContext(ctx, "tcp", address)
}
func TestLocalTwoNodeSOCKS(t *testing.T) {
	var dnsCalls atomic.Int32
	resolver := net.DefaultResolver
	net.DefaultResolver = &net.Resolver{PreferGo: true, Dial: func(context.Context, string, string) (net.Conn, error) {
		dnsCalls.Add(1)
		return nil, errors.New("OS DNS is forbidden in the isolated tailnet test")
	}}
	t.Cleanup(func() { net.DefaultResolver = resolver })
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	control := localControl(t)
	a := testNode(t, ctx, control, "client")
	b := testNode(t, ctx, control, "daemon")
	daemonIP := waitPeer(t, ctx, a, "daemon")
	waitPeer(t, ctx, b, "client")
	// Control-plane visibility alone does not establish a WireGuard path.
	// Like upstream's two-node listener fixture, require a real TSMP reply
	// before exercising SOCKS's separate five-second connect deadline.
	lc, err := a.server.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	ping, err := lc.Ping(ctx, daemonIP, tailcfg.PingTSMP)
	if err != nil {
		t.Fatalf("establish encrypted two-node path: %v", err)
	}
	if ping.Err != "" {
		t.Fatalf("establish encrypted two-node path: %s", ping.Err)
	}
	listener, err := b.server.Listen("tcp", ":8080")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	go func() {
		for {
			c, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { defer c.Close(); io.Copy(c, c) }()
		}
	}()
	addr, password, _, err := a.server.Loopback()
	if err != nil {
		t.Fatal(err)
	}
	if c, err := socksDial(ctx, addr, "incorrect-password", "daemon:8080"); err == nil {
		c.Close()
		t.Fatal("proxy accepted bad credentials")
	}
	c, err := socksDial(ctx, addr, password, "daemon.passession.test:8080")
	if err != nil {
		t.Fatal(err)
	}
	c.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := io.WriteString(c, "encrypted two-node echo"); err != nil {
		t.Fatal(err)
	}
	got := make([]byte, len("encrypted two-node echo"))
	if _, err := io.ReadFull(c, got); err != nil {
		t.Fatal(err)
	}
	c.Close()
	if string(got) != "encrypted two-node echo" {
		t.Fatal("incorrect echo")
	}
	// An ordinary local TCP listener must never receive selected-host traffic.
	ordinary, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ordinary.Close()
	var touched atomic.Bool
	go func() {
		c, e := ordinary.Accept()
		if e == nil {
			touched.Store(true)
			c.Close()
		}
	}()
	for _, destination := range []string{ordinary.Addr().String(), "192.0.2.123:80", "missing.passession.test:80", "localhost:80"} {
		if c, err := socksDial(ctx, addr, password, destination); err == nil {
			c.Close()
			t.Fatalf("non-tailnet destination accepted: %s", destination)
		}
	}
	if touched.Load() {
		t.Fatal("ordinary TCP listener received selected-host traffic")
	}
	if calls := dnsCalls.Load(); calls != 0 {
		t.Fatalf("tailnet traffic attempted %d ordinary DNS queries", calls)
	}
	// No log-policy tokens, logs, or plaintext backend side files may appear.
	entries, err := os.ReadDir(a.cfg.StateDir)
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if e.Name() != stateFile {
			t.Errorf("unexpected persistent side file %s", e.Name())
		}
	}
	if _, err := a.call(ctx, 4); err != nil {
		t.Fatal(err)
	}
	st, err := a.status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if st.BackendState == "Running" {
		t.Fatal("logout did not leave Running")
	}
	if _, err := a.call(ctx, 5); err == nil {
		t.Fatal("logged-out node yielded proxy")
	}
}

func TestEncryptedIdentityRestartAndFailedLogout(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var rejectLogout atomic.Bool
	expiryRequests := make(chan tailcfg.RegisterRequest, 8)
	control := localControl(t, func(r *http.Request) (int, string, string) {
		if r.URL.Path != "/machine/register" {
			return 0, "", ""
		}
		body, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		if err != nil {
			return 400, "", "read error"
		}
		r.Body.Close()
		r.Body = io.NopCloser(bytes.NewReader(body))
		var req tailcfg.RegisterRequest
		if json.Unmarshal(body, &req) != nil {
			return 400, "", "invalid register request"
		}
		if !req.Expiry.IsZero() && req.Expiry.Before(time.Now()) {
			select {
			case expiryRequests <- req:
			default:
			}
			if rejectLogout.Load() {
				return 503, "", "isolated control unavailable"
			}
		}
		return 0, "", ""
	})
	var key [32]byte
	if _, err := rand.Read(key[:]); err != nil {
		t.Fatal(err)
	}
	cfg := config{StateDir: t.TempDir(), Hostname: "persistent", ControlURL: control.HTTPTestServer.URL}
	first := startTestNode(t, ctx, cfg, key)
	authorizeTestNode(t, ctx, control, first)
	lc, _ := first.server.LocalClient()
	before, err := lc.Status(ctx)
	if err != nil {
		t.Fatal(err)
	}
	first.cancel()
	if err := first.shutdown(); err != nil {
		t.Fatal(err)
	}
	second := startTestNode(t, ctx, cfg, key)
	after, err := second.server.Up(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if before.Self.PublicKey != after.Self.PublicKey {
		t.Fatal("encrypted restart changed node identity")
	}
	rejectLogout.Store(true)
	logoutCtx, stop := context.WithTimeout(ctx, 2*time.Second)
	_, err = second.call(logoutCtx, 4)
	stop()
	if err == nil {
		t.Fatal("offline logout was reported confirmed")
	}
	if err := second.shutdown(); err != nil {
		t.Fatal(err)
	}
	third := startTestNode(t, ctx, cfg, key)
	restored, err := third.server.Up(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if before.Self.PublicKey != restored.Self.PublicKey {
		t.Fatal("failed logout lost original node identity")
	}
	rejectLogout.Store(false)
	for len(expiryRequests) > 0 {
		<-expiryRequests
	}
	if _, err := third.call(ctx, 4); err != nil {
		t.Fatal(err)
	}
	select {
	case request := <-expiryRequests:
		if request.NodeKey != before.Self.PublicKey {
			t.Fatal("logout expired the wrong node")
		}
	case <-ctx.Done():
		t.Fatal("control never received a node-key expiry request")
	}
}

// TestGuiFixture is an opt-in real control/DERP/tsnet bridge for native GUI QA.
// It never contacts a real account. Point daemon at an isolated local daemon;
// the GUI connects ws://fixture.passession.test:6767 using the emitted control URL.
func TestGuiFixture(t *testing.T) {
	file := os.Getenv("PASEO_TAILNET_FIXTURE_FILE")
	daemon := os.Getenv("PASEO_TAILNET_FIXTURE_DAEMON")
	if file == "" || daemon == "" {
		t.Skip("set PASEO_TAILNET_FIXTURE_FILE and PASEO_TAILNET_FIXTURE_DAEMON for GUI QA")
	}
	if !filepath.IsAbs(file) {
		t.Fatal("fixture file must be absolute")
	}
	target, err := netip.ParseAddrPort(daemon)
	if err != nil || !target.Addr().IsLoopback() {
		t.Fatal("fixture daemon must be literal loopback host:port")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Minute)
	defer cancel()
	control := localControl(t)
	n := testNode(t, ctx, control, "fixture")
	listener, err := n.server.Listen("tcp", ":6767")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	go func() {
		for {
			c, err := listener.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				up, err := net.DialTimeout("tcp", daemon, 5*time.Second)
				if err != nil {
					return
				}
				defer up.Close()
				done := make(chan struct{})
				go func() { io.Copy(up, c); close(done) }()
				io.Copy(c, up)
				c.Close()
				up.Close()
				<-done
			}()
		}
	}()
	controlURL, err := url.Parse(control.HTTPTestServer.URL)
	if err != nil {
		t.Fatal(err)
	}
	port, err := strconv.Atoi(controlURL.Port())
	if err != nil {
		t.Fatal(err)
	}
	ports := []int{port}
	for _, region := range control.DERPMap.Regions {
		for _, node := range region.Nodes {
			if node.DERPPort > 0 && !slices.Contains(ports, node.DERPPort) {
				ports = append(ports, node.DERPPort)
			}
		}
	}
	slices.Sort(ports)
	b, _ := json.Marshal(map[string]any{"controlUrl": control.HTTPTestServer.URL, "endpoint": "ws://fixture.passession.test:6767", "localForwardPorts": ports})
	if err := os.WriteFile(file, b, 0600); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(file)
	t.Log("GUI fixture ready")
	<-ctx.Done()
	if !errors.Is(ctx.Err(), context.DeadlineExceeded) {
		t.Fatal(ctx.Err())
	}
}
