package main

import (
	"encoding/binary"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

func validateNative(r request) error {
	switch r.InstallerFormat {
	case "appimage":
		file, err := os.Open(r.StagedArtifact)
		if err != nil {
			return err
		}
		defer file.Close()
		b := make([]byte, 20)
		if _, err = file.ReadAt(b, 0); err != nil {
			return err
		}
		machine := uint16(62)
		if runtime.GOARCH == "arm64" {
			machine = 183
		}
		if string(b[:4]) != "\x7fELF" || b[4] != 2 || b[5] != 1 || string(b[8:11]) != "AI\x02" || binary.LittleEndian.Uint16(b[18:20]) != machine {
			return errors.New("artifact is not a matching type-2 AppImage")
		}
		return nil
	case "zip":
		return nil // extracted digest-bound bundle is checked before ready
	case "exe":
		if strings.ContainsAny(r.StagedArtifact+r.Destination, "\"") {
			return errors.New("quotes are not valid installer paths")
		}
		if err := verifyPublisherContinuity(r.CurrentExecutable, r.StagedArtifact, "installer", "installed app"); err != nil {
			return err
		}
		return checkRegisteredInstallation(r.Destination)
	case "deb", "rpm":
		if _, err := os.Stat("/usr/bin/pkexec"); err != nil {
			return errors.New("package updates require polkit pkexec")
		}
		var fields, owner string
		var err error
		if r.InstallerFormat == "deb" {
			fields, err = commandOutput("/usr/bin/dpkg-deb", "--show", "--showformat=${Package}\n${Version}\n${Architecture}\n", r.StagedArtifact)
			if err != nil {
				return err
			}
			owner, err = commandOutput("/usr/bin/dpkg-query", "-S", r.CurrentExecutable)
			if err != nil {
				return err
			}
			found := false
			for _, line := range strings.Split(owner, "\n") {
				packageName, path, ok := strings.Cut(line, ": ")
				if ok && (packageName == "paseo-sessions" || strings.HasPrefix(packageName, "paseo-sessions:")) && path == r.CurrentExecutable {
					found = true
				}
			}
			if !found {
				return errors.New("current GUI is not owned by paseo-sessions")
			}
		} else {
			fields, err = commandOutput("/usr/bin/rpm", "-qp", "--qf", "%{NAME}\n%{VERSION}\n%{ARCH}\n", r.StagedArtifact)
			if err != nil {
				return err
			}
			owner, err = commandOutput("/usr/bin/rpm", "-qf", "--qf", "%{NAME}", r.CurrentExecutable)
			if err != nil {
				return err
			}
			if owner != "paseo-sessions" {
				return errors.New("current GUI is not owned by paseo-sessions")
			}
		}
		arch := "amd64"
		if runtime.GOARCH == "arm64" {
			arch = "arm64"
		}
		if r.InstallerFormat == "rpm" {
			arch = "x86_64"
			if runtime.GOARCH == "arm64" {
				arch = "aarch64"
			}
		}
		if fields != "paseo-sessions\n"+r.Expected.Version+"\n"+arch {
			return errors.New("package identity/version/architecture mismatch")
		}
		return nil
	default:
		return errors.New("unknown artifact format")
	}
}

// Only fixed platform commands are constructed from an admitted request.
func nativeInstallerCommand(r request) (*exec.Cmd, error) {
	switch r.InstallerFormat {
	case "exe":
		return windowsInstallerCommand(r)
	case "deb":
		return exec.Command("/usr/bin/pkexec", "/usr/bin/dpkg", "--install", r.StagedArtifact), nil
	case "rpm":
		return exec.Command("/usr/bin/pkexec", "/usr/bin/rpm", "-U", "--", r.StagedArtifact), nil
	default:
		return nil, errors.New("unsupported installer")
	}
}
func validateMacApp(old, new, version string) error {
	plist := filepath.Join(new, "Contents", "Info.plist")
	id, err := commandOutput("/usr/libexec/PlistBuddy", "-c", "Print :CFBundleIdentifier", plist)
	if err != nil {
		return err
	}
	actualVersion, err := commandOutput("/usr/libexec/PlistBuddy", "-c", "Print :CFBundleShortVersionString", plist)
	if err != nil {
		return err
	}
	if id != "sh.paseo.sessions.desktop" || actualVersion != version {
		return errors.New("app bundle identity/version mismatch")
	}
	if err = exec.Command("/usr/bin/codesign", "--verify", "--deep", "--strict", new).Run(); err != nil {
		return err
	}
	signature := func(path string) (string, error) {
		output, err := exec.Command("/usr/bin/codesign", "-d", "--verbose=4", path).CombinedOutput()
		return string(output), err
	}
	before, err := signature(old)
	if err != nil {
		return err
	}
	after, err := signature(new)
	if err != nil {
		return err
	}
	team := func(text string) string {
		for _, line := range strings.Split(text, "\n") {
			if value, ok := strings.CutPrefix(line, "TeamIdentifier="); ok && value != "not set" {
				return value
			}
		}
		return ""
	}
	if previous := team(before); previous != "" {
		if team(after) != previous {
			return errors.New("macOS publisher changed")
		}
		return exec.Command("/usr/sbin/spctl", "--assess", "--type", "execute", new).Run()
	}
	if !strings.Contains(before, "Signature=adhoc") || !strings.Contains(after, "Signature=adhoc") {
		return errors.New("unverifiable macOS publisher")
	}
	return nil
}
