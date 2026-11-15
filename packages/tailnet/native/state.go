// Copyright (c) Passession contributors
// SPDX-License-Identifier: Apache-2.0
package main

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"tailscale.com/atomicfile"
	"tailscale.com/ipn"
)

const stateFile = "tailscaled.enc"

var stateAAD = []byte("Passession embedded tailscale state v1")

type encryptedStore struct {
	mu     sync.Mutex
	path   string
	aead   cipher.AEAD
	values map[ipn.StateKey][]byte
}

func openStore(dir string, key []byte) (*encryptedStore, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	s := &encryptedStore{path: filepath.Join(dir, stateFile), aead: aead, values: make(map[ipn.StateKey][]byte)}
	// Refuse legacy plaintext rather than accidentally starting a different identity.
	for _, name := range []string{"tailscaled.state", "tailscaled.log.conf", "tailscaled.log1.txt", "tailscaled.log2.txt"} {
		if _, err := os.Stat(filepath.Join(dir, name)); err == nil {
			return nil, fmt.Errorf("unprotected legacy Tailscale state exists; migrate or remove it explicitly")
		} else if !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	b, err := os.ReadFile(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return s, nil
	}
	if err != nil {
		return nil, err
	}
	if len(b) < 4+aead.NonceSize()+aead.Overhead() || string(b[:4]) != "PST1" {
		return nil, errors.New("invalid encrypted Tailscale state")
	}
	plain, err := aead.Open(nil, b[4:4+aead.NonceSize()], b[4+aead.NonceSize():], stateAAD)
	if err != nil {
		return nil, errors.New("Tailscale state authentication failed; unlock the original OS credential")
	}
	defer clear(plain)
	if err := json.Unmarshal(plain, &s.values); err != nil {
		return nil, errors.New("invalid authenticated Tailscale state")
	}
	if s.values == nil {
		s.values = make(map[ipn.StateKey][]byte)
	}
	return s, nil
}
func (s *encryptedStore) ReadState(key ipn.StateKey) ([]byte, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	b, ok := s.values[key]
	if !ok {
		return nil, ipn.ErrStateNotExist
	}
	return append([]byte(nil), b...), nil
}
func (s *encryptedStore) WriteState(key ipn.StateKey, value []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	old, had := s.values[key]
	if value == nil {
		delete(s.values, key)
	} else {
		s.values[key] = append([]byte(nil), value...)
	}
	if err := s.save(); err != nil {
		if had {
			s.values[key] = old
		} else {
			delete(s.values, key)
		}
		return err
	}
	clear(old)
	return nil
}
func (s *encryptedStore) save() error {
	plain, err := json.Marshal(s.values)
	if err != nil {
		return err
	}
	defer clear(plain)
	nonce := make([]byte, s.aead.NonceSize())
	if _, err = rand.Read(nonce); err != nil {
		return err
	}
	out := append([]byte("PST1"), nonce...)
	out = s.aead.Seal(out, nonce, plain, stateAAD)
	return atomicfile.WriteFile(s.path, out, 0600)
}

func (s *encryptedStore) checkpoint() ([]byte, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := s.save(); err != nil {
		return nil, err
	}
	return os.ReadFile(s.path)
}

// Called only after all backend writers have shut down.
func (s *encryptedStore) restore(checkpoint []byte) error {
	return atomicfile.WriteFile(s.path, checkpoint, 0600)
}
