package main

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

// Different daemon homes may use the same installed GUI. Lock both the host
// (in main.go) and the installation, without requiring write access to /usr.
func acquireInstallationLease(root string) (*os.File, error) {
	cache, err := os.UserCacheDir()
	if err != nil {
		return nil, err
	}
	if err = os.MkdirAll(cache, 0700); err != nil {
		return nil, err
	}
	cache, err = filepath.EvalSymlinks(cache)
	if err != nil {
		return nil, err
	}
	directory := filepath.Join(cache, "passession-update-locks")
	if err = os.Mkdir(directory, 0700); err != nil && !errors.Is(err, os.ErrExist) {
		return nil, err
	}
	if err = privatePath(directory, true); err != nil {
		return nil, err
	}
	if runtime.GOOS == "windows" {
		root = strings.ToLower(normalizedWindowsPath(root))
	}
	key := sha256.Sum256([]byte(root))
	return acquireLease(filepath.Join(directory, hex.EncodeToString(key[:])+".lock"))
}
