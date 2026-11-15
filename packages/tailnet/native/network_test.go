package main

import (
	"net"
	"strings"
	"testing"
)

func TestNetworkSnapshotPreservesOSInterfaceAddresses(t *testing.T) {
	snapshot, interfaces, err := decodeNetworkSnapshot([]byte(`{
		"interfaces": [
			{"name":"wlan0","index":2147483646,"mtu":1500,"up":true,"multicast":true,"broadcast":true,"addresses":["192.0.2.7/24","fe80::1234/64"]},
			{"name":"down0","index":2147483645,"mtu":1280,"addresses":[]}
		],
		"default_interface":"wlan0","default_gateway":"192.0.2.1"
	}`))
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.DefaultInterface != "wlan0" || snapshot.DefaultGateway != "192.0.2.1" || len(interfaces) != 2 {
		t.Fatalf("lost OS metadata: %+v", snapshot)
	}
	iface := interfaces[0]
	if iface.Name != "wlan0" || iface.Index != 2147483646 || iface.MTU != 1500 || iface.Flags != net.FlagUp|net.FlagMulticast|net.FlagBroadcast {
		t.Fatalf("lost interface metadata: %+v", iface)
	}
	// Deliberately nonexistent indexes: neither Addrs call may query netlink.
	addresses, err := iface.Addrs()
	if err != nil || len(addresses) != 2 || addresses[0].String() != "192.0.2.7/24" || addresses[1].String() != "fe80::1234/64" {
		t.Fatalf("lost host addresses/prefixes: %v, %v", addresses, err)
	}
	if interfaces[1].AltAddrs == nil {
		t.Fatal("nil AltAddrs would fall through to Go netlink")
	}
	addresses, err = interfaces[1].Addrs()
	if err != nil || len(addresses) != 0 {
		t.Fatalf("empty-address OS interface queried netlink: %v, %v", addresses, err)
	}
}

func TestNetworkSnapshotRejectsMissingOrFailedOSState(t *testing.T) {
	for _, raw := range []string{
		``, `{}`, `{"interfaces":null}`, `{"error":"java.net.SocketException: denied"}`,
		`{"interfaces":[{"name":"wlan0","index":0,"mtu":1500,"addresses":[]}]}`,
		`{"interfaces":[{"name":"wlan0","index":1,"mtu":1500}]}`,
		`{"interfaces":[{"name":"wlan0","index":1,"mtu":1500,"addresses":["invalid"]}]}`,
		`{"interfaces":[{"name":"wlan0","index":1,"mtu":1500,"addresses":[]},{"name":"wlan0","index":2,"mtu":1500,"addresses":[]}]}`,
		`{"interfaces":[],"default_gateway":"not-an-address"}`,
	} {
		_, interfaces, err := decodeNetworkSnapshot([]byte(raw))
		if err == nil || interfaces != nil {
			t.Errorf("accepted unusable OS snapshot %s: %v, %v", raw, interfaces, err)
		}
	}
	_, _, err := decodeNetworkSnapshot([]byte(`{"error":"java.net.SocketException: denied"}`))
	if err == nil || !strings.Contains(err.Error(), "SocketException: denied") {
		t.Fatalf("OS error was lost: %v", err)
	}
	_, interfaces, err := decodeNetworkSnapshot([]byte(`{"interfaces":[]}`))
	if err != nil || interfaces == nil || len(interfaces) != 0 {
		t.Fatalf("explicit OS empty list is distinct from enumeration failure: %v, %v", interfaces, err)
	}
}

func TestNetworkSnapshotReplacesChangedAndRemovedAddresses(t *testing.T) {
	for _, raw := range []string{
		`{"interfaces":[{"name":"wlan0","index":2,"mtu":1500,"addresses":["192.0.2.7/24"]}]}`,
		`{"interfaces":[{"name":"wlan0","index":2,"mtu":1500,"addresses":["198.51.100.8/24"]}]}`,
		`{"interfaces":[]}`,
	} {
		snapshot, interfaces, err := decodeNetworkSnapshot([]byte(raw))
		if err != nil || len(interfaces) != len(snapshot.Interfaces) {
			t.Fatalf("snapshot was not replaced: %v, %v", interfaces, err)
		}
		for i, iface := range interfaces {
			addresses, err := iface.Addrs()
			if err != nil || addresses[0].String() != snapshot.Interfaces[i].Addresses[0] {
				t.Fatalf("stale interface address: %v, %v", addresses, err)
			}
		}
	}
}
