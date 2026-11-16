package main

import (
	"bytes"
	"encoding/binary"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
	"unsafe"
)

// WinTrust reads these structures through raw pointers; their Go layout must
// match the 64-bit C ABI.
func TestWinTrustStructuresMatchNativeLayout(t *testing.T) {
	if unsafe.Sizeof(uintptr(0)) != 8 {
		t.Skip("layout constants are for 64-bit Windows")
	}
	var data winTrustData
	var info winTrustFileInfo
	var member winTrustCatalogInfo
	var catalog catalogInfo
	var cert cryptProviderCert
	for name, c := range map[string][2]uintptr{
		"WINTRUST_DATA size":                         {unsafe.Sizeof(data), 88},
		"WINTRUST_DATA.pFile":                        {unsafe.Offsetof(data.Choice), 40},
		"WINTRUST_DATA.hWVTStateData":                {unsafe.Offsetof(data.StateData), 56},
		"WINTRUST_DATA.pSignatureSettings":           {unsafe.Offsetof(data.SignatureSettings), 80},
		"WINTRUST_FILE_INFO size":                    {unsafe.Sizeof(info), 32},
		"WINTRUST_CATALOG_INFO size":                 {unsafe.Sizeof(member), 72},
		"WINTRUST_CATALOG_INFO.hMemberFile":          {unsafe.Offsetof(member.MemberFile), 32},
		"WINTRUST_CATALOG_INFO.cbCalculatedFileHash": {unsafe.Offsetof(member.CalculatedFileHashSize), 48},
		"WINTRUST_CATALOG_INFO.hCatAdmin":            {unsafe.Offsetof(member.CatAdmin), 64},
		"CATALOG_INFO size":                          {unsafe.Sizeof(catalog), 524},
		"CRYPT_PROVIDER_CERT.pCert":                  {unsafe.Offsetof(cert.Cert), 8},
	} {
		if c[0] != c[1] {
			t.Errorf("%s = %d, want %d", name, c[0], c[1])
		}
	}
}

// Inbox Windows executables are catalog-signed; they must verify through the
// catalog signer rather than fail closed or pass as unsigned.
func TestNativeAuthenticodeVerifiesInboxCatalogSigner(t *testing.T) {
	t.Setenv("PSModulePath", filepath.Join(t.TempDir(), "PowerShell", "7", "Modules"))
	system := os.Getenv("SystemRoot")
	if system == "" {
		t.Fatal("SystemRoot missing")
	}
	inbox := filepath.Join(system, "System32", "cmd.exe")
	identity, err := inspectAuthenticode(inbox)
	if err != nil || !identity.signed || !bytes.Contains(identity.subject, []byte("Microsoft")) {
		t.Fatalf("inbox image: signed=%v subject=%q, %v", identity.signed, identity.subject, err)
	}
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	// The unsigned test executable cannot replace a signed installation, but a
	// signed image may replace an unsigned one.
	if err = verifyPublisherContinuity(inbox, executable, "installer", "installed app"); err == nil || !strings.Contains(err.Error(), "installer publisher changed") {
		t.Fatalf("signed to unsigned: %v", err)
	}
	if err = verifyPublisherContinuity(executable, inbox, "installer", "installed app"); err != nil {
		t.Fatalf("unsigned to signed: %v", err)
	}
}

func TestNativeAuthenticodeClassifiesUnsignedAndTamperedImages(t *testing.T) {
	// A PowerShell 7 module path inherited by the helper broke Windows
	// PowerShell's Microsoft.PowerShell.Security; verification no longer loads it.
	poisoned := filepath.Join(t.TempDir(), "PowerShell", "7", "Modules")
	t.Setenv("PSModulePath", poisoned+string(os.PathListSeparator)+os.Getenv("PSModulePath"))
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	// go test binaries carry no Authenticode signature and are in no catalog.
	image, err := os.ReadFile(executable)
	if err != nil {
		t.Fatal(err)
	}
	directory := t.TempDir()
	unsigned := filepath.Join(directory, "unsigned.exe")
	if err = os.WriteFile(unsigned, image, 0600); err != nil {
		t.Fatal(err)
	}
	identity, err := inspectAuthenticode(unsigned)
	if err != nil || identity.signed {
		t.Fatalf("unsigned image: %+v, %v", identity, err)
	}
	if err = verifyPublisherContinuity(unsigned, unsigned, "installer", "installed app"); err != nil {
		t.Fatal(err)
	}

	// Declare a certificate table holding a WIN_CERTIFICATE with no valid
	// PKCS#7 signature: an invalid signature, never an unsigned image.
	entry, _, _, err := peSecurityDirectory(bytes.NewReader(image), int64(len(image)))
	if err != nil || entry < 0 {
		t.Fatalf("test executable security directory: %d, %v", entry, err)
	}
	tampered := bytes.Clone(image)
	for len(tampered)%8 != 0 {
		tampered = append(tampered, 0)
	}
	offset := len(tampered)
	certificate := bytes.Repeat([]byte{0xa5}, 32)
	binary.LittleEndian.PutUint32(certificate, uint32(len(certificate)))
	binary.LittleEndian.PutUint16(certificate[4:], 0x0200) // WIN_CERT_REVISION_2_0
	binary.LittleEndian.PutUint16(certificate[6:], 0x0002) // WIN_CERT_TYPE_PKCS_SIGNED_DATA
	tampered = append(tampered, certificate...)
	binary.LittleEndian.PutUint32(tampered[entry:], uint32(offset))
	binary.LittleEndian.PutUint32(tampered[entry+4:], uint32(len(certificate)))
	invalid := filepath.Join(directory, "tampered.exe")
	if err = os.WriteFile(invalid, tampered, 0600); err != nil {
		t.Fatal(err)
	}
	if identity, err = inspectAuthenticode(invalid); err == nil {
		t.Fatalf("tampered image classified as %+v", identity)
	}
	if err = verifyPublisherContinuity(unsigned, invalid, "installer", "installed app"); err == nil || !strings.Contains(err.Error(), "invalid installer signature") {
		t.Fatalf("tampered candidate: %v", err)
	}
	if err = verifyPublisherContinuity(invalid, unsigned, "installer", "installed app"); err == nil || !strings.Contains(err.Error(), "invalid installed app signature") {
		t.Fatalf("tampered installation: %v", err)
	}
}

func TestRegisteredInstallationReadsCurrentUserStringValues(t *testing.T) {
	createKey := advapi32.NewProc("RegCreateKeyExW")
	setValue := advapi32.NewProc("RegSetValueExW")
	deleteKey := advapi32.NewProc("RegDeleteKeyW")
	subkey := `Software\Paseo Sessions Updater Test ` + strconv.Itoa(os.Getpid()) + "-" + strconv.FormatInt(time.Now().UnixNano(), 36)
	path, err := syscall.UTF16PtrFromString(subkey)
	if err != nil {
		t.Fatal(err)
	}
	var key syscall.Handle
	// REG_OPTION_VOLATILE: the key cannot outlive this logon if cleanup fails.
	if result, _, _ := createKey.Call(uintptr(syscall.HKEY_CURRENT_USER), uintptr(unsafe.Pointer(path)), 0, 0, 1, syscall.KEY_SET_VALUE, 0, uintptr(unsafe.Pointer(&key)), 0); uint32(result) != 0 {
		t.Fatal(syscall.Errno(uint32(result)))
	}
	t.Cleanup(func() {
		syscall.RegCloseKey(key)
		deleteKey.Call(uintptr(syscall.HKEY_CURRENT_USER), uintptr(unsafe.Pointer(path)))
	})
	name, err := syscall.UTF16PtrFromString("InstallDir")
	if err != nil {
		t.Fatal(err)
	}
	set := func(kind uint32, data []byte) {
		t.Helper()
		var first *byte
		if len(data) > 0 {
			first = &data[0]
		}
		if result, _, _ := setValue.Call(uintptr(key), uintptr(unsafe.Pointer(name)), 0, uintptr(kind), uintptr(unsafe.Pointer(first)), uintptr(len(data))); uint32(result) != 0 {
			t.Fatal(syscall.Errno(uint32(result)))
		}
	}
	text := func(s string) []byte {
		u, err := syscall.UTF16FromString(s)
		if err != nil {
			t.Fatal(err)
		}
		return unsafe.Slice((*byte)(unsafe.Pointer(&u[0])), len(u)*2)
	}

	root := filepath.Join(t.TempDir(), "Paseo Sessions")
	t.Setenv("PASEO_UPDATER_TEST_ROOT", root)
	set(syscall.REG_EXPAND_SZ, text(`%PASEO_UPDATER_TEST_ROOT%\`))
	registered, err := registeredInstallDirectory(subkey)
	if err != nil || registered != root+`\` {
		t.Fatalf("expanded install directory %q, %v", registered, err)
	}
	if !sameRegisteredInstallation(registered, strings.ToUpper(root)) {
		t.Fatalf("%q does not match its own destination", registered)
	}

	set(syscall.REG_SZ, text(filepath.Join(root, "..", "Paseo Sessions")))
	if registered, err = registeredInstallDirectory(subkey); err != nil || !sameRegisteredInstallation(registered, root) {
		t.Fatalf("normalized install directory %q, %v", registered, err)
	}

	set(syscall.REG_SZ, text(root+" Old"))
	if registered, err = registeredInstallDirectory(subkey); err != nil || sameRegisteredInstallation(registered, root) {
		t.Fatalf("different install directory %q accepted, %v", registered, err)
	}

	set(syscall.REG_SZ, text(`Paseo Sessions`))
	if registered, err = registeredInstallDirectory(subkey); err != nil || sameRegisteredInstallation(registered, filepath.Join(mustGetwd(t), "Paseo Sessions")) {
		t.Fatalf("relative install directory %q accepted, %v", registered, err)
	}

	set(syscall.REG_SZ, append(text(root), text(`C:\Other`)...))
	if registered, err = registeredInstallDirectory(subkey); err == nil {
		t.Fatalf("embedded NUL install directory read as %q", registered)
	}

	set(syscall.REG_DWORD, []byte{1, 0, 0, 0})
	if registered, err = registeredInstallDirectory(subkey); err == nil {
		t.Fatalf("REG_DWORD install directory read as %q", registered)
	}
}

func mustGetwd(t *testing.T) string {
	t.Helper()
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	return wd
}
