package main

import (
	"archive/zip"
	"bytes"
	"crypto/sha512"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

const maxJSON = 64 * 1024

func readJSON(path string, value any) ([]byte, error) {
	if err := regular(path); err != nil {
		return nil, err
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	b, err := io.ReadAll(io.LimitReader(f, maxJSON+1))
	if err != nil {
		return nil, err
	}
	if len(b) > maxJSON {
		return nil, errors.New("JSON exceeds size limit")
	}
	if err = unambiguousJSON(b); err != nil {
		return nil, err
	}
	d := json.NewDecoder(bytes.NewReader(b))
	d.DisallowUnknownFields()
	if err = d.Decode(value); err != nil {
		return nil, err
	}
	if err = d.Decode(new(any)); err != io.EOF {
		return nil, errors.New("trailing JSON content")
	}
	return b, nil
}

func atomicJSON(path string, value any) error {
	b, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if len(b) > maxJSON {
		return errors.New("JSON exceeds size limit")
	}
	f, err := os.CreateTemp(filepath.Dir(path), ".write-")
	if err != nil {
		return err
	}
	temp := f.Name()
	defer os.Remove(temp)
	if err = f.Chmod(0600); err == nil {
		_, err = f.Write(append(b, '\n'))
	}
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = replaceFile(temp, path); err != nil {
		return err
	}
	return syncDirectory(filepath.Dir(path))
}

func within(root, path string) bool {
	r, err := filepath.Rel(root, path)
	return err == nil && r != "." && r != ".." && !strings.HasPrefix(r, ".."+string(filepath.Separator)) && !filepath.IsAbs(r)
}

// Every existing path component must be real. In particular, never follow a
// request-supplied symlink into user data while copying or replacing an app.
func cleanPath(path string) error {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || strings.ContainsAny(path, "\x00\r\n") {
		return errors.New("path must be normalized and absolute")
	}
	for p := path; ; p = filepath.Dir(p) {
		st, err := os.Lstat(p)
		if err != nil {
			return err
		}
		if st.Mode()&os.ModeSymlink != 0 {
			return errors.New("path contains a symbolic link")
		}
		if p == filepath.Dir(p) {
			break
		}
	}
	return nil
}
func regular(path string) error {
	if err := cleanPath(path); err != nil {
		return err
	}
	st, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !st.Mode().IsRegular() {
		return errors.New("expected a regular file")
	}
	return nil
}
func digest(path string) (string, error) {
	if err := regular(path); err != nil {
		return "", err
	}
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha512.New()
	if _, err = io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
func copyFile(source, destination string, mode os.FileMode) error {
	if err := regular(source); err != nil {
		return err
	}
	in, err := os.Open(source)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(destination, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
	if err != nil {
		return err
	}
	_, err = io.Copy(out, in)
	if err == nil {
		err = out.Sync()
	}
	ce := out.Close()
	if err == nil {
		err = ce
	}
	return err
}

// ZIP extraction is bound to the digest-verified archive, not a mutable caller
// supplied expanded directory. Relative framework links may only stay inside it.
func extractApp(archive, directory string) (string, error) {
	z, err := zip.OpenReader(archive)
	if err != nil {
		return "", err
	}
	defer z.Close()
	if err = os.Mkdir(directory, 0700); err != nil {
		return "", err
	}
	var app string
	var total uint64
	type link struct{ path, target string }
	var links []link
	for _, f := range z.File {
		name := strings.TrimSuffix(f.Name, "/")
		if strings.HasPrefix(name, "__MACOSX/") || name == "__MACOSX" {
			continue
		}
		if strings.Contains(name, "\\") || filepath.IsAbs(name) || filepath.ToSlash(filepath.Clean(name)) != name {
			return "", errors.New("unsafe archive path")
		}
		first := strings.Split(name, "/")[0]
		if !strings.HasSuffix(first, ".app") {
			return "", errors.New("archive has non-app root")
		}
		if app == "" {
			app = first
		}
		if app != first {
			return "", errors.New("archive has multiple apps")
		}
		dest := filepath.Join(directory, filepath.FromSlash(name))
		if !within(directory, dest) {
			return "", errors.New("archive traversal")
		}
		if f.UncompressedSize64 > (8<<30)-total {
			return "", errors.New("expanded archive exceeds limit")
		}
		total += f.UncompressedSize64
		if f.FileInfo().IsDir() {
			if err = os.MkdirAll(dest, 0755); err != nil {
				return "", err
			}
			continue
		}
		if err = os.MkdirAll(filepath.Dir(dest), 0755); err != nil {
			return "", err
		}
		input, err := f.Open()
		if err != nil {
			return "", err
		}
		if f.Mode()&os.ModeSymlink != 0 {
			b, e := io.ReadAll(io.LimitReader(input, 4097))
			input.Close()
			if e != nil {
				return "", e
			}
			target := string(b)
			resolved := filepath.Clean(filepath.Join(filepath.Dir(dest), target))
			if len(b) > 4096 || filepath.IsAbs(target) || !within(filepath.Join(directory, app), resolved) {
				return "", errors.New("archive symlink escapes app")
			}
			links = append(links, link{dest, target})
			continue
		}
		if !f.Mode().IsRegular() {
			input.Close()
			return "", errors.New("archive contains special file")
		}
		out, e := os.OpenFile(dest, os.O_WRONLY|os.O_CREATE|os.O_EXCL, f.Mode().Perm()&0755)
		if e != nil {
			input.Close()
			return "", e
		}
		_, e = io.Copy(out, io.LimitReader(input, int64(f.UncompressedSize64)+1))
		input.Close()
		if e == nil {
			e = out.Sync()
		}
		ce := out.Close()
		if e == nil {
			e = ce
		}
		if e != nil {
			return "", e
		}
	}
	// Create links last, so archive entries can never write through a link.
	for _, l := range links {
		if err = os.Symlink(l.target, l.path); err != nil {
			return "", err
		}
	}
	if app == "" {
		return "", errors.New("empty application archive")
	}
	for _, l := range links {
		resolved, e := filepath.EvalSymlinks(l.path)
		if e != nil {
			return "", e
		}
		if !within(filepath.Join(directory, app), resolved) {
			return "", errors.New("resolved archive symlink escapes app")
		}
	}
	return filepath.Join(directory, app), syncDirectory(directory)
}

func copyTree(source, destination string) error {
	return filepath.WalkDir(source, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(source, path)
		if err != nil {
			return err
		}
		dest := filepath.Join(destination, rel)
		if entry.IsDir() {
			return os.Mkdir(dest, 0755)
		}
		if entry.Type()&os.ModeSymlink != 0 {
			target, err := os.Readlink(path)
			if err != nil {
				return err
			}
			if filepath.IsAbs(target) || !within(source, filepath.Clean(filepath.Join(filepath.Dir(path), target))) {
				return fmt.Errorf("unsafe app link")
			}
			return os.Symlink(target, dest)
		}
		st, err := entry.Info()
		if err != nil {
			return err
		}
		if !st.Mode().IsRegular() {
			return errors.New("special file in app")
		}
		return copyFile(path, dest, st.Mode().Perm()&0755)
	})
}

func treeDigest(root string) (string, error) {
	h := sha512.New()
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		relative, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		st, err := entry.Info()
		if err != nil {
			return err
		}
		fmt.Fprintf(h, "%d:%s:%o:%d\\n", len(relative), relative, st.Mode(), st.Size())
		if entry.Type()&os.ModeSymlink != 0 {
			target, err := os.Readlink(path)
			if err != nil {
				return err
			}
			fmt.Fprintf(h, "%d:%s\\n", len(target), target)
			return nil
		}
		if entry.IsDir() {
			return nil
		}
		if !st.Mode().IsRegular() {
			return errors.New("special file in replacement")
		}
		f, err := os.Open(path)
		if err != nil {
			return err
		}
		_, err = io.Copy(h, f)
		ce := f.Close()
		if err == nil {
			err = ce
		}
		return err
	})
	if err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
