//go:build !windows

package main

import "errors"

func inspectAuthenticode(string) (authenticodeIdentity, error) {
	return authenticodeIdentity{}, errors.New("Authenticode verification requires Windows")
}

func checkRegisteredInstallation(string) error {
	return errors.New("registered installation check requires Windows")
}
