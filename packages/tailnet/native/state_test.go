package main

import (
	"bytes"
	"crypto/rand"
	"os"
	"path/filepath"
	"testing"

	"tailscale.com/ipn"
)

func TestEncryptedState(t *testing.T) {
	dir := t.TempDir()
	key := make([]byte, 32)
	if _, err := rand.Read(key); err != nil {
		t.Fatal(err)
	}
	store, err := openStore(dir, key)
	if err != nil {
		t.Fatal(err)
	}
	secret := []byte("private-node-key-never-on-disk")
	if err := store.WriteState(ipn.StateKey("node"), secret); err != nil {
		t.Fatal(err)
	}
	first, err := os.ReadFile(filepath.Join(dir, stateFile))
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(first, secret) {
		t.Fatal("plaintext secret persisted")
	}
	if err := store.WriteState(ipn.StateKey("node"), secret); err != nil {
		t.Fatal(err)
	}
	second, _ := os.ReadFile(filepath.Join(dir, stateFile))
	if bytes.Equal(first, second) {
		t.Fatal("AEAD nonce reused")
	}
	reopened, err := openStore(dir, key)
	if err != nil {
		t.Fatal(err)
	}
	value, err := reopened.ReadState(ipn.StateKey("node"))
	if err != nil || !bytes.Equal(value, secret) {
		t.Fatalf("roundtrip: %v", err)
	}
	value[0] ^= 1
	again, _ := reopened.ReadState(ipn.StateKey("node"))
	if !bytes.Equal(again, secret) {
		t.Fatal("ReadState leaked mutable storage")
	}
	wrong := append([]byte(nil), key...)
	wrong[0] ^= 1
	if _, err := openStore(dir, wrong); err == nil {
		t.Fatal("wrong key accepted")
	}
	second[len(second)-1] ^= 1
	if err := os.WriteFile(filepath.Join(dir, stateFile), second, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := openStore(dir, key); err == nil {
		t.Fatal("tampered ciphertext accepted")
	}
}
func TestRejectsPlaintextState(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "tailscaled.state"), []byte("legacy secret"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := openStore(dir, make([]byte, 32)); err == nil {
		t.Fatal("legacy plaintext was silently ignored")
	}
}
