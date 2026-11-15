// Copyright (c) Tailscale Inc & AUTHORS; Passession contributors
// SPDX-License-Identifier: BSD-3-Clause
// Portable replacement for libtailscale's Unix descriptor bridge. Loopback and
// in-memory LocalAPI share tsnet on every target, including Windows and iOS.
package main

/*
#include <stdint.h>
#include <stdlib.h>
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unsafe"

	"tailscale.com/envknob"
	"tailscale.com/hostinfo"
	"tailscale.com/ipn"
	"tailscale.com/ipn/ipnstate"
	"tailscale.com/paseo-libtailscale/tsnet"
	"tailscale.com/types/logger"
)

// Release builds stamp the independently compiled component, not its container.
var (
	componentVersion   = "dev"
	componentSourceSHA = ""
	componentInputHash = ""
)

func main() {}
func init() {
	// Runs before any node goroutines; no global VPN, remote logs or netlog upload.
	envknob.SetNoLogsNoSupport()
	hostinfo.SetApp("Passession")
}

type config struct {
	StateDir   string `json:"state_dir"`
	Hostname   string `json:"hostname"`
	ControlURL string `json:"control_url"`
}
type node struct {
	cfg                config
	key                [32]byte
	ctx                context.Context
	cancel             context.CancelFunc
	gate               chan struct{}
	server             *tsnet.Server
	store              *encryptedStore
	logoutCheckpoint   []byte
	started            bool
	closing            sync.Once
	closed             chan struct{}
	closeErr           error
	stopNetworkMonitor func()
}

var registry = struct {
	sync.Mutex
	next  uint64
	nodes map[uint64]*node
}{nodes: make(map[uint64]*node)}

type result struct {
	Value any    `json:"value,omitempty"`
	Error string `json:"error,omitempty"`
}

func output(value any, err error) *C.char {
	r := result{Value: value}
	if err != nil {
		r.Value = nil
		r.Error = err.Error()
	}
	b, e := json.Marshal(r)
	if e != nil {
		b = []byte(`{"error":"native result serialization failed"}`)
	}
	return C.CString(string(b))
}

//export PaseoAbiVersion
func PaseoAbiVersion() C.uint32_t { return 1 }

//export PaseoIdentity
func PaseoIdentity() *C.char {
	return output(struct {
		Version    string `json:"version"`
		SourceSHA  string `json:"sourceSha"`
		InputHash  string `json:"inputHash"`
		ABIVersion uint32 `json:"abiVersion"`
	}{componentVersion, componentSourceSHA, componentInputHash, uint32(PaseoAbiVersion())}, nil)
}

//export PaseoNew
func PaseoNew(raw *C.char, key *C.uchar) C.uint64_t {
	if raw == nil || key == nil {
		return 0
	}
	var cfg config
	if json.Unmarshal([]byte(C.GoString(raw)), &cfg) != nil {
		return 0
	}
	ctx, cancel := context.WithCancel(context.Background())
	n := &node{cfg: cfg, ctx: ctx, cancel: cancel, gate: make(chan struct{}, 1), closed: make(chan struct{})}
	copy(n.key[:], unsafe.Slice((*byte)(unsafe.Pointer(key)), 32))
	registry.Lock()
	defer registry.Unlock()
	if len(registry.nodes) != 0 {
		cancel()
		clear(n.key[:])
		return 0
	}
	registry.next++
	id := registry.next
	registry.nodes[id] = n
	return C.uint64_t(id)
}
func lookup(id uint64) *node { registry.Lock(); defer registry.Unlock(); return registry.nodes[id] }

//export PaseoCall
func PaseoCall(id C.uint64_t, operation C.int) *C.char {
	n := lookup(uint64(id))
	if n == nil {
		return output(nil, errors.New("embedded node is closed"))
	}
	ctx, cancel := context.WithTimeout(n.ctx, 15*time.Second)
	defer cancel()
	select {
	case n.gate <- struct{}{}:
	case <-ctx.Done():
		return output(nil, ctx.Err())
	}
	done := make(chan result, 1)
	go func() {
		defer func() { <-n.gate }()
		v, e := n.call(ctx, int(operation))
		r := result{Value: v}
		if e != nil {
			r.Error = e.Error()
		}
		done <- r
	}()
	select {
	case r := <-done:
		if r.Error != "" {
			return output(nil, errors.New(r.Error))
		}
		return output(r.Value, nil)
	case <-ctx.Done():
		// Start has no upstream context API. Fence its late completion and leave
		// the node registered until Close drains it; never release its state lock.
		if operation == 1 {
			n.cancel()
		}
		return output(nil, ctx.Err())
	}
}

func (n *node) call(ctx context.Context, op int) (any, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if op == 1 {
		if n.started {
			return n.status(ctx)
		}
		if !filepath.IsAbs(n.cfg.StateDir) {
			return nil, errors.New("state directory must be absolute")
		}
		if err := validateControl(n.cfg.ControlURL); err != nil {
			return nil, err
		}
		if err := os.MkdirAll(n.cfg.StateDir, 0700); err != nil {
			return nil, err
		}
		store, err := openStore(n.cfg.StateDir, n.key[:])
		clear(n.key[:])
		if err != nil {
			return nil, err
		}
		n.store = store
		n.server = &tsnet.Server{Dir: n.cfg.StateDir, Store: store, Hostname: n.cfg.Hostname, ControlURL: n.cfg.ControlURL, Logf: logger.Discard, UserLogf: logger.Discard}
		err = n.server.Start()
		// Close is valid after Start returned, including partially initialized Start.
		n.started = true
		if err != nil {
			return nil, err
		}
		if startPlatformNetworkMonitor != nil {
			n.stopNetworkMonitor = startPlatformNetworkMonitor(n.server)
		}
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		return n.status(ctx)
	}
	if !n.started {
		return nil, errors.New("embedded node is not started")
	}
	lc, err := n.server.LocalClient()
	if err != nil {
		return nil, err
	}
	switch op {
	case 2:
		return n.status(ctx)
	case 3:
		if _, err := lc.EditPrefs(ctx, &ipn.MaskedPrefs{Prefs: ipn.Prefs{WantRunning: true}, WantRunningSet: true}); err != nil {
			return nil, err
		}
		if err := lc.StartLoginInteractive(ctx); err != nil {
			return nil, err
		}
		tick := time.NewTicker(100 * time.Millisecond)
		defer tick.Stop()
		for {
			st, err := lc.StatusWithoutPeers(ctx)
			if err != nil {
				return nil, err
			}
			if st.AuthURL != "" {
				return st.AuthURL, nil
			}
			if st.BackendState == "Running" {
				return nil, errors.New("node is already logged in")
			}
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-tick.C:
			}
		}
	case 4:
		// LocalAPI waits for the control-server response to a node-key-expiry
		// request. This does not delete the device from the admin console.
		checkpoint, err := n.store.checkpoint()
		if err != nil {
			return nil, err
		}
		n.logoutCheckpoint = checkpoint
		err = lc.Logout(ctx)
		if err == nil {
			clear(n.logoutCheckpoint)
			n.logoutCheckpoint = nil
		} else {
			n.cancel()
		}
		return nil, err
	case 5:
		st, err := lc.StatusWithoutPeers(ctx)
		if err != nil {
			return nil, err
		}
		if st.BackendState != "Running" {
			return nil, errors.New("Tailscale is not connected")
		}
		addr, password, _, err := n.server.Loopback()
		if err != nil {
			return nil, err
		}
		return map[string]string{"endpoint": addr, "username": "tsnet", "password": password}, nil
	default:
		return nil, errors.New("unknown native operation")
	}
}
func validateControl(raw string) error {
	if raw == "" {
		return nil
	}
	u, err := url.Parse(raw)
	if err != nil {
		return errors.New("invalid control URL")
	}
	if u.User != nil || u.Fragment != "" || u.RawQuery != "" || u.Hostname() == "" {
		return errors.New("invalid control URL")
	}
	ip := net.ParseIP(u.Hostname())
	loopback := ip != nil && ip.IsLoopback()
	if u.Scheme != "https" && !(u.Scheme == "http" && loopback) {
		return errors.New("control URL requires HTTPS (HTTP is allowed only on loopback)")
	}
	return nil
}

type status struct {
	BackendState string   `json:"backend_state"`
	Account      *string  `json:"account"`
	Tailnet      *string  `json:"tailnet"`
	NodeName     *string  `json:"node_name"`
	IPs          []string `json:"ips"`
	Health       []string `json:"health"`
}

func (n *node) status(ctx context.Context) (status, error) {
	if platformNetworkError != nil {
		if err := platformNetworkError(); err != nil {
			return status{}, err
		}
	}
	lc, err := n.server.LocalClient()
	if err != nil {
		return status{}, err
	}
	st, err := lc.StatusWithoutPeers(ctx)
	if err != nil {
		return status{}, err
	}
	return sanitizeStatus(st), nil
}
func sanitizeStatus(st *ipnstate.Status) status {
	s := status{BackendState: st.BackendState, IPs: []string{}, Health: st.Health}
	if s.Health == nil {
		s.Health = []string{}
	}
	for _, ip := range st.TailscaleIPs {
		s.IPs = append(s.IPs, ip.String())
	}
	if st.CurrentTailnet != nil {
		s.Tailnet = &st.CurrentTailnet.Name
	}
	if st.Self != nil {
		name := strings.TrimSuffix(st.Self.DNSName, ".")
		if name == "" {
			name = st.Self.HostName
		}
		s.NodeName = &name
		if user, ok := st.User[st.Self.UserID]; ok {
			s.Account = &user.LoginName
		}
	}
	return s
}

//export PaseoClose
func PaseoClose(id C.uint64_t) *C.char {
	n := lookup(uint64(id))
	if n == nil {
		return output(nil, nil)
	}
	n.closing.Do(func() {
		n.cancel()
		go func() {
			n.gate <- struct{}{}
			defer func() { <-n.gate; clear(n.key[:]); close(n.closed) }()
			n.closeErr = n.shutdown()
		}()
	})
	select {
	case <-n.closed:
		registry.Lock()
		delete(registry.nodes, uint64(id))
		registry.Unlock()
		return output(nil, n.closeErr)
	case <-time.After(10 * time.Second):
		return output(nil, fmt.Errorf("embedded node shutdown is still draining"))
	}
}

func (n *node) shutdown() error {
	var err error
	if n.stopNetworkMonitor != nil {
		n.stopNetworkMonitor()
		n.stopNetworkMonitor = nil
	}
	if n.started {
		err = n.server.Close()
		n.started = false
	}
	if n.logoutCheckpoint != nil {
		err = errors.Join(err, n.store.restore(n.logoutCheckpoint))
		clear(n.logoutCheckpoint)
		n.logoutCheckpoint = nil
	}
	return err
}

//export PaseoFree
func PaseoFree(ptr *C.char) { C.free(unsafe.Pointer(ptr)) }
