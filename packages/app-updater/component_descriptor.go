package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
)

type componentBuildDescriptor struct {
	SchemaVersion    int               `json:"schemaVersion"`
	ID               string            `json:"id"`
	Target           string            `json:"target"`
	Identity         componentIdentity `json:"identity"`
	Executable       string            `json:"executable"`
	ExecutableSHA256 string            `json:"executableSha256"`
	Requires         map[string]uint32 `json:"requires"`
	Licenses         []string          `json:"licenses"`
	Dependencies     map[string]string `json:"dependencies"`
}

func validateComponentDescriptor(root string, c componentRef, complete bool) error {
	p := filepath.Join(root, "component-build-"+c.ID+".json")
	if err := regular(p); err != nil {
		return err
	}
	f, err := os.Open(p)
	if err != nil {
		return err
	}
	data, err := io.ReadAll(io.LimitReader(f, componentInventoryLimit+1))
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	if len(data) > componentInventoryLimit {
		return errors.New("component descriptor exceeds limit")
	}
	if err = unambiguousJSON(data); err != nil {
		return err
	}
	var d componentBuildDescriptor
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err = decoder.Decode(&d); err != nil {
		return err
	}
	if decoder.Decode(new(any)) != io.EOF || d.SchemaVersion != 1 || d.ID != c.ID || d.Target != c.Target || d.Identity != c.Identity || d.Executable != c.Executable || d.ExecutableSHA256 != c.ExecutableSHA256 || !reflect.DeepEqual(d.Requires, c.Requires) {
		return errors.New("embedded component descriptor differs from inventory")
	}
	for p, expected := range d.Dependencies {
		if !relativeComponentPath(p) || !sha256Pattern.MatchString(expected) || p == c.Executable {
			return errors.New("invalid component dependency image")
		}
		actual, e := componentFileHash(filepath.Join(root, filepath.FromSlash(p)))
		if e != nil {
			return e
		}
		if actual != expected {
			return errors.New("component dependency content changed")
		}
	}
	for _, license := range d.Licenses {
		if !relativeComponentPath(license) {
			return errors.New("unsafe component license path")
		}
		if _, ok := d.Dependencies[license]; !ok {
			return errors.New("component license missing content digest")
		}
	}
	if !complete {
		return nil
	}
	return filepath.WalkDir(root, func(p string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if p == root || entry.IsDir() || entry.Type()&os.ModeSymlink != 0 {
			return nil
		}
		rel, err := filepath.Rel(root, p)
		if err != nil {
			return err
		}
		rel = filepath.ToSlash(rel)
		if rel == c.Executable {
			return nil
		}
		if strings.HasPrefix(rel, "component-build-") && strings.HasSuffix(rel, ".json") && !strings.Contains(rel, "/") {
			id := strings.TrimSuffix(strings.TrimPrefix(rel, "component-build-"), ".json")
			if validComponentID(id) {
				return nil
			}
		}
		if _, ok := d.Dependencies[rel]; !ok {
			return errors.New("unlisted file in component object")
		}
		return nil
	})
}
