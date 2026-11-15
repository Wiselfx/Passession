package tsnet

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"tailscale.com/logpolicy"
	"tailscale.com/types/logger"
)

func TestBackendLogsDirWithoutAmbientDirectories(t *testing.T) {
	root := t.TempDir()
	// Reproduce the relevant Android process conditions without changing the
	// host's directories or permissions. Test the actual upstream resolver.
	for _, name := range []string{"HOME", "USERPROFILE", "XDG_CACHE_HOME", "TS_LOGS_DIR"} {
		t.Setenv(name, "")
		if err := os.Unsetenv(name); err != nil {
			t.Fatal(err)
		}
	}
	for _, name := range []string{"TMPDIR", "TMP", "TEMP"} {
		t.Setenv(name, filepath.Join(root, "unavailable-temp"))
	}
	t.Chdir(filepath.VolumeName(root) + string(os.PathSeparator))
	if err := withBackendLogsDir(root, func() error {
		if got := logpolicy.LogsDir(logger.Discard); got != root {
			t.Errorf("backend selected ambient directory %q, want private root %q", got, root)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if value, present := os.LookupEnv("TS_LOGS_DIR"); present {
		t.Fatalf("constructor leaked directory override %q", value)
	}
	for _, name := range []string{"HOME", "USERPROFILE", "XDG_CACHE_HOME"} {
		if value, present := os.LookupEnv(name); present {
			t.Errorf("constructor changed %s to %q", name, value)
		}
	}
	for _, name := range []string{"TMPDIR", "TMP", "TEMP"} {
		if got := os.Getenv(name); got != filepath.Join(root, "unavailable-temp") {
			t.Errorf("constructor changed %s to %q", name, got)
		}
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("directory selection created unexpected side files: %v", entries)
	}
}

func TestBackendLogsDirRestoresPreviousValueOnConstructorError(t *testing.T) {
	root := t.TempDir()
	previous := t.TempDir()
	t.Setenv("TS_LOGS_DIR", previous)
	failure := errors.New("backend construction failed")
	err := withBackendLogsDir(root, func() error {
		if got := logpolicy.LogsDir(logger.Discard); got != root {
			t.Errorf("backend selected ambient directory %q, want %q", got, root)
		}
		return failure
	})
	if !errors.Is(err, failure) {
		t.Fatalf("constructor failure was lost: %v", err)
	}
	if got := os.Getenv("TS_LOGS_DIR"); got != previous {
		t.Fatalf("constructor did not restore %q: got %q", previous, got)
	}
}
