package main

import (
	"archive/zip"
	"crypto/sha512"
	"encoding/hex"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

// The test executable doubles as a bounded, real GUI/daemon process fixture.
// This code is absent from production go build output. No installed app,
// package manager, elevation, network listener, or user daemon is touched.
func init() {
	role := os.Getenv("PASEO_UPDATER_FIXTURE_ROLE")
	if role == "" || strings.HasPrefix(role, "component-") {
		return
	}
	if role == "exit" {
		os.Exit(0)
	}
	if role == "idle" || role == "daemon" {
		time.Sleep(15 * time.Second)
		os.Exit(0)
	}
	if role == "installerFailure" {
		time.Sleep(300 * time.Millisecond)
		os.Exit(2)
	}
	if role != "gui" {
		os.Exit(3)
	}
	directory := os.Getenv("PASEO_UPDATE_OPERATION_DIRECTORY")
	if directory == "" {
		time.Sleep(15 * time.Second)
		os.Exit(0)
	}
	exe, err := os.Executable()
	if err != nil {
		os.Exit(4)
	}
	daemon := exec.Command(filepath.Join(filepath.Dir(exe), "runtime", "daemon"+exeSuffix()))
	daemon.Env = append(os.Environ(), "PASEO_UPDATER_FIXTURE_ROLE=daemon")
	if err = daemon.Start(); err != nil {
		os.Exit(5)
	}
	defer func() { _ = daemon.Process.Kill(); _ = daemon.Wait() }()
	gui, err := inspectProcess(os.Getpid())
	if err != nil {
		os.Exit(6)
	}
	dp, err := inspectProcess(daemon.Process.Pid)
	if err != nil {
		os.Exit(7)
	}
	var actual identity
	if _, err = readJSON(filepath.Join(filepath.Dir(exe), "identity.json"), &actual); err != nil {
		os.Exit(8)
	}
	mode := os.Getenv("PASEO_UPDATER_FIXTURE_HEALTH")
	if mode == "wrongHost" {
		actual.ServerID = "other-host"
	}
	h := health{1, os.Getenv("PASEO_UPDATE_OPERATION_ID"), os.Getenv("PASEO_UPDATE_NONCE"), actual, os.Getenv("PASEO_HOME"), gui.identity, dp.identity, dp.executable}
	if mode != "none" {
		if err = atomicJSON(filepath.Join(directory, "health.json"), h); err != nil {
			os.Exit(9)
		}
	}
	deadline := time.Now().Add(12 * time.Second)
	for time.Now().Before(deadline) {
		if _, err = os.Stat(filepath.Join(directory, "stop.json")); err == nil {
			if mode == "refuseStop" {
				time.Sleep(10 * time.Millisecond)
				continue
			}
			_ = daemon.Process.Kill()
			_ = daemon.Wait()
			if err = atomicJSON(filepath.Join(directory, "stopped.json"), map[string]any{"schemaVersion": 1, "operationId": h.OperationID, "nonce": h.Nonce}); err != nil {
				os.Exit(10)
			}
			os.Exit(0)
		}
		time.Sleep(10 * time.Millisecond)
	}
	_ = daemon.Process.Kill()
	_ = daemon.Wait()
	os.Exit(0)
}
func exeSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
func mustWrite(t *testing.T, path string, data []byte, mode os.FileMode) {
	t.Helper()
	if err := os.WriteFile(path, data, mode); err != nil {
		t.Fatal(err)
	}
}
func mustMkdir(t *testing.T, path string) {
	t.Helper()
	if err := os.MkdirAll(path, 0700); err != nil {
		t.Fatal(err)
	}
}
func fixtureBinary(t *testing.T, path string) {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	exe, err = filepath.EvalSymlinks(exe)
	if err != nil {
		t.Fatal(err)
	}
	if err = copyFile(exe, path, 0755); err != nil {
		t.Fatal(err)
	}
}
func canonicalTemp(t *testing.T) string {
	t.Helper()
	path, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	return path
}
func fixtureProcess(t *testing.T, path, role string) (processIdentity, *exec.Cmd) {
	t.Helper()
	cmd := exec.Command(path)
	cmd.Env = append(os.Environ(), "PASEO_UPDATER_FIXTURE_ROLE="+role)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	p, err := inspectProcess(cmd.Process.Pid)
	if err != nil {
		_ = cmd.Process.Kill()
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if !exited(p.identity) {
			_ = cmd.Process.Kill()
		}
		_ = cmd.Wait()
	})
	return p.identity, cmd
}
func killFixture(p processIdentity) {
	if p.PID <= 1 || exited(p) {
		return
	}
	current, err := inspectProcess(p.PID)
	if err != nil || current.identity != p {
		return
	}
	child, err := os.FindProcess(p.PID)
	if err == nil {
		_ = child.Kill()
	}
}
func fixtureOperation(t *testing.T) *operation {
	t.Helper()
	base := t.TempDir()
	base, err := filepath.EvalSymlinks(base)
	if err != nil {
		t.Fatal(err)
	}
	root := filepath.Join(base, "Installed.app")
	opdir := filepath.Join(base, "operation")
	home := filepath.Join(base, "home")
	swap := filepath.Join(base, ".passession-update-11111111-1111-4111-8111-111111111111")
	for _, path := range []string{root, opdir, home, swap} {
		mustMkdir(t, path)
	}
	mustWrite(t, filepath.Join(home, "marker"), []byte("user data"), 0600)
	candidate := filepath.Join(swap, "candidate")
	mustMkdir(t, candidate)
	expected := identity{"host-a", "2.0.0", "source-new"}
	for _, path := range []string{root, candidate} {
		mustMkdir(t, filepath.Join(path, "runtime"))
		fixtureBinary(t, filepath.Join(path, "gui"+exeSuffix()))
		fixtureBinary(t, filepath.Join(path, "runtime", "daemon"+exeSuffix()))
		actual := expected
		if path == root {
			actual.Version = "1.0.0"
			actual.SourceSHA = "source-old"
		}
		if err = atomicJSON(filepath.Join(path, "identity.json"), actual); err != nil {
			t.Fatal(err)
		}
	}
	mustWrite(t, filepath.Join(root, "marker"), []byte("old"), 0600)
	mustWrite(t, filepath.Join(candidate, "marker"), []byte("new"), 0600)
	dead, cmd := fixtureProcess(t, filepath.Join(root, "gui"+exeSuffix()), "idle")
	_ = cmd.Process.Kill()
	_ = cmd.Wait()
	r := request{SchemaVersion: 1, OperationID: "11111111-1111-4111-8111-111111111111", OperationDirectory: opdir, Mode: "directorySwap", InstallerFormat: "zip", CurrentRoot: root, Destination: root, CurrentExecutable: filepath.Join(root, "gui"+exeSuffix()), RelaunchExecutable: filepath.Join(root, "gui"+exeSuffix()), ParentProcess: dead, Expected: expected, DaemonHome: home}
	me, err := inspectProcess(os.Getpid())
	if err != nil {
		t.Fatal(err)
	}
	o := &operation{request: r, journal: journal{SchemaVersion: 1, Step: "preparing", Candidate: candidate, Backup: filepath.Join(swap, "previous"), Status: status{SchemaVersion: 1, OperationID: r.OperationID, Expected: expected, HelperProcess: me.identity, Nonce: strings.Repeat("a", 64)}}, parentTimeout: time.Second, healthTimeout: 2 * time.Second, stopTimeout: 2 * time.Second, interval: 10 * time.Millisecond}
	o.journal.CandidateDigest, err = treeDigest(candidate)
	if err != nil {
		t.Fatal(err)
	}
	o.journal.PreviousDigest, err = treeDigest(root)
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("PASEO_UPDATER_FIXTURE_ROLE", "gui")
	t.Setenv("PASEO_UPDATER_FIXTURE_HEALTH", "good")
	t.Cleanup(func() {
		killFixture(o.journal.Launched)
		if o.journal.Health != nil {
			killFixture(o.journal.Health.GUIProcess)
			killFixture(o.journal.Health.DaemonProcess)
		}
		var h health
		if _, err := readJSON(filepath.Join(opdir, "health.json"), &h); err == nil {
			killFixture(h.GUIProcess)
			killFixture(h.DaemonProcess)
		}
	})
	return o
}
func marker(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(filepath.Join(path, "marker"))
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestSwapCommitsOnlyWithLiveMatchingGUIAndDaemon(t *testing.T) {
	o := fixtureOperation(t)
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	if err := o.startAndCheck(); err != nil {
		t.Fatal(err)
	}
	if o.journal.Status.Phase != "committed" || o.journal.Health == nil {
		t.Fatalf("not committed: %+v", o.journal.Status)
	}
	if marker(t, o.request.Destination) != "new" || marker(t, o.journal.Backup) != "old" {
		t.Fatal("swap or retained backup incorrect")
	}
	if marker(t, o.request.DaemonHome) != "user data" {
		t.Fatal("daemon home changed")
	}
	if exited(o.journal.Health.GUIProcess) || exited(o.journal.Health.DaemonProcess) {
		t.Fatal("committed without live fixture processes")
	}
}
func TestWrongHostRollsBackAfterCooperativeStop(t *testing.T) {
	o := fixtureOperation(t)
	t.Setenv("PASEO_UPDATER_FIXTURE_HEALTH", "wrongHost")
	o.healthTimeout = 300 * time.Millisecond
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	if err := o.startAndCheck(); err != nil {
		t.Fatal(err)
	}
	if o.journal.Status.Phase != "rolledBack" || marker(t, o.request.Destination) != "old" {
		t.Fatalf("unsafe host acceptance: %+v", o.journal.Status)
	}
	if marker(t, o.request.DaemonHome) != "user data" {
		t.Fatal("rollback changed daemon home")
	}
	if marker(t, filepath.Join(filepath.Dir(o.journal.Backup), "failed")) != "new" {
		t.Fatal("failed app not preserved")
	}
}
func TestHealthTimeoutWithoutHealthStillRequiresStoppedAck(t *testing.T) {
	o := fixtureOperation(t)
	t.Setenv("PASEO_UPDATER_FIXTURE_HEALTH", "none")
	o.healthTimeout = 300 * time.Millisecond
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	if err := o.startAndCheck(); err != nil {
		t.Fatal(err)
	}
	if o.journal.Status.Phase != "rolledBack" || !o.authorized("stopped.json") {
		t.Fatal("rollback lacked owned daemon shutdown acknowledgment")
	}
}
func TestLiveUncooperativeGUIIsNeverKilledForRollback(t *testing.T) {
	o := fixtureOperation(t)
	t.Setenv("PASEO_UPDATER_FIXTURE_HEALTH", "refuseStop")
	o.healthTimeout = 300 * time.Millisecond
	o.stopTimeout = 100 * time.Millisecond
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	if err := o.launch(true); err != nil {
		t.Fatal(err)
	}
	if err := o.installFailure(errors.New("fixture readiness failure")); err == nil {
		t.Fatal("accepted unsafe rollback")
	}
	if o.journal.Status.Phase != "failed" || !o.journal.Status.RecoveryRequired || exited(o.journal.Launched) {
		t.Fatal("live process was killed or recovery hidden")
	}
	if marker(t, o.request.Destination) != "new" || marker(t, o.journal.Backup) != "old" {
		t.Fatal("live installation was replaced")
	}
}
func TestInterruptedSwapRestoresOldFiles(t *testing.T) {
	o := fixtureOperation(t)
	o.journal.Step = "movingNew"
	if err := os.Rename(o.request.Destination, o.journal.Backup); err != nil {
		t.Fatal(err)
	}
	if err := o.persist(); err != nil {
		t.Fatal(err)
	}
	if err := o.recover(); err != nil {
		t.Fatal(err)
	}
	if o.journal.Status.Phase != "rolledBack" || marker(t, o.request.Destination) != "old" {
		t.Fatal("interrupted rename not recovered")
	}
}
func TestInterruptedUnrecordedLaunchPreservesBothTrees(t *testing.T) {
	o := fixtureOperation(t)
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	o.journal.LaunchPending = true
	o.journal.Step = "starting"
	if err := o.recover(); err == nil {
		t.Fatal("ambiguous launch accepted")
	}
	if !o.journal.Status.RecoveryRequired || marker(t, o.request.Destination) != "new" || marker(t, o.journal.Backup) != "old" {
		t.Fatal("ambiguous launch modified installation")
	}
}
func TestNativeInstallerFailureDoesNotPretendRollback(t *testing.T) {
	o := fixtureOperation(t)
	o.request.Mode = "nativeInstaller"
	o.journal.Step = "nativeStarted"
	if err := o.installFailure(errors.New("installer exit 2")); err == nil {
		t.Fatal("installer failure swallowed")
	}
	if o.journal.Status.Phase != "failed" || !o.journal.Status.RecoveryRequired || marker(t, o.request.Destination) != "old" {
		t.Fatal("installer failure misreported")
	}
}
func TestPreparedReplacementMutationIsRejected(t *testing.T) {
	o := fixtureOperation(t)
	mustWrite(t, filepath.Join(o.journal.Candidate, "marker"), []byte("changed"), 0600)
	if err := o.install(); err == nil {
		t.Fatal("changed replacement installed")
	}
	if marker(t, o.request.Destination) != "old" {
		t.Fatal("old app changed")
	}
}
func TestHandoffRequiresExactOperationAndNonce(t *testing.T) {
	o := fixtureOperation(t)
	if o.authorized("handoff.json") {
		t.Fatal("missing handoff accepted")
	}
	ack := map[string]any{"schemaVersion": 1, "operationId": o.request.OperationID, "nonce": "wrong"}
	if err := atomicJSON(filepath.Join(o.request.OperationDirectory, "handoff.json"), ack); err != nil {
		t.Fatal(err)
	}
	if o.authorized("handoff.json") {
		t.Fatal("wrong nonce accepted")
	}
	ack["nonce"] = o.journal.Status.Nonce
	if err := atomicJSON(filepath.Join(o.request.OperationDirectory, "handoff.json"), ack); err != nil {
		t.Fatal(err)
	}
	if !o.authorized("handoff.json") {
		t.Fatal("valid handoff rejected")
	}
}
func TestProcessBirthIdentityCannotBeReused(t *testing.T) {
	me, err := inspectProcess(os.Getpid())
	if err != nil {
		t.Fatal(err)
	}
	if exited(me.identity) {
		t.Fatal("current process reported dead")
	}
	changed := me.identity
	changed.BirthToken += "-different"
	if !exited(changed) {
		t.Fatal("reused PID accepted as original parent")
	}
}
func TestExclusiveLeaseAndRelease(t *testing.T) {
	path := filepath.Join(canonicalTemp(t), "lease")
	first, err := acquireLease(path)
	if err != nil {
		t.Fatal(err)
	}
	if second, err := acquireLease(path); err == nil {
		second.Close()
		first.Close()
		t.Fatal("duplicate lease accepted")
	}
	first.Close()
	next, err := acquireLease(path)
	if err != nil {
		t.Fatal(err)
	}
	next.Close()
}
func TestDigestAndSymlinkSafety(t *testing.T) {
	root := canonicalTemp(t)
	file := filepath.Join(root, "artifact")
	mustWrite(t, file, []byte("one"), 0600)
	before, err := digest(file)
	if err != nil {
		t.Fatal(err)
	}
	mustWrite(t, file, []byte("two"), 0600)
	after, err := digest(file)
	if err != nil {
		t.Fatal(err)
	}
	if before == after {
		t.Fatal("digest did not bind bytes")
	}
	link := filepath.Join(root, "link")
	if err = os.Symlink(file, link); err != nil {
		if runtime.GOOS == "windows" {
			t.Skip("symlink privilege unavailable")
		}
		t.Fatal(err)
	}
	if _, err = digest(link); err == nil {
		t.Fatal("symlink artifact accepted")
	}
}
func TestArchiveTraversalAndLinkEscapeRejected(t *testing.T) {
	for _, name := range []string{"../escape", "/absolute", "App.app/link"} {
		t.Run(name, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "app.zip")
			f, err := os.Create(path)
			if err != nil {
				t.Fatal(err)
			}
			z := zip.NewWriter(f)
			header := &zip.FileHeader{Name: name}
			header.SetMode(0644)
			if name == "App.app/link" {
				header.SetMode(os.ModeSymlink | 0755)
			}
			member, err := z.CreateHeader(header)
			if err != nil {
				t.Fatal(err)
			}
			_, _ = member.Write([]byte("../../outside"))
			if err = z.Close(); err != nil {
				t.Fatal(err)
			}
			f.Close()
			if _, err = extractApp(path, filepath.Join(dir, "expanded")); err == nil {
				t.Fatal("unsafe archive admitted")
			}
		})
	}
}
func TestTerminalOperationRetryDoesNotInstallAgain(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("Linux request fixture")
	}
	o := fixtureOperation(t)
	t.Setenv("XDG_CACHE_HOME", filepath.Join(t.TempDir(), "cache"))
	o.request.Mode = "nativeInstaller"
	o.request.InstallerFormat = "deb"
	o.request.StagedArtifact = filepath.Join(o.request.OperationDirectory, "artifact.deb")
	mustWrite(t, o.request.StagedArtifact, []byte("not an installer"), 0600)
	sum, err := digest(o.request.StagedArtifact)
	if err != nil {
		t.Fatal(err)
	}
	o.request.SHA512 = sum
	requestPath := filepath.Join(o.request.OperationDirectory, "request.json")
	if err = atomicJSON(requestPath, o.request); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(requestPath)
	if err != nil {
		t.Fatal(err)
	}
	hash := sha512.Sum512(raw)
	o.journal.RequestDigest = hex.EncodeToString(hash[:])
	o.journal.Status.Phase = "committed"
	if err = o.persist(); err != nil {
		t.Fatal(err)
	}
	if err = runRequest(requestPath); err != nil {
		t.Fatal(err)
	}
	if err = runRequest(requestPath); err != nil {
		t.Fatal(err)
	}
	if marker(t, o.request.Destination) != "old" {
		t.Fatal("terminal operation installed again")
	}
	o.request.Expected.ServerID = "another-host"
	if err = atomicJSON(requestPath, o.request); err != nil {
		t.Fatal(err)
	}
	if err = runRequest(requestPath); err == nil {
		t.Fatal("operation ID reused for another host")
	}
}
func TestDigestFailureLeavesOriginalParentAlive(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("Linux request fixture")
	}
	o := fixtureOperation(t)
	t.Setenv("XDG_CACHE_HOME", filepath.Join(t.TempDir(), "cache"))
	parent, _ := fixtureProcess(t, o.request.CurrentExecutable, "idle")
	o.request.ParentProcess = parent
	o.request.Mode = "nativeInstaller"
	o.request.InstallerFormat = "deb"
	o.request.StagedArtifact = filepath.Join(o.request.OperationDirectory, "artifact.deb")
	mustWrite(t, o.request.StagedArtifact, []byte("invalid artifact"), 0600)
	o.request.SHA512 = strings.Repeat("0", 128)
	path := filepath.Join(o.request.OperationDirectory, "request.json")
	if err := atomicJSON(path, o.request); err != nil {
		t.Fatal(err)
	}
	if err := runRequest(path); err == nil {
		t.Fatal("incorrect digest accepted")
	}
	var s status
	if _, err := readJSON(filepath.Join(o.request.OperationDirectory, "status.json"), &s); err != nil {
		t.Fatal(err)
	}
	if s.Ready || s.Phase != "failed" || exited(parent) {
		t.Fatal("preparation failure stopped parent or authorized handoff")
	}
}
func TestRelaunchPreservesHomeAndClearsStaleAppImageState(t *testing.T) {
	r := request{DaemonHome: "/isolated/home", OperationDirectory: "/isolated/op", OperationID: "operation"}
	env := updateEnvironment([]string{"PASEO_HOME=/wrong", "PASEO_UPDATE_NONCE=stale", "APPIMAGE=/old", "APPDIR=/old-mount", "LD_LIBRARY_PATH=/old-libs", "KEEP=present"}, r, "fresh", true)
	text := strings.Join(env, "\n")
	for _, bad := range []string{"/wrong", "stale", "/old"} {
		if strings.Contains(text, bad) {
			t.Fatal("stale relaunch environment retained")
		}
	}
	for _, good := range []string{"PASEO_HOME=/isolated/home", "PASEO_UPDATE_NONCE=fresh", "KEEP=present"} {
		if !strings.Contains(text, good) {
			t.Fatal("required relaunch environment missing")
		}
	}
}

func TestAuthorizationStateRemainsPendingUntilRealInstallerExit(t *testing.T) {
	o := fixtureOperation(t)
	o.request.Mode = "nativeInstaller"
	cmd := exec.Command(o.request.CurrentExecutable)
	cmd.Env = append(os.Environ(), "PASEO_UPDATER_FIXTURE_ROLE=installerFailure")
	done := make(chan error, 1)
	go func() { done <- o.runInstaller(cmd, true) }()
	sawPending := waitUntil(time.Second, 10*time.Millisecond, func() bool {
		var s status
		_, err := readJSON(filepath.Join(o.request.OperationDirectory, "status.json"), &s)
		return err == nil && s.Phase == "waitingForAuthorization" && !s.Ready && s.RecoveryRequired
	})
	err := <-done
	if !sawPending || err == nil {
		t.Fatal("authorization wait or real installer failure was hidden")
	}
	if err = o.installFailure(err); err == nil || o.journal.Status.Phase != "failed" || !o.journal.Status.RecoveryRequired {
		t.Fatal("installer failure falsely committed or rolled back")
	}
}

func TestInterruptedNativeInstallerIsNotReplayed(t *testing.T) {
	o := fixtureOperation(t)
	o.request.Mode = "nativeInstaller"
	o.journal.Step = "nativeStarted"
	if err := o.recover(); err == nil {
		t.Fatal("interrupted native installation accepted")
	}
	if o.journal.Status.Phase != "failed" || !o.journal.Status.RecoveryRequired || marker(t, o.request.Destination) != "old" {
		t.Fatal("interrupted installation changed files")
	}
}

func TestHandoffRequiresBothApprovalAndExactParentExit(t *testing.T) {
	o := fixtureOperation(t)
	o.parentTimeout = 30 * time.Millisecond
	if o.awaitHandoff() {
		t.Fatal("parent exit alone authorized installation")
	}
	path := filepath.Join(o.request.OperationDirectory, "handoff.json")
	if err := atomicJSON(path, map[string]any{"schemaVersion": 1, "operationId": o.request.OperationID, "nonce": o.journal.Status.Nonce}); err != nil {
		t.Fatal(err)
	}
	if !o.awaitHandoff() {
		t.Fatal("approved handoff rejected after parent exit")
	}
	parent, _ := fixtureProcess(t, o.request.CurrentExecutable, "idle")
	o.request.ParentProcess = parent
	if o.awaitHandoff() {
		t.Fatal("approval authorized installation while parent was alive")
	}
	if exited(parent) {
		t.Fatal("helper terminated parent")
	}
}

func TestRequestRejectsUserDataDestinationAndCommandFields(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("Linux request fixture")
	}
	o := fixtureOperation(t)
	r := o.request
	r.Mode = "nativeInstaller"
	r.InstallerFormat = "deb"
	r.StagedArtifact = filepath.Join(r.OperationDirectory, "artifact.deb")
	mustWrite(t, r.StagedArtifact, []byte("fixture"), 0600)
	var err error
	r.SHA512, err = digest(r.StagedArtifact)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(r.OperationDirectory, "request.json")
	if err = atomicJSON(path, r); err != nil {
		t.Fatal(err)
	}
	if err = validateRequest(&r, path); err != nil {
		t.Fatal(err)
	}
	bad := r
	bad.Destination = bad.DaemonHome
	if err = validateRequest(&bad, path); err == nil {
		t.Fatal("user-data destination accepted")
	}
	bad = r
	bad.DaemonHome = bad.CurrentRoot
	if err = validateRequest(&bad, path); err == nil {
		t.Fatal("daemon home overlaps installation")
	}
	bad = r
	bad.StagedArtifact = filepath.Join(r.CurrentRoot, "marker")
	if err = validateRequest(&bad, path); err == nil {
		t.Fatal("artifact outside operation directory accepted")
	}
	mustWrite(t, path, []byte(`{"schemaVersion":1,"command":"unsafe"}`), 0600)
	if _, err = readJSON(path, &bad); err == nil {
		t.Fatal("unknown command field accepted")
	}
}

func TestInterruptedCompletedRestoreIsRecognizedByDigest(t *testing.T) {
	o := fixtureOperation(t)
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(o.request.Destination, filepath.Join(filepath.Dir(o.journal.Backup), "failed")); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(o.journal.Backup, o.request.Destination); err != nil {
		t.Fatal(err)
	}
	o.journal.Step = "rollingBack"
	if err := o.recover(); err != nil {
		t.Fatal(err)
	}
	if o.journal.Status.Phase != "rolledBack" || marker(t, o.request.Destination) != "old" {
		t.Fatal("completed restoration was not recovered")
	}
}

func TestModifiedRecoveryBackupIsNeverRestored(t *testing.T) {
	o := fixtureOperation(t)
	if err := o.install(); err != nil {
		t.Fatal(err)
	}
	mustWrite(t, filepath.Join(o.journal.Backup, "marker"), []byte("modified backup"), 0600)
	if err := o.rollback(errors.New("readiness failed")); err == nil {
		t.Fatal("modified backup accepted")
	}
	if !o.journal.Status.RecoveryRequired || marker(t, o.request.Destination) != "new" {
		t.Fatal("modified backup replaced current application")
	}
}
