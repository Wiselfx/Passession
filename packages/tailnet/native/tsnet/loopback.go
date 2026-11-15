// Copyright (c) Passession contributors
// SPDX-License-Identifier: BSD-3-Clause
package tsnet

import (
	"net"
	"sync"
)

// Closing a listener alone does not close accepted SOCKS handshakes or HTTP
// keep-alives. Keep them owned by the node, not by leaked serving goroutines.
type trackedListener struct {
	net.Listener
	mu          sync.Mutex
	closed      bool
	connections map[*trackedConn]struct{}
}
type trackedConn struct {
	net.Conn
	owner *trackedListener
	once  sync.Once
}

func trackListener(listener net.Listener) net.Listener {
	return &trackedListener{Listener: listener, connections: make(map[*trackedConn]struct{})}
}
func (l *trackedListener) Accept() (net.Conn, error) {
	for {
		c, err := l.Listener.Accept()
		if err != nil {
			return nil, err
		}
		l.mu.Lock()
		if l.closed {
			l.mu.Unlock()
			c.Close()
			return nil, net.ErrClosed
		}
		if len(l.connections) >= 512 {
			l.mu.Unlock()
			c.Close()
			continue
		}
		tracked := &trackedConn{Conn: c, owner: l}
		l.connections[tracked] = struct{}{}
		l.mu.Unlock()
		return tracked, nil
	}
}
func (l *trackedListener) Close() error {
	l.mu.Lock()
	l.closed = true
	connections := make([]*trackedConn, 0, len(l.connections))
	for c := range l.connections {
		connections = append(connections, c)
	}
	l.mu.Unlock()
	err := l.Listener.Close()
	for _, c := range connections {
		c.Close()
	}
	return err
}
func (c *trackedConn) Close() error {
	var err error
	c.once.Do(func() { err = c.Conn.Close(); c.owner.mu.Lock(); delete(c.owner.connections, c); c.owner.mu.Unlock() })
	return err
}
func (c *trackedConn) CloseRead() error {
	if half, ok := c.Conn.(interface{ CloseRead() error }); ok {
		return half.CloseRead()
	}
	return c.Close()
}
func (c *trackedConn) CloseWrite() error {
	if half, ok := c.Conn.(interface{ CloseWrite() error }); ok {
		return half.CloseWrite()
	}
	return c.Close()
}
