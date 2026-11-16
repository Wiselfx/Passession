package main

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/binary"
	"math/big"
	"testing"
	"time"
)

func codeSigningCertificate(t *testing.T, subject pkix.Name, serial int64) []byte {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	template := &x509.Certificate{SerialNumber: big.NewInt(serial), Subject: subject, NotBefore: time.Unix(0, 0), NotAfter: time.Unix(1<<31, 0), ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageCodeSigning}}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	return der
}

func signerSubject(t *testing.T, der []byte) authenticodeIdentity {
	t.Helper()
	subject, err := certificateSubject(der)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(subject, parsed.RawSubject) {
		t.Fatalf("subject DER %x, want %x", subject, parsed.RawSubject)
	}
	return authenticodeIdentity{signed: true, subject: subject}
}

func TestAuthenticodePublisherContinuityUsesVerifiedSubjectDER(t *testing.T) {
	paseo := pkix.Name{CommonName: "Paseo", Organization: []string{"Paseo"}, Country: []string{"US"}}
	installed := signerSubject(t, codeSigningCertificate(t, paseo, 1))
	// A renewed certificate keeps the publisher with a new key and serial.
	renewed := signerSubject(t, codeSigningCertificate(t, paseo, 2))
	other := signerSubject(t, codeSigningCertificate(t, pkix.Name{CommonName: "Paseo", Organization: []string{"Other"}, Country: []string{"US"}}, 3))
	unsigned := authenticodeIdentity{}
	for _, c := range []struct {
		name                 string
		installed, candidate authenticodeIdentity
		allowed              bool
	}{
		{"signed to same publisher", installed, renewed, true},
		{"signed to other publisher", installed, other, false},
		{"signed to unsigned", installed, unsigned, false},
		{"unsigned to unsigned", unsigned, unsigned, true},
		{"unsigned to signed", unsigned, other, true},
	} {
		if publisherContinues(c.installed, c.candidate) != c.allowed {
			t.Errorf("%s: allowed=%v", c.name, !c.allowed)
		}
	}
}

func TestCertificateSubjectRejectsMalformedDER(t *testing.T) {
	der := codeSigningCertificate(t, pkix.Name{CommonName: "Paseo"}, 1)
	for name, input := range map[string][]byte{
		"truncated":     der[:len(der)-1],
		"trailing data": append(bytes.Clone(der), 0),
		"empty":         nil,
	} {
		if _, err := certificateSubject(input); err == nil {
			t.Errorf("%s certificate accepted", name)
		}
	}
}

func peFixture(magic uint16, directories uint32, address, length uint32) (image []byte, entry int64) {
	const pe = 0x80
	count, table := 92, 96
	if magic == 0x20b {
		count, table = 108, 112
	}
	optionalSize := table + 16*8
	image = make([]byte, pe+24+optionalSize)
	copy(image, "MZ")
	binary.LittleEndian.PutUint32(image[0x3c:], pe)
	copy(image[pe:], "PE\x00\x00")
	binary.LittleEndian.PutUint16(image[pe+20:], uint16(optionalSize))
	optional := image[pe+24:]
	binary.LittleEndian.PutUint16(optional, magic)
	binary.LittleEndian.PutUint32(optional[count:], directories)
	binary.LittleEndian.PutUint32(optional[table+32:], address)
	binary.LittleEndian.PutUint32(optional[table+36:], length)
	return image, int64(pe + 24 + table + 32)
}

func TestPESecurityDirectoryDistinguishesAbsentCertificateTable(t *testing.T) {
	for _, c := range []struct {
		name                       string
		magic                      uint16
		directories, address, size uint32
		declared                   bool
	}{
		{"PE32+ unsigned", 0x20b, 16, 0, 0, true},
		{"PE32+ certificate table", 0x20b, 16, 0x400, 0x20, true},
		{"PE32 certificate table", 0x10b, 16, 0x400, 0x20, true},
		{"PE32+ size-only table", 0x20b, 16, 0, 0x20, true},
		{"undeclared security directory", 0x20b, 4, 0x400, 0x20, false},
	} {
		image, want := peFixture(c.magic, c.directories, c.address, c.size)
		entry, address, size, err := peSecurityDirectory(bytes.NewReader(image), int64(len(image)))
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if !c.declared {
			want = -1
			c.address, c.size = 0, 0
		}
		if entry != want || address != c.address || size != c.size {
			t.Errorf("%s: entry=%d address=%#x size=%#x", c.name, entry, address, size)
		}
	}
	valid, _ := peFixture(0x20b, 16, 0, 0)
	for name, mutate := range map[string]func([]byte) []byte{
		"not MZ":               func(b []byte) []byte { b[0] = 'X'; return b },
		"not PE":               func(b []byte) []byte { b[0x80] = 'X'; return b },
		"header beyond file":   func(b []byte) []byte { binary.LittleEndian.PutUint32(b[0x3c:], uint32(len(b))); return b },
		"unknown optional":     func(b []byte) []byte { binary.LittleEndian.PutUint16(b[0x80+24:], 0x107); return b },
		"truncated optional":   func(b []byte) []byte { return b[:len(b)-1] },
		"short directory list": func(b []byte) []byte { binary.LittleEndian.PutUint16(b[0x80+20:], 112+4*8); return b },
	} {
		image := mutate(bytes.Clone(valid))
		if _, _, _, err := peSecurityDirectory(bytes.NewReader(image), int64(len(image))); err == nil {
			t.Errorf("%s image accepted", name)
		}
	}
}
