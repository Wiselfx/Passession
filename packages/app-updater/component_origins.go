package main

import (
	"errors"
	"reflect"
	"strings"
)

func validComponentVersion(version string) bool {
	if len(version) > 128 || !versionPattern.MatchString(version) {
		return false
	}
	withoutBuild, _, _ := strings.Cut(version, "+")
	_, prerelease, present := strings.Cut(withoutBuild, "-")
	if !present {
		return true
	}
	for _, part := range strings.Split(prerelease, ".") {
		if part == "" {
			return false
		}
		numeric := true
		for _, c := range part {
			if c < '0' || c > '9' {
				numeric = false
			}
		}
		if numeric && len(part) > 1 && part[0] == '0' {
			return false
		}
	}
	return true
}
func componentOriginVersion(tag string) (string, error) {
	for _, prefix := range []string{"desktop-windows-v", "desktop-linux-v", "desktop-macos-v", "desktop-v", "android-v", "app-v", "v"} {
		if strings.HasPrefix(tag, prefix) {
			version := strings.TrimPrefix(tag, prefix)
			if validComponentVersion(version) {
				return version, nil
			}
			break
		}
	}
	return "", errors.New("invalid pinned component release tag")
}
func validateComponentOrigins(target componentTarget, targetID, collectionVersion, collectionSource string) error {
	for _, id := range componentIDs {
		c, ok := target.Components[id]
		if !ok {
			return errors.New("origin inventory is incomplete")
		}
		deliveryVersion, err := componentOriginVersion(c.Artifact.OriginTag)
		if err != nil {
			return err
		}
		deliverySource := c.Identity.SourceSHA
		cohortPackage := false
		// A compiled identity is never relabeled to match a freshly signed app
		// container. Only an explicit signed-OS app/tailnet PackageRef can bind
		// that distinct container delivery version/source.
		if strings.HasPrefix(targetID, "macos-") && (id == "app" || id == "tailnet") {
			for _, group := range target.AtomicGroups {
				members := make(map[string]bool)
				for _, member := range group {
					members[member] = true
				}
				if !members["app"] || !members["tailnet"] || !members[id] {
					continue
				}
				for _, pkg := range target.Packages {
					if !reflect.DeepEqual(pkg.Artifact, c.Artifact) {
						continue
					}
					if pkg.Target != targetID || !validComponentVersion(pkg.Version) || pkg.Version != deliveryVersion || !sourcePattern.MatchString(pkg.SourceSHA) {
						return errors.New("invalid signed cohort delivery identity")
					}
					for _, member := range group {
						ref, exists := target.Components[member]
						if !exists || !reflect.DeepEqual(ref.Artifact, c.Artifact) || pkg.Components[member] != ref.Identity {
							return errors.New("signed cohort package does not bind every member")
						}
					}
					if cohortPackage && deliverySource != pkg.SourceSHA {
						return errors.New("ambiguous signed cohort provenance")
					}
					deliverySource = pkg.SourceSHA
					cohortPackage = true
				}
			}
		}
		if !cohortPackage && deliveryVersion != c.Identity.Version {
			return errors.New("component artifact origin relabels compiled identity")
		}
		if collectionVersion != "" && deliveryVersion == collectionVersion && deliverySource != collectionSource {
			return errors.New("current-origin component source does not match collection provenance")
		}
	}
	return nil
}
