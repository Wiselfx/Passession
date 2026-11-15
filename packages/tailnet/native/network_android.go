package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"errors"
	"sync"

	"tailscale.com/net/netmon"
	"tailscale.com/paseo-libtailscale/tsnet"
)

var androidNetwork = struct {
	sync.Mutex
	interfaces []netmon.Interface
	err        error
	monitors   map[*netmon.Monitor]bool
}{err: errors.New("Android network interface provider is not initialized"), monitors: make(map[*netmon.Monitor]bool)}

func init() {
	// Register once, before any tsnet goroutines. The supplied addresses keep
	// both interface enumeration AND Interface.Addrs away from Go netlink.
	netmon.RegisterInterfaceGetter(func() ([]netmon.Interface, error) {
		androidNetwork.Lock()
		defer androidNetwork.Unlock()
		return androidNetwork.interfaces, androidNetwork.err
	})
	platformNetworkError = func() error {
		androidNetwork.Lock()
		defer androidNetwork.Unlock()
		return androidNetwork.err
	}
	startPlatformNetworkMonitor = func(server *tsnet.Server) func() {
		monitor := server.Sys().NetMon.Get()
		androidNetwork.Lock()
		androidNetwork.monitors[monitor] = true
		// Covers changes between netmon.New's first snapshot and attachment.
		monitor.InjectEvent()
		androidNetwork.Unlock()
		return func() {
			androidNetwork.Lock()
			delete(androidNetwork.monitors, monitor)
			androidNetwork.Unlock()
		}
	}
}

// PaseoAndroidNetworkUpdate accepts an OS snapshot or an explicit provider
// error. Failed reads replace, rather than silently preserve, stale state.
// The caller owns raw; the result uses PaseoFree, like every other C ABI reply.
//
//export PaseoAndroidNetworkUpdate
func PaseoAndroidNetworkUpdate(raw *C.char) *C.char {
	var bytes []byte
	if raw != nil {
		bytes = []byte(C.GoString(raw))
	}
	snapshot, interfaces, err := decodeNetworkSnapshot(bytes)
	androidNetwork.Lock()
	androidNetwork.interfaces, androidNetwork.err = interfaces, err
	if err == nil {
		netmon.UpdateLastKnownDefaultRouteInterface(snapshot.DefaultInterface)
		netmon.UpdateLastKnownDefaultGateway(snapshot.DefaultGateway)
	}
	for monitor := range androidNetwork.monitors {
		monitor.InjectEvent()
	}
	androidNetwork.Unlock()
	return output(nil, err)
}
