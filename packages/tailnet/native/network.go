package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/netip"

	"tailscale.com/net/netmon"
	"tailscale.com/paseo-libtailscale/tsnet"
)

// Android installs these hooks before any node starts. Other platforms retain
// upstream interface enumeration and monitoring unchanged.
var startPlatformNetworkMonitor func(*tsnet.Server) func()
var platformNetworkError func() error

type networkSnapshot struct {
	Interfaces       []networkInterface `json:"interfaces"`
	DefaultInterface string             `json:"default_interface"`
	DefaultGateway   string             `json:"default_gateway"`
	Error            string             `json:"error"`
}

type networkInterface struct {
	Name         string   `json:"name"`
	Index        int      `json:"index"`
	MTU          int      `json:"mtu"`
	Up           bool     `json:"up"`
	Loopback     bool     `json:"loopback"`
	PointToPoint bool     `json:"point_to_point"`
	Multicast    bool     `json:"multicast"`
	Broadcast    bool     `json:"broadcast"`
	Addresses    []string `json:"addresses"`
}

func decodeNetworkSnapshot(raw []byte) (networkSnapshot, []netmon.Interface, error) {
	var snapshot networkSnapshot
	if err := json.Unmarshal(raw, &snapshot); err != nil {
		return snapshot, nil, fmt.Errorf("Android network snapshot: %w", err)
	}
	if snapshot.Error != "" {
		return snapshot, nil, fmt.Errorf("Android network interfaces: %s", snapshot.Error)
	}
	if snapshot.Interfaces == nil {
		return snapshot, nil, errors.New("Android network snapshot is missing interfaces")
	}
	if snapshot.DefaultGateway != "" {
		if _, err := netip.ParseAddr(snapshot.DefaultGateway); err != nil {
			return snapshot, nil, fmt.Errorf("Android default gateway: %w", err)
		}
	}
	interfaces := make([]netmon.Interface, 0, len(snapshot.Interfaces))
	seenNames := make(map[string]bool, len(snapshot.Interfaces))
	seenIndexes := make(map[int]bool, len(snapshot.Interfaces))
	for _, item := range snapshot.Interfaces {
		if item.Addresses == nil {
			return snapshot, nil, fmt.Errorf("Android interface %s is missing its address list", item.Name)
		}
		if item.Name == "" || item.Index <= 0 || item.MTU < 0 || seenNames[item.Name] || seenIndexes[item.Index] {
			return snapshot, nil, errors.New("Android network snapshot has invalid or duplicate interface metadata")
		}
		seenNames[item.Name], seenIndexes[item.Index] = true, true
		iface := &net.Interface{Name: item.Name, Index: item.Index, MTU: item.MTU}
		if item.Up {
			iface.Flags |= net.FlagUp
		}
		if item.Loopback {
			iface.Flags |= net.FlagLoopback
		}
		if item.PointToPoint {
			iface.Flags |= net.FlagPointToPoint
		}
		if item.Multicast {
			iface.Flags |= net.FlagMulticast
		}
		if item.Broadcast {
			iface.Flags |= net.FlagBroadcast
		}
		// Non-nil AltAddrs prevents net.Interface.Addrs from using netlink,
		// including when this real interface currently has no addresses.
		addresses := make([]net.Addr, 0, len(item.Addresses))
		for _, rawAddress := range item.Addresses {
			ip, prefix, err := net.ParseCIDR(rawAddress)
			if err != nil {
				return snapshot, nil, fmt.Errorf("Android interface %s address: %w", item.Name, err)
			}
			prefix.IP = ip // Preserve the host address, not the masked network.
			addresses = append(addresses, prefix)
		}
		interfaces = append(interfaces, netmon.Interface{Interface: iface, AltAddrs: addresses})
	}
	return snapshot, interfaces, nil
}
