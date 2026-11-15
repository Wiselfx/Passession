package tsnet

import (
	"net"
	"sync"
	"testing"
	"time"
)

func TestTrackedLoopbackClose(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	tracked := trackListener(listener)
	client, err := net.DialTimeout("tcp", listener.Addr().String(), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	server, err := tracked.Accept()
	if err != nil {
		t.Fatal(err)
	}
	// Simultaneous connection and listener close must neither deadlock nor
	// leave a pre-authentication SOCKS handshake alive after node disconnect.
	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); server.Close() }()
	go func() { defer wg.Done(); tracked.Close() }()
	done := make(chan struct{})
	go func() { wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("loopback shutdown deadlocked")
	}
	client.SetReadDeadline(time.Now().Add(time.Second))
	var b [1]byte
	_, err = client.Read(b[:])
	if err == nil {
		t.Fatal("accepted loopback socket remained open")
	}
	if timeout, ok := err.(net.Error); ok && timeout.Timeout() {
		t.Fatal("accepted loopback socket was not closed")
	}
}
