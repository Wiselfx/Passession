// passession-update-helper is a short-lived desktop-owned updater, not a service.
package main

import (
	"bytes"
	"crypto/rand"
	"crypto/sha512"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strings"
	"time"
)

type identity struct {
	ServerID  string `json:"serverId"`
	Version   string `json:"version"`
	SourceSHA string `json:"sourceSha"`
}
type processIdentity struct {
	PID        int    `json:"pid"`
	BirthToken string `json:"birthToken"`
}
type request struct {
	SchemaVersion      int             `json:"schemaVersion"`
	OperationID        string          `json:"operationId"`
	OperationDirectory string          `json:"operationDirectory"`
	Mode               string          `json:"mode"`
	InstallerFormat    string          `json:"installerFormat"`
	CurrentExecutable  string          `json:"currentExecutable"`
	CurrentRoot        string          `json:"currentRoot"`
	ParentProcess      processIdentity `json:"parentProcess"`
	StagedArtifact     string          `json:"stagedArtifact"`
	SHA512             string          `json:"sha512"`
	StagedRoot         string          `json:"stagedRoot,omitempty"`
	Expected           identity        `json:"expected"`
	DaemonHome         string          `json:"daemonHome"`
	Destination        string          `json:"destination"`
	RelaunchExecutable string          `json:"relaunchExecutable"`
	RelaunchArgs       []string        `json:"relaunchArgs"`
}
type status struct {
	SchemaVersion    int             `json:"schemaVersion"`
	OperationID      string          `json:"operationId"`
	Phase            string          `json:"phase"`
	Ready            bool            `json:"ready"`
	Expected         identity        `json:"expected"`
	UpdatedAt        string          `json:"updatedAt"`
	Message          string          `json:"message"`
	RecoveryRequired bool            `json:"recoveryRequired"`
	HelperProcess    processIdentity `json:"helperProcess"`
	Nonce            string          `json:"nonce"`
}
type health struct {
	SchemaVersion    int             `json:"schemaVersion"`
	OperationID      string          `json:"operationId"`
	Nonce            string          `json:"nonce"`
	Expected         identity        `json:"expected"`
	DaemonHome       string          `json:"daemonHome"`
	GUIProcess       processIdentity `json:"guiProcess"`
	DaemonProcess    processIdentity `json:"daemonProcess"`
	DaemonExecutable string          `json:"daemonExecutable"`
}
type journal struct {
	SchemaVersion   int             `json:"schemaVersion"`
	RequestDigest   string          `json:"requestDigest"`
	Status          status          `json:"status"`
	Step            string          `json:"step"`
	Candidate       string          `json:"candidate,omitempty"`
	CandidateDigest string          `json:"candidateDigest,omitempty"`
	PreviousDigest  string          `json:"previousDigest,omitempty"`
	Backup          string          `json:"backup,omitempty"`
	Launched        processIdentity `json:"launched"`
	Health          *health         `json:"health,omitempty"`
	LaunchPending   bool            `json:"launchPending"`
}
type operation struct {
	request                                             request
	journal                                             journal
	parentTimeout, healthTimeout, stopTimeout, interval time.Duration
}

var uuidPattern = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)
var digestPattern = regexp.MustCompile(`^[0-9a-f]{128}$`)

func main() {
	if len(os.Args) == 2 && os.Args[1] == "--identity" {
		if err := json.NewEncoder(os.Stdout).Encode(helperIdentity()); err != nil {
			os.Exit(1)
		}
		return
	}
	if len(os.Args) != 3 || os.Args[1] != "--request" {
		os.Exit(2)
	}
	if err := runRequest(os.Args[2]); err != nil {
		fmt.Fprintln(os.Stderr, "Desktop update helper:", err)
		os.Exit(1)
	}
}
func runRequest(path string) error {
	var envelope map[string]json.RawMessage
	if _, err := readJSON(path, &envelope); err != nil {
		return err
	}
	var schema int
	if err := json.Unmarshal(envelope["schemaVersion"], &schema); err != nil {
		return err
	}
	if schema == 2 {
		return runComponentRequest(path)
	}
	var r request
	raw, err := readJSON(path, &r)
	if err != nil {
		return err
	}
	if err = validateRequest(&r, path); err != nil {
		return err
	}
	lease, err := acquireLease(filepath.Join(r.OperationDirectory, "lease"))
	if err != nil {
		return err
	}
	defer lease.Close()
	// Serialize different operation IDs for this exact host as well.
	hostLease, err := acquireLease(filepath.Join(r.DaemonHome, ".desktop-update-install.lock"))
	if err != nil {
		return err
	}
	defer hostLease.Close()
	installLease, err := acquireInstallationLease(r.CurrentRoot)
	if err != nil {
		return err
	}
	defer installLease.Close()
	hash := sha512.Sum512(raw)
	op := operation{request: r, parentTimeout: 120 * time.Second, healthTimeout: 120 * time.Second, stopTimeout: 120 * time.Second, interval: 200 * time.Millisecond}
	journalPath := filepath.Join(r.OperationDirectory, "journal.json")
	if _, err = os.Lstat(journalPath); err == nil {
		if _, err = readJSON(journalPath, &op.journal); err != nil {
			return err
		}
		if op.journal.SchemaVersion != 1 || op.journal.RequestDigest != hex.EncodeToString(hash[:]) || op.journal.Status.OperationID != r.OperationID || op.journal.Status.Expected != r.Expected {
			return errors.New("operation request does not match durable journal")
		}
		sibling := swapDirectory(r)
		if (op.journal.Candidate != "" && op.journal.Candidate != filepath.Join(sibling, "candidate")) || (op.journal.Backup != "" && op.journal.Backup != filepath.Join(sibling, "previous")) {
			return errors.New("journal replacement paths do not belong to this operation")
		}
		if terminal(op.journal.Status.Phase) {
			return op.settleTerminal()
		}
		me, err := inspectProcess(os.Getpid())
		if err != nil {
			return err
		}
		op.journal.Status.HelperProcess = me.identity
		return op.recover()
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	me, err := inspectProcess(os.Getpid())
	if err != nil {
		return err
	}
	nonce := make([]byte, 32)
	if _, err = rand.Read(nonce); err != nil {
		return err
	}
	op.journal = journal{SchemaVersion: 1, RequestDigest: hex.EncodeToString(hash[:]), Step: "preparing", Status: status{SchemaVersion: 1, OperationID: r.OperationID, Expected: r.Expected, HelperProcess: me.identity, Nonce: hex.EncodeToString(nonce)}}
	if err = op.phase("preparation", "Validating the local application update.", false); err != nil {
		return err
	}
	if err = op.prepare(); err != nil {
		message := "Update preparation failed; the running application was not stopped."
		if errors.Is(err, os.ErrPermission) {
			message = "The operating system denied access to this installation. An authorized native installer is required; the running application was not stopped."
		}
		return op.fail(message, false, err)
	}
	if err = op.phase("waitingForParent", "Prepared; waiting for the owning application to stop its daemon and exit.", false); err != nil {
		return err
	}
	if !op.awaitHandoff() {
		return op.fail("No authorized application handoff completed; update cancelled before installation.", false, nil)
	}
	// The staged bytes may have changed while the GUI was draining sessions.
	if actual, e := digest(r.StagedArtifact); e != nil || actual != r.SHA512 {
		return op.fail("Staged artifact changed after preparation.", false, e)
	}
	if err = op.install(); err != nil {
		return op.installFailure(err)
	}
	return op.startAndCheck()
}
func terminal(phase string) bool {
	return phase == "committed" || phase == "failed" || phase == "rolledBack"
}

// The replaced application and helper staging exist only while the update can
// roll back. Failed and rolled-back operations keep them for recovery.
func swapDirectory(r request) string {
	return filepath.Join(filepath.Dir(r.Destination), ".passession-update-"+r.OperationID)
}

// Re-running a committed operation also discards material an older helper
// retained after commit.
func (o *operation) settleTerminal() error {
	if o.journal.Status.Phase == "committed" {
		if o.journal.Backup != "" || o.journal.Candidate != "" {
			o.journal.Backup, o.journal.Candidate = "", ""
			if err := o.persist(); err != nil {
				return err
			}
		}
		o.discardReplaced()
	}
	return atomicJSON(filepath.Join(o.request.OperationDirectory, "status.json"), o.journal.Status)
}
func (o *operation) commit(message string) error {
	o.journal.Step = "committed"
	o.journal.Backup, o.journal.Candidate = "", ""
	if err := o.phase("committed", message, false); err != nil {
		return err
	}
	o.discardReplaced()
	return nil
}

// Best-effort: the update is already durably committed, so a removal failure is
// only reported. Native installers own no swap directory; their staged artifact
// is caller input that binds the request identity.
func (o *operation) discardReplaced() {
	paths := []string{filepath.Join(o.request.OperationDirectory, "expanded-helper")}
	if o.request.Mode != "nativeInstaller" {
		paths = append(paths, swapDirectory(o.request))
	}
	for _, path := range paths {
		if err := removeTree(path); err != nil {
			fmt.Fprintln(os.Stderr, "Desktop update helper: warning: cannot remove", path+":", err)
		}
	}
}
func (o *operation) persist() error {
	return atomicJSON(filepath.Join(o.request.OperationDirectory, "journal.json"), o.journal)
}
func (o *operation) phase(phase, message string, recovery bool) error {
	o.journal.Status.Phase = phase
	o.journal.Status.Ready = phase == "waitingForParent"
	o.journal.Status.Message = message
	o.journal.Status.RecoveryRequired = recovery
	o.journal.Status.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	if err := o.persist(); err != nil {
		return err
	}
	return atomicJSON(filepath.Join(o.request.OperationDirectory, "status.json"), o.journal.Status)
}
func (o *operation) fail(message string, recovery bool, cause error) error {
	if err := o.phase("failed", message, recovery); err != nil {
		return err
	}
	if cause != nil {
		return cause
	}
	return errors.New(message)
}
func (o *operation) awaitHandoff() bool {
	return waitUntil(o.parentTimeout, o.interval, func() bool { return o.authorized("handoff.json") && exited(o.request.ParentProcess) })
}
func (o *operation) authorized(name string) bool {
	path := filepath.Join(o.request.OperationDirectory, name)
	if err := privatePath(path, false); err != nil {
		return false
	}
	var ack struct {
		SchemaVersion int    `json:"schemaVersion"`
		OperationID   string `json:"operationId"`
		Nonce         string `json:"nonce"`
	}
	_, err := readJSON(path, &ack)
	return err == nil && ack.SchemaVersion == 1 && ack.OperationID == o.request.OperationID && ack.Nonce == o.journal.Status.Nonce
}
func validateRequest(r *request, path string) error {
	if r.SchemaVersion != 1 || !uuidPattern.MatchString(r.OperationID) || !digestPattern.MatchString(r.SHA512) {
		return errors.New("invalid request identity or digest")
	}
	if r.Expected.ServerID == "" || r.Expected.Version == "" || r.Expected.SourceSHA == "" || len(r.Expected.ServerID) > 256 || len(r.Expected.Version) > 128 || len(r.Expected.SourceSHA) > 128 {
		return errors.New("missing expected host/release identity")
	}
	if r.ParentProcess.PID <= 1 || r.ParentProcess.BirthToken == "" || len(r.RelaunchArgs) != 0 {
		return errors.New("invalid parent identity or relaunch arguments")
	}
	if r.Destination != r.CurrentRoot {
		return errors.New("destination must be the installed app root")
	}
	if filepath.Join(r.OperationDirectory, "request.json") != path {
		return errors.New("request must be operationDirectory/request.json")
	}
	for _, p := range []string{r.OperationDirectory, r.StagedArtifact, r.DaemonHome} {
		if err := cleanPath(p); err != nil {
			return err
		}
	}
	if err := privatePath(r.OperationDirectory, true); err != nil {
		return err
	}
	if err := privatePath(path, false); err != nil {
		return err
	}
	if err := ownedPath(r.DaemonHome); err != nil {
		return err
	}
	if !within(r.OperationDirectory, r.StagedArtifact) || within(r.CurrentRoot, r.OperationDirectory) || within(r.CurrentRoot, r.DaemonHome) || r.CurrentRoot == r.DaemonHome {
		return errors.New("staging or daemon data overlaps installation")
	}
	if filepath.Dir(r.CurrentRoot) == r.CurrentRoot || !filepath.IsAbs(r.CurrentRoot) || filepath.Clean(r.CurrentRoot) != r.CurrentRoot {
		return errors.New("unsafe installation root")
	}
	if r.StagedRoot != "" && !within(r.OperationDirectory, r.StagedRoot) {
		return errors.New("expanded application outside staging")
	}
	switch r.Mode {
	case "nativeInstaller":
		if (runtime.GOOS == "windows" && r.InstallerFormat == "exe") || (runtime.GOOS == "linux" && (r.InstallerFormat == "deb" || r.InstallerFormat == "rpm")) {
		} else {
			return errors.New("unsupported native installer")
		}
		if filepath.Dir(r.CurrentExecutable) != r.CurrentRoot || r.RelaunchExecutable != r.CurrentExecutable {
			return errors.New("GUI is not directly inside the installed root")
		}
	case "directorySwap":
		if runtime.GOOS != "darwin" || r.InstallerFormat != "zip" || !strings.HasSuffix(r.CurrentRoot, ".app") || filepath.Dir(r.CurrentExecutable) != filepath.Join(r.CurrentRoot, "Contents", "MacOS") || r.RelaunchExecutable != r.CurrentExecutable {
			return errors.New("invalid app bundle update")
		}
	case "appImageSwap":
		if runtime.GOOS != "linux" || r.InstallerFormat != "appimage" || r.RelaunchExecutable != r.Destination {
			return errors.New("invalid AppImage update")
		}
	default:
		return errors.New("unknown update mode")
	}
	return nil
}
func (o *operation) prepare() error {
	r := o.request
	for _, p := range []string{r.CurrentRoot, r.CurrentExecutable, r.StagedArtifact} {
		if err := cleanPath(p); err != nil {
			return err
		}
	}
	parent, err := inspectProcess(r.ParentProcess.PID)
	if err != nil {
		return err
	}
	if parent.identity != r.ParentProcess || !samePath(parent.executable, r.CurrentExecutable) {
		return errors.New("parent identity does not match installed GUI")
	}
	if err = ownedProcess(parent); err != nil {
		return err
	}
	if r.Mode == "appImageSwap" {
		if err = validateAppImageProcess(parent, r.CurrentRoot); err != nil {
			return err
		}
	}
	if actual, e := digest(r.StagedArtifact); e != nil {
		return e
	} else if actual != r.SHA512 {
		return errors.New("artifact digest mismatch")
	}
	if err = validateNative(r); err != nil {
		return err
	}
	if r.Mode == "nativeInstaller" {
		return nil
	}
	o.journal.PreviousDigest, err = treeDigest(r.CurrentRoot)
	if err != nil {
		return err
	}
	sibling := swapDirectory(r)
	if err = os.Mkdir(sibling, 0700); err != nil {
		return err
	}
	if err = syncDirectory(filepath.Dir(sibling)); err != nil {
		return err
	}
	o.journal.Candidate = filepath.Join(sibling, "candidate")
	o.journal.Backup = filepath.Join(sibling, "previous")
	if err = o.persist(); err != nil {
		return err
	}
	if r.Mode == "directorySwap" {
		root, e := extractApp(r.StagedArtifact, filepath.Join(r.OperationDirectory, "expanded-helper"))
		if e != nil {
			return e
		}
		if e = validateMacApp(r.CurrentRoot, root, r.Expected.Version); e != nil {
			return e
		}
		if e = copyTree(root, o.journal.Candidate); e != nil {
			return e
		}
	} else {
		if err = copyFile(r.StagedArtifact, o.journal.Candidate, 0755); err != nil {
			return err
		}
	}
	if err = syncDirectory(sibling); err != nil {
		return err
	}
	o.journal.CandidateDigest, err = treeDigest(o.journal.Candidate)
	if err != nil {
		return err
	}
	return o.persist()
}
func (o *operation) install() error {
	if err := cleanPath(o.request.Destination); err != nil {
		return err
	}
	if o.request.Mode != "nativeInstaller" {
		current, err := treeDigest(o.journal.Candidate)
		if err != nil {
			return err
		}
		if current != o.journal.CandidateDigest {
			return errors.New("prepared replacement changed before installation")
		}
	}
	if o.request.Mode == "nativeInstaller" {
		cmd, err := nativeInstallerCommand(o.request)
		if err != nil {
			return err
		}
		return o.runInstaller(cmd, runtime.GOOS == "linux")
	}
	previous, err := treeDigest(o.request.Destination)
	if err != nil {
		return err
	}
	if o.journal.PreviousDigest == "" || previous != o.journal.PreviousDigest {
		return errors.New("installed application changed after preparation")
	}
	o.journal.Step = "movingOld"
	if err := o.phase("installing", "Replacing the application; previous files are retained for recovery.", true); err != nil {
		return err
	}
	if err := os.Rename(o.request.Destination, o.journal.Backup); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(o.request.Destination)); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(o.journal.Backup)); err != nil {
		return err
	}
	o.journal.Step = "movingNew"
	if err := o.persist(); err != nil {
		return err
	}
	if err := os.Rename(o.journal.Candidate, o.request.Destination); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(o.request.Destination)); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(o.journal.Backup)); err != nil {
		return err
	}
	o.journal.Step = "installed"
	return o.persist()
}

// The authorization grant is internal to pkexec. Until it exits, report the
// combined authorization/package-manager wait rather than guessing consent.
func (o *operation) runInstaller(cmd *exec.Cmd, authorization bool) error {
	o.journal.Step = "nativeStarted"
	phase := "installing"
	message := "Installing the verified native application."
	if authorization {
		phase = "waitingForAuthorization"
		message = "Waiting for system authorization and package-manager completion."
	}
	if err := o.phase(phase, message, true); err != nil {
		return err
	}
	return cmd.Run()
}
func (o *operation) launch(update bool) error {
	cmd := exec.Command(o.request.RelaunchExecutable)
	cmd.Dir = filepath.Dir(o.request.RelaunchExecutable)
	cmd.Env = updateEnvironment(os.Environ(), o.request, o.journal.Status.Nonce, update)
	detach(cmd)
	o.journal.LaunchPending = true
	if err := o.persist(); err != nil {
		return err
	}
	if err := cmd.Start(); err != nil {
		o.journal.LaunchPending = false
		_ = o.persist()
		return err
	}
	// Do not Wait: the GUI outlives the helper. Process identity is durably bound
	// before checking health; recovery never guesses a reused PID.
	p, err := inspectProcess(cmd.Process.Pid)
	if err != nil {
		_ = cmd.Process.Release()
		return err
	}
	o.journal.Launched = p.identity
	o.journal.LaunchPending = false
	_ = cmd.Process.Release()
	return o.persist()
}
func updateEnvironment(base []string, r request, nonce string, update bool) []string {
	out := make([]string, 0, len(base)+4)
	for _, v := range base {
		key, _, _ := strings.Cut(v, "=")
		if key == "PASEO_HOME" || strings.HasPrefix(key, "PASEO_UPDATE_") || key == "APPIMAGE" || key == "APPDIR" || key == "LD_LIBRARY_PATH" || key == "LD_PRELOAD" {
			continue
		}
		out = append(out, v)
	}
	out = append(out, "PASEO_HOME="+r.DaemonHome)
	if update {
		out = append(out, "PASEO_UPDATE_OPERATION_DIRECTORY="+r.OperationDirectory, "PASEO_UPDATE_OPERATION_ID="+r.OperationID, "PASEO_UPDATE_NONCE="+nonce)
	}
	return out
}
func (o *operation) startAndCheck() error {
	o.journal.Step = "starting"
	if err := o.phase("starting", "Starting the updated application with the original daemon home.", true); err != nil {
		return err
	}
	if err := o.launch(true); err != nil {
		return o.installFailure(err)
	}
	o.journal.Step = "checking"
	if err := o.phase("checking", "Waiting for matching live GUI and owned daemon identity.", true); err != nil {
		return err
	}
	if waitUntil(o.healthTimeout, o.interval, func() bool { return o.checkHealth() == nil }) {
		return o.commit("Updated application and daemon identities confirmed.")
	}
	return o.installFailure(errors.New("updated application health deadline expired"))
}
func (o *operation) checkHealth() error {
	var h health
	path := filepath.Join(o.request.OperationDirectory, "health.json")
	if err := privatePath(path, false); err != nil {
		return err
	}
	if _, err := readJSON(path, &h); err != nil {
		return err
	}
	if h.SchemaVersion != 1 || h.OperationID != o.request.OperationID || h.Nonce != o.journal.Status.Nonce || h.Expected != o.request.Expected || h.DaemonHome != o.request.DaemonHome || h.DaemonProcess.PID == h.GUIProcess.PID {
		return errors.New("health identity mismatch")
	}
	if h.GUIProcess != o.journal.Launched && (o.request.Mode != "appImageSwap" || !appImageDescendant(h.GUIProcess, o.journal.Launched)) {
		return errors.New("GUI is not the launched process or its AppImage child")
	}
	gui, err := inspectProcess(h.GUIProcess.PID)
	if err != nil {
		return err
	}
	daemon, err := inspectProcess(h.DaemonProcess.PID)
	if err != nil {
		return err
	}
	if gui.identity != h.GUIProcess || daemon.identity != h.DaemonProcess || !samePath(daemon.executable, h.DaemonExecutable) {
		return errors.New("health processes changed")
	}
	if err = ownedProcess(gui); err != nil {
		return err
	}
	if err = ownedProcess(daemon); err != nil {
		return err
	}
	root := o.request.Destination
	if o.request.Mode == "appImageSwap" {
		// AppImage execs its GUI from a mount. Bind daemon to that exact live mount,
		// never to the old GUI's (now unmounted) staging path.
		root = filepath.Dir(gui.executable)
		if filepath.Base(gui.executable) != filepath.Base(o.request.CurrentExecutable) {
			return errors.New("unexpected AppImage GUI")
		}
		if err = validateAppImageProcess(gui, o.request.Destination); err != nil {
			return err
		}
	} else if !samePath(gui.executable, o.request.RelaunchExecutable) {
		return errors.New("unexpected relaunched GUI")
	}
	if !within(root, daemon.executable) {
		return errors.New("daemon executable is not bundled with live GUI")
	}
	o.journal.Health = &h
	return o.persist()
}
func (o *operation) stopUpdated() bool {
	if o.journal.LaunchPending {
		return false
	}
	if o.journal.Launched.PID == 0 {
		return true
	}
	// Even an unhealthy GUI receives the stop request; it must drain/guard its own
	// daemon. We never send signals to an unproven daemon or force-kill a GUI.
	if err := atomicJSON(filepath.Join(o.request.OperationDirectory, "stop.json"), map[string]any{"schemaVersion": 1, "operationId": o.request.OperationID, "nonce": o.journal.Status.Nonce}); err != nil {
		return false
	}
	return waitUntil(o.stopTimeout, o.interval, func() bool {
		if !exited(o.journal.Launched) {
			return false
		}
		if o.journal.Health != nil && (!exited(o.journal.Health.DaemonProcess) || !exited(o.journal.Health.GUIProcess)) {
			return false
		}
		// stop acknowledgment is emitted only after the exact owned daemon stopped.
		return o.authorized("stopped.json")
	})
}
func (o *operation) installFailure(cause error) error {
	if o.request.Mode == "nativeInstaller" {
		message := "Native installation or readiness failed; automatic rollback is unavailable. Retained journal requires recovery."
		var exit *exec.ExitError
		if errors.As(cause, &exit) {
			message = fmt.Sprintf("Native installer exited with code %d; installation was not confirmed and automatic rollback is unavailable.", exit.ExitCode())
			if runtime.GOOS == "linux" && (exit.ExitCode() == 126 || exit.ExitCode() == 127) {
				message = fmt.Sprintf("System authorization was cancelled or unavailable, or the privileged installer could not start (exit %d). Installation was not confirmed; inspect package state before retrying.", exit.ExitCode())
			}
		}
		return o.fail(message, true, cause)
	}
	if !o.stopUpdated() {
		return o.fail("Updated application did not stop safely; previous application is retained for recovery.", true, cause)
	}
	return o.rollback(cause)
}
func (o *operation) rollback(cause error) error {
	if !exited(o.request.ParentProcess) {
		return o.fail("Original application is still running; recovery was not attempted.", true, cause)
	}
	backup := o.journal.Backup
	if backup == "" {
		return o.fail("No replacement was installed.", false, cause)
	}
	if _, err := os.Lstat(backup); errors.Is(err, os.ErrNotExist) {
		actual, e := treeDigest(o.request.Destination)
		if e != nil || o.journal.PreviousDigest == "" || actual != o.journal.PreviousDigest {
			return o.fail("Installation changed before replacement; preserving it for recovery.", true, e)
		}
		if e = o.launch(false); e != nil {
			return o.fail("Installation was not replaced, but original relaunch failed.", true, e)
		}
		return o.fail("Update failed before replacement; the unchanged application was relaunched.", false, cause)
	} else if err != nil {
		return o.fail("Cannot inspect recovery backup.", true, err)
	}
	previous, err := treeDigest(backup)
	if err != nil || o.journal.PreviousDigest == "" || previous != o.journal.PreviousDigest {
		return o.fail("Recovery backup changed; refusing to restore unverified files.", true, err)
	}
	o.journal.Step = "rollingBack"
	if err := o.persist(); err != nil {
		return err
	}
	if _, err := os.Lstat(o.request.Destination); err == nil {
		failed := filepath.Join(filepath.Dir(backup), "failed")
		if _, err = os.Lstat(failed); !errors.Is(err, os.ErrNotExist) {
			return o.fail("Recovery destination already exists; preserving both applications.", true, err)
		}
		if err = os.Rename(o.request.Destination, failed); err != nil {
			return o.fail("Cannot move failed replacement for recovery.", true, err)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := os.Rename(backup, o.request.Destination); err != nil {
		return o.fail("Cannot restore previous application.", true, err)
	}
	if err := syncDirectory(filepath.Dir(o.request.Destination)); err != nil {
		return err
	}
	if err := syncDirectory(filepath.Dir(backup)); err != nil {
		return err
	}
	o.journal.Step = "restored"
	o.journal.Launched = processIdentity{}
	if err := o.persist(); err != nil {
		return err
	}
	if err := o.launch(false); err != nil {
		return o.fail("Previous application files restored, but relaunch failed.", true, err)
	}
	return o.phase("rolledBack", "Previous application files restored and relaunched; update was not committed.", false)
}
func (o *operation) recover() error {
	if o.journal.LaunchPending {
		return o.fail("Process launch was interrupted before identity was recorded; preserving both applications for recovery.", true, nil)
	}
	if o.journal.Step == "rollingBack" {
		if _, err := os.Lstat(o.journal.Backup); errors.Is(err, os.ErrNotExist) {
			actual, e := treeDigest(o.request.Destination)
			if e != nil || o.journal.PreviousDigest == "" || actual != o.journal.PreviousDigest {
				return o.fail("Interrupted restoration could not be proven; preserving files for recovery.", true, e)
			}
			o.journal.Step = "restored"
			o.journal.Launched = processIdentity{}
			if err = o.persist(); err != nil {
				return err
			}
		}
	}
	switch o.journal.Step {
	case "preparing":
		return o.fail("Helper interrupted before handoff; no installation was started.", false, nil)
	case "nativeStarted":
		return o.fail("Native installer was interrupted; inspect system package state before retrying.", true, nil)
	case "committed":
		return o.commit("Updated application and daemon identities confirmed.")
	case "restored":
		if o.journal.Launched.PID != 0 && !exited(o.journal.Launched) {
			return o.phase("rolledBack", "Previous application was restored and is running.", false)
		}
		if err := o.launch(false); err != nil {
			return o.fail("Previous application restored but relaunch failed.", true, err)
		}
		return o.phase("rolledBack", "Previous application restored and relaunched.", false)
	case "movingOld", "movingNew", "installed", "starting", "checking", "rollingBack":
		if o.journal.Launched.PID != 0 && o.checkHealth() == nil {
			return o.commit("Recovered matching live application and daemon.")
		}
		if o.request.Mode == "nativeInstaller" {
			return o.fail("Native update readiness interrupted; recovery required.", true, nil)
		}
		if !o.stopUpdated() {
			return o.fail("Interrupted update has live processes; recovery deferred without killing them.", true, nil)
		}
		return o.rollback(errors.New("interrupted replacement"))
	default:
		return o.fail("Unknown recovery journal state; no files changed.", true, nil)
	}
}
func waitUntil(timeout, interval time.Duration, predicate func() bool) bool {
	deadline := time.Now().Add(timeout)
	for {
		if predicate() {
			return true
		}
		if time.Now().After(deadline) {
			return false
		}
		time.Sleep(interval)
	}
}
func normalizedWindowsPath(path string) string {
	path = filepath.Clean(path)
	if strings.HasPrefix(strings.ToUpper(path), `\\?\UNC\`) {
		return `\\` + path[8:]
	}
	if strings.HasPrefix(path, `\\?\`) {
		return path[4:]
	}
	return path
}
func samePath(a, b string) bool {
	if runtime.GOOS == "windows" {
		return strings.EqualFold(normalizedWindowsPath(a), normalizedWindowsPath(b))
	}
	return a == b
}
func commandOutput(path string, args ...string) (string, error) {
	cmd := exec.Command(path, args...)
	cmd.Env = append(os.Environ(), "LC_ALL=C")
	hideWindow(cmd)
	var out bytes.Buffer
	cmd.Stdout = &out
	if err := cmd.Run(); err != nil {
		return "", err
	}
	if out.Len() > maxJSON {
		return "", errors.New("native metadata exceeds limit")
	}
	return strings.TrimSpace(out.String()), nil
}
